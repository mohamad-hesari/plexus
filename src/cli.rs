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
  /// Talk to a `plexus run -W` running in this project: tasks, logs, start/stop, ports.
  Client(ClientArgs),
  /// Instructions for AI agents, printed from this binary so they match its version.
  Skills(SkillsArgs),
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
    long,
    default_value = "false",
    help = "Capture the mouse so the wheel scrolls the log. Off by default because capturing the mouse takes click-drag text selection away from your terminal; press 'm' to toggle it at runtime. Inside tmux this also needs `set -g mouse on`, otherwise tmux never asks the terminal for mouse events and nothing reaches plexus."
  )]
  pub mouse: bool,

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

  #[arg(
    short = 'D',
    long,
    help = "This is only effective when the build-depends-on flag is set to true, and filters commands that depend on the changed tasks"
  )]
  pub depends_on: Option<Vec<String>>,

  #[arg(
    long,
    default_value = "600",
    value_parser = parse_mode,
    help = "Octal permission bits for node_modules/plexus.sock and plexus.token in watch mode (Unix only). Inside an unprivileged container the client's uid is shifted, so either map it (incus: shift=true) or widen this, e.g. 666"
  )]
  pub socket_mode: u32,

  #[arg(
    long,
    value_name = "ADDR:PORT",
    help = "In watch mode, also accept control connections over TCP (e.g. 0.0.0.0:0). Writes node_modules/plexus.port and a token to node_modules/plexus.token; clients pass --tcp and need the token"
  )]
  pub control_tcp: Option<String>,
}

fn parse_mode(s: &str) -> Result<u32, String> {
  u32::from_str_radix(s.trim_start_matches("0o"), 8)
    .ok()
    .filter(|m| *m <= 0o777)
    .ok_or_else(|| format!("'{}' is not an octal mode like 600", s))
}

#[derive(Args, Debug, Clone)]
pub struct ClientArgs {
  #[arg(
    long,
    global = true,
    help = "Project root; default is the first directory upwards with a running plexus"
  )]
  pub root: Option<String>,

  #[arg(
    long,
    global = true,
    help = "Socket path (Unix) or pipe name (Windows) to connect to directly"
  )]
  pub socket: Option<String>,

  #[arg(
    long,
    global = true,
    value_name = "HOST:PORT",
    help = "Connect over TCP to a plexus started with --control-tcp"
  )]
  pub tcp: Option<String>,

  #[arg(
    long,
    global = true,
    help = "Token file for --tcp; default is node_modules/plexus.token found upwards"
  )]
  pub token_file: Option<String>,

  #[arg(long, global = true, help = "Print one JSON object per line instead of text")]
  pub json: bool,

  #[command(subcommand)]
  pub action: ClientAction,
}

#[derive(Args, Debug, Clone)]
pub struct SkillsArgs {
  #[arg(
    long,
    global = true,
    help = "Command to show in the text instead of `plexus`, e.g. \"pnpm exec plexus\""
  )]
  pub cmd: Option<String>,

  #[command(subcommand)]
  pub action: SkillsAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SkillsAction {
  /// Available skills and reference docs.
  List {
    #[arg(long)]
    json: bool,
  },
  /// Print a skill (as SKILL.md) or reference doc; without a slug, how to use them.
  Get { slug: Option<String> },
}

#[derive(Subcommand, Debug, Clone)]
pub enum ClientAction {
  /// Packages loaded with -f and the commands loaded with -C.
  List,
  /// Status, pid and listening ports of each command. TARGET is `pkg` or `pkg:command`.
  Status { target: Option<String> },
  /// Print a command's output.
  Logs {
    target: String,
    #[arg(short, long, help = "Keep streaming new output")]
    follow: bool,
    #[arg(short = 'n', long, help = "Only the last N lines")]
    tail: Option<usize>,
  },
  /// Start a command and stream its run; exits 0 if it succeeds.
  Start {
    target: String,
    #[arg(long, help = "Return right after triggering (use for long-lived commands like dev)")]
    no_wait: bool,
  },
  /// Stop a running command.
  Stop { target: String },
  /// Restart a command and stream the new run; exits 0 if it succeeds.
  Restart {
    target: String,
    #[arg(long, help = "Return right after triggering (use for long-lived commands like dev)")]
    no_wait: bool,
  },
  /// Listening TCP ports discovered under each command's process tree.
  Ports { target: Option<String> },
  /// Listen locally and relay every connection to PORT of PACKAGE through plexus.
  Forward {
    package: String,
    port: u16,
    #[arg(
      long,
      value_name = "ADDR:PORT",
      help = "Local listen address, default 127.0.0.1:<PORT>"
    )]
    listen: Option<String>,
    #[arg(long, help = "Run in the background and return once the forward is up")]
    detach: bool,
  },
  /// Active forwards.
  Forwards,
  /// Stop forwards by id, by `pkg` or `pkg:port`, or all of them.
  Unforward {
    selector: Option<String>,
    #[arg(long)]
    all: bool,
  },
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
