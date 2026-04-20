use std::sync::Arc;

use tokio::sync::Mutex;

use crate::{
    cli, log,
    share::{AppEvent, AppInterface, EventBus},
};

pub struct Console {
    _app: Option<Arc<Mutex<crate::app::App>>>,
    _handle: tokio::task::JoinHandle<()>,
}

impl Drop for Console {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

impl AppInterface for Console {
    async fn set_app(&mut self, _app: crate::app::App) {
        // No need to store the app instance for console logging
        self._app = Some(Arc::new(Mutex::new(_app)));
    }

    async fn wait(&self) {
        let mut set = tokio::task::JoinSet::new();
        let mut rx = EventBus::global().subscribe();
        set.spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if let AppEvent::Quit = msg {
                    log!("Received quit event. Shutting down console interface...");
                    break;
                }
            }
        });

        if let Some(app) = &self._app {
            let app = Arc::clone(app);
            set.spawn(async move {
                let app = app.lock().await;
                crate::app::App::run(&app).await;
                log!("App has finished running.");
            });
        }

        log!("Console interface is running. Waiting for events...");
        while let Some(res) = set.join_next().await {
            if let Err(e) = res {
                eprintln!("Error in console interface: {:?}", e);
            }
        }
        log!("Console interface has shut down.");
        self._handle.abort();
    }
}

impl Console {
    pub async fn new(cli: &cli::Cli) -> Self {
        let cli = cli.clone();
        let mut rx = EventBus::global().subscribe();
        let handle = tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    AppEvent::Log(log_msg) => {
                        if cli.verbose {
                            println!("[LOG]: {}", log_msg);
                        }
                    }
                    AppEvent::TaskStatus(task) => {
                        if cli.verbose {
                            println!("[{}]: Status being change to {}", task.name, task.status);
                        }
                    }
                    AppEvent::TaskOutput(e) => {
                        println!("[{}]: {}", e.name, e.output);
                    }
                    AppEvent::FileChanged(e) => {
                        println!("[{}]: File changed", e.task_name);
                    }
                    _ => (),
                }
            }
        });
        Self {
            _handle: handle,
            _app: None,
        }
    }
}
