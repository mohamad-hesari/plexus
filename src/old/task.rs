use core::panic;
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::share::{AppEvent, TaskOutputEvent, TaskStatus, TaskStatusEvent};
use crate::{emit, log, pnpm};

#[derive(Debug)]
pub struct Task {
    _name: String,
    _path: String,
    _cmd: Option<String>,
    _status: TaskStatus,
    _mutex: tokio::sync::Mutex<()>,
    _handle: Option<tokio::task::JoinHandle<()>>,
    _project: pnpm::Project,
}

// static ENV_KEY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*([A-Z_][A-Z0-9_]*)\s*=").unwrap());
static ENV_KEY_RE: OnceLock<regex::Regex> = OnceLock::new();

fn get_env_key_re() -> &'static regex::Regex {
    ENV_KEY_RE.get_or_init(|| regex::Regex::new(r"^\s*([A-Z_][A-Z0-9_]*)\s*=([^\n]*)").unwrap())
}

async fn get_env_from_file(path: &Path, envs: &mut HashMap<String, String>) {
    if path.exists() {
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

impl Task {
    pub fn new(name: String, path: String, cmd: Option<String>, project: pnpm::Project) -> Self {
        Self {
            _name: name,
            _path: path,
            _cmd: cmd.clone(),
            _status: if cmd.is_none() {
                TaskStatus::NoCommand
            } else {
                TaskStatus::NotStarted
            },
            _handle: None,
            _mutex: tokio::sync::Mutex::new(()),
            _project: project,
        }
    }

    pub async fn stop(&mut self) {
        let _ = self._mutex.lock().await;
        if let Some(handle) = &self._handle {
            handle.abort();
            self._handle = None;
        }
        self.send_status(TaskStatus::NotStarted);
    }

    pub fn send_status(&mut self, status: TaskStatus) {
        self._status = status.clone();
        emit!(AppEvent::TaskStatus(TaskStatusEvent {
            name: self._name.clone(),
            status,
        }));
    }

    pub async fn start(me: Arc<Mutex<Self>>) {
        let cloned_me = Arc::clone(&me);
        let mut self_me = me.lock().await;
        log!("Attempting to start task {}...", self_me._name);
        log!(
            "Task {} is currently in status: {}",
            self_me._name,
            self_me._status
        );
        if matches!(self_me._status, TaskStatus::Running) {
            return;
        }

        log!("Task {} command: {:?}", self_me._name, self_me._cmd);
        if self_me._cmd.is_none() {
            self_me.send_status(TaskStatus::NoCommand);
            return;
        }

        self_me.send_status(TaskStatus::Starting);

        let path = self_me._path.clone();
        let name = self_me._name.clone();
        let cmd = self_me._cmd.clone().unwrap_or_default();

        // let seld_finished: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {
        //     self_me.send_status(TaskStatus::Finished);
        // });

        let handle = tokio::spawn(async move {
            let envs = get_envs(&path).await;
            log!("Spawning process for task {} with command: {}", name, cmd);
            log!(
                "Spawning process with envs: {}",
                serde_json::to_string(&envs).unwrap_or_default()
            );
            let mut child = Command::new("pnpm")
                .arg("--filter")
                .arg(&name)
                .arg(&cmd)
                .envs(&envs)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|_| panic!("Failed to start task {}", name));

            log!("Process for task {} started successfully", name);

            let stdout = child.stdout.take().expect("no stdout");
            let stderr = child.stderr.take().expect("no stderr");

            log!("Capturing output for task {}...", name);

            let mut stdout_reader = BufReader::new(stdout).lines();
            let mut stderr_reader = BufReader::new(stderr).lines();

            loop {
                tokio::select! {
                    line = stdout_reader.next_line() => {
                        if let Ok(Some(l)) = line {
                                emit!(AppEvent::TaskOutput(TaskOutputEvent {
                                    name: name.clone(),
                                    output: l.clone(),
                                    timestamp: std::time::SystemTime::now(),
                                    is_error: false,
                                }));
                        } else {
                            break;
                        }
                    }
                    line = stderr_reader.next_line() => {
                        if let Ok(Some(l)) = line {
                            emit!(AppEvent::TaskOutput(TaskOutputEvent {
                                name: name.clone(),
                                output: l.clone(),
                                timestamp: std::time::SystemTime::now(),
                                is_error: true,
                            }));
                        }
                    }
                }
            }

            log!("Waiting for task {} to finish...", name);
            let _ = child.wait().await;
            log!("Task {} finished", name);
            let mut task = cloned_me.lock().await;
            task.send_status(TaskStatus::Finished);
        });

        self_me.send_status(TaskStatus::Running);
        log!(
            "Task {} started with command: {}",
            self_me._name.clone(),
            self_me._cmd.clone().unwrap_or_default()
        );
        self_me._handle = Some(handle);
    }

    pub fn project(&self) -> &pnpm::Project {
        &self._project
    }

    pub fn status(&self) -> &TaskStatus {
        &self._status
    }

    pub fn cmd(&self) -> &Option<String> {
        &self._cmd
    }

    pub fn name(&self) -> &str {
        &self._name
    }
    //
    // pub fn path(&self) -> &str {
    //     &self._path
    // }
    //
    // pub fn send_event(&self, event: TaskEvent) {
    //     let _ = self._tx.send(event);
    // }
    //
    // pub async fn receive_event(&mut self) -> Option<TaskEvent> {
    //     self._rx.recv().await
    // }
}
