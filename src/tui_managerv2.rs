use std::sync::{Arc, atomic::AtomicBool};

use crate::task_managerv2;

pub struct TuiManager {
  _task_manager: Arc<task_managerv2::TaskManager>,
  _is_running: Arc<AtomicBool>,
}

impl TuiManager {
  pub fn new(task: Arc<task_managerv2::TaskManager>, is_running: Arc<AtomicBool>) -> Self {
    Self {
      _task_manager: task,
      _is_running: is_running,
    }
  }

  pub async fn main_loop(&self) {
    loop {
      if !self._is_running.load(std::sync::atomic::Ordering::Relaxed) {
        break;
      }
      tokio_select!(
        biased,
        match .. {
          _ => {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
          }
        }
      );
    }
  }
}
