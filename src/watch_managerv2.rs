use std::{
  collections::HashMap,
  path::Path,
  sync::{Arc, atomic::AtomicBool},
  time::Duration,
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use notify::{RecursiveMode, Watcher};
use tokio::{
  sync::{Mutex, RwLock},
  task::yield_now,
  time::sleep,
};
use tracing::{debug, info, trace, warn};

use crate::{task_managerv2, watch_manager::Debouncer};

pub struct WatchManager {
  _task_manager: Arc<task_managerv2::TaskManager>,
  _is_running: Arc<AtomicBool>,
  _state: Arc<RwLock<WatchState>>,
  _ignores: Vec<String>,
}

struct FileGlobSet {
  include: GlobSet,
  exclude: GlobSet,
}

impl FileGlobSet {
  pub fn is_match<P: AsRef<Path> + Clone>(&self, path: P) -> bool {
    self.include.is_match(path.clone()) && !self.exclude.is_match(path)
  }
}

struct WatchState {
  pub watcher: notify::RecommendedWatcher,
  pub rx: Arc<RwLock<tokio::sync::mpsc::UnboundedReceiver<notify::Event>>>,
  pub tasks_to_watch: HashMap<String, FileGlobSet>,
}

impl WatchManager {
  pub fn new(task: Arc<task_managerv2::TaskManager>, is_running: Arc<AtomicBool>, ignores: Vec<String>) -> Self {
    let (sx, rx) = tokio::sync::mpsc::unbounded_channel();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
      Ok(event) => match event.kind {
        notify::EventKind::Modify(modify) => {
          if matches!(
            modify,
            notify::event::ModifyKind::Data(_) | notify::event::ModifyKind::Name(_)
          ) {
            let _ = sx.send(event);
          }
        }
        notify::EventKind::Create(_) | notify::EventKind::Remove(_) => {
          let _ = sx.send(event);
        }
        _ => {}
      },
      Err(e) => {
        debug!("Watch error {:?}", e);
      }
    })
    .expect("Failed to create file watcher");
    Self {
      _task_manager: task,
      _is_running: is_running,
      _state: Arc::new(RwLock::new(WatchState {
        watcher,
        rx: Arc::new(RwLock::new(rx)),
        tasks_to_watch: HashMap::new(),
      })),
      _ignores: ignores,
    }
  }

  async fn init_watch_state(&self) {
    let mut state = self._state.write().await;
    let data = self._task_manager.get_watch_data().await;

    for d in data {
      let have_globs = !d.includes.is_empty() || !d.excludes.is_empty();
      trace!(
        "****** initalizing watch for task {}, have_globs: {}",
        d.task_id, have_globs
      );
      let mut include_set = GlobSetBuilder::new();
      let mut exclude_set = GlobSetBuilder::new();

      // Add global ignore globs from CLI
      if self._ignores.len() > 0 {
        for ignore in &self._ignores {
          exclude_set.add(Glob::new(&format!("{}", ignore)).expect("Invalid global ignore glob pattern"));
        }
      }

      if have_globs {
        trace!(
          "Task {} has custom globs, using them to determine which files to watch",
          d.task_id
        );
        for include in d.includes {
          include_set.add(Glob::new(&include).expect("Invalid include glob pattern"));
        }
        for exclude in d.excludes {
          exclude_set.add(Glob::new(&format!("{}", exclude)).expect("Invalid exclude glob pattern"));
        }
      }
      if !d.use_default_watch_ignore {
        state
          .watcher
          .watch(Path::new(&d.task_path), RecursiveMode::Recursive)
          .expect("Failed to watch directory");
        if !have_globs {
          warn!(
            "Task {} is watching {} without using default watch ignore and without any globs, which may lead to watching more files than intended. Consider adding include/exclude globs or enabling default watch ignore.",
            d.task_id, d.task_path
          );
          include_set.add(Glob::new(&format!("{}/**/*", d.task_path)).expect("Invalid glob pattern"));
        }
      } else {
        let walk = WalkBuilder::new(&d.task_path)
          .standard_filters(true)
          .max_depth(Some(1))
          .min_depth(Some(1))
          .build();
        for entry in walk {
          if let Ok(entry) = entry {
            let final_path = entry.path().canonicalize().unwrap();
            debug!("Watching path: {:?}", final_path);
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
              state
                .watcher
                .watch(&final_path, RecursiveMode::NonRecursive)
                .expect("Failed to watch file");
              include_set.add(Glob::new(&final_path.to_string_lossy()).expect("Invalid glob pattern"));
            } else if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
              state
                .watcher
                .watch(&final_path, RecursiveMode::Recursive)
                .expect("Failed to watch directory");
              include_set
                .add(Glob::new(&format!("{}/**/*", final_path.to_string_lossy())).expect("Invalid glob pattern"));
            }
          }
        }
      }
      let file_glob_set = FileGlobSet {
        include: include_set.build().expect("Failed to build include glob set"),
        exclude: exclude_set.build().expect("Failed to build exclude glob set"),
      };
      state.tasks_to_watch.insert(d.task_id.clone(), file_glob_set);
    }
  }

  pub async fn main_loop(&self) {
    loop {
      let is_watch_mode = self._task_manager.is_watch_mode().await;
      let is_running = self._is_running.load(std::sync::atomic::Ordering::Relaxed);
      debug!(
        "Checking watch mode: is_watch_mode={}, is_running={}",
        is_watch_mode, is_running
      );
      if is_watch_mode {
        break;
      }
      if !is_running {
        return;
      }
      sleep(Duration::from_millis(1000)).await;
      yield_now().await;
    }
    info!(name = "WatchManager", stdout = "Watching for file changes...");
    self.init_watch_state().await;
    let state = self._state.read().await;
    let mut rx = state.rx.write().await;
    let debounder = Arc::new(Mutex::new(Debouncer::new()));
    loop {
      if !self._is_running.load(std::sync::atomic::Ordering::Relaxed) {
        break;
      }
      tokio_select!(
        biased,
        match .. {
          .. if let event = rx.recv() => {
            trace!("Received file event: {:?}", event);
            if let Some(event) = event {
              for (task_id, glob_set) in &state.tasks_to_watch {
                let mut matched = false;
                for path in &event.paths {
                  if glob_set.is_match(path) {
                    matched = true;
                    break;
                  }
                }
                if !matched {
                  trace!(
                    "Event {:?} did not match any globs for task {}, skipping",
                    event, task_id
                  );
                  continue;
                }
                let task_id = task_id.clone();
                let event = event.clone();
                let tm = Arc::clone(&self._task_manager);
                debounder
                  .lock()
                  .await
                  .debounce(Duration::from_millis(500), async move || {
                    info!("File change {:?}", event);
                    tm.file_changed(&task_id, event.paths).await;
                  })
                  .await;
              }
            } else {
              debug!("Watcher channel closed");
              break;
            }
          }
          _ => {
            sleep(Duration::from_millis(100)).await;
            yield_now().await;
          }
        }
      );
    }
  }
}
