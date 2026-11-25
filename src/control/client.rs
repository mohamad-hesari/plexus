use std::{
  io::{BufRead, Write},
  path::{Path, PathBuf},
};

use tokio::{
  io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
  net::{TcpListener, TcpStream},
};

use super::{
  protocol::{
    CommandInfo, CommandResult, Envelope, ForwardSelector, PROTOCOL_VERSION, Reply, Request, read_line, write_line,
  },
  transport::{BoxConn, LocalAddr, TOKEN_FILE, connect_local, connect_tcp, control_dir, find_local, local_addr_at},
};
use crate::{
  cli::{ClientAction, ClientArgs},
  task_managerv2::TaskStatus,
};

pub const EXIT_UNREACHABLE: i32 = 2;
const CHILD_ENV: &str = "PLEXUS_FORWARD_CHILD";

type Reader = BufReader<ReadHalf<BoxConn>>;
type Writer = WriteHalf<BoxConn>;

#[derive(Debug, Clone)]
enum Endpoint {
  Local(LocalAddr),
  Tcp { addr: String, token: String },
}

struct Failure {
  code: i32,
  message: String,
}

impl Failure {
  fn unreachable(message: impl Into<String>) -> Self {
    Self {
      code: EXIT_UNREACHABLE,
      message: message.into(),
    }
  }

  fn failed(message: impl Into<String>) -> Self {
    Self {
      code: 1,
      message: message.into(),
    }
  }
}

fn find_token(start: &Path) -> Option<PathBuf> {
  start
    .ancestors()
    .map(|d| control_dir(d).join(TOKEN_FILE))
    .find(|p| p.exists())
}

fn endpoint(args: &ClientArgs) -> Result<Endpoint, Failure> {
  let cwd = std::env::current_dir().map_err(|e| Failure::unreachable(e.to_string()))?;
  let start = args.root.as_ref().map(PathBuf::from).unwrap_or(cwd);
  if let Some(addr) = &args.tcp {
    let file = args
      .token_file
      .as_ref()
      .map(PathBuf::from)
      .or_else(|| find_token(&start))
      .ok_or_else(|| {
        Failure::unreachable(format!(
          "--tcp needs a token: pass --token-file, or run from inside the project so node_modules/{} is found",
          TOKEN_FILE
        ))
      })?;
    let token = std::fs::read_to_string(&file)
      .map_err(|e| Failure::unreachable(format!("cannot read token file {}: {}", file.display(), e)))?;
    return Ok(Endpoint::Tcp {
      addr: addr.clone(),
      token: token.trim().to_string(),
    });
  }
  if let Some(s) = &args.socket {
    return Ok(Endpoint::Local(if cfg!(windows) {
      LocalAddr::Pipe(s.clone())
    } else {
      LocalAddr::Socket(PathBuf::from(s))
    }));
  }
  let found = if args.root.is_some() {
    local_addr_at(&start)
  } else {
    find_local(&start).map(|(_, a)| a)
  };
  found.map(Endpoint::Local).ok_or_else(|| {
    Failure::unreachable(format!(
      "no running plexus found from {} upwards (looked for node_modules/{}); start one with `plexus run -W ...`",
      start.display(),
      if cfg!(windows) {
        super::transport::NAME_FILE
      } else {
        super::transport::SOCK_FILE
      }
    ))
  })
}

async fn open(ep: &Endpoint, req: Request) -> Result<(Reader, Writer), Failure> {
  let (conn, token) = match ep {
    Endpoint::Local(addr) => {
      let conn = connect_local(addr).await.map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::PermissionDenied {
          "\nhint: the socket mode (plexus run --socket-mode) does not let this user connect. Inside an \
           unprivileged container the mount must map your uid (incus: shift=true or raw.idmap)."
        } else if e.kind() == std::io::ErrorKind::ConnectionRefused || e.kind() == std::io::ErrorKind::NotFound {
          "\nhint: the socket file is stale; the plexus that created it is no longer running."
        } else {
          ""
        };
        Failure::unreachable(format!("cannot connect to {}: {}{}", addr, e, hint))
      })?;
      (conn, None)
    }
    Endpoint::Tcp { addr, token } => {
      let conn = connect_tcp(addr)
        .await
        .map_err(|e| Failure::unreachable(format!("cannot connect to {}: {}", addr, e)))?;
      (conn, Some(token.clone()))
    }
  };
  let (r, mut w) = tokio::io::split(conn);
  let env = Envelope {
    v: PROTOCOL_VERSION,
    token,
    req,
  };
  write_line(&mut w, &env)
    .await
    .map_err(|e| Failure::unreachable(e.to_string()))?;
  Ok((BufReader::new(r), w))
}

async fn next(r: &mut Reader) -> Result<Option<(Reply, String)>, Failure> {
  let mut raw = String::new();
  use tokio::io::AsyncBufReadExt;
  let n = r
    .read_line(&mut raw)
    .await
    .map_err(|e| Failure::failed(format!("connection lost: {}", e)))?;
  if n == 0 {
    return Ok(None);
  }
  let reply: Reply = serde_json::from_str(&raw).map_err(|e| Failure::failed(format!("bad reply: {}", e)))?;
  Ok(Some((reply, raw.trim_end().to_string())))
}

fn status_name(s: &TaskStatus) -> String {
  s.to_string().trim_start_matches("TS").to_string()
}

fn print_log(target: &str, line: &str, prefix: bool) {
  let (text, err) = if let Some(t) = line.strip_prefix("[ERR]: ") {
    (t, true)
  } else if let Some(t) = line.strip_prefix("[OUT]: ") {
    (t, false)
  } else {
    (line, false)
  };
  let text = if prefix {
    format!("[{}] {}", target, text)
  } else {
    text.to_string()
  };
  if err {
    eprintln!("{}", text);
  } else {
    println!("{}", text);
  }
}

fn print_commands(commands: &[CommandInfo], only_ports: bool) {
  let rows: Vec<[String; 5]> = commands
    .iter()
    .filter(|c| !only_ports || !c.ports.is_empty())
    .map(|c| {
      let ports = c
        .ports
        .iter()
        .map(|p| format!("{}{}", p.addr, if p.alive { "" } else { " (not answering)" }))
        .collect::<Vec<_>>()
        .join(", ");
      let status = match &c.blocked_by {
        Some(dep) => format!("{} (blocked: {} failed)", status_name(&c.status), dep),
        None => status_name(&c.status),
      };
      [
        c.package.clone(),
        c.command.clone(),
        status,
        c.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
        if ports.is_empty() { "-".into() } else { ports },
      ]
    })
    .collect();
  if rows.is_empty() {
    println!(
      "{}",
      if only_ports {
        "no listening ports discovered"
      } else {
        "no commands loaded"
      }
    );
    return;
  }
  let header = ["PACKAGE", "COMMAND", "STATUS", "PID", "PORTS"];
  let mut widths = header.map(str::len);
  for row in &rows {
    for (w, cell) in widths.iter_mut().zip(row) {
      *w = (*w).max(cell.len());
    }
  }
  let line = |cells: [&str; 5]| {
    let mut s = String::new();
    for (i, c) in cells.iter().enumerate() {
      if i == 4 {
        s.push_str(c);
      } else {
        s.push_str(&format!("{:<w$}  ", c, w = widths[i]));
      }
    }
    println!("{}", s.trim_end());
  };
  line(header);
  for row in &rows {
    line([&row[0], &row[1], &row[2], &row[3], &row[4]]);
  }
}

fn results_code(results: &[CommandResult], ok: impl Fn(&CommandResult) -> bool) -> i32 {
  if results.iter().all(ok) { 0 } else { 1 }
}

fn print_results(results: &[CommandResult]) {
  for r in results {
    match &r.note {
      Some(n) => eprintln!("{}: {} ({})", r.target, status_name(&r.status), n),
      None => eprintln!("{}: {}", r.target, status_name(&r.status)),
    }
  }
}

/// Reads replies to the end, printing as it goes, and turns the last one into an exit code.
async fn drain(mut r: Reader, json: bool, ok: impl Fn(&CommandResult) -> bool) -> Result<i32, Failure> {
  let mut targets = std::collections::HashSet::new();
  loop {
    let Some((reply, raw)) = next(&mut r).await? else {
      return Err(Failure::failed("plexus closed the connection"));
    };
    if json {
      println!("{}", raw);
    }
    match reply {
      Reply::Error { message } => return Err(Failure::failed(message)),
      Reply::Log { target, line } => {
        if json {
          continue;
        }
        targets.insert(target.clone());
        print_log(&target, &line, targets.len() > 1);
      }
      Reply::Ok => return Ok(0),
      Reply::Done { results } => {
        if !json {
          print_results(&results);
        }
        return Ok(results_code(&results, ok));
      }
      Reply::Packages { packages } => {
        if !json {
          for p in &packages {
            println!("{}  ({})", p.name, p.path);
            for c in &p.commands {
              println!("  {}", c);
            }
          }
        }
        return Ok(0);
      }
      Reply::Forwards { forwards } => {
        if !json {
          if forwards.is_empty() {
            println!("no active forwards");
          }
          for f in &forwards {
            println!(
              "{}  {} -> {}:{}  since {}",
              f.id,
              f.listen,
              f.package,
              f.port,
              f.since.format("%H:%M:%S")
            );
          }
        }
        return Ok(0);
      }
      Reply::Stopped { count } => {
        if !json {
          println!("stopped {} forward{}", count, if count == 1 { "" } else { "s" });
        }
        return Ok(if count == 0 { 1 } else { 0 });
      }
      other => return Err(Failure::failed(format!("unexpected reply: {:?}", other))),
    }
  }
}

async fn status(ep: &Endpoint, target: Option<String>, json: bool, only_ports: bool) -> Result<i32, Failure> {
  let (mut r, _w) = open(ep, Request::Status { target }).await?;
  match next(&mut r).await? {
    Some((Reply::Commands { commands }, raw)) => {
      if json {
        println!("{}", raw);
      } else {
        print_commands(&commands, only_ports);
      }
      Ok(0)
    }
    Some((Reply::Error { message }, _)) => Err(Failure::failed(message)),
    other => Err(Failure::failed(format!("unexpected reply: {:?}", other.map(|o| o.1)))),
  }
}

fn parse_selector(selector: Option<String>, all: bool) -> Result<ForwardSelector, Failure> {
  if all {
    return Ok(ForwardSelector::All);
  }
  let Some(s) = selector else {
    return Err(Failure::failed("give a forward id, a package[:port], or --all"));
  };
  if let Ok(id) = s.parse::<u64>() {
    return Ok(ForwardSelector::Id { id });
  }
  match s.rsplit_once(':') {
    Some((pkg, port)) => match port.parse::<u16>() {
      Ok(port) => Ok(ForwardSelector::Target {
        package: pkg.to_string(),
        port: Some(port),
      }),
      Err(_) => Err(Failure::failed(format!("'{}' is not a port", port))),
    },
    None => Ok(ForwardSelector::Target { package: s, port: None }),
  }
}

async fn relay(ep: Endpoint, package: String, port: u16, mut tcp: TcpStream, quiet: bool) {
  tcp.set_nodelay(true).ok();
  let res: Result<(), String> = async {
    let (mut r, w) = open(&ep, Request::Connect { target: package, port })
      .await
      .map_err(|f| f.message)?;
    match read_line::<_, Reply>(&mut r).await {
      Ok(Some(Reply::Ok)) => {}
      Ok(Some(Reply::Error { message })) => return Err(message),
      Ok(other) => return Err(format!("unexpected reply: {:?}", other)),
      Err(e) => return Err(e.to_string()),
    }
    let pending = r.buffer().to_vec();
    let mut conn = r.into_inner().unsplit(w);
    if !pending.is_empty() {
      tcp.write_all(&pending).await.map_err(|e| e.to_string())?;
    }
    tokio::io::copy_bidirectional(&mut tcp, &mut conn)
      .await
      .map(|_| ())
      .map_err(|e| e.to_string())
  }
  .await;
  if let Err(e) = res
    && !quiet
  {
    eprintln!("relay: {}", e);
  }
}

/// Re-runs this command without `--detach` in the background and reports its first line.
fn detach(json: bool) -> i32 {
  let exe = match std::env::current_exe() {
    Ok(e) => e,
    Err(e) => {
      eprintln!("cannot find the plexus executable: {}", e);
      return 1;
    }
  };
  let args: Vec<_> = std::env::args_os().skip(1).filter(|a| a != "--detach").collect();
  let mut cmd = std::process::Command::new(exe);
  cmd
    .args(args)
    .env(CHILD_ENV, "1")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
  }
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
  }
  let mut child = match cmd.spawn() {
    Ok(c) => c,
    Err(e) => {
      eprintln!("cannot start the background forward: {}", e);
      return 1;
    }
  };
  let mut first = String::new();
  let stdout = child.stdout.take().expect("piped stdout");
  let _ = std::io::BufReader::new(stdout).read_line(&mut first);
  match first.trim_end().split_once(' ') {
    Some(("OK", msg)) => {
      match serde_json::from_str::<serde_json::Value>(msg) {
        Ok(mut v) if v.is_object() => {
          v["pid"] = child.id().into();
          println!("{}", v);
        }
        _ => println!("{} (pid {})", msg, child.id()),
      }
      0
    }
    Some(("ERR", msg)) => {
      if json {
        println!("{}", serde_json::json!({"type": "error", "message": msg}));
      } else {
        eprintln!("{}", msg);
      }
      let _ = child.wait();
      1
    }
    _ => {
      eprintln!("background forward exited without reporting");
      let _ = child.wait();
      1
    }
  }
}

async fn forward(
  ep: &Endpoint,
  package: String,
  port: u16,
  listen: Option<String>,
  json: bool,
) -> Result<i32, Failure> {
  let child = std::env::var_os(CHILD_ENV).is_some();
  let report = |ok: bool, msg: &str| {
    if child {
      println!("{} {}", if ok { "OK" } else { "ERR" }, msg);
      let _ = std::io::stdout().flush();
    } else if ok {
      println!("{}", msg);
    } else {
      eprintln!("{}", msg);
    }
  };
  let listen = listen.unwrap_or_else(|| format!("127.0.0.1:{}", port));
  let listener = match TcpListener::bind(&listen).await {
    Ok(l) => l,
    Err(e) => {
      report(false, &format!("cannot listen on {}: {}", listen, e));
      return Ok(1);
    }
  };
  let bound = listener.local_addr().map(|a| a.to_string()).unwrap_or(listen);
  let (mut reg, _reg_w) = match open(
    ep,
    Request::RegisterForward {
      target: package.clone(),
      port,
      listen: bound.clone(),
    },
  )
  .await
  {
    Ok(x) => x,
    Err(f) => {
      report(false, &f.message);
      return Ok(f.code);
    }
  };
  let id = match read_line::<_, Reply>(&mut reg).await {
    Ok(Some(Reply::Forward { id })) => id,
    Ok(Some(Reply::Error { message })) => {
      report(false, &message);
      return Ok(1);
    }
    other => {
      report(false, &format!("unexpected reply: {:?}", other));
      return Ok(1);
    }
  };
  if json {
    let line = serde_json::json!({"type": "forward", "id": id, "listen": bound, "package": package, "port": port});
    report(true, &line.to_string());
  } else {
    report(
      true,
      &format!(
        "forward {}: {} -> {}:{} (stop with `plexus client unforward {}`)",
        id, bound, package, port, id
      ),
    );
  }
  loop {
    tokio::select! {
      conn = listener.accept() => {
        if let Ok((tcp, _)) = conn {
          tokio::spawn(relay(ep.clone(), package.clone(), port, tcp, child));
        }
      }
      msg = read_line::<_, Reply>(&mut reg) => {
        if !child {
          match msg {
            Ok(Some(Reply::StopForward)) => eprintln!("forward {} stopped", id),
            _ => eprintln!("forward {}: plexus went away", id),
          }
        }
        return Ok(0);
      }
      _ = tokio::signal::ctrl_c() => return Ok(0),
    }
  }
}

pub async fn run(args: ClientArgs) -> i32 {
  if let ClientAction::Forward { detach: true, .. } = &args.action
    && std::env::var_os(CHILD_ENV).is_none()
  {
    return detach(args.json);
  }
  let ep = match endpoint(&args) {
    Ok(ep) => ep,
    Err(f) => {
      eprintln!("{}", f.message);
      return f.code;
    }
  };
  let json = args.json;
  let succeeded = |r: &CommandResult| r.status.is_successed();
  let res = match args.action {
    ClientAction::List => match open(&ep, Request::List).await {
      Ok((r, _w)) => drain(r, json, |_| true).await,
      Err(f) => Err(f),
    },
    ClientAction::Status { target } => status(&ep, target, json, false).await,
    ClientAction::Ports { target } => status(&ep, target, json, true).await,
    ClientAction::Logs { target, follow, tail } => match open(&ep, Request::Logs { target, tail, follow }).await {
      Ok((r, _w)) => drain(r, json, |_| true).await,
      Err(f) => Err(f),
    },
    ClientAction::Start { target, no_wait } => match open(&ep, Request::Start { target, wait: !no_wait }).await {
      Ok((r, _w)) if no_wait => drain(r, json, |_| true).await,
      Ok((r, _w)) => drain(r, json, succeeded).await,
      Err(f) => Err(f),
    },
    ClientAction::Restart { target, no_wait } => match open(&ep, Request::Restart { target, wait: !no_wait }).await {
      Ok((r, _w)) if no_wait => drain(r, json, |_| true).await,
      Ok((r, _w)) => drain(r, json, succeeded).await,
      Err(f) => Err(f),
    },
    ClientAction::Stop { target } => match open(&ep, Request::Stop { target }).await {
      Ok((r, _w)) => {
        drain(r, json, |r| {
          r.status.is_stopped() || r.note.as_deref() == Some("not running")
        })
        .await
      }
      Err(f) => Err(f),
    },
    ClientAction::Forward {
      package, port, listen, ..
    } => forward(&ep, package, port, listen, json).await,
    ClientAction::Forwards => match open(&ep, Request::ListForwards).await {
      Ok((r, _w)) => drain(r, json, |_| true).await,
      Err(f) => Err(f),
    },
    ClientAction::Unforward { selector, all } => match parse_selector(selector, all) {
      Ok(selector) => match open(&ep, Request::StopForwards { selector }).await {
        Ok((r, _w)) => drain(r, json, |_| true).await,
        Err(f) => Err(f),
      },
      Err(f) => Err(f),
    },
  };
  match res {
    Ok(code) => code,
    Err(f) => {
      if json {
        println!("{}", serde_json::json!({"type": "error", "message": f.message}));
      } else {
        eprintln!("{}", f.message);
      }
      f.code
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn selectors() {
    assert_eq!(parse_selector(None, true).ok(), Some(ForwardSelector::All));
    assert_eq!(
      parse_selector(Some("7".into()), false).ok(),
      Some(ForwardSelector::Id { id: 7 })
    );
    assert_eq!(
      parse_selector(Some("@s/web:3000".into()), false).ok(),
      Some(ForwardSelector::Target {
        package: "@s/web".into(),
        port: Some(3000)
      })
    );
    assert_eq!(
      parse_selector(Some("web".into()), false).ok(),
      Some(ForwardSelector::Target {
        package: "web".into(),
        port: None
      })
    );
    assert!(parse_selector(Some("web:x".into()), false).is_err());
    assert!(parse_selector(None, false).is_err());
  }
}
