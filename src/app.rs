use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use clap::Parser;
use globset::GlobSet;
use tokio::sync::{Mutex, RwLock};

use crate::{
    app_state::AppState, cli::Cli, pnpm::Pnpm, task_manager::TaskManager,
    watch_manager::WatchManager,
};

pub struct AppTask {
    pub name: String,
    pub command: String,
    pub path: String,
    pub sx: tokio::sync::mpsc::UnboundedSender<()>,
    pub rx: RwLock<tokio::sync::mpsc::UnboundedReceiver<()>>,
}

impl AppTask {
    pub fn new(name: String, command: String, path: String) -> Self {
        let (sx, rx) = tokio::sync::mpsc::unbounded_channel();
        AppTask {
            name,
            command,
            path,
            sx,
            rx: RwLock::new(rx),
        }
    }
}

#[derive(Clone)]
pub struct FileMatcher {
    pub include_set: GlobSet,
    pub exclude_set: GlobSet,
    pub project_root: String,
}

pub struct ActualWatcher {
    pub watcher: notify::RecommendedWatcher,
    pub rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<notify::Event>>>,
}

pub struct AppWatcher {
    pub name: String,
    pub path: String,
    pub glob_set: Option<FileMatcher>,
}

pub struct App {
    pub state: Arc<AppState>,
    pub pnpm: Arc<Mutex<Pnpm>>,
    pub cli: Arc<Cli>,
    pub tasks: Arc<Mutex<HashMap<String, Arc<AppTask>>>>,
    pub aborts: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>,
    pub watchers: Arc<RwLock<Vec<AppWatcher>>>,
    pub watcher: Arc<Mutex<Option<ActualWatcher>>>,
}

impl App {
    fn new() -> Self {
        let pnpm = Arc::new(Mutex::new(Pnpm::default()));
        let state = Arc::new(AppState::new());
        let mut cli = Cli::parse();
        if cli.tui {
            cli.watch = true;
            cli.console = false;
            cli.web = false;
            cli.log_console = false;
        }
        let cli = Arc::new(cli);
        App {
            state,
            pnpm,
            cli,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            aborts: Arc::new(Mutex::new(HashMap::new())),
            watchers: Arc::new(RwLock::new(vec![])),
            watcher: Arc::new(Mutex::new(None)),
        }
    }

    pub fn instance() -> &'static Self {
        static INSTANCE: OnceLock<App> = OnceLock::new();
        INSTANCE.get_or_init(App::new)
    }

    pub async fn initialize(&self) {
        let mut watcher = self.watcher.lock().await;
        let new_watcher = self.initialize_watcher().await;
        *watcher = Some(new_watcher);
        self.init_tasks().await;
    }
}

#[macro_export]
macro_rules! emit {
    ($event:expr) => {
        $crate::app::App::instance().state.emit($event).await;
    };
}
