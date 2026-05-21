use better_tokio_select::tokio_select;
use clap::Parser;
use owo_colors::{AnsiColors, OwoColorize};
use schemars::schema_for;
use std::{
  hash::{DefaultHasher, Hash, Hasher},
  path::Path,
  sync::{Arc, atomic::AtomicBool},
};
use time::macros::format_description;
use tracing::{Event, Level, Metadata, debug, info};
use tracing_core::field::Visit;
use tracing_subscriber::{
  fmt::{FmtContext, FormatEvent, FormatFields, time::UtcTime},
  layer::{Context, Filter},
  registry::LookupSpan,
};

use plexus::{
  app::App,
  cli::{Commands, NewCli},
  config::{Config, init_default_config},
  task_manager::TaskManager,
  task_managerv2::{self},
  tui_manager::TuiManager,
  tui_managerv2,
  watch_manager::WatchManager,
  watch_managerv2,
};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*};

#[derive(Default)]
struct CustomFieldVisitor {
  name: Option<String>,
  message: Option<String>,
}

impl Visit for CustomFieldVisitor {
  fn record_str(&mut self, field: &tracing_core::Field, value: &str) {
    if field.name() == "name" {
      self.name = Some(value.to_string());
    } else if field.name() == "stdout" || field.name() == "stderr" {
      self.message = Some(value.to_string());
    }
  }

  fn record_debug(&mut self, field: &tracing_core::Field, value: &dyn std::fmt::Debug) {
    let field_name = field.name();
    if field_name == "name" {
      self.name = Some(format!("{:?}", value).trim_matches('"').to_string());
    } else if field_name == "stdout" || field_name == "stderr" {
      self.message = Some(format!("{:?}", value).trim_matches('"').to_string());
    }
  }
}

struct CustomConsoleFormatter {
  is_custom_mode: bool,
}

impl<S, N> FormatEvent<S, N> for CustomConsoleFormatter
where
  S: tracing_core::Subscriber + for<'a> LookupSpan<'a>,
  N: for<'writer> FormatFields<'writer> + 'static,
{
  fn format_event(
    &self,
    ctx: &FmtContext<'_, S, N>,
    mut writer: tracing_subscriber::fmt::format::Writer<'_>,
    event: &Event<'_>,
  ) -> std::fmt::Result {
    // Mode A: Default standard log layout if a CLI level was requested
    if !self.is_custom_mode {
      write!(writer, "[{}] ", event.metadata().level())?;
      ctx.field_format().format_fields(writer.by_ref(), event)?;
      return writeln!(writer);
    }

    // Mode B: Custom "[name]: message" layout
    let mut visitor = CustomFieldVisitor::default();
    event.record(&mut visitor);

    if let (Some(name), Some(message)) = (visitor.name, visitor.message) {
      let mut hasher = DefaultHasher::new();
      name.hash(&mut hasher);
      let hash = hasher.finish();

      let color_palette = [
        AnsiColors::Red,
        AnsiColors::Green,
        AnsiColors::Yellow,
        AnsiColors::Blue,
        AnsiColors::Magenta,
        AnsiColors::Cyan,
        AnsiColors::BrightRed,
        AnsiColors::BrightGreen,
        AnsiColors::BrightYellow,
        AnsiColors::BrightBlue,
        AnsiColors::BrightMagenta,
        AnsiColors::BrightCyan,
      ];
      let assigned_color = color_palette[(hash % color_palette.len() as u64) as usize];

      let tag_string = format!("[{}]", name);
      writeln!(writer, "{}: {}", tag_string.color(assigned_color), message)?;
    } else {
      // Fallback for standard warnings/errors running in custom mode
      write!(writer, "[{}] ", event.metadata().level())?;
      ctx.field_format().format_fields(writer.by_ref(), event)?;
      writeln!(writer)?;
    }
    Ok(())
  }
}

pub struct DynamicFilter {
  cli_level: Option<Level>,
}

impl<S> Filter<S> for DynamicFilter {
  fn enabled(&self, metadata: &Metadata<'_>, _: &Context<'_, S>) -> bool {
    if let Some(max_level) = self.cli_level {
      // Rule A: If user provided a CLI log level, only show logs at or above that level
      metadata.level() <= &max_level
    } else {
      // Rule B: If NO CLI log level provided, allow WARN and ERROR through automatically
      if metadata.level() <= &Level::WARN {
        return true;
      }
      // For INFO and below, let the custom formatter decide based on fields (checked next)
      if metadata.level() == &Level::INFO {
        return metadata
          .fields()
          .iter()
          .any(|f| f.name() == "stdout" || f.name() == "stderr");
      }
      false
    }
  }
}

pub fn init_tracing(
  _has_tui: bool,
  has_console: bool,
  is_comapct: bool,
  log_level: Option<String>,
  log_file: Option<String>,
) -> (Option<WorkerGuard>, Option<WorkerGuard>) {
  // let tui_layer = if has_tui { Some(TuiTracingLayer::new()) } else { None };
  let cli_level = log_level.as_ref().and_then(|l| l.parse::<Level>().ok());
  let is_custom_mode = cli_level.is_none();
  let filter = DynamicFilter { cli_level };
  // let (filter_layer, is_custom_mode) = match &log_level {
  //   Some(level) => {
  //     // Level passed -> Run regular filtration
  //     let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
  //     (filter, false)
  //   }
  //   None => {
  //     // No level passed -> Target only logs that feature a stdout or stderr key
  //     let filter = EnvFilter::new("info[stdout],info[stderr]");
  //     (filter, true)
  //   }
  // };

  let (console_layer, _console_guard) = if has_console {
    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
    let layer = fmt::layer().with_writer(writer).with_ansi(false);
    if is_custom_mode {
      (
        Some(
          layer
            .event_format(CustomConsoleFormatter { is_custom_mode })
            .with_filter(filter)
            .boxed(),
        ),
        Some(guard),
      )
    } else if is_comapct {
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

  let (file_layer, _guard) = if let Some(log_file) = &log_file {
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
    // .with(tui_layer)
    // .with(
    //   filter_layer,
    //   // EnvFilter::try_from_default_env()
    //   //   .unwrap_or_else(|_| EnvFilter::new(if is_verbose { verbose_name } else { "info" })),
    // )
    .with(console_layer)
    .with(file_layer)
    .init();

  (_console_guard, _guard)
}

#[tokio::main]
async fn _old_main() {
  let cli = App::instance().cli.clone();
  let (g1, g2) = init_tracing(cli.tui, cli.console, cli.compact, None, cli.log_file.clone());
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

#[tokio::main]
async fn main() {
  let cli = NewCli::parse();
  match cli.command {
    Commands::PrintSchema(args) => {
      let schema = schema_for!(Config);
      let schema_json = serde_json::to_string_pretty(&schema).unwrap();
      if let Some(output) = args.output {
        std::fs::write(output, schema_json).expect("Failed to write schema to file ");
      } else {
        println!("{}", serde_json::to_string_pretty(&schema).unwrap());
      }
      return;
    }
    Commands::Config(args) => {
      if args.share.cwd.is_some() {
        std::env::set_current_dir(args.share.cwd.unwrap()).expect("Failed to change working directory");
      }
      if args.generate {
        let mut config = init_default_config().await;
        for p in config.projects.iter_mut() {
          let Some(watches) = &p.watches else {
            continue;
          };
          let watches = watches
            .iter()
            .filter(|w| w.path.is_some() || w.include.is_some() || w.exclude.is_some())
            .collect::<Vec<_>>();
          if watches.is_empty() {
            p.watches = None;
          }
        }
        let config_json = serde_json::to_string_pretty(&config).unwrap();
        let config_path = args.config_file.unwrap_or_else(|| "plexus.json".to_string());
        std::fs::write(config_path, config_json).expect("Failed to write config to file");
        let schema_path = Path::new(&config.schema);
        if !schema_path.exists() {
          let schema = schema_for!(Config);
          let schema_json = serde_json::to_string_pretty(&schema).unwrap();
          std::fs::write(schema_path, schema_json).expect("Failed to write schema to file");
        }
        println!("Default configuration generated successfully.");
        return;
      }
    }
    Commands::Run(cli) => {
      let (g1, g2) = init_tracing(cli.tui, !cli.tui, cli.compact, cli.log_level, cli.log_file.clone());
      if cli.run_share.cwd.is_some() {
        std::env::set_current_dir(cli.run_share.cwd.unwrap()).expect("Failed to change working directory");
      }
      let config_path = cli.config_path.clone();
      let use_file_config = config_path.is_some_and(|f| Path::new(&f).exists());
      let is_running = Arc::new(AtomicBool::new(true));
      let config = Arc::new(if use_file_config {
        if let Some(f) = &cli.config_path {
          let reader = std::fs::File::open(&f).expect("Failed to open config file");
          serde_json::from_reader(reader).expect("Failed to parse config file")
        } else {
          init_default_config().await
        }
      } else {
        init_default_config().await
      });
      let task_manager = Arc::new(task_managerv2::TaskManager::new(
        Arc::clone(&is_running),
        cli.show_colors,
      ));
      let mut set = tokio::task::JoinSet::new();
      if cli.watch {
        let watch_manager = Arc::new(watch_managerv2::WatchManager::new(
          Arc::clone(&task_manager),
          Arc::clone(&is_running),
          cli.watch_ignore.clone(),
        ));
        set.spawn(async move {
          watch_manager.main_loop().await;
        });
      }
      if cli.tui {
        let tui_manager = Arc::new(tui_managerv2::TuiManager::new(
          Arc::clone(&task_manager),
          Arc::clone(&is_running),
        ));
        set.spawn(async move {
          tui_manager.main_loop().await;
        });
      } else {
        set.spawn(async move {
          loop {
            if !is_running.load(std::sync::atomic::Ordering::Relaxed) {
              break;
            }
            debug!("Waiting for Ctrl+C...");
            tokio_select!(
              biased,
              match .. {
                .. if let event = tokio::signal::ctrl_c() => {
                  if let Err(e) = event {
                    eprintln!("Failed to listen for Ctrl+C: {}", e);
                    return;
                  }
                  info!("Shutting down Plexus...");
                  is_running.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                _ => {
                  tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                  tokio::task::yield_now().await;
                }
              }
            );
          }
        });
        info!("Plexus is running. Press Ctrl+C to exit.");
      }
      debug!(
        "Loading tasks from config..., filter: {:?}, commands: {:?}",
        cli.filter, cli.commands
      );
      let commands_len = cli.commands.len() as i8;
      task_manager
        .load_tasks(Arc::clone(&config), cli.filter, cli.commands)
        .await;
      // Task manager main loop will block until watch mode is exited or the app is shutting down
      let result = task_manager
        .main_loop(cli.watch, if cli.sequential { commands_len } else { -1 })
        .await;
      if !result {
        std::process::exit(1);
      }
      drop(g1);
      drop(g2);
    }
  }
}
