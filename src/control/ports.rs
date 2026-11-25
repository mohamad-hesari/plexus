use std::{
  collections::{HashMap, HashSet},
  net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  time::Duration,
};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tracing::{debug, warn};

use crate::task_managerv2::{PortInfo, TaskStore};

const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// The address to dial for a listener: a wildcard bind is reached over loopback.
pub fn dial_addr(addr: SocketAddr) -> SocketAddr {
  match addr.ip() {
    IpAddr::V4(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port()),
    IpAddr::V6(ip) if ip.is_unspecified() => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port()),
    _ => addr,
  }
}

/// Every pid under each root, root included, keyed by the root's command id.
pub fn descendants(roots: &HashMap<String, u32>, parents: &[(u32, u32)]) -> HashMap<u32, String> {
  let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
  for (pid, parent) in parents {
    children.entry(*parent).or_default().push(*pid);
  }
  let mut owner = HashMap::new();
  for (cmd_id, root) in roots {
    let mut stack = vec![*root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
      if !seen.insert(pid) {
        continue;
      }
      owner.insert(pid, cmd_id.clone());
      if let Some(kids) = children.get(&pid) {
        stack.extend(kids);
      }
    }
  }
  owner
}

fn scan(sys: &mut System, roots: &HashMap<String, u32>) -> Result<HashMap<String, Vec<PortInfo>>, String> {
  sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
  let parents: Vec<(u32, u32)> = sys
    .processes()
    .iter()
    .filter_map(|(pid, p)| p.parent().map(|pp| (pid.as_u32(), pp.as_u32())))
    .collect();
  let owner = descendants(roots, &parents);
  let listeners = listeners::get_all().map_err(|e| e.to_string())?;
  let mut out: HashMap<String, Vec<PortInfo>> = HashMap::new();
  for l in listeners {
    if l.protocol != listeners::Protocol::TCP || l.state != listeners::SocketState::Listen {
      continue;
    }
    let Some(cmd_id) = owner.get(&l.process.pid) else {
      continue;
    };
    out.entry(cmd_id.clone()).or_default().push(PortInfo {
      addr: l.socket,
      pid: l.process.pid,
      process: l.process.name.clone(),
      alive: false,
    });
  }
  for ports in out.values_mut() {
    ports.sort_by_key(|p| (p.addr.port(), p.addr.is_ipv6()));
    ports.dedup_by_key(|p| p.addr);
  }
  Ok(out)
}

async fn probe(addr: SocketAddr) -> bool {
  matches!(
    tokio::time::timeout(PROBE_TIMEOUT, tokio::net::TcpStream::connect(dial_addr(addr))).await,
    Ok(Ok(_))
  )
}

/// Polls the process tree of every running command for TCP listeners, and checks each one
/// still accepts connections.
pub async fn main_loop(store: Arc<TaskStore>, is_running: Arc<AtomicBool>) {
  let mut sys = match System::new() {
    Ok(s) => Some(s),
    Err(e) => {
      warn!("Port discovery disabled, cannot read the process table: {}", e);
      None
    }
  };
  let mut warned = false;
  let mut next = tokio::time::Instant::now() + SCAN_INTERVAL;
  while is_running.load(Ordering::Relaxed) {
    if tokio::time::Instant::now() < next {
      tokio::time::sleep(Duration::from_millis(100)).await;
      continue;
    }
    next = tokio::time::Instant::now() + SCAN_INTERVAL;
    let Some(mut s) = sys.take() else { continue };
    let roots = store.get_pids().await;
    if roots.is_empty() {
      sys = Some(s);
      store.set_ports(HashMap::new()).await;
      continue;
    }
    let joined = tokio::task::spawn_blocking(move || {
      let r = scan(&mut s, &roots);
      (s, r)
    })
    .await;
    let Ok((s, result)) = joined else { continue };
    sys = Some(s);
    let mut found = match result {
      Ok(found) => found,
      Err(e) => {
        if !warned {
          warn!("Port discovery failed: {}", e);
          warned = true;
        }
        continue;
      }
    };
    for ports in found.values_mut() {
      for p in ports.iter_mut() {
        p.alive = probe(p.addr).await;
      }
    }
    debug!("Discovered ports: {:?}", found);
    store.set_ports(found).await;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn descendants_follow_the_whole_tree_and_nothing_else() {
    let roots = HashMap::from([("dev".to_string(), 10u32)]);
    // 10 pnpm -> 11 sh -> 12 node -> 13 next-worker; 20 is unrelated.
    let parents = [(11, 10), (12, 11), (13, 12), (20, 1), (21, 20)];
    let owner = descendants(&roots, &parents);
    for pid in [10, 11, 12, 13] {
      assert_eq!(owner.get(&pid).map(String::as_str), Some("dev"));
    }
    assert!(!owner.contains_key(&20) && !owner.contains_key(&21));
  }

  #[test]
  fn wildcard_binds_are_dialled_over_loopback() {
    assert_eq!(
      dial_addr("0.0.0.0:3000".parse().unwrap()),
      "127.0.0.1:3000".parse().unwrap()
    );
    assert_eq!(dial_addr("[::]:3000".parse().unwrap()), "[::1]:3000".parse().unwrap());
    assert_eq!(dial_addr("[::1]:5173".parse().unwrap()), "[::1]:5173".parse().unwrap());
  }

  #[tokio::test]
  async fn finds_a_listener_owned_by_this_process() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let roots = HashMap::from([("me".to_string(), std::process::id())]);
    let mut sys = System::new().unwrap();
    let found = scan(&mut sys, &roots).unwrap();
    let ports: Vec<u16> = found.get("me").into_iter().flatten().map(|p| p.addr.port()).collect();
    assert!(ports.contains(&port), "{:?} should contain {}", ports, port);
  }
}
