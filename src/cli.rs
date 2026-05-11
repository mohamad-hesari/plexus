use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "plexus",
    bin_name = "plexus",
    display_name = "Plexus: Monorepo Task Orchestrator",
    author = "Mohammad Hesari <mohamad.hesari@hotmail.com>",
    // env!("CARGO_PKG_VERSION") pulls the version from Cargo.toml at compile time
    version = env!("CARGO_PKG_VERSION"), 
    about = "A TUI for dependency-aware pnpm task execution",
    long_about = "Plexus is a high-performance terminal UI designed for pnpm monorepos. 
It executes filtered workspace tasks by traversing the dependency graph from the bottom up, 
ensuring that internal dependencies are fully built before dependent tasks begin.

Note: All prerequisite tasks must be finite processes (e.g., build, test, lint). 
Long-running processes like 'vite' or 'tsc --watch' should only be used for the final target task."
)]
pub struct Cli {
    #[arg(
        short,
        long,
        default_value = "false",
        help = "Use TUI interface, if this is true, the watch flag will be set to true also this flag will override the console and web flags"
    )]
    pub tui: bool,

    #[arg(short, long, default_value = "false", help = "Use Console interface")]
    pub console: bool,

    #[arg(short, long, default_value = "false", help = "Use Web interface")]
    pub web: bool,

    #[arg(short, long, default_value = "false")]
    pub verbose: bool,

    #[arg(
        long,
        alias = "log-file",
        help = "The file to log the output of the tasks to"
    )]
    pub log_file: Option<String>,

    #[arg(
        long,
        alias = "log-console",
        help = "Log the output of the tasks to the console",
        default_value = "false"
    )]
    pub log_console: bool,

    #[arg(
        short,
        long,
        help = "Filter tasks to run, only tasks that match the filter will be run"
    )]
    pub filter: Vec<String>,

    #[arg(
        short = 'C',
        long,
        help = "The command to run (e.g. dev, build, test, etc.)"
    )]
    pub commands: Vec<String>,

    #[arg(
        short = 'W',
        long,
        default_value = "false",
        help = "Watch the tasks and rerun them when their dependencies change"
    )]
    pub watch: bool,

    #[arg(
        short = 'I',
        long,
        help = "Ignore files that match the glob pattern when watching for changes"
    )]
    pub watch_ignore: Vec<String>,

    #[arg(
        short,
        long,
        default_value = "false",
        help = "Show all the dependencies of the tasks, event those that are not going to be run"
    )]
    pub show_all_dependencies: bool,
}
