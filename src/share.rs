use std::sync::OnceLock;
use tokio::sync::broadcast;

use crate::app::App;

pub trait AppInterface {
    async fn set_app(&mut self, app: App);
    async fn wait(&self);
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::share::EventBus::global().emit($crate::share::AppEvent::Log(format!($($arg)*)));
    };
}

#[macro_export]
macro_rules! emit {
    ($event:expr) => {
        $crate::share::EventBus::global().emit($event);
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Stoped,
    NotStarted,
    Starting,
    Running,
    Finished,
    NoCommand,
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status_str = match self {
            TaskStatus::NotStarted => "Not Started",
            TaskStatus::Starting => "Starting",
            TaskStatus::Running => "Running",
            TaskStatus::Finished => "Finished",
            TaskStatus::NoCommand => "No Command",
            TaskStatus::Stoped => "Stopped",
        };
        write!(f, "{}", status_str)
    }
}

#[derive(Debug, Clone)]
pub struct TaskStatusEvent {
    pub name: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone)]
pub struct TaskOutputEvent {
    pub name: String,
    pub output: String,
    pub timestamp: std::time::SystemTime,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct FileChangedEvent {
    pub task_name: String,
    pub event: notify::Event,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    Log(String),
    TaskStatus(TaskStatusEvent),
    TaskOutput(TaskOutputEvent),
    FileChanged(FileChangedEvent),
    TuiStart(String),
    TuiStop(String),
    TuiRestart(String),
    Quit,
}

#[derive(Debug, Clone)]
pub struct EventBus {
    _tx: broadcast::Sender<AppEvent>,
}

impl EventBus {
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<EventBus> = OnceLock::new();
        INSTANCE.get_or_init(EventBus::new)
    }

    fn new() -> Self {
        let (tx, _) = broadcast::channel(100);
        Self { _tx: tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self._tx.subscribe()
    }

    pub fn emit(&self, event: AppEvent) {
        let _ = self._tx.send(event);
    }
}
