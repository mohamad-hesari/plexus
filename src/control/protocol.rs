use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::task_managerv2::{PortInfo, TaskStatus};

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_LINE: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
  pub v: u32,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub token: Option<String>,
  pub req: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
  List,
  Status {
    target: Option<String>,
  },
  Logs {
    target: String,
    tail: Option<usize>,
    follow: bool,
  },
  Start {
    target: String,
    wait: bool,
  },
  Stop {
    target: String,
  },
  Restart {
    target: String,
    wait: bool,
  },
  Connect {
    target: String,
    port: u16,
  },
  RegisterForward {
    target: String,
    port: u16,
    listen: String,
  },
  ListForwards,
  StopForwards {
    selector: ForwardSelector,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ForwardSelector {
  All,
  Id { id: u64 },
  Target { package: String, port: Option<u16> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
  Error { message: String },
  Ok,
  Packages { packages: Vec<PackageInfo> },
  Commands { commands: Vec<CommandInfo> },
  Log { target: String, line: String },
  Done { results: Vec<CommandResult> },
  Forward { id: u64 },
  Forwards { forwards: Vec<ForwardInfo> },
  StopForward,
  Stopped { count: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
  pub name: String,
  pub path: String,
  pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInfo {
  pub package: String,
  pub command: String,
  pub script: String,
  pub status: TaskStatus,
  pub pid: Option<u32>,
  pub ports: Vec<PortInfo>,
  pub runs: u64,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub blocked_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
  pub target: String,
  pub status: TaskStatus,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardInfo {
  pub id: u64,
  pub package: String,
  pub port: u16,
  pub listen: String,
  pub since: chrono::DateTime<chrono::Utc>,
}

pub async fn write_line<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, value: &T) -> std::io::Result<()> {
  let mut buf = serde_json::to_vec(value)?;
  buf.push(b'\n');
  w.write_all(&buf).await?;
  w.flush().await
}

/// `Ok(None)` on a clean EOF.
pub async fn read_line<R: AsyncBufRead + Unpin, T: DeserializeOwned>(r: &mut R) -> std::io::Result<Option<T>> {
  let mut buf = Vec::new();
  let n = (&mut *r).take(MAX_LINE as u64).read_until(b'\n', &mut buf).await?;
  if n == 0 {
    return Ok(None);
  }
  if buf.last() != Some(&b'\n') && n >= MAX_LINE {
    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "line too long"));
  }
  serde_json::from_slice(&buf)
    .map(Some)
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `pkg` or `pkg:command`. npm package names cannot contain `:`, scripts can (`build:prod`),
/// so the split is at the first colon.
pub fn split_target(target: &str) -> (&str, Option<&str>) {
  match target.split_once(':') {
    Some((pkg, cmd)) if !cmd.is_empty() => (pkg, Some(cmd)),
    Some((pkg, _)) => (pkg, None),
    None => (target, None),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn split_target_keeps_colons_in_the_script_name() {
    assert_eq!(split_target("web"), ("web", None));
    assert_eq!(split_target("@scope/web:build"), ("@scope/web", Some("build")));
    assert_eq!(split_target("web:build:prod"), ("web", Some("build:prod")));
    assert_eq!(split_target("web:"), ("web", None));
  }

  #[tokio::test]
  async fn envelope_round_trips_over_a_stream() {
    let (a, b) = tokio::io::duplex(1024);
    let mut a = a;
    let env = Envelope {
      v: PROTOCOL_VERSION,
      token: None,
      req: Request::StopForwards {
        selector: ForwardSelector::Target {
          package: "web".into(),
          port: Some(3000),
        },
      },
    };
    write_line(&mut a, &env).await.unwrap();
    let mut r = tokio::io::BufReader::new(b);
    let got: Envelope = read_line(&mut r).await.unwrap().unwrap();
    assert!(matches!(
      got.req,
      Request::StopForwards { selector: ForwardSelector::Target { ref package, port: Some(3000) } } if package == "web"
    ));
    drop(a);
    let eof: Option<Envelope> = read_line(&mut r).await.unwrap();
    assert!(eof.is_none());
  }
}
