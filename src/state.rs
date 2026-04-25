use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use tokio::{
    sync::RwLock,
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
    pub _tasks: Vec<String>,
    pub _outputs: HashMap<String, Vec<String>>,
    pub _statuses: HashMap<String, TaskStatus>,
    pub _handles: JoinSet<JoinHandle<()>>,
}

pub struct TaskState {
    pub running: bool,
    pub name: String,
    pub status: TaskStatus,
    pub outputs: Vec<String>,
}

impl State {
    pub fn running(&self) -> bool {
        self._running
    }

    pub fn tasks(&self) -> &Vec<String> {
        &self._tasks
    }

    pub fn get_output(&self, task_name: &str) -> Vec<String> {
        let mut result = vec![];
        if let Some(outputs) = self._outputs.get(task_name) {
            result.extend(outputs.clone());
        }
        result
    }
}

pub enum StateEvent {
    Output { task_name: String, output: String },
    Quit,
}

pub struct AppState {
    _state: RwLock<Arc<State>>,
}

impl AppState {
    fn new() -> Self {
        AppState {
            _state: RwLock::new(Arc::new(State {
                _running: false,
                _tasks: vec![],
                _outputs: HashMap::new(),
                _statuses: HashMap::new(),
                _handles: JoinSet::new(),
            })),
        }
    }

    pub fn instance() -> &'static Self {
        static INSTANCE: OnceLock<AppState> = OnceLock::new();
        INSTANCE.get_or_init(AppState::new)
    }

    pub async fn add_tasks(&self, tasks: Vec<String>) {
        let lock = self._state.write().await;
        let mut state = state._tasks.extend(tasks.clone());
        for task in tasks {
            state
                ._statuses
                .insert(task.clone(), TaskStatus::Initialized);
            state._outputs.insert(task.clone(), vec![]);
        }
    }

    pub async fn spwan_handle<F>(&self, task: F) -> AbortHandle
    where
        F: Future<Output = JoinHandle<()>>,
        F: Send + 'static,
    {
        let mut state = self._state.lock().await;
        state._handles.spawn(task)
    }

    pub async fn emit(&self, event: StateEvent) {
        let mut state = self._state.lock().await;
        match event {
            StateEvent::Output { task_name, output } => {
                state._outputs.entry(task_name).or_default().push(output);
            }
            StateEvent::Quit => {
                state._running = false;
            }
        }
    }

    pub async fn get_state(&self) -> Arc<State> {
        let state = self._state.read().await;
        Arc::clone(&state)
    }
    // let state = self._state.lock().await;
    // Arc::new(State {
    //     _running: state._running,
    //     _tasks: state._tasks.clone(),
    //     _outputs: state._outputs.clone(),
    //     _statuses: state._statuses.clone(),
    //     _handles: JoinSet::new(), // Don't clone handles
    // })

    pub async fn get_task_state(&self, task_name: &str) -> Option<TaskState> {
        let state = self._state.lock().await;
        if let Some(status) = state._statuses.get(task_name) {
            Some(TaskState {
                running: state._running,
                name: task_name.to_string(),
                status: status.clone(),
                outputs: state._outputs.get(task_name).unwrap_or(&vec![]).clone(),
            })
        } else {
            None
        }
    }

    pub async fn wait_for_all(&self) {
        let mut handles = {
            let mut state = self._state.lock().await;
            std::mem::take(&mut state._handles)
        };
        while let Some(handle) = handles.join_next().await {
            if let Err(e) = handle {
                eprintln!("Task failed: {:?}", e);
            }
        }
    }
}
