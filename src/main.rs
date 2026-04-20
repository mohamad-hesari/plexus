use clap::Parser;

use crate::share::AppInterface;

mod app;
mod cli;
mod console;
mod pnpm;
mod share;
mod task;
mod tui;
mod watcher;

enum Interface {
    Console(console::Console),
    Tui(tui::Tui),
}

impl AppInterface for Interface {
    async fn set_app(&mut self, app: crate::app::App) {
        match self {
            Interface::Console(console) => console.set_app(app).await,
            Interface::Tui(tui) => tui.set_app(app).await,
        }
    }

    async fn wait(&self) {
        match self {
            Interface::Console(console) => console.wait().await,
            Interface::Tui(tui) => tui.wait().await,
        }
    }
}

#[tokio::main]
async fn main() {
    let mut cli = cli::Cli::parse();
    if cli.interface == cli::Interface::Tui {
        cli.watch = true;
    }

    let mut app_instance = match cli.interface {
        cli::Interface::Console => Interface::Console(console::Console::new(&cli).await),
        cli::Interface::Tui => Interface::Tui(tui::Tui::new().await),
        // Interface::Web => {
        //     log!("Running tasks with TUI...");
        //     Box::new(web::Web::new().await)
        // }
    };

    let app = app::App::new(cli.clone()).await;
    app_instance.set_app(app).await;
    app_instance.wait().await;
}
