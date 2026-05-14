use std::{fs, path::Path, sync::Arc, time::Duration};

use globset::{Glob, GlobSetBuilder};
use notify::Watcher;
use tokio::{sync::Mutex, task::JoinHandle, time::sleep};
use tracing::{debug, info};

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
            .filter_map(|v| {
                v.get("path")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string())
            })
            .collect()
    } else {
        vec![]
    }
}

fn get_project_info(
    path: &str,
    file_name: &str,
    search_refrences: bool,
) -> (Vec<String>, Vec<String>) {
    let tsconfig_path = Path::new(path).join(file_name);
    if tsconfig_path.exists() {
        let tsconfig_content = std::fs::read_to_string(tsconfig_path).unwrap_or_else(|_| {
            eprintln!("Failed to read {} for project {}", file_name, path);
            "{}".to_string()
        });
        debug!(
            "{} content for project {}: {}",
            file_name, path, tsconfig_content
        );
        let tsconfig: serde_json::Value =
            serde_json::from_str(&tsconfig_content).unwrap_or_else(|_| {
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
        exc_builder.add(Glob::new("**/node_modules/**").unwrap());
        exc_builder.add(Glob::new("**/dist/**").unwrap());
        exc_builder.add(Glob::new("**/build/**").unwrap());
        exc_builder.add(Glob::new("**/.next/**").unwrap());

        Self {
            project_root,
            include_set: inc_builder.build().expect("Failed to build include set"),
            exclude_set: exc_builder.build().expect("Failed to build exclude set"),
        }
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
    fn watch(&self) -> impl Future<Output = ()> + Send;
    fn unwatch(&self) -> impl Future<Output = ()> + Send;
}

impl WatchManager for App {
    async fn initialize_watcher(&self) -> ActualWatcher {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => match event.kind {
                    notify::EventKind::Create(_)
                    | notify::EventKind::Modify(_)
                    | notify::EventKind::Remove(_) => {
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

    async fn watch(&self) {
        debug!("Starting watch manager");
        let watcher_cloned = Arc::clone(&self.watcher);
        {
            let mut watchers_guard = self.watchers.write().await;
            let pnpm_lock = self.pnpm.lock().await;
            let projects = pnpm_lock.projects().clone();
            let tasks = self.tasks.lock().await;
            let mut actual_watcher = watcher_cloned.lock().await;
            for task_name in tasks.keys() {
                let (project_name, _) = task_name.split_once(':').unwrap_or((task_name, ""));
                let project = projects.get(project_name).unwrap();
                let tsconfig_path = Path::new(project.path()).join("tsconfig.json");
                let (includes, excludes) = if tsconfig_path.exists() {
                    let (includes, excludes) =
                        get_project_info(project.path(), "tsconfig.json", true);
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
                    Some(FileMatcher::new(
                        project.path().to_string(),
                        &includes,
                        &excludes,
                    ))
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
let project_path_str = project.path(); 
    let project_root = Path::new(project_path_str);
                    let watcher = &mut actual_watcher.watcher;

                    for entry in fs::read_dir(project_root).unwrap_or_else(|_|
                        panic!("Failed to read project directory {}", project.path())
                    ) {
                        let entry = entry.unwrap_or_else(|_| panic!("Failed to read entry in project directory {}", project.path()));
                        let path = entry.path();

                        if path.is_dir() {
                            match &glob_set {
                                Some(glob_set) if glob_set.is_match(&path) => {
                                    watcher.watch(&path, notify:: RecursiveMode::Recursive).unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
                                }
                                _ => (),
                            }
                            // let name = path.file_name().unwrap().to_string_lossy();
                            // if name == "node_modules" || name == ".git" {
                            //     continue; // Skip the heavy stuff
                            // }
                            // watcher.watch(&path, notify:: RecursiveMode::Recursive).unwrap_or_else(|_| panic!("Failed to watch directory {}", path.display()));
                        } else {
                            if let Some(glob_set) = &glob_set
                                && glob_set.is_match(&path) {
                            watcher.watch(&path, notify:: RecursiveMode::NonRecursive).unwrap_or_else(|_| panic!("Failed to watch file {}", path.display()) );
                                }
                        }
                    }
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

        info!("File watchers initialized for all tasks, starting event loop");
        let state = Arc::clone(&self.state);
        let this_watcher_state = Arc::clone(&state);
        let watchers_cloned = Arc::clone(&self.watchers);
        let actual_watcher = Arc::clone(&self.watcher);
        state
            .spawn(async move {
                let actual_watcher = actual_watcher.lock().await;

                if let Some(actual_watcher) = &*actual_watcher {
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

                        tokio::select! {
                            event = rx.recv() => {
                                debug!("Received file event: {:?}", event);
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
                            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                                tokio::task::yield_now().await; 
                            }
                        }
                    }
                    info!("Main watcher loop exiting");
                }
            }).await;
        // for watcher in watchers.iter() {
        //     let rx = Arc::clone(&watcher.rx);
        //     let task_name = watcher.name.clone();
        //     let glob_set = watcher.glob_set.clone();
        //     let this_watcher_state = Arc::clone(&state);
        //     state
        //         .spawn(async move {
        //             tokio::time::sleep(Duration::from_secs(10)).await;
        //             info!("Started watcher for task {}", task_name);
        //             let debounder = Arc::new(Mutex::new(Debouncer { handle: None }));
        //             let mut rx = rx.lock().await;
        //             loop {
        //                 if !this_watcher_state.is_running() {
        //                     break;
        //                 }
        //
        //                 tokio::select! {
        //                     event = rx.recv() => {
        //                         debug!("Received event for task {}: {:?}", task_name, event);
        //                         if let Some(event) = event {
        //                             if let Some(glob_set) = &glob_set {
        //                                 let mut matched = false;
        //                                 for path in &event.paths {
        //                                     if glob_set.is_match(path) {
        //                                         matched = true;
        //                                         break;
        //                                     }
        //                                 }
        //                                 if !matched {
        //                                     continue;
        //                                 }
        //                             }
        //                             let name = task_name.to_string();
        //                             let event = event.clone();
        //                             debounder
        //                                 .lock()
        //                                 .await
        //                                 .debounce(Duration::from_millis(500), async move || {
        //                                     info!("File change event for task {}: {:?}", name, event);
        //                                     emit!(StateEvent::FileChanged {
        //                                         task_name: name.clone()
        //                                     });
        //                                 })
        //                                 .await;
        //                         } else {
        //                             // Channel closed, exit the loop
        //                             break;
        //                         }
        //                     }
        //                     _ = tokio::time::sleep(Duration::from_millis(200)) => {
        //                         tokio::task::yield_now().await; // Yield to allow other tasks to run, especially important if there are many watchers or other tasks that need CPU time
        //                     }
        //                 }
        //             }
        //             info!("Watcher task for {} exiting", task_name);
        //         })
        //         .await;
        // }
    }

    async fn unwatch(&self) {
        debug!("Stopping watch manager");
    }
}
