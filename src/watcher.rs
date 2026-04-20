use std::{path::Path, sync::Arc, time::Duration};

use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::Watcher;
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
    time::sleep,
};

use crate::{
    emit, log, pnpm,
    share::{AppEvent, FileChangedEvent},
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
        log!(
            "{} content for project {}: {}",
            file_name,
            path,
            tsconfig_content
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
        log!("No {} found for project {}", file_name, path);
        (vec![], vec![])
    }
}

pub struct Debouncer {
    handle: Option<JoinHandle<()>>,
}

impl Debouncer {
    pub async fn debounce(&mut self, delay: Duration, action: impl FnOnce() + Send + 'static) {
        // 1. Abort the previous timer if it's still running
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }

        // 2. Spawn a new timer
        self.handle = Some(tokio::spawn(async move {
            sleep(delay).await;
            action();
        }));
    }
}

#[derive(Clone)]
pub struct FileMatcher {
    include_set: GlobSet,
    exclude_set: GlobSet,
    project_root: String,
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
        // self.include_set.is_match(path) && !self.exclude_set.is_match(path)
    }
}

pub struct ProjectWatcherInner {
    _name: String,
    _path: String,
    _glob_set: Option<FileMatcher>,
    _rx: Arc<Mutex<mpsc::UnboundedReceiver<notify::Event>>>,
    _watcher: Arc<Mutex<notify::RecommendedWatcher>>,
    _handle: Option<tokio::task::JoinHandle<()>>,
}

pub struct ProjectWatcher(Arc<Mutex<ProjectWatcherInner>>);

impl ProjectWatcher {
    pub async fn new(project: &pnpm::Project) -> Self {
        let tsconfig_path = Path::new(project.path()).join("tsconfig.json");
        let (includes, excludes) = if tsconfig_path.exists() {
            let (includes, excludes) = get_project_info(project.path(), "tsconfig.json", true);
            log!(
                "Project {} tsconfig.json includes: {:?}, excludes: {:?}",
                project.name(),
                includes,
                excludes
            );
            (includes, excludes)
        } else {
            log!("No tsconfig.json found for project {}", project.name());
            (vec![], vec![])
        };

        let task_name = project.name().to_string();
        let glob_pattern = if !includes.is_empty() || !excludes.is_empty() {
            Some(FileMatcher::new(
                project.path().to_string(),
                &includes,
                &excludes,
            ))
        } else {
            None
        };
        // let glob_pattern = if !includes.is_empty() || !excludes.is_empty() {
        //     let mut builder = globset::GlobSetBuilder::new();
        //     includes.iter().for_each(|pattern| {
        //         builder.add(globset::Glob::new(pattern).unwrap());
        //     });
        //     excludes.iter().for_each(|pattern| {
        //         builder.add(globset::Glob::new(format!("!{}", pattern).as_str()).unwrap());
        //     });
        //     log!(
        //         "Built glob pattern for project {}: includes: {:?}, excludes: {:?}",
        //         task_name,
        //         includes,
        //         excludes
        //     );
        //     Some(builder.build().unwrap())
        // } else {
        //     None
        // };

        let (tx, rx) = mpsc::unbounded_channel();

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
                    log!("Watch error for task {}: {:?}", task_name.clone(), e);
                }
            })
            .expect("Failed to create file watcher");

        //
        // let handle = tokio::spawn(async move {
        //     while let Some(res) = rx.recv().await {
        //         match res {
        //             Ok(event) => {
        //                 println!("File change detected: {:?}", event);
        //                 // Here you can trigger task restarts or other actions based on the event
        //             }
        //             Err(e) => println!("Watch error: {:?}", e),
        //         }
        //     }
        // });
        //
        Self(Arc::new(Mutex::new(ProjectWatcherInner {
            _name: project.name().to_string(),
            _path: project.path().to_string(),
            _glob_set: glob_pattern,
            _rx: Arc::new(Mutex::new(rx)),
            _watcher: Arc::new(Mutex::new(watcher)),
            _handle: None,
        })))
    }

    pub async fn start(&mut self) {
        let handle;
        {
            let self_me = self.0.lock().await;
            log!("Starting watcher for project {}", self_me._name);
            let mut watcher = self_me._watcher.lock().await;
            // The watcher is already running in the background, so we just need to keep the handle alive
            watcher
                .watch(
                    Path::new(self_me._path.as_str()),
                    notify::RecursiveMode::Recursive,
                )
                .expect("Failed to watch project directory");

            let handler_task_name = self_me._name.as_str().to_string();
            let rx = Arc::clone(&self_me._rx);
            let glob_set = self_me._glob_set.clone();

            handle = Some(tokio::spawn(async move {
                let mut rx = rx.lock().await;
                let debounder = Arc::new(Mutex::new(Debouncer { handle: None }));
                while let Some(event) = rx.recv().await {
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
                    let name = handler_task_name.to_string();
                    let event = event.clone();
                    debounder
                        .lock()
                        .await
                        .debounce(Duration::from_millis(500), move || {
                            log!("File change event for task {}: {:?}", name, event);
                            emit!(AppEvent::FileChanged(FileChangedEvent {
                                task_name: name,
                                event,
                            }));
                        })
                        .await;
                }
            }));
        }
        let mut self_me = self.0.lock().await;
        self_me._handle = handle;
    }

    pub async fn stop(&mut self) {
        {
            let me = self.0.lock().await;
            log!("Stopping watcher for project {}", me._name);
            let mut watcher = me._watcher.lock().await;
            watcher
                .unwatch(Path::new(me._path.as_str()))
                .expect("Failed to unwatch project directory");
        }
        let mut me = self.0.lock().await;
        if let Some(handle) = &me._handle {
            handle.abort();
            me._handle = None;
        }
    }
}
