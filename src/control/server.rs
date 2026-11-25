use std::{
  collections::HashMap,
  path::PathBuf,
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
  },
  time::Duration,
};

use interprocess::local_socket::tokio::prelude::*;
use tokio::{
  io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf},
  sync::{Mutex, oneshot},
  time::sleep,
};
use tracing::{debug, info, warn};

use super::{
  ports::dial_addr,
  protocol::{
    CommandInfo, CommandResult, Envelope, ForwardInfo, ForwardSelector, PROTOCOL_VERSION, PackageInfo, Reply, Request,
    read_line, split_target, write_line,
  },
  transport::{BindError, BoundLocal, BoundTcp, BoxConn, LocalAddr, bind_local, bind_tcp, tokens_match},
};
use crate::task_managerv2::{Task, TaskManager, TaskStatus, TaskStore, sorted_commands};

const POLL: Duration = Duration::from_millis(150);

pub struct ServerOptions {
  pub root: PathBuf,
  pub mode: u32,
  pub tcp: Option<String>,
}

pub struct Bound {
  local: BoundLocal,
  tcp: Option<BoundTcp>,
  root: PathBuf,
  mode: u32,
}

impl Bound {
  pub fn local_addr(&self) -> &LocalAddr {
    &self.local.addr
  }

  pub fn tcp_addr(&self) -> Option<std::net::SocketAddr> {
    self.tcp.as_ref().and_then(|t| t.listener.local_addr().ok())
  }
}

/// Binds before tasks load, so a second instance fails before it spawns anything.
pub async fn bind(opts: &ServerOptions) -> Result<Bound, BindError> {
  let local = bind_local(&opts.root, opts.mode).await?;
  let tcp = match &opts.tcp {
    Some(addr) => match bind_tcp(&opts.root, addr, opts.mode).await {
      Ok(t) => Some(t),
      Err(e) => {
        for f in &local.files {
          let _ = std::fs::remove_file(f);
        }
        return Err(BindError::Io(e));
      }
    },
    None => None,
  };
  Ok(Bound {
    local,
    tcp,
    root: opts.root.clone(),
    mode: opts.mode,
  })
}

struct ForwardEntry {
  info: ForwardInfo,
  stop: Option<oneshot::Sender<()>>,
}

pub struct ControlServer {
  tm: Arc<TaskManager>,
  store: Arc<TaskStore>,
  is_running: Arc<AtomicBool>,
  forwards: Mutex<HashMap<u64, ForwardEntry>>,
  next_forward: AtomicU64,
}

#[derive(Debug, Clone)]
struct Resolved {
  task_id: String,
  cmd_id: String,
  package: String,
  target: String,
}

async fn client_gone<R: AsyncRead + Unpin>(r: &mut R) {
  let mut b = [0u8; 64];
  loop {
    match r.read(&mut b).await {
      Ok(0) | Err(_) => return,
      Ok(_) => continue,
    }
  }
}

type Reader = BufReader<ReadHalf<BoxConn>>;
type Writer = WriteHalf<BoxConn>;

impl ControlServer {
  pub fn new(tm: Arc<TaskManager>, is_running: Arc<AtomicBool>) -> Arc<Self> {
    Arc::new(Self {
      store: tm.get_store(),
      tm,
      is_running,
      forwards: Mutex::new(HashMap::new()),
      next_forward: AtomicU64::new(1),
    })
  }

  pub async fn main_loop(self: Arc<Self>, bound: Bound) {
    let Bound {
      mut local,
      tcp,
      root,
      mode,
    } = bound;
    let mut files = local.files.clone();
    let token = tcp.as_ref().map(|t| t.token.clone());
    if let Some(t) = &tcp {
      files.extend(t.files.clone());
    }
    info!("Control socket listening on {}", local.addr);
    let mut recheck = tokio::time::interval(Duration::from_secs(3));
    while self.is_running.load(Ordering::Relaxed) {
      tokio::select! {
        conn = local.listener.accept() => match conn {
          Ok(conn) => self.spawn_conn(Box::new(conn), None),
          Err(e) => debug!("Control socket accept failed: {}", e),
        },
        conn = async { tcp.as_ref().unwrap().listener.accept().await }, if tcp.is_some() => match conn {
          Ok((conn, peer)) => {
            debug!("Control TCP connection from {}", peer);
            conn.set_nodelay(true).ok();
            self.spawn_conn(Box::new(conn), token.clone());
          }
          Err(e) => debug!("Control TCP accept failed: {}", e),
        },
        _ = recheck.tick() => {
          // pnpm can wipe node_modules under a running plexus.
          if files.iter().any(|f| !f.exists()) {
            warn!("Control socket files disappeared, binding again");
            drop(local.listener);
            for f in &files {
              let _ = std::fs::remove_file(f);
            }
            match bind_local(&root, mode).await {
              Ok(l) => local = l,
              Err(e) => {
                warn!("Could not bind the control socket again: {}", e);
                break;
              }
            }
            files = local.files.clone();
            if let Some(t) = &tcp {
              files.extend(t.files.clone());
              let dir = super::transport::control_dir(&root);
              let _ = std::fs::write(dir.join(super::transport::TOKEN_FILE), &t.token);
              let _ = super::transport::set_mode(&dir.join(super::transport::TOKEN_FILE), mode);
              if let Ok(a) = t.listener.local_addr() {
                let _ = std::fs::write(dir.join(super::transport::PORT_FILE), a.to_string());
              }
            }
          }
        }
        _ = sleep(Duration::from_millis(200)) => {}
      }
    }
    for f in &files {
      let _ = std::fs::remove_file(f);
    }
    debug!("Control socket closed");
  }

  fn spawn_conn(self: &Arc<Self>, conn: BoxConn, token: Option<String>) {
    let this = Arc::clone(self);
    tokio::spawn(async move {
      if let Err(e) = this.handle(conn, token).await {
        debug!("Control connection ended: {}", e);
      }
    });
  }

  async fn handle(self: Arc<Self>, conn: BoxConn, token: Option<String>) -> std::io::Result<()> {
    let (r, mut w) = tokio::io::split(conn);
    let mut r = BufReader::new(r);
    let env: Envelope = match tokio::time::timeout(Duration::from_secs(10), read_line(&mut r)).await {
      Ok(Ok(Some(env))) => env,
      Ok(Ok(None)) => return Ok(()),
      Ok(Err(e)) => return error(&mut w, format!("bad request: {}", e)).await,
      Err(_) => return error(&mut w, "timed out waiting for a request").await,
    };
    if env.v != PROTOCOL_VERSION {
      return error(
        &mut w,
        format!(
          "protocol version {} not supported, this plexus speaks {}",
          env.v, PROTOCOL_VERSION
        ),
      )
      .await;
    }
    if let Some(expected) = &token
      && !tokens_match(expected, env.token.as_deref())
    {
      return error(&mut w, "invalid or missing token").await;
    }
    debug!("Control request: {:?}", env.req);
    match env.req {
      Request::List => {
        let packages = self.packages().await;
        write_line(&mut w, &Reply::Packages { packages }).await
      }
      Request::Status { target } => match self.resolve_opt(target.as_deref()).await {
        Ok(resolved) => {
          let commands = self.infos(&resolved).await;
          write_line(&mut w, &Reply::Commands { commands }).await
        }
        Err(e) => error(&mut w, e).await,
      },
      Request::Logs { target, tail, follow } => match self.resolve(&target).await {
        Ok(resolved) => self.logs(r, w, resolved, tail, follow).await,
        Err(e) => error(&mut w, e).await,
      },
      Request::Start { target, wait } => match self.resolve(&target).await {
        Ok(resolved) => self.run(r, w, resolved, false, wait).await,
        Err(e) => error(&mut w, e).await,
      },
      Request::Restart { target, wait } => match self.resolve(&target).await {
        Ok(resolved) => self.run(r, w, resolved, true, wait).await,
        Err(e) => error(&mut w, e).await,
      },
      Request::Stop { target } => match self.resolve(&target).await {
        Ok(resolved) => {
          let mut results = Vec::new();
          for c in &resolved {
            let status = self.store.get_status(&c.cmd_id).await;
            let mut note = None;
            if status.is_running() || status.is_starting() {
              if !self.tm.stop_command(&c.task_id, &c.cmd_id, false).await {
                note = Some("did not stop within 5s".to_string());
              }
            } else {
              note = Some("not running".to_string());
            }
            results.push(CommandResult {
              target: c.target.clone(),
              status: self.store.get_status(&c.cmd_id).await,
              note,
            });
          }
          write_line(&mut w, &Reply::Done { results }).await
        }
        Err(e) => error(&mut w, e).await,
      },
      Request::Connect { target, port } => self.connect(r, w, &target, port).await,
      Request::RegisterForward { target, port, listen } => self.register_forward(r, w, &target, port, listen).await,
      Request::ListForwards => {
        let mut forwards: Vec<ForwardInfo> = self.forwards.lock().await.values().map(|f| f.info.clone()).collect();
        forwards.sort_by_key(|f| f.id);
        write_line(&mut w, &Reply::Forwards { forwards }).await
      }
      Request::StopForwards { selector } => {
        let count = self.stop_forwards(&selector).await;
        write_line(&mut w, &Reply::Stopped { count }).await
      }
    }
  }

  async fn packages(&self) -> Vec<PackageInfo> {
    let mut tasks = self.store.get_all_tasks().await;
    tasks.sort_by(|a, b| a._name.cmp(&b._name));
    tasks
      .iter()
      .map(|t| {
        let mut commands: Vec<String> = sorted_commands(&t._commands)
          .iter()
          .map(|c| c._command.to_string())
          .collect();
        commands.dedup();
        PackageInfo {
          name: t._name.clone(),
          path: t.path().to_string(),
          commands,
        }
      })
      .collect()
  }

  fn find_task<'a>(tasks: &'a [Task], pkg: &str) -> Result<&'a Task, String> {
    if let Some(t) = tasks.iter().find(|t| t._name == pkg) {
      return Ok(t);
    }
    let suffix = format!("/{}", pkg);
    let matches: Vec<&Task> = tasks.iter().filter(|t| t._name.ends_with(&suffix)).collect();
    match matches.as_slice() {
      [one] => Ok(one),
      [] => {
        let mut names: Vec<&str> = tasks.iter().map(|t| t._name.as_str()).collect();
        names.sort();
        Err(format!("unknown package '{}'; loaded: {}", pkg, names.join(", ")))
      }
      many => Err(format!(
        "'{}' is ambiguous: {}",
        pkg,
        many.iter().map(|t| t._name.as_str()).collect::<Vec<_>>().join(", ")
      )),
    }
  }

  async fn resolve(&self, target: &str) -> Result<Vec<Resolved>, String> {
    let tasks = self.store.get_all_tasks().await;
    let (pkg, cmd) = split_target(target);
    let task = Self::find_task(&tasks, pkg)?;
    let all = sorted_commands(&task._commands);
    let picked: Vec<_> = all.iter().filter(|c| cmd.is_none_or(|n| c._command == n)).collect();
    if picked.is_empty() {
      let mut names: Vec<String> = all.iter().map(|c| c._command.to_string()).collect();
      names.dedup();
      return Err(format!(
        "package '{}' has no loaded command '{}'; loaded: {}",
        task._name,
        cmd.unwrap_or_default(),
        names.join(", ")
      ));
    }
    Ok(
      picked
        .into_iter()
        .map(|c| Resolved {
          task_id: task._id.clone(),
          cmd_id: c._id.clone(),
          package: task._name.clone(),
          target: format!("{}:{}", task._name, c._command),
        })
        .collect(),
    )
  }

  async fn resolve_opt(&self, target: Option<&str>) -> Result<Vec<Resolved>, String> {
    if let Some(t) = target {
      return self.resolve(t).await;
    }
    let mut out = Vec::new();
    for p in self.packages().await {
      out.extend(self.resolve(&p.name).await?);
    }
    Ok(out)
  }

  async fn infos(&self, resolved: &[Resolved]) -> Vec<CommandInfo> {
    let mut out = Vec::new();
    for c in resolved {
      let cmd = self.store.get_task_command(&c.task_id, &c.cmd_id).await;
      let status = self.store.get_status(&c.cmd_id).await;
      let blocked_by = if status.is_init() {
        self.store.failed_dependency(&c.task_id, &c.cmd_id).await
      } else {
        None
      };
      out.push(CommandInfo {
        package: c.package.clone(),
        command: cmd._command.to_string(),
        script: cmd._command.script().to_string(),
        status,
        pid: self.store.get_pid(&c.cmd_id).await,
        ports: self.store.get_ports(&c.cmd_id).await,
        runs: self.store.get_run(&c.cmd_id).await,
        blocked_by,
      });
    }
    out
  }

  async fn logs(
    &self,
    mut r: Reader,
    mut w: Writer,
    resolved: Vec<Resolved>,
    tail: Option<usize>,
    follow: bool,
  ) -> std::io::Result<()> {
    let mut offsets = Vec::new();
    for c in &resolved {
      let len = self.store.log_len(&c.cmd_id).await;
      offsets.push(tail.map_or(0, |t| len.saturating_sub(t)));
    }
    loop {
      for (c, off) in resolved.iter().zip(offsets.iter_mut()) {
        for line in self.store.get_logs_from(&c.cmd_id, *off).await {
          *off += 1;
          write_line(
            &mut w,
            &Reply::Log {
              target: c.target.clone(),
              line,
            },
          )
          .await?;
        }
      }
      if !follow || !self.is_running.load(Ordering::Relaxed) {
        break;
      }
      tokio::select! {
        _ = client_gone(&mut r) => return Ok(()),
        _ = sleep(POLL) => {}
      }
    }
    write_line(&mut w, &Reply::Ok).await
  }

  async fn run(
    &self,
    mut r: Reader,
    mut w: Writer,
    resolved: Vec<Resolved>,
    restart: bool,
    wait: bool,
  ) -> std::io::Result<()> {
    struct Watch {
      c: Resolved,
      after_run: u64,
      offset: usize,
      note: Option<String>,
      already_running: bool,
    }
    let mut watches = Vec::new();
    for c in resolved {
      let status = self.store.get_status(&c.cmd_id).await;
      let run = self.store.get_run(&c.cmd_id).await;
      let offset = self.store.log_len(&c.cmd_id).await;
      if status.is_stopping() {
        return error(&mut w, format!("{} is still stopping, try again shortly", c.target)).await;
      }
      let busy = status.is_running() || status.is_starting();
      let mut after_run = run;
      if busy && restart {
        if !self.tm.stop_command(&c.task_id, &c.cmd_id, false).await {
          return error(&mut w, format!("{} did not stop within 5s", c.target)).await;
        }
        self.tm.start_command(&c.task_id, &c.cmd_id, true).await;
      } else if busy {
        // Already running: follow the run in progress.
        after_run = run.saturating_sub(1);
      } else {
        self.tm.start_command(&c.task_id, &c.cmd_id, true).await;
      }
      watches.push(Watch {
        c,
        after_run,
        offset,
        note: None,
        already_running: busy && !restart,
      });
    }
    if !wait {
      let mut results = Vec::new();
      for wch in &watches {
        results.push(CommandResult {
          target: wch.c.target.clone(),
          status: self.store.get_status(&wch.c.cmd_id).await,
          note: Some(if wch.already_running { "already running" } else { "triggered" }.into()),
        });
      }
      return write_line(&mut w, &Reply::Done { results }).await;
    }
    loop {
      let mut all_done = true;
      for wch in watches.iter_mut() {
        for line in self.store.get_logs_from(&wch.c.cmd_id, wch.offset).await {
          wch.offset += 1;
          write_line(
            &mut w,
            &Reply::Log {
              target: wch.c.target.clone(),
              line,
            },
          )
          .await?;
        }
        if wch.note.is_some() {
          continue;
        }
        let status = self.store.get_status(&wch.c.cmd_id).await;
        let run = self.store.get_run(&wch.c.cmd_id).await;
        if run > wch.after_run && status.is_finished() {
          continue;
        }
        if status.is_init()
          && let Some(dep) = self.store.failed_dependency(&wch.c.task_id, &wch.c.cmd_id).await
        {
          wch.note = Some(format!("blocked: dependency {} failed", dep));
          continue;
        }
        all_done = false;
      }
      if all_done {
        break;
      }
      if !self.is_running.load(Ordering::Relaxed) {
        return error(&mut w, "plexus is shutting down").await;
      }
      tokio::select! {
        _ = client_gone(&mut r) => return Ok(()),
        _ = sleep(POLL) => {}
      }
    }
    let mut results = Vec::new();
    for wch in watches {
      let status = self.store.get_status(&wch.c.cmd_id).await;
      results.push(CommandResult {
        target: wch.c.target,
        status: if wch.note.is_some() { TaskStatus::Failed } else { status },
        note: wch.note,
      });
    }
    write_line(&mut w, &Reply::Done { results }).await
  }

  async fn connect(&self, r: Reader, mut w: Writer, target: &str, port: u16) -> std::io::Result<()> {
    let resolved = match self.resolve(split_target(target).0).await {
      Ok(r) => r,
      Err(e) => return error(&mut w, e).await,
    };
    let mut known = Vec::new();
    for c in &resolved {
      known.extend(self.store.get_ports(&c.cmd_id).await);
    }
    let mut candidates: Vec<_> = known.iter().filter(|p| p.addr.port() == port).collect();
    candidates.sort_by_key(|p| !p.alive);
    let mut upstream = None;
    for p in &candidates {
      if let Ok(s) = tokio::net::TcpStream::connect(dial_addr(p.addr)).await {
        upstream = Some(s);
        break;
      }
    }
    let Some(mut upstream) = upstream else {
      let mut ports: Vec<u16> = known.iter().map(|p| p.addr.port()).collect();
      ports.sort();
      ports.dedup();
      let msg = if candidates.is_empty() {
        format!(
          "{} has no discovered listener on port {}; discovered: {}",
          resolved[0].package,
          port,
          if ports.is_empty() {
            "none".to_string()
          } else {
            ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
          }
        )
      } else {
        format!("port {} of {} refused the connection", port, resolved[0].package)
      };
      return error(&mut w, msg).await;
    };
    upstream.set_nodelay(true).ok();
    write_line(&mut w, &Reply::Ok).await?;
    let pending = r.buffer().to_vec();
    let mut conn = r.into_inner().unsplit(w);
    if !pending.is_empty() {
      upstream.write_all(&pending).await?;
    }
    tokio::io::copy_bidirectional(&mut conn, &mut upstream).await?;
    Ok(())
  }

  async fn register_forward(
    &self,
    mut r: Reader,
    mut w: Writer,
    target: &str,
    port: u16,
    listen: String,
  ) -> std::io::Result<()> {
    let package = match self.resolve(split_target(target).0).await {
      Ok(r) => r[0].package.clone(),
      Err(e) => return error(&mut w, e).await,
    };
    let id = self.next_forward.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = oneshot::channel();
    self.forwards.lock().await.insert(
      id,
      ForwardEntry {
        info: ForwardInfo {
          id,
          package: package.clone(),
          port,
          listen,
          since: chrono::Utc::now(),
        },
        stop: Some(tx),
      },
    );
    info!(
      name = package,
      stdout = format!("Forward {} registered for port {}", id, port)
    );
    let res = async {
      write_line(&mut w, &Reply::Forward { id }).await?;
      loop {
        tokio::select! {
          _ = &mut rx => {
            write_line(&mut w, &Reply::StopForward).await?;
            return Ok(());
          }
          _ = client_gone(&mut r) => return Ok(()),
          _ = sleep(Duration::from_millis(250)) => {
            if !self.is_running.load(Ordering::Relaxed) {
              let _ = write_line(&mut w, &Reply::StopForward).await;
              return Ok(());
            }
          }
        }
      }
    }
    .await;
    self.forwards.lock().await.remove(&id);
    info!(name = package, stdout = format!("Forward {} closed", id));
    res
  }

  async fn stop_forwards(&self, selector: &ForwardSelector) -> usize {
    let tasks = self.store.get_all_tasks().await;
    let ids: Vec<u64> = {
      let mut forwards = self.forwards.lock().await;
      let mut ids = Vec::new();
      for (id, f) in forwards.iter_mut() {
        let hit = match selector {
          ForwardSelector::All => true,
          ForwardSelector::Id { id: want } => id == want,
          ForwardSelector::Target { package, port } => {
            let name = Self::find_task(&tasks, package).map_or(package.as_str(), |t| t._name.as_str());
            f.info.package == name && port.is_none_or(|p| p == f.info.port)
          }
        };
        if hit {
          if let Some(tx) = f.stop.take() {
            let _ = tx.send(());
          }
          ids.push(*id);
        }
      }
      ids
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
      let forwards = self.forwards.lock().await;
      if ids.iter().all(|id| !forwards.contains_key(id)) {
        break;
      }
      drop(forwards);
      sleep(Duration::from_millis(50)).await;
    }
    ids.len()
  }
}

async fn error<W: AsyncWrite + Unpin>(w: &mut W, message: impl Into<String>) -> std::io::Result<()> {
  write_line(
    w,
    &Reply::Error {
      message: message.into(),
    },
  )
  .await
}
