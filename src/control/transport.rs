use std::{
  io,
  path::{Path, PathBuf},
  time::Duration,
};

use interprocess::local_socket::{
  ListenerOptions,
  tokio::{Listener, Stream, prelude::*},
};
use tokio::{
  io::{AsyncRead, AsyncWrite},
  net::{TcpListener, TcpStream},
};

pub const SOCK_FILE: &str = "plexus.sock";
pub const NAME_FILE: &str = "plexus.name";
pub const PORT_FILE: &str = "plexus.port";
pub const TOKEN_FILE: &str = "plexus.token";

pub trait Conn: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Conn for T {}
pub type BoxConn = Box<dyn Conn>;

pub fn control_dir(root: &Path) -> PathBuf {
  root.join("node_modules")
}

/// Where a local client finds the server.
#[derive(Debug, Clone)]
pub enum LocalAddr {
  /// Unix: the socket file.
  Socket(PathBuf),
  /// Windows: the full pipe path, `\\.\pipe\plexus_<hash>`.
  Pipe(String),
}

impl std::fmt::Display for LocalAddr {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      LocalAddr::Socket(p) => write!(f, "{}", p.display()),
      LocalAddr::Pipe(p) => write!(f, "{}", p),
    }
  }
}

#[derive(Debug)]
pub enum BindError {
  AlreadyRunning(String),
  Io(io::Error),
}

impl std::fmt::Display for BindError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      BindError::AlreadyRunning(at) => write!(f, "another plexus is already serving {}", at),
      BindError::Io(e) => write!(f, "{}", e),
    }
  }
}

impl From<io::Error> for BindError {
  fn from(e: io::Error) -> Self {
    BindError::Io(e)
  }
}

/// A name short enough for `sun_path`, plus whatever keeps it valid.
#[cfg(unix)]
struct SockName {
  name: PathBuf,
  _dir: Option<std::fs::File>,
}

/// `sun_path` holds 108 bytes on Linux and 104 on macOS. A deep monorepo overflows that, so on
/// Linux the directory is opened and the socket addressed through `/proc/self/fd/<fd>/`.
#[cfg(unix)]
fn sock_name(path: &Path) -> io::Result<SockName> {
  const LIMIT: usize = 100;
  if path.as_os_str().len() < LIMIT {
    return Ok(SockName {
      name: path.to_path_buf(),
      _dir: None,
    });
  }
  #[cfg(target_os = "linux")]
  {
    use std::os::fd::AsRawFd;
    let dir = std::fs::File::open(path.parent().unwrap_or(Path::new(".")))?;
    let file = path.file_name().unwrap_or_default();
    let name = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(file);
    Ok(SockName { name, _dir: Some(dir) })
  }
  #[cfg(not(target_os = "linux"))]
  Err(io::Error::new(
    io::ErrorKind::InvalidInput,
    format!(
      "socket path is {} bytes, over the {} byte limit: {}",
      path.as_os_str().len(),
      LIMIT,
      path.display()
    ),
  ))
}

pub fn pipe_name(root: &Path) -> String {
  use sha2::{Digest, Sha256};
  let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
  let key = canonical.to_string_lossy().to_lowercase();
  let hash = hex::encode(Sha256::digest(key.as_bytes()));
  format!(r"\\.\pipe\plexus_{}", &hash[..16])
}

pub async fn connect_local(addr: &LocalAddr) -> io::Result<BoxConn> {
  let stream = match addr {
    #[cfg(unix)]
    LocalAddr::Socket(path) => {
      use interprocess::local_socket::{GenericFilePath, ToFsName};
      let short = sock_name(path)?;
      let name = short.name.as_os_str().to_fs_name::<GenericFilePath>()?;
      Stream::connect(name).await?
    }
    #[cfg(windows)]
    LocalAddr::Pipe(pipe) => {
      use interprocess::local_socket::{GenericNamespaced, ToNsName};
      let ns = pipe.trim_start_matches(r"\\.\pipe\");
      Stream::connect(ns.to_ns_name::<GenericNamespaced>()?).await?
    }
    #[allow(unreachable_patterns)]
    other => {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{} is not usable on this platform", other),
      ));
    }
  };
  Ok(Box::new(stream))
}

pub async fn connect_tcp(addr: &str) -> io::Result<BoxConn> {
  let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("connect to {} timed out", addr)))??;
  stream.set_nodelay(true).ok();
  Ok(Box::new(stream))
}

/// Walks up from `start` to the first directory whose `node_modules` holds a control endpoint.
/// Each pnpm package has its own `node_modules`, so the first one found is not necessarily it.
pub fn find_local(start: &Path) -> Option<(PathBuf, LocalAddr)> {
  for dir in start.ancestors() {
    if let Some(addr) = local_addr_at(dir) {
      return Some((dir.to_path_buf(), addr));
    }
  }
  None
}

pub fn local_addr_at(root: &Path) -> Option<LocalAddr> {
  let dir = control_dir(root);
  if cfg!(unix) {
    let sock = dir.join(SOCK_FILE);
    sock.exists().then_some(LocalAddr::Socket(sock))
  } else {
    let name = std::fs::read_to_string(dir.join(NAME_FILE)).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| LocalAddr::Pipe(name.to_string()))
  }
}

pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
  }
  #[cfg(not(unix))]
  {
    let _ = (path, mode);
    Ok(())
  }
}

async fn is_answering(addr: &LocalAddr) -> bool {
  matches!(
    tokio::time::timeout(Duration::from_secs(1), connect_local(addr)).await,
    Ok(Ok(_))
  )
}

pub struct BoundLocal {
  pub listener: Listener,
  pub addr: LocalAddr,
  /// Files to delete on shutdown.
  pub files: Vec<PathBuf>,
}

/// Binds the control endpoint under `<root>/node_modules`. A live server there is an error; a
/// socket left behind by a crash is removed and reclaimed.
pub async fn bind_local(root: &Path, mode: u32) -> Result<BoundLocal, BindError> {
  let dir = control_dir(root);
  std::fs::create_dir_all(&dir)?;
  #[cfg(unix)]
  {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    use std::os::unix::fs::FileTypeExt;
    let path = dir.join(SOCK_FILE);
    let addr = LocalAddr::Socket(path.clone());
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
      if is_answering(&addr).await {
        return Err(BindError::AlreadyRunning(path.display().to_string()));
      }
      if !meta.file_type().is_socket() {
        return Err(BindError::Io(io::Error::new(
          io::ErrorKind::AlreadyExists,
          format!("{} exists and is not a socket", path.display()),
        )));
      }
      std::fs::remove_file(&path)?;
    }
    let short = sock_name(&path)?;
    let name = short.name.as_os_str().to_fs_name::<GenericFilePath>()?;
    // reclaim_name would unlink through the /proc/self/fd alias after the fd is gone.
    let listener = ListenerOptions::new().name(name).reclaim_name(false).create_tokio()?;
    set_mode(&path, mode)?;
    Ok(BoundLocal {
      listener,
      addr,
      files: vec![path],
    })
  }
  #[cfg(windows)]
  {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    let _ = mode;
    let pipe = pipe_name(root);
    let addr = LocalAddr::Pipe(pipe.clone());
    if is_answering(&addr).await {
      return Err(BindError::AlreadyRunning(pipe));
    }
    let ns = pipe.trim_start_matches(r"\\.\pipe\").to_string();
    let listener = ListenerOptions::new()
      .name(ns.to_ns_name::<GenericNamespaced>()?)
      .create_tokio()?;
    let name_file = dir.join(NAME_FILE);
    std::fs::write(&name_file, &pipe)?;
    Ok(BoundLocal {
      listener,
      addr,
      files: vec![name_file],
    })
  }
}

pub struct BoundTcp {
  pub listener: TcpListener,
  pub token: String,
  pub files: Vec<PathBuf>,
}

pub async fn bind_tcp(root: &Path, addr: &str, mode: u32) -> io::Result<BoundTcp> {
  let listener = TcpListener::bind(addr).await?;
  let local = listener.local_addr()?;
  let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
  let dir = control_dir(root);
  std::fs::create_dir_all(&dir)?;
  let token_file = dir.join(TOKEN_FILE);
  let _ = std::fs::remove_file(&token_file);
  std::fs::write(&token_file, &token)?;
  set_mode(&token_file, mode)?;
  let port_file = dir.join(PORT_FILE);
  std::fs::write(&port_file, local.to_string())?;
  Ok(BoundTcp {
    listener,
    token,
    files: vec![token_file, port_file],
  })
}

pub fn tokens_match(expected: &str, got: Option<&str>) -> bool {
  let Some(got) = got else { return false };
  let (a, b) = (expected.as_bytes(), got.as_bytes());
  if a.len() != b.len() {
    return false;
  }
  a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
  use super::*;
  #[cfg(target_os = "linux")]
  use crate::control::protocol::{read_line, write_line};

  #[cfg(target_os = "linux")]
  fn deep_root(base: &Path) -> PathBuf {
    let mut p = base.to_path_buf();
    while p.as_os_str().len() < 140 {
      p.push("a-rather-long-directory-name");
    }
    std::fs::create_dir_all(&p).unwrap();
    p
  }

  #[cfg(target_os = "linux")]
  #[tokio::test]
  async fn binds_and_connects_on_a_path_longer_than_sun_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = deep_root(tmp.path());
    let bound = bind_local(&root, 0o600).await.unwrap();
    assert!(
      control_dir(&root).join(SOCK_FILE).as_os_str().len() > 108,
      "the test must exceed sun_path"
    );
    let server = tokio::spawn(async move {
      let conn = bound.listener.accept().await.unwrap();
      let mut r = tokio::io::BufReader::new(conn);
      let got: String = read_line(&mut r).await.unwrap().unwrap();
      write_line(&mut r, &format!("echo {}", got)).await.unwrap();
    });
    let (_, addr) = find_local(&root).unwrap();
    let mut conn = tokio::io::BufReader::new(connect_local(&addr).await.unwrap());
    write_line(&mut conn, &"hi").await.unwrap();
    let reply: String = read_line(&mut conn).await.unwrap().unwrap();
    assert_eq!(reply, "echo hi");
    server.await.unwrap();
  }

  #[tokio::test]
  async fn a_live_server_refuses_a_second_bind_and_a_stale_one_is_reclaimed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let first = bind_local(root, 0o600).await.unwrap();
    let accept = tokio::spawn(async move {
      let _ = first.listener.accept().await;
      first
    });
    assert!(matches!(
      bind_local(root, 0o600).await,
      Err(BindError::AlreadyRunning(_))
    ));
    let first = accept.await.unwrap();
    drop(first.listener);
    #[cfg(unix)]
    assert!(
      control_dir(root).join(SOCK_FILE).exists(),
      "the stale socket is left behind"
    );
    bind_local(root, 0o600).await.expect("a stale socket is reclaimed");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn socket_gets_the_requested_mode() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let bound = bind_local(tmp.path(), 0o660).await.unwrap();
    let mode = std::fs::metadata(&bound.files[0]).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o660);
  }

  #[test]
  fn token_compare() {
    assert!(tokens_match("abc", Some("abc")));
    assert!(!tokens_match("abc", Some("abd")));
    assert!(!tokens_match("abc", Some("ab")));
    assert!(!tokens_match("abc", None));
  }
}
