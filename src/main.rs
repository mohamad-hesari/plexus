use std::sync::Arc;
use time::macros::format_description;
use tracing::info;
use tracing_subscriber::fmt::time::UtcTime; // Or LocalTime

use plexus::{
    app::App,
    cli::Cli,
    task_manager::TaskManager,
    tui_manager::{TuiManager, TuiTracingLayer},
    watch_manager::WatchManager,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

pub fn init_tracing(cli: Arc<Cli>) -> (Option<WorkerGuard>, Option<WorkerGuard>) {
    // let console_layer_tokio = console_subscriber::ConsoleLayer::builder()
    //     .server_addr(([127, 0, 0, 1], 6670)) // Use 6670 instead of 6669
    //     .spawn();

    let tui_layer = if cli.tui {
        Some(TuiTracingLayer::new())
    } else {
        None
    };

    // let console_layer = if cli.log_console {
    //     Some(
    //         fmt::layer()
    //             .with_writer(std::io::stdout)
    //             .with_target(true)
    //             .with_thread_ids(true),
    //     )
    // } else {
    //     None
    // };

    let (console_layer, _console_guard) = if cli.log_console {
        let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
        let layer = fmt::layer().with_writer(writer).with_ansi(false);
        if cli.compact {
            let description = format_description!("[hour]:[minute]:[second]");
            let timer = UtcTime::new(description);
            (
                Some(
                    layer
                        .compact() // Use compact style
                        .with_timer(timer) // Use HH:mm:ss
                        .with_target(false) // Remove module path for cleanliness
                        .boxed(),
                ),
                Some(guard),
            )
        } else {
            (
                Some(
                    layer
                        .with_target(true) // Keep full module path
                        .boxed(),
                ),
                Some(guard),
            )
        }
    } else {
        (None, None)
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

    (_console_guard, _guard)
}

#[tokio::main]
async fn main() {
    let (g1, g2) = init_tracing(Arc::clone(&App::instance().cli));
    info!("Starting Plexus...");
    App::instance().initialize().await;
    App::instance().start().await;
    if App::instance().cli.watch {
        info!("Plexus is running. Press Ctrl+C to exit.");
        App::instance().watch().await;
    }
    if App::instance().cli.tui {
        info!("Plexus is starting the TUI...");
        App::instance().start_tui().await;
    }
    info!("Plexus is waiting for tasks to complete...");
    let failed = App::instance().state.wait_for_all().await;

    drop(g1);
    drop(g2);

    if failed {
        std::process::exit(1);
    }
}
