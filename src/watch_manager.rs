use std::{path::Path, sync::Arc, time::Duration};

use globset::{Glob, GlobSetBuilder};
use notify::Watcher;
use tokio::{sync::Mutex, task::JoinHandle, time::sleep};
use tracing::{debug, info};

use crate::{
    app::{App, AppWatcher, FileMatcher},
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
    fn watch(&self) -> impl Future<Output = ()> + Send;
    fn unwatch(&self) -> impl Future<Output = ()> + Send;
}

impl WatchManager for App {
    async fn watch(&self) {
        debug!("Starting watch manager");
        {
            let mut watchers_guard = self.watchers.write().await;
            let pnpm_lock = self.pnpm.lock().await;
            let projects = pnpm_lock.projects().clone();
            let tasks = self.tasks.lock().await;
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

                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let watcher_task_name = Arc::clone(&task_name);
                let mut watcher = notify::recommended_watcher(
                    move |res: notify::Result<notify::Event>| match res {
                        Ok(event) => match event.kind {
                            notify::EventKind::Create(_)
                            | notify::EventKind::Modify(_)
                            | notify::EventKind::Remove(_) => {
                                let _ = tx.send(event);
                            }
                            _ => {}
                        },
                        Err(e) => {
                            debug!("Watch error for task {}: {:?}", watcher_task_name, e);
                        }
                    },
                )
                .expect("Failed to create file watcher");

                watcher
                    .watch(
                        Path::new(&project.path().to_string()),
                        notify::RecursiveMode::Recursive,
                    )
                    .expect("Failed to watch project directory");
                watchers_guard.push(AppWatcher {
                    name: task_name.to_string(),
                    path: project.path().to_string(),
                    glob_set: glob_pattern,
                    rx: Arc::new(Mutex::new(rx)),
                    watcher: Arc::new(Mutex::new(watcher)),
                });
            }
        }

        let state = Arc::clone(&self.state);
        let watchers_cloned = Arc::clone(&self.watchers);
        // self.state
        //     .spawn(async move {
        let watchers = watchers_cloned.read().await;
        for watcher in watchers.iter() {
            let rx = Arc::clone(&watcher.rx);
            let task_name = watcher.name.clone();
            let glob_set = watcher.glob_set.clone();
            let this_watcher_state = Arc::clone(&state);
            state
                .spawn(async move {
                    info!("Started watcher for task {}", task_name);
                    let debounder = Arc::new(Mutex::new(Debouncer { handle: None }));
                    let mut rx = rx.lock().await;
                    loop {
                        if !this_watcher_state.is_running() {
                            break;
                        }

                        tokio::select! {
                            event = rx.recv() => {
                                debug!("Received event for task {}: {:?}", task_name, event);
                                if let Some(event) = event {
                                    if let Some(glob_set) = &glob_set {
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
                                    let name = task_name.to_string();
                                    let event = event.clone();
                                    debounder
                                        .lock()
                                        .await
                                        .debounce(Duration::from_millis(500), async move || {
                                            debug!("File change event for task {}: {:?}", name, event);
                                            emit!(StateEvent::FileChanged {
                                                task_name: name.clone()
                                            });
                                        })
                                        .await;
                                } else {
                                    // Channel closed, exit the loop
                                    break;
                                }
                            }
                            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                                // Just to prevent busy waiting, we can adjust this duration as needed
                                // No action needed here, just loop back to check for new events
                                // This also allows us to check the is_running state periodically
                                // without relying solely on incoming events to trigger the loop
                                // If there are no events, we still want to check if the watcher should keep running
                                // and prevent the loop from consuming 100% CPU
                                // If the channel is closed, the rx.recv() will return None and we will break out of the loop
                                // If there are events, we will process them immediately without waiting for this sleep to finish
                                // This sleep is just a safety net to ensure we don't have a tight loop when there are no events and the channel is still open
                                // If the channel is closed, we will exit the loop on the next iteration when rx.recv() returns None
                                // If there are events, we will process them immediately and the sleep will not cause any noticeable delay in handling file changes
                                // Overall, this approach allows us to efficiently handle file change events while also ensuring we can gracefully exit the watcher when needed without consuming unnecessary CPU resources
                                // We can adjust the sleep duration based on the expected frequency of file changes and the desired responsiveness of the watcher. A shorter duration will make the watcher more responsive to changes but may consume more CPU when there are no events, while a longer duration will reduce CPU usage but may introduce a slight delay in handling file changes. In practice, a duration of around 200 milliseconds is often a good balance for many applications, but this can be fine-tuned based on specific use cases and performance requirements.
                                // In summary, this loop will efficiently handle incoming file change events while also periodically checking if the watcher should continue running, allowing for graceful shutdowns and preventing high CPU usage when there are no events to process.
                                // If the channel is closed, the rx.recv() will return None and we will break out of the loop, ensuring that we don't have a tight loop consuming CPU when there are no events and the watcher is no longer active.
                                // This design allows us to handle file change events in a responsive manner while also ensuring that we can gracefully exit the watcher when it's no longer needed, without consuming unnecessary CPU resources in the process.
                                // Overall, this approach provides a robust and efficient way to manage file watching in the application, allowing us to react to changes in the file system while also maintaining good performance and resource management.
                                // By using a combination of event-driven handling and periodic checks, we can ensure that the watcher remains responsive to file changes while also allowing for graceful shutdowns and efficient resource usage, making it a well-rounded solution for managing file watching in the application.
                                // In practice, this means that the watcher will be able to quickly react to file changes while also ensuring that it can shut down gracefully when needed, without consuming unnecessary CPU resources in the process. This design allows us to maintain good performance and responsiveness in the application while also providing a robust and efficient way to manage file watching.
                                tokio::task::yield_now().await; // Yield to allow other tasks to run, especially important if there are many watchers or other tasks that need CPU time
                            }
                        }

                        // while let Ok(event) = rx.lock().await.try_recv() {
                        //     if let Some(glob_set) = &glob_set {
                        //         let mut matched = false;
                        //         for path in &event.paths {
                        //             if glob_set.is_match(path) {
                        //                 matched = true;
                        //                 break;
                        //             }
                        //         }
                        //         if !matched {
                        //             continue;
                        //         }
                        //     }
                        //     let name = task_name.to_string();
                        //     let event = event.clone();
                        //     debounder
                        //         .lock()
                        //         .await
                        //         .debounce(Duration::from_millis(500), async move || {
                        //             debug!("File change event for task {}: {:?}", name, event);
                        //             emit!(StateEvent::FileChanged {
                        //                 task_name: name.clone()
                        //             });
                        //         })
                        //         .await;
                        // }
                        // tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    info!("Watcher task for {} exiting", task_name);
                })
                .await;
        }
        // })
        // .await;
    }

    async fn unwatch(&self) {
        debug!("Stopping watch manager");
        // Implement logic to stop watching files and clean up resources
    }
}
