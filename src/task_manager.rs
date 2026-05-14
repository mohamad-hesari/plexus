use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, OnceLock},
};

use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
};
use tracing::{debug, info};

use crate::{
    app::{App, AppTask},
    app_state::{StateEvent, StatusChangeEvent, TaskStatus},
    emit,
};
use command_group::AsyncCommandGroup;
// The trait for tokio support

static ENV_KEY_RE: OnceLock<regex::Regex> = OnceLock::new();

fn get_env_key_re() -> &'static regex::Regex {
    ENV_KEY_RE.get_or_init(|| regex::Regex::new(r"^\s*([A-Z_][A-Z0-9_]*)\s*=([^\n]*)").unwrap())
}

async fn get_env_from_file(path: &Path, envs: &mut HashMap<String, String>) {
    if !path.exists() {
        return;
    }
    let read_to_string = fs::read_to_string(path).await;
    if let Ok(contents) = read_to_string {
        for line in contents.lines() {
            let line_trimmed = line.trim();
            if line_trimmed.is_empty() || line_trimmed.starts_with("#") {
                continue;
            }
            if let Some(caps) = get_env_key_re().captures(line) {
                let key = &caps[1];
                let value = &caps[2].trim();
                envs.insert(key.to_string(), value.to_string());
            }
        }
    }
}

async fn get_envs(path: &str) -> HashMap<String, String> {
    let mut envs = HashMap::new();
    envs.insert("PNPM_TASK_TUI".to_string(), "1".to_string());
    let current_path = std::env::current_dir()
        .unwrap_or_else(|_| panic!("Failed to get current directory for task"))
        .display()
        .to_string();
    get_env_from_file(
        Path::new(current_path.as_str())
            .join(".env.local")
            .as_path(),
        &mut envs,
    )
    .await;
    get_env_from_file(
        Path::new(current_path.as_str()).join(".env").as_path(),
        &mut envs,
    )
    .await;
    get_env_from_file(Path::new(path).join(".env.local").as_path(), &mut envs).await;
    get_env_from_file(Path::new(path).join(".env").as_path(), &mut envs).await;
    envs
}

pub trait TaskManager {
    fn init_tasks(&self) -> impl Future<Output = ()> + Send;
    fn run_task(&self, task_name: &str) -> impl Future<Output = ()> + Send;
    fn stop_task(&self, task_name: &str) -> impl Future<Output = ()> + Send;
    fn start(&self) -> impl Future<Output = ()> + Send;
}

impl TaskManager for App {
    async fn init_tasks(&self) {
        debug!("init tasks");
        let mut pnpm_lock = self.pnpm.lock().await;
        pnpm_lock.load_projects(&self.cli.filter, 0).await;
        println!(
            "Loaded {} projects from pnpm-lock.yaml",
            pnpm_lock.projects().len()
        );
        let mut tasks = self.tasks.lock().await;
        let mut tasks_map = HashMap::new();
        for (name, project) in pnpm_lock.projects() {
            debug!(%name, "Loading project");
            for command in self.cli.commands.iter() {
                if !project.scripts().contains(command) {
                    debug!(%name, %command, "Project does not have command, skipping");
                    continue;
                }
                let final_name = format!("{}:{}", name, command);

                tasks.insert(
                    final_name.clone(),
                    Arc::new(AppTask::new(
                        name.to_string(),
                        command.to_string(),
                        project.path().to_string(),
                    )),
                );
                tasks_map.insert(
                    final_name.clone(),
                    project
                        .deps()
                        .iter()
                        .map(|s| format!("{}:{}", s, command))
                        .collect::<Vec<_>>(),
                );
            }
        }
        self.state.add_tasks(tasks_map.clone()).await;
        println!("Initialized {} tasks", tasks.len());
    }

    async fn start(&self) {
        let tasks = {
            let tasks = self.tasks.lock().await;
            tasks.keys().cloned().collect::<Vec<_>>()
        };
        let state = Arc::clone(&self.state);
        let pnpm = Arc::clone(&self.pnpm);
        let project_names = {
            let pnpm_lock = pnpm.lock().await;
            pnpm_lock
                .projects()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        debug!(
            %project_names,
            "PNPM projects",
        );
        let app_tasks = Arc::clone(&self.tasks);
        let aborts = Arc::clone(&self.aborts);
        let avaiable_tasks = self.tasks.lock().await.keys().cloned().collect::<Vec<_>>();
        let mut rx_status_changes = self.state.get_status_change_receiver();
        let is_watching_flag = self.cli.watch;
        self.state
            .spawn(async move {
                let mut all_started_once = false;
                loop {
                    if !state.is_running() {
                        debug!("App is not running, exiting task manager loop");
                        break;
                    }
                    for task_name in &tasks {
                        let task = state.get_task_state(task_name).await;
                        let (project_name, command) = task.name().split_once(':').unwrap();
                        debug!(%task_name, %project_name, %command, status = ?task.status(), "Checking task");
                        if *task.status() == TaskStatus::Initialized {
                            let deps = {
                                let pnpm_lock = pnpm.lock().await;
                                let project =
                                    pnpm_lock.projects().get(project_name).unwrap_or_else(|| {
                                        panic!(
                                            "Project {} not found for task {}",
                                            task.name(),
                                            task_name
                                        )
                                    });

                                project
                                    .deps()
                                    .iter()
                                    .map(|name| format!("{}:{}", name, command))
                                    .collect::<Vec<_>>()
                            };
                            let mut all_deps_finished = true;
                            for dep in deps {
                                if !avaiable_tasks.contains(&dep) {
                                    debug!(%task_name, %dep, "Dependency task not found, skipping");
                                    continue;
                                }
                                let dep_task = state.get_task_state(&dep).await;
                                if *dep_task.status() != TaskStatus::Finished {
                                    all_deps_finished = false;
                                    break;
                                }
                            }
                            if all_deps_finished {
                                debug!(%task_name, "All dependencies finished, starting task");
                                let app_task = {
                                    let tasks = app_tasks.lock().await;
                                    tasks.get(task_name).cloned()
                                }
                                .unwrap();
                                let handle = state
                                    .spawn(run_task(task.name().to_string(), Arc::clone(&app_task)))
                                    .await;
                                aborts.lock().await.insert(task_name.to_string(), handle);
                                debug!(%task_name, "Task started");
                            }
                        } else {
                            continue;
                        }
                    }

                    if !all_started_once {
                        let mut all_started = true;

                        for task_name in &tasks {
                            let task = state.get_task_state(task_name).await;
                            if *task.status() == TaskStatus::Initialized {
                                debug!(%task_name, "Task has not been started yet");
                                all_started = false;
                            } else if *task.status() == TaskStatus::Failed
                                && !is_watching_flag 
                            {
                                debug!(%task_name, "Task has failed");
                                panic!("Task {} failed, exiting", task_name);
                            }
                        }

                        if all_started {
                            all_started_once = true;
                            debug!("All tasks have been started at least once");
                        }
                    }

                    if all_started_once {
                        if is_watching_flag {
                            loop {
                                if rx_status_changes.changed().await.is_err() { break; }

                                let is_quit = rx_status_changes
                                    .borrow_and_update()
                                    .as_ref() // Look inside the Option
                                    .map(|s| *s == StatusChangeEvent::StatusChanged) // Check the enum
                                    .unwrap_or(false); // If None, it's not a quit event

                                if is_quit { break; }
                            }
                        } else {
                            loop {
                                let mut all_finished = true;
                                for task_name in &tasks {
                                    let task = state.get_task_state(task_name).await;
                                    if *task.status() == TaskStatus::Running {
                                        debug!(%task_name, "Task is still running, waiting...");
                                        all_finished = false;
                                    } else if *task.status() == TaskStatus::Failed {
                                        debug!(%task_name, "Task has failed");
                                        panic!("Task {} failed, exiting", task_name);
                                    }
                                }
                                if all_finished {
                                    debug!("All tasks finished, exiting task manager loop");
                                    emit!(StateEvent::Quit);
                                    return;
                                }
                            }
                        }
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            })
            .await;
        debug!("Task manager started");
    }

    async fn run_task(&self, task_name: &str) {
        let task = {
            let tasks = self.tasks.lock().await;
            tasks.get(task_name).cloned()
        }
        .unwrap_or_else(|| panic!("Task {} not found", task_name));
        // let task_name = Arc::new(task_name.to_string());
        emit!(StateEvent::Status {
            task_name: task_name.to_string(),
            status: TaskStatus::Starting,
        });
        debug!(%task_name, "Starting task");
        let handle = self
            .state
            .spawn(run_task(task_name.to_string(), Arc::clone(&task)))
            .await;
        self.aborts
            .lock()
            .await
            .insert(task_name.to_string(), handle);
    }

    async fn stop_task(&self, task_name: &str) {
        debug!(%task_name, "Stopping task");
        let task = {
            let tasks = self.tasks.lock().await;
            tasks.get(task_name).cloned()
        }
        .unwrap_or_else(|| panic!("Task {} not found", task_name));
        task.sx
            .send(())
            .unwrap_or_else(|_| panic!("Failed to send stop signal to task {}", task_name));

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let mut aborts = self.aborts.lock().await;
        if let Some(handle) = aborts.remove(task_name) {
            debug!(%task_name, "Aborting task");
            handle.abort();
        }
        emit!(StateEvent::Status {
            task_name: task_name.to_string(),
            status: TaskStatus::Stopped,
        });
    }
}

fn run_task(task_name: String, task: Arc<AppTask>) -> impl Future<Output = ()> + Send {
    let task_name_thread = Arc::new(task_name.to_string());
    debug!(%task_name_thread, "Spawning task");
    async move {
        let task_name = Arc::clone(&task_name_thread);
        // let stdout_tmp = NamedTempFile::new().unwrap_or_else(|_| {
        //     panic!(
        //         "Failed to create temp file for stdout of task {}",
        //         task_name
        //     )
        // });
        // let stderr_tmp = NamedTempFile::new().unwrap_or_else(|_| {
        //     panic!(
        //         "Failed to create temp file for stderr of task {}",
        //         task_name
        //     )
        // });
        //
        // let stdout_path = stdout_tmp.path().to_path_buf();
        // let stdout_file = stdout_tmp.reopen().unwrap_or_else(|_| {
        //     panic!(
        //         "Failed to reopen temp file for stdout of task {}",
        //         task_name
        //     )
        // });
        // let stderr_file = stderr_tmp.reopen().unwrap_or_else(|_| {
        //     panic!(
        //         "Failed to reopen temp file for stderr of task {}",
        //         task_name
        //     )
        // });
        let envs = get_envs(&task.path).await;
        let mut child = tokio::process::Command::new("pnpm")
            .arg("--filter")
            .arg(&task.name)
            .arg(&task.command)
            .envs(&envs)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("FORCE_COLOR", "0")
            // .stdout(stdout_file)
            // .stderr(stderr_file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .group()
            .spawn()
            .unwrap_or_else(|_| panic!("Failed to start task {}", task.name));

        emit!(StateEvent::Status {
            task_name: task_name.to_string(),
            status: TaskStatus::Running,
        });

        debug!(%task_name, "Process started for task");

        // let (tx, rx) = std::sync::mpsc::channel();
        // // let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
        // // watcher.watch(&stdout_path, RecursiveMode::NonRecursive)?;
        // let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        //     if let Ok(event) = res
        //         && event.kind.is_modify()
        //     {
        //         let _ = tx.send(());
        //     }
        // })
        // .unwrap_or_else(|_| panic!("Failed to create file watcher for task {}", task_name));
        // watcher
        //     .watch(&stdout_path, RecursiveMode::NonRecursive)
        //     .unwrap_or_else(|_| panic!("Failed to watch stdout file for task {}", task_name));
        // let task_name_thread = task_name.to_string();
        //
        // tokio::spawn(async move {
        //     let mut last_pos = 0;
        //     let mut file = File::open(&stdout_path).expect("Failed to open log");
        //
        //     for _ in rx {
        //         // Check file size and read new data
        //         if let Ok(metadata) = file.metadata() {
        //             let len = metadata.len();
        //             if len > last_pos {
        //                 let _ = file.seek(SeekFrom::Start(last_pos));
        //                 let mut buffer = vec![0; (len - last_pos) as usize];
        //                 let _ = file.read_exact(&mut buffer);
        //
        //                 let output = String::from_utf8_lossy(&buffer);
        //
        //                 emit!(StateEvent::Output {
        //                     task_name: task_name_thread.to_string(),
        //                     output: format!("[OUT]: {}", output),
        //                 });
        //
        //                 last_pos = len;
        //             } else if len < last_pos {
        //                 // Handle file truncation (clear screen)
        //                 last_pos = 0;
        //                 let _ = file.seek(SeekFrom::Start(0));
        //             }
        //         }
        //     }
        // });
        //
        // let status = child
        //     .wait()
        //     .await
        //     .unwrap_or_else(|_| panic!("Failed to wait for task {}", task_name));
        // debug!("Vite exited with status: {}", status);

        // let mut stdout = child.inner().stdout.take().expect("no stdout");
        // let mut stderr = child.inner().stderr.take().expect("no stderr");
        //
        // let mut rx = task.rx.write().await;
        //
        // let mut stdout_buf = [0u8; 4096];
        // let mut stderr_buf = [0u8; 4096];
        //
        // loop {
        //     if !App::instance().state.is_running() {
        //         let _ = child.kill().await;
        //         break;
        //     }
        //
        //     tokio::select! {
        //         // 1. High priority: Stop signal
        //         _ = rx.recv() => {
        //             let _ = child.kill().await;
        //             break;
        //         }
        //
        //         // 2. Read RAW bytes from stdout
        //         n = stdout.read(&mut stdout_buf) => {
        //             match n {
        //                 Ok(0) => break, // Stream closed
        //                 Ok(len) => {
        //                     let output = String::from_utf8_lossy(&stdout_buf[..len]);
        //                     // We use lossy to handle partial ANSI codes safely
        //                     emit!(StateEvent::Output{
        //                         task_name: task_name.to_string(),
        //                         output: format!("[OUT]: {}", output),
        //                     });
        //                 }
        //                 Err(_) => break,
        //             }
        //         }
        //
        //         // 3. Read RAW bytes from stderr
        //         n = stderr.read(&mut stderr_buf) => {
        //             if let Ok(len) = n {
        //                 if len == 0 { break; }
        //                 let output = String::from_utf8_lossy(&stderr_buf[..len]);
        //                 emit!(StateEvent::Output{
        //                     task_name: task_name.to_string(),
        //                     output: format!("[ERR]: {}", output),
        //                 });
        //             }
        //         }
        //         _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
        //             tokio::task::yield_now().await;
        //         }
        //     }
        // }

        let stdout = child.inner().stdout.take().expect("no stdout");
        let stderr = child.inner().stderr.take().expect("no stderr");
        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut stderr_reader = BufReader::new(stderr).lines();

        let mut rx = task.rx.write().await;

        loop {
            if !App::instance().state.is_running() {
                debug!(%task_name, "App is not running, killing process");
                let _ = child.kill().await;
                break;
            }
            tokio::select! {
                // biased;
                _ = rx.recv() => {
                    debug!(%task_name, "Received stop signal for task");
                    let _ = child.kill().await;
                    break;
                }
                line = stdout_reader.next_line() => {
                    match line {
                        Ok(Some(l)) if !l.trim().is_empty() => {
                            emit!(StateEvent::Output{
                                task_name: task_name.to_string(),
                                output: format!("[OUT]: {}", l.clone()),
                            });
                            // tokio::task::yield_now().await;
                        }
                        Ok(Some(_)) => {
                            // tokio::task::yield_now().await;
                        }
                        _ => break,
                    }
                }
                line = stderr_reader.next_line() => {
                    if let Ok(Some(l)) = line {
                        emit!(StateEvent::Output{
                            task_name: task_name.to_string(),
                            output: format!("[ERR]: {}", l.clone()),
                        });
                        debug!(%task_name, stderr = %l);
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    tokio::task::yield_now().await;
                }
            }
        }

        let mut finished_status = TaskStatus::Finished;
        if let Ok(status) = child.wait().await {
            info!(%task_name, exit_status = ?status, "Process exited");
            if !status.success() {
                finished_status = TaskStatus::Failed;
                info!(%task_name, "Process exited with failure");
            }
        } else {
            info!(%task_name, "Failed to wait for process");
        }

        emit!(StateEvent::Status {
            task_name: task_name.to_string(),
            status: finished_status,
        });
    }
}
