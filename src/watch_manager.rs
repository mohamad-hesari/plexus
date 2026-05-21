use globset::{Glob, GlobSetBuilder};
use notify::Watcher;
use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::JoinHandle, time::sleep};
use tracing::{debug, error, info, trace};

use crate::{
  app::{ActualWatcher, App, AppWatcher, FileMatcher},
  app_state::StateEvent,
  emit,
};

fn get_includes(json: &serde_json::Value) -> Vec<String> {
  if let Some(includes) = json.get("include").and_then(|v| v.as_array()) {
    includes
      .iter()
      .filter_map(|v| v.as_str().map(|s| s.to_string()))
      .collect()
  } else {
    vec![]
  }
}

fn get_excludes(json: &serde_json::Value) -> Vec<String> {
  if let Some(excludes) = json.get("exclude").and_then(|v| v.as_array()) {
    excludes
      .iter()
      .filter_map(|v| v.as_str().map(|s| s.to_string()))
      .collect()
  } else {
    vec![]
  }
}

fn get_refrences(json: &serde_json::Value) -> Vec<String> {
  if let Some(refrences) = json.get("references").and_then(|v| v.as_array()) {
    refrences
      .iter()
      .filter_map(|v| v.get("path").and_then(|p| p.as_str()).map(|s| s.to_string()))
      .collect()
  } else {
    vec![]
  }
}

fn get_project_info(path: &str, file_name: &str, search_refrences: bool) -> (Vec<String>, Vec<String>) {
  let tsconfig_path = Path::new(path).join(file_name);
  if tsconfig_path.exists() {
    let tsconfig_content = std::fs::read_to_string(tsconfig_path).unwrap_or_else(|_| {
      eprintln!("Failed to read {} for project {}", file_name, path);
      "{}".to_string()
    });
    debug!("{} content for project {}: {}", file_name, path, tsconfig_content);
    let tsconfig: serde_json::Value = serde_json::from_str(&tsconfig_content).unwrap_or_else(|_| {
      eprintln!("Failed to parse {} for project {}", file_name, path);
      serde_json::json!({})
    });
    let mut includes = get_includes(&tsconfig);
    let mut excludes = get_excludes(&tsconfig);
    if search_refrences {
      let refrences = get_refrences(&tsconfig);
      refrences.iter().for_each(|r| {
        let (ref_includes, ref_excludes) = get_project_info(path, r, false);
        includes.extend(ref_includes);
        excludes.extend(ref_excludes);
      });
    }
    (includes, excludes)
  } else {
    debug!("No {} found for project {}", file_name, path);
    (vec![], vec![])
  }
}

pub struct Debouncer {
  handle: Option<JoinHandle<()>>,
}

impl Debouncer {
  pub fn new() -> Self {
    Self { handle: None }
  }
  pub async fn debounce<F, Fut>(&mut self, delay: Duration, action: F)
  where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    // 1. Abort the previous timer if it's still running
    if let Some(handle) = self.handle.take() {
      handle.abort();
    }

    // 2. Spawn a new timer
    self.handle = Some(tokio::spawn(async move {
      sleep(delay).await;
      action().await;
    }));
  }
}

fn normalize_pattern(pat: &str) -> String {
  if !pat.contains('*') && !pat.contains('?') {
    // It's a directory like "src" -> make it "src/**"
    format!("{}/**", pat.trim_end_matches('/'))
  } else {
    pat.to_string()
  }
}

/// Finds all files/directories matching `include` patterns while respecting `exclude` patterns.
/// Returns a list of tuples containing (Absolute Path, Is Directory).
fn get_included_paths(
  root: &str,
  include: &[&str],
  exclude: &[&str],
) -> Result<Vec<(PathBuf, bool)>, globwalk::GlobError> {
  // 1. Prepare include patterns: Combine root with patterns and clean them
  let cleaned_includes: Vec<String> = include
    .iter()
    .map(|pat| {
      // TypeScript treats "src" as "src/**/*" implicitly if it's a directory,
      // but to be safe for a glob engine, we make sure it evaluates correctly.
      // let full_path = root_path.join(pat).clean();
      // full_path.to_string_lossy().into_owned()
      pat.to_string() // Keep patterns as they are, since globwalk will resolve them relative to root
    })
    .collect();

  // 2. Prepare exclude patterns: TypeScript excludes are relative to the root
  let cleaned_excludes: Vec<String> = exclude
    .iter()
    .map(|pat| {
      // Prepend `!` because globwalk uses `!` for negation/exclusion
      format!("!{}", pat)
    })
    .collect();

  // 3. Combine both lists into one for GlobWalk
  let mut all_patterns = cleaned_includes;
  all_patterns.extend(cleaned_excludes);

  let mut results = Vec::new();

  debug!("Using the following patterns for globwalk:");
  for pattern in &all_patterns {
    debug!("  {}", pattern);
  }

  // 4. Walk the filesystem starting at the root directory
  let walker = globwalk::GlobWalkerBuilder::from_patterns(root, &all_patterns)
    .follow_links(true)
    .build()?;

  for dir_entry in walker.flatten() {
    let path = dir_entry.into_path();
    let is_dir = path.is_dir();
    results.push((path, is_dir));
  }

  Ok(results)
}

impl FileMatcher {
  pub fn new(project_root: String, includes: &[String], excludes: &[String]) -> Self {
    let mut inc_builder = GlobSetBuilder::new();
    for pat in includes {
      inc_builder.add(Glob::new(&normalize_pattern(pat)).expect("Invalid include pattern"));
    }

    let mut exc_builder = GlobSetBuilder::new();
    for pat in excludes {
      exc_builder.add(Glob::new(&normalize_pattern(pat)).expect("Invalid exclude pattern"));
    }
    // exc_builder.add(Glob::new("**/node_modules/**").unwrap());
    // exc_builder.add(Glob::new("**/dist/**").unwrap());
    // exc_builder.add(Glob::new("**/build/**").unwrap());
    // exc_builder.add(Glob::new("**/.next/**").unwrap());

    Self {
      project_root,
      include_set: inc_builder.build().expect("Failed to build include set"),
      exclude_set: exc_builder.build().expect("Failed to build exclude set"),
      includes: includes.to_vec(),
      excludes: excludes.to_vec(),
    }
  }

  pub fn get_watch_paths(&self, task_name: &str) -> Vec<(String, bool)> {
    debug!(
      "Getting watch paths for task {} with project root {} and includes {:?} and excludes {:?}",
      task_name, self.project_root, self.includes, self.excludes
    );
    let results = get_included_paths(
      &self.project_root,
      &self.includes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
      &self.excludes.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    )
    .unwrap_or_else(|e| {
      error!("Error getting included paths for task {}: {:?}", task_name, e);
      vec![]
    })
    .into_iter()
    .map(|(path, is_dir)| (path.to_string_lossy().to_string(), is_dir))
    .collect();
    debug!("Watch paths for task {}: {:?}", task_name, results);
    results
    // let mut paths = vec![];
    // let project_path_str = self.project_root.clone();
    // let project_root = Path::new(&project_path_str);
    //
    // for entry in fs::read_dir(project_root).unwrap_or_else(|_|
    //     panic!("Failed to read project directory {}", project_path_str)
    // ) {
    //     let entry = entry.unwrap_or_else(|_| panic!("Failed to read entry in project directory {}", project_path_str));
    //     let path = entry.path();
    //
    //     if path.is_dir() {
    //
    //         match self.include_set.is_match(&path) {
    //             true => {
    //             debug!("Watching directory {} for task {} with glob pattern", path.display(), task_name);
    //                 paths.push((path.to_string_lossy().to_string(), true));
    //             }
    //             false => {
    //                 debug!("Skipping directory {} for task {} as it does not match glob pattern", path.display(), task_name);
    //             }
    //         }
    //     } else {
    //         if self.include_set.is_match(&path) {
    //             paths.push((path.to_string_lossy().to_string(), false));
    //         }
    //     }
    // }
    // paths
  }

  pub fn is_match<P: AsRef<Path>>(&self, path: P) -> bool {
    let path = path.as_ref();
    if let Ok(relative_path) = path.strip_prefix(&self.project_root) {
      self.include_set.is_match(relative_path) && !self.exclude_set.is_match(relative_path)
    } else {
      false
    }
  }
}

pub trait WatchManager {
  fn initialize_watcher(&self) -> impl Future<Output = ActualWatcher>;
  fn watch_step1(&self) -> impl Future<Output = ()> + Send;
  fn watch(&self) -> impl Future<Output = ()> + Send;
  fn unwatch(&self) -> impl Future<Output = ()> + Send;
}

impl WatchManager for App {
  async fn initialize_watcher(&self) -> ActualWatcher {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
      Ok(event) => match event.kind {
        notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_) => {
          let _ = tx.send(event);
        }
        _ => {}
      },
      Err(e) => {
        debug!("Watch error {:?}", e);
      }
    })
    .expect("Failed to create file watcher");
    ActualWatcher {
      watcher,
      rx: Arc::new(Mutex::new(rx)),
    }
    // Arc::new(Mutex::new(ActualWatcher { watcher, rx }))
  }

  async fn watch_step1(&self) {
    let mut watchers_guard = self.watchers.write().await;
    let pnpm_lock = self.pnpm.lock().await;
    let projects = pnpm_lock.projects().clone();
    let tasks = self.tasks.lock().await;
    let mut actual_watcher = self.watcher.lock().await;
    for task_name in tasks.keys() {
      let (project_name, _) = task_name.split_once(':').unwrap_or((task_name, ""));
      let project = projects.get(project_name).unwrap();
      let tsconfig_path = Path::new(project.path()).join("tsconfig.json");
      let (includes, excludes) = if tsconfig_path.exists() {
        let (includes, excludes) = get_project_info(project.path(), "tsconfig.json", true);
        debug!(
          "Project {} tsconfig.json includes: {:?}, excludes: {:?}",
          project.name(),
          includes,
          excludes
        );
        (includes, excludes)
      } else {
        debug!("No tsconfig.json found for project {}", project.name());
        (vec![], vec![])
      };

      let mut excludes = excludes.clone();

      self.cli.watch_ignore.iter().for_each(|pat| {
        debug!(
          "Adding CLI watch ignore pattern for project {}: {}",
          project.name(),
          pat
        );
        excludes.push(pat.clone());
      });

      let task_name = Arc::new(project.name().to_string());
      let glob_pattern = if !includes.is_empty() || !excludes.is_empty() {
        Some(FileMatcher::new(project.path().to_string(), &includes, &excludes))
      } else {
        None
      };

      // let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
      // let watcher_task_name = Arc::clone(&task_name);
      // let mut watcher = notify::recommended_watcher(
      //     move |res: notify::Result<notify::Event>| match res {
      //         Ok(event) => match event.kind {
      //             notify::EventKind::Create(_)
      //             | notify::EventKind::Modify(_)
      //             | notify::EventKind::Remove(_) => {
      //                 let _ = tx.send(event);
      //             }
      //             _ => {}
      //         },
      //         Err(e) => {
      //             debug!("Watch error for task {}: {:?}", watcher_task_name, e);
      //         }
      //     },
      // )
      // .expect("Failed to create file watcher");

      let glob_set = glob_pattern.clone();
      if let Some(actual_watcher) = &mut *actual_watcher {
        // let project_root = Path::new(&project.path().to_string());
        // let project_path_str = project.path();
        //     let project_root = Path::new(project_path_str);
        let watcher = &mut actual_watcher.watcher;

        for (path_str, is_dir) in glob_set
          .as_ref()
          .map(|gm| gm.get_watch_paths(&task_name))
          .unwrap_or_else(|| vec![(project.path().to_string(), true)])
        {
          let path = Path::new(&path_str);
          if is_dir {
            debug!(
              "Watching directory {} for task {} with glob pattern",
              path.display(),
              task_name
            );
            watcher
              .watch(path, notify::RecursiveMode::Recursive)
              .unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
          } else {
            debug!(
              "Watching file {} for task {} with glob pattern",
              path.display(),
              task_name
            );
            watcher
              .watch(path, notify::RecursiveMode::NonRecursive)
              .unwrap_or_else(|_| panic!("Failed to watch file {}", path.display()));
          }
        }

        // for entry in fs::read_dir(project_root).unwrap_or_else(|_|
        //     panic!("Failed to read project directory {}", project.path())
        // ) {
        //     let entry = entry.unwrap_or_else(|_| panic!("Failed to read entry in project directory {}", project.path()));
        //     let path = entry.path();
        //
        //     if path.is_dir() {
        //         if let Some(glob_set) = &glob_set {
        //             match glob_set.is_match(&path) {
        //                 true => {
        //                 debug!("Watching directory {} for task {} with glob pattern", path.display(), task_name);
        //                 watcher.watch(&path, notify:: RecursiveMode::Recursive).unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
        //                 }
        //                 false => {
        //                     debug!("Skipping directory {} for task {} as it does not match glob pattern", path.display(), task_name);
        //                 }
        //             }
        //         }
        //         // let name = path.file_name().unwrap().to_string_lossy();
        //         // if name == "node_modules" || name == ".git" {
        //         //     continue; // Skip the heavy stuff
        //         // }
        //         // watcher.watch(&path, notify:: RecursiveMode::Recursive).unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
        //     } else {
        //         if let Some(glob_set) = &glob_set
        //             && glob_set.is_match(&path) {
        //         watcher.watch(&path, notify:: RecursiveMode::NonRecursive).unwrap_or_else(|_| panic!("Failed to watch file {}", path.display()) );
        //             }
        //     }
        // }
        // actual_watcher
        //     .watcher
        //     .watch(
        //         Path::new(&project.path().to_string()),
        //         notify::RecursiveMode::Recursive,
        //     )
        //     .expect("Failed to watch project directory");
      }
      watchers_guard.push(AppWatcher {
        name: task_name.to_string(),
        path: project.path().to_string(),
        glob_set: glob_pattern,
      });
    }
  }

  async fn watch(&self) {
    debug!("Starting watch manager");
    self.watch_step1().await;
    // {
    //   let mut watchers_guard = self.watchers.write().await;
    //   let pnpm_lock = self.pnpm.lock().await;
    //   let projects = pnpm_lock.projects().clone();
    //   let tasks = self.tasks.lock().await;
    //   let mut actual_watcher = watcher_cloned.lock().await;
    //   for task_name in tasks.keys() {
    //     let (project_name, _) = task_name.split_once(':').unwrap_or((task_name, ""));
    //     let project = projects.get(project_name).unwrap();
    //     let tsconfig_path = Path::new(project.path()).join("tsconfig.json");
    //     let (includes, excludes) = if tsconfig_path.exists() {
    //       let (includes, excludes) = get_project_info(project.path(), "tsconfig.json", true);
    //       debug!(
    //         "Project {} tsconfig.json includes: {:?}, excludes: {:?}",
    //         project.name(),
    //         includes,
    //         excludes
    //       );
    //       (includes, excludes)
    //     } else {
    //       debug!("No tsconfig.json found for project {}", project.name());
    //       (vec![], vec![])
    //     };
    //
    //     let mut excludes = excludes.clone();
    //
    //     self.cli.watch_ignore.iter().for_each(|pat| {
    //       debug!(
    //         "Adding CLI watch ignore pattern for project {}: {}",
    //         project.name(),
    //         pat
    //       );
    //       excludes.push(pat.clone());
    //     });
    //
    //     let task_name = Arc::new(project.name().to_string());
    //     let glob_pattern = if !includes.is_empty() || !excludes.is_empty() {
    //       Some(FileMatcher::new(project.path().to_string(), &includes, &excludes))
    //     } else {
    //       None
    //     };
    //
    //     // let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    //     // let watcher_task_name = Arc::clone(&task_name);
    //     // let mut watcher = notify::recommended_watcher(
    //     //     move |res: notify::Result<notify::Event>| match res {
    //     //         Ok(event) => match event.kind {
    //     //             notify::EventKind::Create(_)
    //     //             | notify::EventKind::Modify(_)
    //     //             | notify::EventKind::Remove(_) => {
    //     //                 let _ = tx.send(event);
    //     //             }
    //     //             _ => {}
    //     //         },
    //     //         Err(e) => {
    //     //             debug!("Watch error for task {}: {:?}", watcher_task_name, e);
    //     //         }
    //     //     },
    //     // )
    //     // .expect("Failed to create file watcher");
    //
    //     let glob_set = glob_pattern.clone();
    //     if let Some(actual_watcher) = &mut *actual_watcher {
    //       // let project_root = Path::new(&project.path().to_string());
    //       // let project_path_str = project.path();
    //       //     let project_root = Path::new(project_path_str);
    //       let watcher = &mut actual_watcher.watcher;
    //
    //       for (path_str, is_dir) in glob_set
    //         .as_ref()
    //         .map(|gm| gm.get_watch_paths(&task_name))
    //         .unwrap_or_else(|| vec![(project.path().to_string(), true)])
    //       {
    //         let path = Path::new(&path_str);
    //         if is_dir {
    //           debug!(
    //             "Watching directory {} for task {} with glob pattern",
    //             path.display(),
    //             task_name
    //           );
    //           watcher
    //             .watch(path, notify::RecursiveMode::Recursive)
    //             .unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
    //         } else {
    //           debug!(
    //             "Watching file {} for task {} with glob pattern",
    //             path.display(),
    //             task_name
    //           );
    //           watcher
    //             .watch(path, notify::RecursiveMode::NonRecursive)
    //             .unwrap_or_else(|_| panic!("Failed to watch file {}", path.display()));
    //         }
    //       }
    //
    //       // for entry in fs::read_dir(project_root).unwrap_or_else(|_|
    //       //     panic!("Failed to read project directory {}", project.path())
    //       // ) {
    //       //     let entry = entry.unwrap_or_else(|_| panic!("Failed to read entry in project directory {}", project.path()));
    //       //     let path = entry.path();
    //       //
    //       //     if path.is_dir() {
    //       //         if let Some(glob_set) = &glob_set {
    //       //             match glob_set.is_match(&path) {
    //       //                 true => {
    //       //                 debug!("Watching directory {} for task {} with glob pattern", path.display(), task_name);
    //       //                 watcher.watch(&path, notify:: RecursiveMode::Recursive).unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
    //       //                 }
    //       //                 false => {
    //       //                     debug!("Skipping directory {} for task {} as it does not match glob pattern", path.display(), task_name);
    //       //                 }
    //       //             }
    //       //         }
    //       //         // let name = path.file_name().unwrap().to_string_lossy();
    //       //         // if name == "node_modules" || name == ".git" {
    //       //         //     continue; // Skip the heavy stuff
    //       //         // }
    //       //         // watcher.watch(&path, notify:: RecursiveMode::Recursive).unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
    //       //     } else {
    //       //         if let Some(glob_set) = &glob_set
    //       //             && glob_set.is_match(&path) {
    //       //         watcher.watch(&path, notify:: RecursiveMode::NonRecursive).unwrap_or_else(|_| panic!("Failed to watch file {}", path.display()) );
    //       //             }
    //       //     }
    //       // }
    //       // actual_watcher
    //       //     .watcher
    //       //     .watch(
    //       //         Path::new(&project.path().to_string()),
    //       //         notify::RecursiveMode::Recursive,
    //       //     )
    //       //     .expect("Failed to watch project directory");
    //     }
    //     watchers_guard.push(AppWatcher {
    //       name: task_name.to_string(),
    //       path: project.path().to_string(),
    //       glob_set: glob_pattern,
    //     });
    //   }
    // }

    info!("File watchers initialized for all tasks, starting event loop");
    let state = Arc::clone(&self.state);
    let this_watcher_state = Arc::clone(&state);
    let watchers_cloned = Arc::clone(&self.watchers);
    let actual_watcher = Arc::clone(&self.watcher);
    state
      .spawn(async move {
        let actual_watcher = actual_watcher.lock().await;

        let Some(actual_watcher) = &*actual_watcher else {
          error!("Watcher not initialized, cannot start watch loop");
          return;
        };
        let debounder = Arc::new(Mutex::new(Debouncer { handle: None }));
        let rx = Arc::clone(&actual_watcher.rx);

        tokio::time::sleep(Duration::from_secs(10)).await;
        info!("Started main watcher loop");

        let watchers = watchers_cloned.read().await;
        let mut rx = rx.lock().await;
        loop {
          if !this_watcher_state.is_running() {
            break;
          }

          tokio_select!(
            biased,
            match .. {
              .. if let event = rx.recv() => {
                trace!("Received file event: {:?}", event);
                if let Some(event) = event {
                  for watcher in watchers.iter() {
                    if let Some(glob_set) = &watcher.glob_set {
                      let mut matched = false;
                      for path in &event.paths {
                        if glob_set.is_match(path) {
                          matched = true;
                          break;
                        }
                      }
                      if !matched {
                        continue;
                      }
                    }
                    let name = watcher.name.clone();
                    let event = event.clone();
                    debounder
                      .lock()
                      .await
                      .debounce(Duration::from_millis(500), async move || {
                        info!("File change event for task {}: {:?}", name, event);
                        emit!(StateEvent::Output {
                          task_name: name.clone(),
                          output: format!("File change detected: {:?}", event),
                        });
                        tokio::task::yield_now().await;
                        emit!(StateEvent::FileChanged {
                          task_name: name.clone()
                        });
                      })
                      .await;
                  }
                } else {
                  // Channel closed, exit the loop
                  break;
                }
              }
              _ => {
                tokio::time::sleep(Duration::from_millis(200)).await;
                tokio::task::yield_now().await;
              }
            }
          )
        }
        info!("Main watcher loop exiting");
      })
      .await;
  }

  async fn unwatch(&self) {
    debug!("Stopping watch manager");
  }
}
