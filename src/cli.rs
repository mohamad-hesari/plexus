use clap::{Parser, ValueEnum};

#[derive(Parser, Debug, Clone)]
#[command(
    name = "pnpm-task-tui",
    bin_name = "ptt",
    display_name = "pnpm Tasks Terminal UI",
    author = "Mohammad Hesari <mohammad.hesari@hotmail.com>",
    version = "1.0",
    about = "This run pnpm task in parallel considering their dependencies",
    long_about = "This run pnpm task in parallel considering their dependencies. 
It can also watch the tasks and rerun them when their dependencies change. 
All dependencies of the tasks must not be a long live process (e.g. dev) 
and must exit after running (e.g. dev, build, test, etc.)\n
Long live process example: tsc --watch, vite, tsup --watch, etc."
)]
pub struct Cli {
    #[arg(
        short,
        long,
        value_enum,
        default_value = "Tui",
        help = "The interface to use (e.g. console, tui, web)"
    )]
    pub interface: Interface,

    #[arg(short, long, default_value = "false")]
    pub verbose: bool,

    #[arg(
        short,
        long,
        help = "Filter tasks to run, only tasks that match the filter will be run"
    )]
    pub filter: Vec<String>,

    #[arg(short, long, help = "The command to run (e.g. dev, build, test, etc.)")]
    pub command: String,

    #[arg(
        short,
        long,
        default_value = "false",
        help = "Watch the tasks and rerun them when their dependencies change"
    )]
    pub watch: bool,

    #[arg(
        short,
        long,
        default_value = "false",
        help = "Show all the dependencies of the tasks, event those that are not going to be run"
    )]
    pub show_all_dependencies: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Interface {
    Console,
    Tui,
    // Web,
}
