use std::{
  collections::{HashMap, HashSet},
  path::PathBuf,
  sync::Arc,
};

use chrono::{DateTime, TimeDelta, Utc};
use futures::{StreamExt, stream};
use tokio::{
  io::{AsyncBufReadExt, BufReader},
  process::Command,
  task::JoinSet,
  time::Instant,
};

use command_group::AsyncCommandGroup;
use std::{
  fmt::Display,
  sync::atomic::{AtomicBool, Ordering},
  time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
  sync::{
    RwLock,
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
  },
  task::yield_now,
  time::{interval, sleep},
};
use tracing::{debug, error, info, trace, warn};

use crate::{
  config::{Config, ConfigCommand, OptionConfig},
  env::Env,
  hmr_websocket::HmrWebSocket,
};

fn uuid() -> String {
  uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone)]
pub struct TaskManagerWatchData {
  pub task_id: String,
  pub task_path: String,
  pub includes: Vec<String>,
  pub excludes: Vec<String>,
  pub use_default_watch_ignore: bool,
}

pub enum TaskManagerEvent {
  FileChanged {
    task_id: String,
  },
  TaskStatusChanged {
    task_id: String,
    command_id: String,
    status: TaskStatus,
  },
  TaskLogAdded {
    task_id: String,
    command_id: String,
    log: String,
  },
}

pub struct TaskManager {
  _store: Arc<TaskStore>,
  _is_running: Arc<AtomicBool>,
  _is_watch_mode: Arc<AtomicBool>,
  _watch_data_list: Arc<RwLock<Vec<TaskManagerWatchData>>>,
  _rx: Arc<RwLock<UnboundedReceiver<TaskManagerEvent>>>,
  _sx: Arc<RwLock<UnboundedSender<TaskManagerEvent>>>,
  _build_depends_on: bool,
  _depends_on: Option<Vec<String>>,
  _is_loading: Arc<AtomicBool>,
  _hmr_manager: Arc<HmrWebSocket>,
}

impl TaskManager {
  pub fn new(
    hmr_manager: Arc<HmrWebSocket>,
    is_running: Arc<AtomicBool>,
    show_colors: bool,
    build_depends_on: bool,
    depends_on: Option<Vec<String>>,
  ) -> Self {
    let (sx, rx) = unbounded_channel();
    Self {
      _store: Arc::new(TaskStore::new(show_colors)),
      _is_running: is_running,
      _is_watch_mode: Arc::new(AtomicBool::new(false)),
      _watch_data_list: Arc::new(RwLock::new(Vec::new())),
      _rx: Arc::new(RwLock::new(rx)),
      _sx: Arc::new(RwLock::new(sx)),
      _build_depends_on: build_depends_on,
      _depends_on: depends_on,
      _is_loading: Arc::new(AtomicBool::new(true)),
      _hmr_manager: hmr_manager,
    }
  }

  pub async fn is_watch_mode(&self) -> bool {
    self._is_watch_mode.load(Ordering::Relaxed)
  }

  pub async fn get_watch_data(&self) -> Vec<TaskManagerWatchData> {
    let data = self._watch_data_list.read().await;
    data.clone()
  }

  pub async fn get_envent_sender(&self) -> Arc<UnboundedSender<TaskManagerEvent>> {
    let sx = self._sx.read().await;
    Arc::new(sx.clone())
  }

  pub async fn is_loading(&self) -> bool {
    self._is_loading.load(Ordering::Relaxed)
  }

  pub async fn stop_command(&self, task_id: &str, command_id: &str, force: bool) -> bool {
    let task = self._store.get_task(task_id).await;
    let command = task._commands.get(command_id).unwrap_or_else(|| {
      panic!(
        "Trying to stop non-existing command with id: {} for task with id: {}",
        command_id, task_id
      )
    });

    if force || command._status.is_running() || command._status.is_starting() {
      self._store.set_status(command_id, TaskStatus::Stopping).await;
      let start_time = Instant::now();
      loop {
        let status = self._store.get_status(command_id).await;
        if status.is_stopped() {
          info!(
            name = task._name,
            stdout = format!("Command {} is stopped", command._command)
          );
          return true;
        }
        sleep(Duration::from_millis(50)).await;
        if start_time.elapsed() > Duration::from_secs(5) {
          warn!(
            name = task._name,
            stdout = format!("Timeout while waiting for command {} to stop", command._command)
          );
          break;
        }
      }
    }
    false
  }

  pub async fn start_command(&self, task_id: &str, command_id: &str, force: bool) -> bool {
    let task = self._store.get_task(task_id).await;
    let command = task._commands.get(command_id).unwrap_or_else(|| {
      panic!(
        "Trying to start non-existing command with id: {} for task with id: {}",
        command_id, task_id
      )
    });

    if force || command._status.is_init() || command._status.is_finished() {
      self._store.set_status(command_id, TaskStatus::Init).await;
    }
    false
  }

  pub async fn restart_command(&self, task_id: &str, command_id: &str, force: bool) {
    let _ = self.stop_command(task_id, command_id, force).await;
    self.start_command(task_id, command_id, force).await;
  }

  pub fn get_store(&self) -> Arc<TaskStore> {
    Arc::clone(&self._store)
  }

  pub async fn file_changed(&self, _task_id: &str, paths: Vec<PathBuf>) {
    trace!(
      "File changed for task with id: {}, resetting its status and the status of its commands",
      _task_id
    );
    let mut tasks = vec![];
    tasks.push(_task_id.to_string());
    let store_tasks = self._store.get_all_tasks().await;
    let current_changed_task = store_tasks
      .iter()
      .find(|t| t._id == _task_id)
      .unwrap_or_else(|| panic!("Trying to get non-existing task with id: {}", _task_id));
    for path in paths.clone() {
      info!(
        name = current_changed_task._name,
        stdout = format!("File {} changed, resetting status of related tasks", path.display())
      );
    }
    let mut hmr_tasks = HashMap::new();
    if self._build_depends_on {
      loop {
        let mut have_added_more = false;
        for t in &store_tasks {
          let Some(children_id) = &t._children_id else {
            continue;
          };
          if tasks.contains(&t._id) {
            continue;
          }
          if t._commands.values().any(|cmd| cmd._status.is_running()) {
            hmr_tasks.insert(t._name.clone(), paths.clone());
          }

          if children_id.iter().any(|id| tasks.iter().any(|t| t == id)) {
            tasks.push(t._id.clone());
            have_added_more = true;
          }
        }
        if !have_added_more {
          break;
        }
      }
    }
    for task in store_tasks
      .iter()
      .filter(|t| tasks.iter().any(|task_id| task_id == &t._id))
    {
      for cmd in task._commands.values() {
        if self._build_depends_on
          && !self
            ._depends_on
            .as_ref()
            .map_or(true, |depends_on| depends_on.iter().any(|d| cmd._command == d.as_str()))
        {
          continue;
        }
        if cmd._status.is_init() || cmd._status.is_finished() {
          self._store.set_status(&cmd._id, TaskStatus::Init).await;
        }
      }
    }
  }

  #[async_recursion::async_recursion]
  async fn load_tasks_internal(
    &self,
    config: Arc<Config>,
    filters: Vec<String>,
    commands: Vec<String>,
    parent_id: Option<String>,
    depth: u8,
  ) -> Vec<String> {
    if depth > 20 {
      panic!("Dependency depth exceeds the limit of 10");
    }

    let mut loaded_task_ids = Vec::new();
    let mut pnpm_filters = HashSet::new();
    for filter in &filters {
      let project = config.projects.iter().any(|p| p.name == filter.to_string());
      if project {
        pnpm_filters.insert(filter.to_string());
        continue;
      }
      let cmd = Command::new(crate::pnpm_bin::pnpm_or_exit())
        .arg("list")
        .arg("--filter")
        .arg(filter)
        .arg("--depth=-1")
        .arg("--json")
        .output()
        .await
        .and_then(|output| {
          if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            debug!("pnpm list output for filter '{}': {}", filter, output_str);
            let json: serde_json::Value = serde_json::from_str(&output_str)?;
            let packages = json
              .as_array()
              .unwrap_or(&vec![])
              .iter()
              .filter_map(|pkg| {
                pkg
                  .get("name")
                  .and_then(|n| Some(n.to_string()))
                  .map(|s| s.trim_matches('"').to_string())
              })
              .collect::<Vec<_>>();
            Ok(packages)
          } else {
            Ok(vec![])
          }
        })
        .unwrap_or(vec![]);

      for pkg in cmd {
        pnpm_filters.insert(pkg);
      }
    }

    for filter in pnpm_filters {
      if self._store.task_exists(&filter).await {
        continue;
      }
      let project = config
        .projects
        .iter()
        .find(|p| p.name == filter)
        .unwrap_or_else(|| panic!("No project found with name: {}", filter));
      let project_commands = project
        .commands
        .iter()
        .filter(|cmd| {
          if let ConfigCommand::Simple(cmd_str) = cmd {
            if commands.contains(cmd_str) { true } else { false }
          } else if let ConfigCommand::WithDependency {
            command,
            depends_on: _,
            envs: _,
          } = cmd
          {
            if commands.contains(command) { true } else { false }
          } else if let ConfigCommand::Plexus(plexus) = cmd {
            if commands.contains(&plexus.name) { true } else { false }
          } else {
            false
          }
        })
        .cloned()
        .collect::<Vec<_>>();
      if project_commands.is_empty() {
        continue;
      }
      let id = uuid();
      loaded_task_ids.push(id.clone());
      let childs_id = if let OptionConfig::List(depends_on) = &project.depends_on {
        Some(
          self
            .load_tasks_internal(
              config.clone(),
              depends_on.clone(),
              commands.clone(),
              Some(id.clone()),
              depth + 1,
            )
            .await,
        )
      } else {
        None
      };
      let task = Task::new(
        id.clone(),
        project.name.clone(),
        project.path.clone(),
        project_commands,
        parent_id.clone(),
        childs_id,
      );
      self._store.add_task(task).await;
      let watch_data = TaskManagerWatchData {
        task_id: id.clone(),
        task_path: project.path.clone(),
        includes: project
          .watches
          .as_ref()
          .map(|w| w.iter().filter_map(|w| w.include.clone()).flatten().collect())
          .unwrap_or_else(Vec::new),
        excludes: project
          .watches
          .as_ref()
          .map(|w| w.iter().filter_map(|w| w.exclude.clone()).flatten().collect())
          .unwrap_or_else(Vec::new),
        use_default_watch_ignore: project.watches.as_ref().map_or(true, |w| {
          w.iter()
            .all(|w| w.path.is_none() && w.include.is_none() && w.exclude.is_none())
        }),
      };
      {
        let mut watch_data_list = self._watch_data_list.write().await;
        watch_data_list.push(watch_data);
        drop(watch_data_list);
      }
      debug!(
        "Loaded task with id: {}, name: {}, parent_id: {:?}",
        id, project.name, parent_id,
      );
    }
    loaded_task_ids
  }

  pub async fn load_tasks(&self, config: Arc<Config>, filters: Vec<String>, commands: Vec<String>) {
    let config_clone = Arc::clone(&config);
    _ = self.load_tasks_internal(config_clone, filters, commands, None, 0).await;
    let all_tasks = self._store.get_all_tasks().await;

    for task in all_tasks {
      let config_project = config
        .projects
        .iter()
        .find(|p| p.name == task._name)
        .unwrap_or_else(|| panic!("No project found with name: {}", task._name));
      if let OptionConfig::List(depends_on) = &config_project.depends_on {
        self._store.update_task_childs(&task._name, depends_on.clone()).await;
      }
    }
    debug!(
      "Finished loading tasks, total tasks count: {}",
      self._store.get_all_tasks().await.len()
    );
    self._is_loading.store(false, Ordering::Relaxed);
  }

  pub async fn main_loop(&self, _watch_mode: bool, sequential: i8) -> bool {
    let mut result = true;
    let mut last_log_time = Instant::now();
    let log_interval = Duration::from_secs(5);
    let start_time = Instant::now();
    let mut tasks_set = JoinSet::new();
    loop {
      if !self._is_running.load(Ordering::Relaxed) {
        info!("App is not running, exiting main loop");
        break;
      }
      if !_watch_mode {
        if self._store.is_all_finished().await {
          debug!("Not in watch mode, exiting main loop");
          break;
        }
        if self._store.is_any_failed().await {
          debug!("Not in watch mode, exiting main loop with failure");
          result = false;
          break;
        }
      } else {
        let is_watch_mode = self._is_watch_mode.load(Ordering::Relaxed);
        if !is_watch_mode && self._store.is_all_running_or_finished().await {
          self._is_watch_mode.store(true, Ordering::Relaxed);
        }
        if start_time.elapsed() > Duration::from_secs(10) && !is_watch_mode {
          if self._store.is_all_not_init().await {
            self._is_watch_mode.store(true, Ordering::Relaxed);
          } else if self._store.is_any_failed().await {
            self._is_watch_mode.store(true, Ordering::Relaxed);
          }
        }
      }

      if last_log_time.elapsed() >= log_interval {
        trace!(
          name = "TaskManager",
          stdout = format!("Current task statuses: \n{}", {
            let tasks = self._store.get_all_tasks().await;
            let mut status_report = String::new();
            for task in tasks {
              for cmd in task._commands.values() {
                status_report.push_str(&format!(
                  "Task: {}, Command: {}, Status: {}\n",
                  task._name, cmd._command, cmd._status
                ));
              }
            }
            status_report.push_str(&format!(
              "Watch mode: {}, watch mode env: {}, is all runing or finished: {}\n",
              self._is_watch_mode.load(Ordering::Relaxed),
              _watch_mode,
              self._store.is_all_running_or_finished().await
            ));

            status_report.push_str(&format!(
              "start time elapsed: {:.2}s, is_watch_mode: {}, is_all_not_init: {}",
              start_time.elapsed().as_secs_f32(),
              self._is_watch_mode.load(Ordering::Relaxed),
              self._store.is_all_not_init().await,
            ));
            status_report
          })
        );

        // Reset the timer for the next 5 seconds
        last_log_time = Instant::now();
      }

      let commands_to_run = self._store.get_runnable_commands(sequential).await;
      for cmd in commands_to_run {
        let store = Arc::clone(&self._store);
        let is_running = Arc::clone(&self._is_running);
        let cmd_id = cmd._id.clone();
        self._store.set_status(&cmd_id, TaskStatus::Starting).await;
        let hmr_socket = Arc::clone(&self._hmr_manager);
        tasks_set.spawn(async move {
          yield_now().await;
          let timer = Instant::now();
          let name = cmd._name.clone();
          let task_runnder = TaskRunner::new(store, hmr_socket, is_running);
          task_runnder.run_command(cmd).await;
          info!(
            name = name,
            stdout = format!(
              "Finished running command, elapsed time: {}s",
              timer.elapsed().as_secs_f32()
            )
          );
        });
        yield_now().await;
      }

      sleep(Duration::from_millis(50)).await;
      yield_now().await;
    }
    self._is_running.store(false, Ordering::Relaxed);
    if !tasks_set.is_empty() {
      debug!("Waiting for all running tasks to finish...");
      while let Some(res) = tasks_set.join_next().await {
        match res {
          Ok(_) => {
            debug!("A task finished successfully");
          }
          Err(e) => {
            error!(error = ?e, "A task panicked");
          }
        }
      }
    }
    debug!("All threads finished, exiting main loop",);
    return result;
  }
}

pub struct TaskRunner {
  _store: Arc<TaskStore>,
  _is_running: Arc<AtomicBool>,
  _hmr_socket: Arc<HmrWebSocket>,
}

impl TaskRunner {
  pub fn new(store: Arc<TaskStore>, hmr_socket: Arc<HmrWebSocket>, is_running: Arc<AtomicBool>) -> Self {
    Self {
      _store: store,
      _is_running: is_running,
      _hmr_socket: hmr_socket,
    }
  }

  pub async fn run_command(&self, cmd: TaskCommandRunnable) {
    // let commands = match &cmd._command {
    //   TaskCommandType::Simple(cmd_str) => vec![cmd_str.clone()],
    //   TaskCommandType::Plexus(plexus_cmd) => plexus_cmd.actual_commands.clone(),
    // };
    // let have_multiple = commands.len() > 1;
    // if have_multiple {
    //   debug!(
    //     "Task: {:?} Command '{}' has multiple sub-commands: {:?}",
    //     cmd, cmd._command, commands
    //   );
    // }
    // let show_colors = self._store._show_colors;
    // let mut set = JoinSet::new();
    // self._command_running.store(true, Ordering::Relaxed);
    // for (idx, command) in commands.iter().enumerate() {
    let task_name = format!("{}", cmd._name);
    debug!(%cmd._id, %task_name, "Spawning task");
    let store = Arc::clone(&self._store);
    let is_running = Arc::clone(&self._is_running);
    let cmd_id = cmd._id.clone();
    let cmd_path = cmd._path.clone();
    let cmd_name = cmd._name.clone();
    let command = if let TaskCommandType::Simple(cmd_str) = &cmd._command {
      cmd_str.clone()
    } else if let TaskCommandType::Plexus(plexus_cmd) = &cmd._command {
      plexus_cmd.command.clone()
    } else {
      unreachable!()
    };
    // set.spawn(async move {
    let envs = Env::new();
    let mut envs = if let TaskCommandType::Plexus(plexus_cmd) = &cmd._command {
      envs.get_envs_with_specific(&cmd_path, Some(&plexus_cmd.command)).await
    } else {
      envs.get_envs(&cmd_path).await
    };
    let mut custom_envs = vec![
      ("PLEXUS_TASK_PATH", cmd_path.to_string()),
      ("PLEXUS_TASK_NAME", task_name.to_string()),
      ("PLEXUS_TASK_COMMAND", command.to_string()),
      ("PLEXUS_PKG_NAME", cmd_name.to_string()),
      (
        "PLEXUS_WS_URL",
        format!("ws://localhost:{}", self._hmr_socket.port.load(Ordering::Relaxed)),
      ),
    ];
    if self._store._show_colors {
      custom_envs.push(("PLEXUS_SHOW_COLORS", "1".to_string()));
    }
    for (ck, cv) in custom_envs {
      envs.insert(ck.to_string(), cv);
    }
    for (k, v) in envs.iter() {
      debug!(%task_name, "ENV: {}={}", k, v);
      // self._store.add_log(&cmd_id, format!("[ENV]: {}={}", k, v)).await;
    }
    let pnpm = match crate::pnpm_bin::pnpm() {
      Ok(p) => p,
      Err(e) => {
        error!(name = task_name, stderr = %e);
        store.add_log(&cmd_id, format!("[ERR]: {}", e)).await;
        store.set_status(&cmd_id, TaskStatus::Failed).await;
        return;
      }
    };
    let spawned = tokio::process::Command::new(pnpm)
      .arg("--filter")
      .arg(&cmd_name)
      .arg(command)
      .envs(&envs)
      .stdin(std::process::Stdio::null())
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::piped())
      .kill_on_drop(true)
      .group()
      .spawn();
    // A command we cannot spawn should fail its own task, not take down the whole run.
    let mut child = match spawned {
      Ok(child) => child,
      Err(e) => {
        let msg = format!("Failed to start task {} via {}: {}", task_name, pnpm.display(), e);
        error!(name = task_name, stderr = %msg);
        store.add_log(&cmd_id, format!("[ERR]: {}", msg)).await;
        store.set_status(&cmd_id, TaskStatus::Failed).await;
        return;
      }
    };

    debug!(%task_name, "Process started for task");

    store.set_status(&cmd_id, TaskStatus::Running).await;

    let stdout = child.inner().stdout.take().expect("no stdout");
    let stderr = child.inner().stderr.take().expect("no stderr");
    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();
    let mut ticker = interval(Duration::from_secs(5));
    let mut killing = false;
    loop {
      if !is_running.load(Ordering::Relaxed) {
        debug!(%task_name, "App is not running, killing process");
        let _ = child.kill().await;
        break;
      }
      if !killing && store.get_status(&cmd_id).await.is_stopping() {
        debug!(%task_name, "Task is stopping, killing process");
        child
          .start_kill()
          .unwrap_or_else(|_| debug!(%task_name, "Failed to send kill signal to process"));
        killing = true;
      }
      tokio_select!(
        biased,
        match .. {
          .. if let line = stdout_reader.next_line() => {
            match line {
              Ok(Some(l)) if !l.trim().is_empty() => {
                self._store.add_log(&cmd_id, format!("[OUT]: {}", l.clone())).await;
                info!(name = task_name, stdout = %l);
              }
              Ok(Some(_)) => {
                debug!(%task_name, "Stdout reader read an empty line, skipping");
              }
              _ => {
                debug!(%task_name, "Stdout reader reached EOF or encountered an error, stopping reading stdout");
                break;
              }
            }
            yield_now().await;
          }
          .. if let line = stderr_reader.next_line() => {
            if let Ok(Some(l)) = line {
              self._store.add_log(&cmd_id, format!("[ERR]: {}", l.clone())).await;
              error!(name = task_name, stderr = %l);
            }
            yield_now().await;
          }
          .. if let _ = ticker.tick() => {
            let is_running = is_running.load(Ordering::Relaxed);
            trace!(%task_name, "Ticker ticked, checking if process is still alive {}",is_running);
            yield_now().await;
          }
          .. if let _ = sleep(Duration::from_millis(50)) => {
            yield_now().await;
          }
          .. if let _ = tokio::signal::ctrl_c() => {
            debug!(%task_name, "Received Ctrl+C signal, killing process");
            let _ = child.kill().await;
            break;
          }
        }
      );
    }

    self._store.set_last_run_time(&cmd._task_id, Utc::now()).await;

    let mut finished_status = TaskStatus::Successed;
    ticker.reset();
    tokio_select!(
      biased,
      match .. {
        .. if let status = child.wait() => {
          if let Ok(status) = status {
            if !status.success() {
              finished_status = TaskStatus::Failed;
              info!(%task_name, "Process exited with failure");
            } else {
              info!(%task_name, "Process exited successfully");
            }
          } else {
            info!(%task_name, "Failed to wait for process");
            finished_status = TaskStatus::Failed;
          }
        }
        .. if let _ = ticker.tick() => {
          child.kill().await.ok();
          debug!(%task_name, "Ticker ticked while waiting for process to exit, killing process");
          yield_now().await;
        }
      }
    );
    if killing {
      debug!(%task_name, "Process was killed, setting status to Failed");
      finished_status = TaskStatus::Stopped;
    }
    debug!(%cmd_id, %task_name, status = ?finished_status, "Finished running task");
    self._store.set_status(&cmd_id, finished_status.clone()).await;
    // finished_status.clone()
    //   });
    // }

    // let mut task_status = Vec::new();
    // while let Some(res) = set.join_next().await {
    //   match res {
    //     Ok(status) => {
    //       task_status.push(status.clone());
    //       if status.is_failed() {
    //         self._command_running.store(false, Ordering::Relaxed);
    //       }
    //       debug!(%cmd._id, status = ?status, "Sub-command finished with status");
    //     }
    //     Err(e) => {
    //       error!(%cmd._id, error = ?e, "Sub-command task panicked");
    //     }
    //   }
    // }
    // self
    //   ._store
    //   .set_status(
    //     &cmd._id,
    //     if task_status.iter().all(|s| s.is_successed()) {
    //       TaskStatus::Successed
    //     } else {
    //       TaskStatus::Failed
    //     },
    //   )
    //   .await;
  }
}

pub struct TaskStore {
  _tasks: Arc<RwLock<HashMap<String, Task>>>,
  _statuses: Arc<RwLock<HashMap<String, TaskStatus>>>,
  _logs: Arc<RwLock<HashMap<String, Vec<String>>>>,
  _last_run_time: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
  _show_colors: bool,
}

impl TaskStore {
  pub fn new(show_colors: bool) -> Self {
    Self {
      _tasks: Arc::new(RwLock::new(HashMap::new())),
      _statuses: Arc::new(RwLock::new(HashMap::new())),
      _show_colors: show_colors,
      _logs: Arc::new(RwLock::new(HashMap::new())),
      _last_run_time: Arc::new(RwLock::new(HashMap::new())),
    }
  }

  pub async fn add_task(&self, task: Task) {
    let mut tasks = self._tasks.write().await;
    let id = task._id.clone();
    let commands = task._commands.clone();
    tasks.insert(id.clone(), task);
    drop(tasks);
    let mut statuses = self._statuses.write().await;
    for cmd in commands.keys() {
      statuses.insert(cmd.clone(), TaskStatus::Init);
    }
  }

  pub async fn get_task(&self, id: &str) -> Task {
    let tasks = self._tasks.read().await;
    tasks
      .get(id)
      .unwrap_or_else(|| panic!("Trying to get non-existing task with id: {}", id))
      .clone()
  }

  pub async fn task_exists(&self, name: &str) -> bool {
    let tasks = self._tasks.read().await;
    tasks.values().any(|task| task._name == name)
  }

  pub async fn update_task_childs(&self, task_name: &str, childs_id: Vec<String>) {
    let tasks = self._tasks.read().await;
    let childs = tasks
      .values()
      .filter(|t| childs_id.contains(&t._name))
      .map(|t| t._id.clone())
      .collect::<Vec<String>>();
    drop(tasks);
    let mut tasks = self._tasks.write().await;
    let task = tasks
      .values_mut()
      .find(|task| task._name == task_name)
      .unwrap_or_else(|| panic!("Trying to update non-existing task with name: {}", task_name));
    task._children_id = Some(childs);
    drop(tasks);
  }

  async fn can_run(&self, task: &Task, tasks: &HashMap<String, Task>, task_cmd: &TaskCommand) -> bool {
    if let Some(childs) = &task._children_id {
      for child_id in childs {
        let child_task = tasks
          .get(child_id)
          .unwrap_or_else(|| panic!("Trying to get non-existing child task with id: {}", child_id));
        let child_cmd = child_task._commands.values().find(|cmd| *cmd == task_cmd);

        let Some(child_cmd) = child_cmd else {
          continue;
        };

        // if !child_cmd._status.is_finished() || child_cmd._status.is_stopped() {
        if !child_cmd._status.is_successed() {
          return false;
        }
      }
    }
    task
      ._commands
      .values()
      .any(|cmd| cmd == task_cmd && cmd._status.is_init())
  }

  pub async fn get_runnable_commands(&self, sequential: i8) -> Vec<TaskCommandRunnable> {
    let tasks = self._tasks.read().await;
    let tasks_cloned = tasks.clone();
    drop(tasks);

    // If sequential is enabled, only the command with the smallest index that can be
    // run will be returned, otherwise all runnable commands will be returned
    let min_index: i8 = if sequential > -1 {
      let mut ret = 0;
      for i in 0..=sequential {
        if tasks_cloned.values().into_iter().any(|t| {
          t._commands
            .values()
            .into_iter()
            .enumerate()
            .any(|(idx, cmd)| usize::try_from(i).map_or(false, |t| idx == t) && !cmd._status.is_successed())
        }) {
          ret = i;
          break;
        }
      }
      ret
    } else {
      0
    };

    let mut runnable_cmds = Vec::new();
    for task in tasks_cloned.values() {
      for (idx, cmd) in task._commands.values().enumerate() {
        if sequential > -1 && idx as i8 != min_index {
          continue;
        }
        if cmd._status.is_init() || cmd._status.is_finished() {
          if !self.can_run(&task, &tasks_cloned, &cmd).await {
            continue;
          }
          runnable_cmds.push(TaskCommandRunnable::new(cmd, task));
        }
      }
    }
    runnable_cmds
  }

  pub async fn get_task_command(&self, task_id: &str, command_id: &str) -> TaskCommand {
    let tasks = self._tasks.read().await;
    let task = tasks
      .get(task_id)
      .unwrap_or_else(|| panic!("Trying to get non-existing task with id: {}", task_id));
    task
      ._commands
      .get(command_id)
      .unwrap_or_else(|| {
        panic!(
          "Trying to get non-existing command with id: {} for task with id: {}",
          command_id, task_id
        )
      })
      .clone()
  }

  fn get_task_status(&self, task: &Task) -> InternalTaskStatus {
    if task._commands.values().any(|c| {
      c._status.is_running()
        || c._status.is_starting()
        || c._status.is_stopping()
        || c._status.is_stopped()
        || c._status.is_failed()
    }) {
      InternalTaskStatus::Running
    } else {
      InternalTaskStatus::Other
    }
  }

  pub async fn get_all_tasks_with_details(&self) -> Vec<TaskWithDetails> {
    let tasks = self._tasks.read().await;
    let tasks_values = tasks.values().cloned().collect::<Vec<Task>>();
    drop(tasks);
    let mut result = stream::iter(tasks_values)
      .then(|task| async move {
        let task_status = self.get_task_status(&task);
        TaskWithDetails {
          _id: task._id.clone(),
          _name: task._name.clone(),
          _path: task._path.clone(),
          _commands: task._commands.clone(),
          _parent_id: task._parent_id.clone(),
          _children_id: task._children_id.clone(),
          _status: task_status,
          last_run_time: self.get_last_run_time(&task._id).await,
        }
      })
      .collect::<Vec<_>>()
      .await;

    result.sort_by(|a, b| match (a._status, b._status) {
      (InternalTaskStatus::Running, InternalTaskStatus::Running) => a._name.cmp(&b._name),
      (InternalTaskStatus::Other, InternalTaskStatus::Other) => self.compare_last_run_time(a, b),
      (InternalTaskStatus::Running, InternalTaskStatus::Other) => std::cmp::Ordering::Less,
      (InternalTaskStatus::Other, InternalTaskStatus::Running) => std::cmp::Ordering::Greater,
    });

    result
  }

  fn compare_last_run_time(&self, a: &TaskWithDetails, b: &TaskWithDetails) -> std::cmp::Ordering {
    let default_time = Utc::now() + TimeDelta::days(-1);
    let a_time = a.last_run_time.unwrap_or(default_time);
    let b_time = b.last_run_time.unwrap_or(default_time);
    b_time.cmp(&a_time)
  }

  pub async fn get_all_tasks(&self) -> Vec<Task> {
    let tasks = self._tasks.read().await;
    tasks.values().cloned().collect()
  }

  pub async fn is_all_finished(&self) -> bool {
    let statuses = self._statuses.read().await;
    statuses.values().all(|status| status.is_finished())
  }

  pub async fn is_any_running(&self) -> bool {
    let statuses = self._statuses.read().await;
    statuses.values().any(|status| status.is_running())
  }

  pub async fn is_all_running_or_finished(&self) -> bool {
    let statuses = self._statuses.read().await;
    statuses
      .values()
      .all(|status| status.is_running() || status.is_finished())
  }

  pub async fn is_all_not_init(&self) -> bool {
    let statuses = self._statuses.read().await;
    statuses.values().all(|status| !status.is_init())
  }

  pub async fn is_any_failed(&self) -> bool {
    let statuses = self._statuses.read().await;
    statuses.values().any(|status| status.is_failed())
  }

  pub async fn set_last_run_time(&self, id: &str, time: DateTime<Utc>) {
    let mut last_run_time = self._last_run_time.write().await;
    last_run_time.insert(id.to_string(), time);
  }

  pub async fn get_last_run_time(&self, id: &str) -> Option<DateTime<Utc>> {
    let last_run_time = self._last_run_time.read().await;
    last_run_time.get(id).cloned()
  }

  pub async fn set_status(&self, id: &str, status: TaskStatus) {
    let mut tasks = self._tasks.write().await;
    let mut found = false;
    for task in tasks.values_mut() {
      if found {
        break;
      }
      for cmd in task._commands.values_mut() {
        if cmd._id == id {
          cmd._status = status.clone();
          debug!("Set status of command with id: {} to {}", id, status);
          found = true;
          break;
        }
      }
    }
    let mut statuses = self._statuses.write().await;
    statuses.insert(id.to_string(), status.clone());
    drop(statuses);
    drop(tasks);
  }

  pub async fn get_status(&self, id: &str) -> TaskStatus {
    let statuses = self._statuses.read().await;
    statuses
      .get(id)
      .unwrap_or_else(|| panic!("Trying to get status of non-existing command with id: {}", id))
      .clone()
  }

  pub async fn add_log(&self, id: &str, log: String) {
    let mut logs_lock = self._logs.write().await;
    let logs = logs_lock.entry(id.to_string()).or_insert_with(|| Vec::new());
    logs.push(log);
    drop(logs_lock);
  }

  pub async fn get_logs(&self, id: &str) -> Vec<String> {
    let logs_lock = self._logs.read().await;
    logs_lock.get(id).cloned().unwrap_or_else(Vec::new)
  }
}

impl Default for TaskStore {
  fn default() -> Self {
    Self::new(true)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlexusCommand {
  name: String,
  command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskCommandType {
  Simple(String),
  Plexus(TaskPlexusCommand),
}

impl Display for TaskCommandType {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      TaskCommandType::Simple(cmd) => write!(f, "{}", cmd),
      TaskCommandType::Plexus(plexus_cmd) => write!(f, "{}", plexus_cmd.name),
    }
  }
}

impl PartialEq<str> for TaskCommandType {
  fn eq(&self, other: &str) -> bool {
    match self {
      TaskCommandType::Simple(cmd) => cmd == other,
      TaskCommandType::Plexus(plexus_cmd) => plexus_cmd.name == other,
    }
  }
}

impl PartialEq<&str> for TaskCommandType {
  fn eq(&self, other: &&str) -> bool {
    match self {
      TaskCommandType::Simple(cmd) => cmd == *other,
      TaskCommandType::Plexus(plexus_cmd) => plexus_cmd.name == *other,
    }
  }
}

impl PartialEq<String> for TaskCommandType {
  fn eq(&self, other: &String) -> bool {
    match self {
      TaskCommandType::Simple(cmd) => cmd == other,
      TaskCommandType::Plexus(plexus_cmd) => plexus_cmd.name == *other,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCommand {
  pub _id: String,
  pub _command: TaskCommandType,
  pub _status: TaskStatus,
}

impl PartialEq for TaskCommand {
  fn eq(&self, other: &Self) -> bool {
    if let TaskCommandType::Simple(cmd) = &self._command {
      if let TaskCommandType::Simple(other_cmd) = &other._command {
        return cmd == other_cmd;
      } else if let TaskCommandType::Plexus(plexus_cmd) = &other._command {
        return cmd == &plexus_cmd.name;
      }
    } else if let TaskCommandType::Plexus(plexus_cmd) = &self._command {
      if let TaskCommandType::Plexus(other_plexus_cmd) = &other._command {
        return plexus_cmd.name == other_plexus_cmd.name;
      } else if let TaskCommandType::Simple(other_cmd) = &other._command {
        return plexus_cmd.name == *other_cmd;
      }
    }
    false
  }
}

impl PartialEq<str> for TaskCommand {
  fn eq(&self, other: &str) -> bool {
    match &self._command {
      TaskCommandType::Simple(cmd) => cmd == other,
      TaskCommandType::Plexus(plexus_cmd) => plexus_cmd.name == other,
    }
  }
}

impl PartialEq<String> for TaskCommand {
  fn eq(&self, other: &String) -> bool {
    match &self._command {
      TaskCommandType::Simple(cmd) => cmd == other,
      TaskCommandType::Plexus(plexus_cmd) => plexus_cmd.name == *other,
    }
  }
}

impl PartialEq<&TaskCommand> for TaskCommand {
  fn eq(&self, other: &&TaskCommand) -> bool {
    self == *other
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCommandRunnable {
  _id: String,
  _task_id: String,
  _name: String,
  _command: TaskCommandType,
  _path: String,
}

impl TaskCommandRunnable {
  pub fn new(cmd: &TaskCommand, task: &Task) -> Self {
    Self {
      _id: cmd._id.clone(),
      _name: task._name.clone(),
      _command: cmd._command.clone(),
      _path: task._path.clone(),
      _task_id: task._id.clone(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
  pub _id: String,
  pub _name: String,
  _path: String,
  pub _commands: HashMap<String, TaskCommand>,
  _parent_id: Option<String>,
  _children_id: Option<Vec<String>>,
}

/// `Task::_commands` is a HashMap, so iterating it yields a different order on every pass.
/// Anything that addresses a command by index (the log tabs, the task row) has to go
/// through here or it will point at a different command from one frame to the next.
pub fn sorted_commands(commands: &HashMap<String, TaskCommand>) -> Vec<&TaskCommand> {
  let mut out: Vec<&TaskCommand> = commands.values().collect();
  out.sort_by(|a, b| {
    a._command
      .to_string()
      .cmp(&b._command.to_string())
      .then_with(|| a._id.cmp(&b._id))
  });
  out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskWithDetails {
  pub _id: String,
  pub _name: String,
  _path: String,
  pub _commands: HashMap<String, TaskCommand>,
  _parent_id: Option<String>,
  _children_id: Option<Vec<String>>,
  pub _status: InternalTaskStatus,
  last_run_time: Option<DateTime<Utc>>,
}

impl Task {
  pub fn new(
    id: String,
    name: String,
    path: String,
    commands: Vec<ConfigCommand>,
    parent_id: Option<String>,
    children_id: Option<Vec<String>>,
  ) -> Self {
    let cmds = commands
      .into_iter()
      .flat_map(|cmd| {
        if let ConfigCommand::Plexus(plexus_cmd) = &cmd {
          plexus_cmd
            .actual_commands
            .iter()
            .map(|command| {
              let id = format!("{}-{}-{}", name, plexus_cmd.name, uuid());
              (
                id.clone(),
                TaskCommand {
                  _id: id,
                  _command: TaskCommandType::Plexus(TaskPlexusCommand {
                    name: plexus_cmd.name.clone(),
                    command: command.clone(),
                  }),
                  _status: TaskStatus::Init,
                },
              )
            })
            .collect::<Vec<(String, TaskCommand)>>()
        } else {
          let id = format!("{}-{}", name, uuid());
          vec![(
            id.clone(),
            TaskCommand {
              _id: id,
              _command: if let ConfigCommand::WithDependency {
                command,
                depends_on: _,
                envs: _,
              } = cmd
              {
                TaskCommandType::Simple(command)
              } else if let ConfigCommand::Simple(cmd_str) = cmd {
                TaskCommandType::Simple(cmd_str)
              } else {
                unreachable!()
              },
              _status: TaskStatus::Init,
            },
          )]
        }
      })
      .collect();
    Self {
      _id: id,
      _name: name,
      _path: path,
      _commands: cmds,
      _parent_id: parent_id,
      _children_id: children_id,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
  Init,
  Starting,
  Running,
  Failed,
  Successed,
  Stopping,
  Stopped,
}

impl TaskStatus {
  pub fn is_finished(&self) -> bool {
    matches!(self, TaskStatus::Failed | TaskStatus::Successed | TaskStatus::Stopped)
  }

  pub fn is_running(&self) -> bool {
    matches!(self, TaskStatus::Running)
  }

  pub fn is_starting(&self) -> bool {
    matches!(self, TaskStatus::Starting)
  }

  pub fn is_init(&self) -> bool {
    matches!(self, TaskStatus::Init)
  }

  pub fn is_failed(&self) -> bool {
    matches!(self, TaskStatus::Failed)
  }

  pub fn is_successed(&self) -> bool {
    matches!(self, TaskStatus::Successed)
  }

  pub fn is_stopping(&self) -> bool {
    matches!(self, TaskStatus::Stopping)
  }

  pub fn is_stopped(&self) -> bool {
    matches!(self, TaskStatus::Stopped)
  }
}

impl Display for TaskStatus {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let status_str = match self {
      TaskStatus::Init => "Init",
      TaskStatus::Starting => "Starting",
      TaskStatus::Running => "Running",
      TaskStatus::Failed => "Failed",
      TaskStatus::Successed => "Successed",
      TaskStatus::Stopping => "Stopping",
      TaskStatus::Stopped => "Stopped",
    };
    write!(f, "TS{}", status_str)
  }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InternalTaskStatus {
  Running,
  Other,
}
