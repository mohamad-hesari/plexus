use clap::{Args, Parser, Subcommand};

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

  #[arg(long, alias = "log-file", help = "The file to log the output of the tasks to")]
  pub log_file: Option<String>,

  #[arg(
    long,
    alias = "log-console",
    help = "Log the output of the tasks to the console",
    default_value = "false"
  )]
  pub log_console: bool,

  #[arg(
    short = 'D',
    long = "compact",
    help = "Use compact logging format (HH:mm:ss) instead of the default format (YYYY-MM-DD HH:mm:ss)"
  )]
  pub compact: bool,

  #[arg(
    short,
    long,
    help = "Filter tasks to run, only tasks that match the filter will be run"
  )]
  pub filter: Vec<String>,

  #[arg(short = 'C', long, help = "The command to run (e.g. dev, build, test, etc.)")]
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
pub struct NewCli {
  #[command(subcommand)]
  pub command: Commands,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
  Run(RunArgs),
  Config(ConfigArgs),
  #[command(hide = true)]
  PrintSchema(PrintSchemaArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ShareArgs {
  #[arg(
    short = 'd',
    long,
    help = "Execute the command in the current working directory instead of the root of the monorepo"
  )]
  pub cwd: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct PrintSchemaArgs {
  #[arg(
    short,
    long,
    help = "The file to output the schema to, if not provided, the schema will be printed to the console"
  )]
  pub output: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct RunArgs {
  #[command(flatten)]
  pub run_share: ShareArgs,

  #[arg(long, default_value = "false")]
  pub show_colors: bool,

  #[arg(
    short,
    long,
    default_value = "false",
    help = "Use TUI interface, if this is true, the watch flag will be
  set to true also this flag will override the console and web flags"
  )]
  pub tui: bool,

  #[arg(
    short,
    long,
    help = "Use a config file to load the tasks to run, the default config file is plexus.json in the current working directory"
  )]
  pub config_path: Option<String>,

  #[arg(
    short,
    long,
    help = "The log level to use, ex. debug,info,warning,error; the default is command outputs"
  )]
  pub log_level: Option<String>,

  #[arg(long, alias = "log-file", help = "The file to log the output of the tasks to")]
  pub log_file: Option<String>,

  #[arg(
    short = 'P',
    long = "compact",
    help = "Use compact logging format (HH:mm:ss) instead of the default format (YYYY-MM-DD HH:mm:ss)"
  )]
  pub compact: bool,

  #[arg(
    short,
    long,
    help = "Filter tasks to run, only tasks that match the filter will be run"
  )]
  pub filter: Vec<String>,

  #[arg(short = 'C', long, help = "The command to run (e.g. dev, build, test, etc.)")]
  pub commands: Vec<String>,

  #[arg(
    short = 'W',
    long,
    default_value = "false",
    help = "Watch the tasks and rerun them when their dependencies change"
  )]
  pub watch: bool,

  #[arg(
    short = 'S',
    long = "seq",
    default_value = "false",
    help = "Watch the tasks and rerun them when their dependencies change"
  )]
  pub sequential: bool,

  #[arg(
    short = 'I',
    long,
    help = "Ignore files that match the glob pattern when watching for changes"
  )]
  pub watch_ignore: Vec<String>,

  #[arg(
    long,
    default_value = "false",
    help = "When watching for changes, also watch for changes in which tasks depend on the changed tasks"
  )]
  pub build_depends_on: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ConfigArgs {
  #[arg(
    short,
    long,
    default_value = "plexus.json",
    help = "The path to the config file to use"
  )]
  pub config_file: Option<String>,

  #[command(flatten)]
  pub share: ShareArgs,

  #[arg(
    short,
    long,
    default_value = "false",
    help = "Show the current configuration and exit"
  )]
  pub show: bool,

  #[arg(
    short,
    long,
    default_value = "false",
    help = "Generate a default configuration file and exit, if the config file already exists, it will be overwritten"
  )]
  pub generate: bool,

  #[arg(
    short,
    long,
    default_value = "false",
    help = "Add the generated configuration to the existing config file, if the config file already exists"
  )]
  pub add: bool,
}
