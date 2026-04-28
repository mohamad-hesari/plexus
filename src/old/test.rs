use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use tokio::{
    sync::{Mutex, RwLock, watch},
    task::{AbortHandle, JoinHandle, JoinSet},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Initialized,
    Starting,
    Running,
    Finished,
    Failed,
}

pub struct State {
    pub _running: bool,
    pub _tasks: Arc<Vec<String>>,
    pub _outputs: HashMap<String, Arc<RwLock<Vec<String>>>>,
    pub _statuses: HashMap<String, TaskStatus>,
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
    Quit,
}

pub struct AppState {
    _state: RwLock<State>,
    _handles: Mutex<JoinSet<JoinHandle<()>>>,
    _sender: tokio::sync::watch::Sender<bool>,
    _receiver: tokio::sync::watch::Receiver<bool>,
}

impl AppState {
    fn new() -> Self {
        let (tx, rx) = watch::channel(true);
        AppState {
            _state: RwLock::new(State {
                _running: false,
                _tasks: Arc::new(vec![]),
                _outputs: HashMap::new(),
                _statuses: HashMap::new(),
            }),
            _handles: Mutex::new(JoinSet::new()),
            _sender: tx,
            _receiver: rx,
        }
    }

    pub fn instance() -> &'static Self {
        static INSTANCE: OnceLock<AppState> = OnceLock::new();
        INSTANCE.get_or_init(AppState::new)
    }

    pub async fn add_tasks(&self, tasks: Vec<String>) {
        let mut lock = self._state.write().await;
        let new_tasks = Arc::make_mut(&mut lock._tasks);
        new_tasks.extend(tasks.clone());
        for task in tasks {
            lock._statuses.insert(task.clone(), TaskStatus::Initialized);
            lock._outputs
                .insert(task.clone(), Arc::new(RwLock::new(vec![])));
        }
    }

    pub async fn spwan_handle<F>(&self, task: F) -> AbortHandle
    where
        F: Future<Output = JoinHandle<()>>,
        F: Send + 'static,
    {
        let mut handles = self._handles.lock().await;
        handles.spawn(task)
    }

    pub async fn emit(&self, event: StateEvent) {
        let mut state = self._state.write().await;
        match event {
            StateEvent::Output { task_name, output } => {
                if let Some(outputs) = state._outputs.get_mut(&task_name) {
                    let mut outputs = outputs.write().await;
                    outputs.push(output);
                }
            }
            StateEvent::Status { task_name, status } => {
                state._statuses.insert(task_name, status);
            }
            StateEvent::Quit => {
                state._running = false;
                let _ = self._sender.send(false);
            }
        }
    }

    pub async fn get_task_state(&self, task_name: &str) -> Arc<TaskState> {
        let state = self._state.read().await;
        let status = state
            ._statuses
            .get(task_name)
            .unwrap_or_else(|| &TaskStatus::Initialized);
        let outputs = state._outputs.get(task_name).unwrap();
        Arc::new(TaskState {
            _running: state._running,
            _name: task_name.to_string(),
            _status: status.clone(),
            _outputs: Arc::clone(outputs),
        })
    }

    pub async fn get_tasks(&self) -> Arc<Vec<String>> {
        let state = self._state.read().await;
        Arc::clone(&state._tasks)
    }

    pub async fn wait_for_all(&self) {
        let mut rx = self._receiver.clone();

        while *rx.borrow_and_update() == true {
            if rx.changed().await.is_err() {
                break;
            }
        }

        let mut handles = self._handles.lock().await;
        while let Some(handle) = handles.join_next().await {
            if let Err(e) = handle {
                eprintln!("Task failed: {:?}", e);
            }
        }
    }
}
