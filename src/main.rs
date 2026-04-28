use std::sync::Arc;

use pnpm_task_tui::{
    app::App,
    cli::Cli,
    task_manager::TaskManager,
    tui_manager::{TuiManager, TuiTracingLayer},
    watch_manager::WatchManager,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init_tracing(cli: Arc<Cli>) -> Option<WorkerGuard> {
    // let console_layer_tokio = console_subscriber::ConsoleLayer::builder()
    //     .server_addr(([127, 0, 0, 1], 6670)) // Use 6670 instead of 6669
    //     .spawn();

    let tui_layer = if cli.tui {
        Some(TuiTracingLayer::new())
    } else {
        None
    };

    let console_layer = if cli.log_console {
        Some(
            fmt::layer()
                .with_writer(std::io::stdout)
                .with_target(true)
                .with_thread_ids(true),
        )
    } else {
        None
    };

    let (file_layer, _guard) = if let Some(log_file) = &cli.log_file {
        let file_appender = tracing_appender::rolling::daily("./", log_file);
        let (non_blocking_file, guard) = tracing_appender::non_blocking(file_appender);
        (
            Some(fmt::layer().with_writer(non_blocking_file).with_ansi(false)),
            Some(guard),
        )
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        // .with(console_layer_tokio)
        .with(tui_layer)
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(if cli.verbose { "debug" } else { "info" })),
        )
        .with(console_layer)
        .with(file_layer)
        .init();

    _guard
}

#[tokio::main]
async fn main() {
    let _tracing_guard = init_tracing(Arc::clone(&App::instance().cli));
    App::instance().initialize().await;
    App::instance().start().await;
    if App::instance().cli.watch {
        App::instance().watch().await;
    }
    if App::instance().cli.tui {
        App::instance().start_tui().await;
    }
    App::instance().state.wait_for_all().await;
}
