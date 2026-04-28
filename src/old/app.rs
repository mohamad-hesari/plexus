use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicBool},
};

use tokio::{sync::Mutex, task::JoinHandle, time::sleep};

use crate::{
    cli, emit, log, pnpm,
    share::{AppEvent, EventBus, TaskStatus},
    task,
    watcher::ProjectWatcher,
};

pub struct App(Arc<Mutex<AppInner>>);

pub struct AppInner {
    _cli: cli::Cli,
    _pnpm: pnpm::Pnpm,
    _tasks: HashMap<String, Arc<Mutex<task::Task>>>,
    _running: bool,
    _watchers: Vec<ProjectWatcher>,
    _join_set: Option<Arc<Mutex<Vec<JoinHandle<()>>>>>,
}

impl AppInner {
    pub fn running(&self) -> bool {
        self._running
    }
}

impl App {
    pub fn my_clone(&self) -> Arc<Mutex<AppInner>> {
        Arc::clone(&self.0)
    }
    pub async fn new(cli: cli::Cli) -> Self {
        let pnpm = pnpm::Pnpm::new(&cli.filter).await.unwrap_or_else(|_| {
            eprintln!("Error initializing pnpm");
            std::process::exit(1);
        });

        for (name, project) in pnpm.projects() {
            log!("Project: {}, Path: {}", name, project.path());
            log!("Project: {}, Path: {}", name, project.path());
            log!("  Scripts: {}", project.scripts().join(", "));
            log!("  Dependencies:");
            for dep in project.deps() {
                log!("    {}", dep);
            }
        }
        let mut tasks = HashMap::new();
        for (name, project) in pnpm.projects() {
            if tasks.contains_key(name) {
                continue;
            }
            let command = if project.scripts().contains(&cli.command) {
                Some(cli.command.clone())
            } else {
                None
            };

            let task = task::Task::new(
                name.to_string(),
                project.path().to_string(),
                command.clone(),
                project.clone(),
            );
            tasks.insert(name.to_string(), Arc::new(Mutex::new(task)));
        }
        App(Arc::new(Mutex::new(AppInner {
            _cli: cli,
            _pnpm: pnpm,
            _tasks: tasks,
            _running: false,
            _watchers: Vec::new(),
            _join_set: None,
        })))
    }

    async fn run_tasks(&self) -> impl Future<Output = ()> + Send + 'static {
        let task_runner_arc = Arc::clone(&self.0);

        async move {
            let mut is_empty = false;
            loop {
                if is_empty {
                    sleep(std::time::Duration::from_millis(1000)).await;
                }
                let me = task_runner_arc.lock().await;
                if !me._running {
                    log!("Task runner is not running. Exiting...");
                    break;
                }
                let mut ready_tasks = Vec::new();

                for task_arc in me._tasks.values() {
                    let t = task_arc.lock().await;

                    // Skip if already working or done
                    if !matches!(t.status(), TaskStatus::NotStarted) {
                        continue;
                    }
                    if t.cmd().is_none() {
                        continue;
                    }

                    let mut deps_satisfied = true;
                    for d in t.project().deps() {
                        if let Some(dep_arc) = me._tasks.get(d) {
                            let dt = dep_arc.lock().await;
                            log!(
                                "Task {} depends on {}, which is in status: {}",
                                t.name(),
                                d,
                                dt.status()
                            );
                            let dep_status = dt.status().clone();
                            drop(dt); // Drop lock on dependency before next iteration
                            if !matches!(dep_status, TaskStatus::Finished | TaskStatus::NoCommand) {
                                deps_satisfied = false;
                                break;
                            }
                        } else {
                            deps_satisfied = false;
                            break;
                        }
                    }

                    // Drop the lock on 't' implicitly at the end of loop or manually
                    if deps_satisfied {
                        ready_tasks.push(Arc::clone(task_arc));
                    }
                }

                if ready_tasks.is_empty() {
                    let mut all_finished = true;
                    for t_arc in me._tasks.values() {
                        let t = Arc::clone(t_arc);
                        let t = t.lock().await;
                        if !matches!(t.status(), TaskStatus::Finished | TaskStatus::NoCommand) {
                            all_finished = false;
                            drop(t); // Drop lock before next iteration
                            break;
                        }
                        drop(t); // Drop lock before next iteration
                    }

                    if all_finished && !me._cli.watch {
                        break;
                    }

                    is_empty = true;
                    continue;
                }

                is_empty = false;

                for task_arc in ready_tasks {
                    task::Task::start(task_arc).await;
                }
            }
            log!("All tasks have been processed. Exiting task runner...");
            emit!(AppEvent::Quit);
        }
    }

    async fn run_watchers(&self) -> impl Future<Output = ()> + Send + 'static {
        let watcher_arc = Arc::clone(&self.0);
        // let mut watchers = Vec::new();
        let tasks = {
            let me = watcher_arc.lock().await;
            let mut tasks = Vec::new();
            for task in me._tasks.values() {
                let t = task.lock().await;
                if t.cmd().is_some() {
                    tasks.push(t.project().clone());
                }
            }
            tasks
        };
        {
            let mut me = watcher_arc.lock().await;
            for project in tasks {
                let mut watcher = ProjectWatcher::new(&project).await;
                watcher.start().await;
                me._watchers.push(watcher);
            }
        }

        let mut rx = EventBus::global().subscribe();

        async move {
            log!("Starting file watcher...");
            while let Ok(evt) = rx.recv().await {
                // log!("Received event: {:?}", evt);
                match evt {
                    AppEvent::FileChanged(e) => {
                        log!("File changed in task {}", e.task_name);
                        // On file change, we want to reset the status of the task and all its dependents
                        let me = watcher_arc.lock().await;
                        if let Some(changed_task_arc) = me._tasks.get(&e.task_name) {
                            let mut changed_task = changed_task_arc.lock().await;
                            if !matches!(changed_task.status(), TaskStatus::Finished) {
                                drop(changed_task); // Drop lock before
                                continue;
                            }

                            changed_task.send_status(TaskStatus::NotStarted);
                            drop(changed_task); // Drop lock before acquiring another

                            // Now we need to reset all dependents
                            for t_arc in me._tasks.values() {
                                let t = t_arc.lock().await;
                                if t.project().deps().contains(&e.task_name) {
                                    drop(t); // Drop lock before acquiring another
                                    let mut dependent_task = t_arc.lock().await;
                                    if matches!(dependent_task.status(), TaskStatus::Finished) {
                                        dependent_task.send_status(TaskStatus::NotStarted);
                                    }
                                    drop(dependent_task);
                                }
                            }
                        }
                        drop(me);
                    }
                    AppEvent::TuiStop(task_name) => {
                        log!("TUI requested stop for task {}", task_name);
                        let me = watcher_arc.lock().await;
                        if let Some(task_arc) = me._tasks.get(&task_name) {
                            let mut t = task_arc.lock().await;
                            t.stop().await;
                            t.send_status(TaskStatus::Stoped);
                        }
                        drop(me);
                    }
                    AppEvent::TuiStart(task_name) => {
                        log!("TUI requested start for task {}", task_name);
                        let me = watcher_arc.lock().await;
                        if let Some(task_arc) = me._tasks.get(&task_name) {
                            let mut t = task_arc.lock().await;
                            if matches!(t.status(), TaskStatus::Running) {
                                log!(
                                    "Task {} is already running. Ignoring start request.",
                                    task_name
                                );
                                drop(t);
                                drop(me);
                                continue;
                            }
                            t.send_status(TaskStatus::NotStarted);
                        }
                        drop(me);
                    }
                    AppEvent::TuiRestart(task_name) => {
                        log!("TUI requested restart for task {}", task_name);
                        let me = watcher_arc.lock().await;
                        if let Some(task_arc) = me._tasks.get(&task_name) {
                            let mut t = task_arc.lock().await;
                            t.stop().await;
                            t.send_status(TaskStatus::NotStarted);
                        }
                        drop(me);
                    }
                    _ => (),
                }
            }
        }
    }

    pub async fn run(&self) {
        log!("Running the app...");

        let have_watcher = {
            let mut me = self.0.lock().await;
            me._running = true;
            me._cli.watch && !me._tasks.is_empty()
        };
        let mut set = vec![];

        set.push(tokio::spawn(self.run_tasks().await));
        if have_watcher {
            set.push(tokio::spawn(self.run_watchers().await));
        }
        let mut me = self.0.lock().await;
        me._join_set = Some(Arc::new(Mutex::new(set)));
    }

    pub async fn stop(&self) {
        {
            let mut me = self.0.lock().await;
            me._running = false;
            for task_arc in me._tasks.values() {
                let mut t = task_arc.lock().await;
                t.stop().await;
            }
            for watcher in &mut me._watchers {
                watcher.stop().await;
            }
        }
        let join_set = if let Some(join_set) = &self.0.lock().await._join_set {
            log!("App is stopping...");
            Arc::clone(join_set)
        } else {
            log!("No join set found. App may not have been fully initialized.");
            return;
        };

        let mut join_set = join_set.lock().await;
        for handle in join_set.drain(..) {
            handle.abort();
        }
    }

    pub async fn tasks(&self) -> Vec<String> {
        let me = self.0.lock().await;
        let mut task_names = Vec::new();
        for (name, task) in me._tasks.iter() {
            let t = task.lock().await;
            if t.cmd().is_some() {
                task_names.push(name.clone());
            }
            drop(t);
        }
        task_names
    }
}
