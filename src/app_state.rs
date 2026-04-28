use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
};

use tokio::{
    sync::{Mutex, RwLock, watch},
    task::{AbortHandle, JoinSet},
};
use tracing::{Level, debug, info};

use crate::emit;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Initialized,
    Starting,
    Running,
    Finished,
    Failed,
}

pub struct State {
    // pub _running: bool,
    pub _tasks: Arc<RwLock<Vec<String>>>,
    pub _outputs: HashMap<String, Arc<RwLock<Vec<String>>>>,
    pub _statuses: HashMap<String, TaskStatus>,
    pub _tasks_depend_on: HashMap<String, Vec<String>>,
}

pub struct TaskState {
    _running: bool,
    _name: String,
    _status: TaskStatus,
    _outputs: Arc<RwLock<Vec<String>>>,
}

impl TaskState {
    pub fn running(&self) -> bool {
        self._running
    }

    pub fn name(&self) -> &str {
        &self._name
    }

    pub fn status(&self) -> &TaskStatus {
        &self._status
    }

    pub fn outputs(&self) -> Arc<RwLock<Vec<String>>> {
        Arc::clone(&self._outputs)
    }
}

pub enum StateEvent {
    Output {
        task_name: String,
        output: String,
    },
    Status {
        task_name: String,
        status: TaskStatus,
    },
    FileChanged {
        task_name: String,
    },
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusChangeEvent {
    StatusChanged,
}

pub struct AppState {
    _state: RwLock<State>,
    _handles: Arc<Mutex<JoinSet<()>>>,
    _status_changed: Arc<AtomicBool>,
    _sender: tokio::sync::watch::Sender<bool>,
    _receiver: tokio::sync::watch::Receiver<bool>,
    _status_change: (
        tokio::sync::watch::Sender<Option<StatusChangeEvent>>,
        tokio::sync::watch::Receiver<Option<StatusChangeEvent>>,
    ),
    _running: Arc<AtomicBool>,
    _logs: Arc<RwLock<Vec<(Level, String)>>>,
}

impl AppState {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(true);
        AppState {
            _state: RwLock::new(State {
                // _running: true,
                _tasks: Arc::new(RwLock::new(vec![])),
                _outputs: HashMap::new(),
                _statuses: HashMap::new(),
                _tasks_depend_on: HashMap::new(),
            }),
            _handles: Arc::new(Mutex::new(JoinSet::new())),
            _status_changed: Arc::new(AtomicBool::new(false)),
            _status_change: watch::channel(None),
            _sender: tx,
            _receiver: rx,
            _running: Arc::new(AtomicBool::new(true)),
            _logs: Arc::new(RwLock::new(vec![])),
        }
    }

    pub fn get_status_change_receiver(&self) -> watch::Receiver<Option<StatusChangeEvent>> {
        self._status_change.1.clone()
    }

    pub fn get_status_change_sender(&self) -> watch::Sender<Option<StatusChangeEvent>> {
        self._status_change.0.clone()
    }

    pub async fn add_tasks(&self, tasks: HashMap<String, Vec<String>>) {
        debug!("Adding tasks: {:?}", tasks);
        {
            let lock = self._state.write().await;
            let mut new_tasks = lock._tasks.write().await;
            new_tasks.extend(tasks.keys().cloned());
        }
        let mut lock = self._state.write().await;
        for (task, depend_on) in tasks {
            lock._statuses.insert(task.clone(), TaskStatus::Initialized);
            lock._outputs
                .insert(task.clone(), Arc::new(RwLock::new(vec![])));
            lock._tasks_depend_on
                .insert(task.clone(), depend_on.clone());
        }
    }

    pub async fn add_log(&self, level: Level, message: String) {
        let mut logs = self._logs.write().await;
        logs.push((level, message));
        self._status_change
            .0
            .send(None)
            .expect("Failed to send status change");
    }

    pub async fn get_logs(&self) -> Vec<(Level, String)> {
        let logs = self._logs.read().await;
        logs.clone()
    }

    pub async fn get_tasks(&self) -> Vec<String> {
        let state = self._state.read().await;
        let tasks = state._tasks.read().await;
        tasks.clone()
    }

    pub async fn spawn<F>(&self, task: F) -> AbortHandle
    where
        F: Future<Output = ()>,
        F: Send + 'static,
    {
        let mut handles = self._handles.lock().await;
        handles.spawn(task)
    }

    pub async fn emit(&self, event: StateEvent) {
        match event {
            StateEvent::Output { task_name, output } => {
                let mut state = self._state.write().await;
                if let Some(outputs) = state._outputs.get_mut(&task_name) {
                    let mut outputs = outputs.write().await;
                    outputs.push(output);
                }
                self._status_changed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                self._status_change
                    .0
                    .send(None)
                    .expect("Failed to send status change");
            }
            StateEvent::Status { task_name, status } => {
                info!("Task {} status changed to {:?}", task_name, status);
                let mut state = self._state.write().await;
                state._statuses.insert(task_name, status);
                self._status_changed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                self._status_change
                    .0
                    .send(Some(StatusChangeEvent::StatusChanged))
                    .expect("Failed to send status change");
            }
            StateEvent::Quit => {
                info!("Quitting application");
                self._running
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                let _ = self._sender.send(false);
                self._status_change
                    .0
                    .send(Some(StatusChangeEvent::StatusChanged))
                    .expect("Failed to send status change");
            }
            StateEvent::FileChanged { task_name } => {
                info!("File changed for task: {}", task_name);
                let tasks = {
                    let state = self._state.read().await;
                    let mut tasks = vec![];
                    for (task, depend_on) in &state._tasks_depend_on {
                        for dep in depend_on {
                            if dep.starts_with(&task_name) {
                                tasks.push(task.clone());
                                break;
                            }
                        }
                        if task.starts_with(&task_name) {
                            tasks.push(task.clone());
                        }
                    }
                    tasks
                };
                debug!("Tasks to restart: {:?}", tasks);
                let mut state = self._state.write().await;
                for task in tasks {
                    if let Some(status) = state._statuses.get(&task)
                        && *status == TaskStatus::Finished
                    {
                        state
                            ._statuses
                            .insert(task.clone(), TaskStatus::Initialized);
                        debug!("Restarting task: {}", task);
                    }
                }
                self._status_change
                    .0
                    .send(Some(StatusChangeEvent::StatusChanged))
                    .expect("Failed to send status change");
            }
        }
    }

    pub async fn is_status_changed(&self, rest: bool) -> bool {
        let status_change = self
            ._status_changed
            .load(std::sync::atomic::Ordering::SeqCst);
        if status_change {
            if rest {
                self._status_changed
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
            true
        } else {
            false
        }
    }

    // pub async fn isstatus_change(&self) -> Option<u8> {
    //     self._status_change.1.has_changed().ok()?;
    // }

    pub async fn get_task_state(&self, task_name: &str) -> TaskState {
        let state = self._state.read().await;
        let status = state._statuses.get(task_name).unwrap();
        let outputs = state._outputs.get(task_name).unwrap();
        TaskState {
            _running: self._running.load(std::sync::atomic::Ordering::Relaxed),
            _name: task_name.to_string(),
            _status: status.clone(),
            _outputs: Arc::clone(outputs),
        }
    }

    pub fn is_running(&self) -> bool {
        self._running.load(std::sync::atomic::Ordering::Relaxed)
        // let state = self._state.read().await;
        // state._running
    }

    // pub async fn is_running_with_name(&self, name: &str) -> bool {
    //     // let state = self._state.read().await;
    //     debug!("Checking {} is running", name);
    //     // state._running
    //     self._running.load(std::sync::atomic::Ordering::Relaxed)
    // }
    //
    // pub async fn is_running_blocking(&self) -> bool {
    //     let mut rx = self._receiver.clone();
    //     let result = if rx.changed().await.is_err() {
    //         false
    //     } else {
    //         *rx.borrow()
    //     };
    //     drop(rx);
    //     result
    // }

    pub async fn wait_for_all(&self) {
        self.spawn(async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            emit!(StateEvent::Quit);
        })
        .await;
        let mut rx = self._receiver.clone();

        while *rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }

        let clone_handles = Arc::clone(&self._handles);
        let mut set = JoinSet::new();
        let handle = set.spawn(async move {
            let mut handles = clone_handles.lock().await;
            while let Some(handle) = handles.join_next().await {
                if let Err(e) = handle {
                    debug!("Task failed: {:?}", e);
                }
            }
        });

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                debug!("Timeout reached while waiting for tasks to finish");
                handle.abort();
            }
            _ = set.join_all() => {
                debug!("All tasks finished");
            }
        };
        let mut handles = self._handles.lock().await;
        handles.abort_all();
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// 2026-04-25T18:48:46.938092Z DEBUG ThreadId(02) pnpm_task_tui::task_manager: Checking task task_name=tenant:dev project_name=tenant command=dev status=Finished
// 2026-04-25T18:48:46.938131Z DEBUG ThreadId(02) pnpm_task_tui::task_manager: Checking task task_name=spa:dev project_name=spa command=dev status=Running
// 2026-04-25T18:48:46.938144Z DEBUG ThreadId(02) pnpm_task_tui::task_manager: Checking task task_name=plugins:dev project_name=plugins command=dev status=Finished
// 2026-04-25T18:48:46.938158Z DEBUG ThreadId(02) pnpm_task_tui::task_manager: Checking task task_name=core:dev project_name=core command=dev status=Finished
// 2026-04-25T18:48:46.938171Z DEBUG ThreadId(02) pnpm_task_tui::task_manager: Checking task task_name=tw-ui-core:dev project_name=tw-ui-core command=dev status=Finished
// 2026-04-25T18:48:47.836532Z DEBUG ThreadId(27) pnpm_task_tui::watch_manager: File change event for task tenant: Event { kind: Modify(Metadata(Any)), paths: ["/flexBox.ts"], attr:tracker: None, attr:flag: None, attr:info: None, attr:source: None }
// 2026-04-25T18:48:47.836586Z DEBUG ThreadId(27) pnpm_task_tui::app_state: File changed for task: tenant
// 2026-04-25T18:48:47.836604Z DEBUG ThreadId(27) pnpm_task_tui::app_state: Tasks to restart: ["tenant"]
// 2026-04-25T18:48:47.939102Z DEBUG ThreadId(27) pnpm_task_tui::task_manager: Checking task task_name=tenant:dev project_name=tenant command=dev status=Finished
// 2026-04-25T18:48:47.939145Z DEBUG ThreadId(27) pnpm_task_tui::task_manager: Checking task task_name=spa:dev project_name=spa command=dev status=Running
// 2026-04-25T18:48:47.939160Z DEBUG ThreadId(27) pnpm_task_tui::task_manager: Checking task task_name=plugins:dev project_name=plugins command=dev status=Finished
// 2026-04-25T18:48:47.939174Z DEBUG ThreadId(27) pnpm_task_tui::task_manager: Checking task task_name=core:dev project_name=core command=dev status=Finished
// 2026-04-25T18:48:47.939188Z DEBUG ThreadId(27) pnpm_task_tui::task_manager: Checking task task_name=tw-ui-core:dev project_name=tw-ui-core command=dev status=Finished
