use std::sync::Arc;

use crate::{
    emit, log,
    share::{AppEvent, AppInterface, EventBus, TaskStatus},
};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self},
    prelude::*,
    widgets::*,
};
use tokio::{
    fs::OpenOptions,
    io::{self, AsyncWriteExt},
    sync::Mutex,
    time::Instant,
};

#[derive(Debug)]
struct TuiTask {
    name: String,
    status: TaskStatus,
    output: LogEntry,
}

#[derive(Debug)]
struct LogEntry {
    scroll: u16,
    state: ScrollbarState,
    logs: Vec<String>,
    follow: bool,
    viewport_height: Option<u16>,
}

impl LogEntry {
    pub fn output(&self) -> &[String] {
        &self.logs
    }
    // pub fn is_empty(&self) -> bool {
    //     self.logs.is_empty()
    // }
    pub fn clear(&mut self) {
        self.logs.clear();
        self.scroll = 0;
        self.state = ScrollbarState::new(0);
    }
    pub fn new() -> Self {
        Self {
            scroll: 0,
            state: ScrollbarState::default(),
            logs: Vec::new(),
            follow: true,
            viewport_height: None,
        }
    }
    pub fn set_viewport_height(&mut self, height: u16) {
        self.viewport_height = Some(height);
    }
    // Call this whenever a new log line arrives (from Tokio, file watcher, etc.)
    pub fn add_log(&mut self, line: String) {
        self.logs.push(line);

        // Update scrollbar content length
        self.state = self.state.content_length(self.logs.len());

        // Auto-scroll only if we're in follow mode OR already near the bottom
        if self.follow || self.is_near_bottom() {
            self.scroll_to_bottom();
        }
    }

    fn is_near_bottom(&self) -> bool {
        let max_scroll = self.logs.len().saturating_sub(1) as u16;
        self.scroll + 5 >= max_scroll // "near bottom" tolerance
    }

    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = self.logs.len().saturating_sub(1) as u16;
        if let Some(viewport_height) = self.viewport_height {
            if max_scroll > viewport_height {
                self.scroll = max_scroll - viewport_height;
            } else {
                self.scroll = 0;
            }
        } else {
            self.scroll = max_scroll;
        }
        self.state = self.state.position(self.scroll as usize);
    }

    // Manual scrolling (called from event handler)
    pub fn scroll_up(&mut self, lines: u16) {
        self.follow = false; // Exit follow mode when user scrolls up
        self.scroll = self.scroll.saturating_sub(lines);
        self.state = self.state.position(self.scroll as usize);
    }

    pub fn scroll_down(&mut self, lines: u16) {
        let max_scroll = self.logs.len().saturating_sub(1) as u16;
        self.scroll = (self.scroll + lines).min(max_scroll);
        self.state = self.state.position(self.scroll as usize);

        // Re-enable follow if user scrolls back to bottom
        if self.is_near_bottom() {
            self.follow = true;
        }
    }
}

struct TuiInner {
    _app: Option<crate::app::App>,
    _tasks: Arc<Mutex<Vec<TuiTask>>>,
    _selected: Arc<Mutex<String>>,
    _log: Arc<Mutex<LogEntry>>,
    _vertical_scroll: u16, // Current scroll position
    _vertical_scroll_state: ScrollbarState,
}

pub struct Tui(Arc<Mutex<TuiInner>>);

impl AppInterface for Tui {
    async fn set_app(&mut self, app: crate::app::App) {
        let mut me = self.0.lock().await;
        me._app = Some(app);
    }

    async fn wait(&self) {
        {
            let me = self.0.lock().await;
            if let Some(app) = &me._app {
                let mut tasks = me._tasks.lock().await;
                for name in app.tasks().await {
                    tasks.push(TuiTask {
                        name: name.clone(),
                        status: TaskStatus::NotStarted,
                        output: LogEntry::new(),
                    });
                }
            }
        }

        {
            let mut me = self.0.lock().await;
            let _ = me.start_tui().await;
            println!("TUI has exited. Cleaning up...");
        }
    }
}

impl Tui {
    pub async fn new() -> Self {
        let inner = TuiInner::new().await;
        Self(Arc::new(Mutex::new(inner)))
    }
}

pub async fn append_to_log(path: &str, message: &str) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .append(true)
        .create(true)
        .open(path)
        .await?;

    let log_entry = format!("{}\n", message);

    file.write_all(log_entry.as_bytes()).await?;
    file.flush().await?; // Ensure it's on disk (good for logs)
    Ok(())
}

pub struct UiState {
    // vertical_scroll: u16,
    // vertical_scroll_state: ScrollbarState,
    show_log: bool,
    selected: usize,
    total_tasks: usize,
}

impl TuiInner {
    pub async fn new() -> Self {
        Self {
            _app: None,
            _tasks: Arc::new(Mutex::new(Vec::new())),
            _selected: Arc::new(Mutex::new(String::new())),
            _log: Arc::new(Mutex::new(LogEntry::new())),
            _vertical_scroll: 0,
            _vertical_scroll_state: ScrollbarState::default(),
        }
    }

    pub async fn start_tui(&mut self) -> color_eyre::Result<()> {
        color_eyre::install()?;
        let mut terminal = ratatui::init();

        let mut rx = EventBus::global().subscribe();
        let log_clone = Arc::clone(&self._log);
        let task = tokio::spawn(async move {
            while let Ok(_event) = rx.recv().await {
                if let AppEvent::Log(msg) = _event {
                    append_to_log("tui.log", &msg).await.unwrap_or_else(|e| {
                        eprintln!("Failed to write to log file: {:?}", e);
                    });
                    let mut log = log_clone.lock().await;
                    log.add_log(msg);
                }
            }
        });

        if let Some(app) = &self._app {
            app.run().await;
        }

        let result = self.run_app(&mut terminal).await;

        if let Some(app) = &self._app {
            app.stop().await;
        }
        task.abort();
        ratatui::restore();
        result
    }

    async fn run_app(&self, terminal: &mut DefaultTerminal) -> color_eyre::Result<()> {
        let mut task_set = tokio::task::JoinSet::new();
        if let Some(app) = &self._app {
            let a = app.my_clone();
            task_set.spawn(async move {
                loop {
                    let app = a.lock().await;
                    if !app.running() {
                        emit!(AppEvent::Quit);
                        break;
                    }
                    drop(app);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await
                }
            });
        }

        let mut rx = EventBus::global().subscribe();
        let state = {
            let tasks = self._tasks.lock().await;
            Arc::new(Mutex::new(UiState {
                // vertical_scroll: 0,
                // vertical_scroll_state: ScrollbarState::default(),
                show_log: false,
                selected: 0,
                total_tasks: tasks.len(),
            }))
        };
        let (app_tx, mut app_rx) = tokio::sync::mpsc::unbounded_channel::<KeyEvent>();
        let event_state = Arc::clone(&state);
        let log_clone = Arc::clone(&self._log);
        let tasks_clone = Arc::clone(&self._tasks);

        task_set.spawn(async move {
            while let Some(key_event) = app_rx.recv().await {
                let start = Instant::now();

                match key_event.code {
                    event::KeyCode::Char('q') => {
                        log!("Received quit command. Shutting down TUI...");
                        emit!(AppEvent::Quit);
                        break;
                    }
                    event::KeyCode::Char('l') => {
                        let mut state = event_state.lock().await;
                        state.show_log = !state.show_log;
                    }
                    event::KeyCode::Char('c') => {
                        let mut log = log_clone.lock().await;
                        log.clear();
                    }

                    event::KeyCode::Char('r') | event::KeyCode::Enter => {
                        let state = event_state.lock().await;
                        let tasks = tasks_clone.lock().await;
                        let selected = tasks.get(state.selected).map(|t| t.name.clone());
                        if let Some(selected) = selected {
                            emit!(AppEvent::TuiRestart(selected));
                        }
                    }
                    event::KeyCode::Char('s') => {
                        let state = event_state.lock().await;
                        let tasks = tasks_clone.lock().await;
                        if let Some(t) = tasks.get(state.selected) {
                            if t.status == TaskStatus::Running {
                                emit!(AppEvent::TuiStop(t.name.clone()));
                            } else {
                                emit!(AppEvent::TuiStart(t.name.clone()));
                            }
                        }
                        drop(tasks);
                        drop(state);
                    }
                    event::KeyCode::Up | event::KeyCode::Char('k') => {
                        let mut state = event_state.lock().await;
                        state.selected = state.selected.saturating_sub(1).max(0);
                        drop(state);
                    }
                    event::KeyCode::Down | event::KeyCode::Char('j') => {
                        let mut state = event_state.lock().await;
                        state.selected =
                            (state.selected + 1).min(state.total_tasks.saturating_sub(1));
                        drop(state);
                    }
                    KeyCode::PageDown
                    | KeyCode::PageUp
                    | KeyCode::End
                    | KeyCode::Home
                    | KeyCode::Char('f') => {
                        let state = event_state.lock().await;
                        if state.show_log {
                            let mut app = log_clone.lock().await;
                            match key_event.code {
                                KeyCode::PageDown => app.scroll_down(10),
                                KeyCode::PageUp => app.scroll_up(10),

                                KeyCode::End => {
                                    app.follow = true;
                                    app.scroll_to_bottom();
                                }
                                KeyCode::Home => {
                                    app.follow = false;
                                    app.scroll = 0;
                                }
                                KeyCode::Char('f') => {
                                    app.follow = !app.follow;
                                    if app.follow {
                                        app.scroll_to_bottom();
                                    }
                                }
                                _ => (),
                            }
                        } else {
                            let mut tasks = tasks_clone.lock().await;
                            if let Some(task) = tasks.get_mut(state.selected) {
                                match key_event.code {
                                    KeyCode::PageDown => task.output.scroll_down(10),
                                    KeyCode::PageUp => task.output.scroll_up(10),
                                    KeyCode::End => {
                                        task.output.follow = true;
                                        task.output.scroll_to_bottom();
                                    }
                                    KeyCode::Home => {
                                        task.output.follow = false;
                                        task.output.scroll = 0;
                                    }
                                    KeyCode::Char('f') => {
                                        task.output.follow = !task.output.follow;
                                        if task.output.follow {
                                            task.output.scroll_to_bottom();
                                        }
                                    }
                                    _ => (),
                                }
                            }
                        }
                    }
                    _ => (),
                }
                let duration = start.elapsed();
                log!(
                    "Processed key event({}) in {} ms",
                    key_event.code,
                    duration.as_millis()
                );
            }
        });

        // let mut vertical_scroll = 0;
        // let vertical_scroll_state = Arc::new(Mutex::new(ScrollbarState::new(0)));
        loop {
            let draw_state = Arc::clone(&state);
            {
                let tasks = self._tasks.lock().await;
                let cloned_tasks = tasks
                    .iter()
                    .map(|t| TuiTask {
                        name: t.name.clone(),
                        status: t.status.clone(),
                        output: LogEntry::new(),
                    })
                    .collect::<Vec<TuiTask>>();
                drop(tasks);
                let mut tasks = self._tasks.lock().await;
                let mut log = self._log.lock().await;
                let mut state = draw_state.lock().await;
                let task = tasks.get_mut(state.selected).unwrap();
                terminal.draw(|frame| {
                    self.ui(frame, &cloned_tasks, task, &mut state, &mut log);
                })?;
                drop(state);
                drop(log);
                drop(tasks);
            }

            // Non-blocking event check with small timeout
            if event::poll(std::time::Duration::from_millis(50))?
                && let event::Event::Key(key_event) = event::read()?
            {
                app_tx.send(key_event)?;
            }

            let mut quit = false;
            // Process background outputs / finishes
            while let Ok(_event) = rx.try_recv() {
                match _event {
                    AppEvent::TaskStatus(task) => {
                        let mut tasks = self._tasks.lock().await;
                        for t in tasks.iter_mut() {
                            if t.name == task.name {
                                t.status = task.status.clone();
                                break;
                            }
                        }
                        drop(tasks);
                    }
                    AppEvent::TaskOutput(e) => {
                        let mut tasks = self._tasks.lock().await;
                        for t in tasks.iter_mut() {
                            if t.name == e.name {
                                t.output.add_log(e.output.clone());
                                break;
                            }
                        }
                        drop(tasks);
                    }
                    AppEvent::Quit => {
                        log!("Received quit event from background task. Shutting down TUI...");
                        quit = true;
                        break;
                    }
                    _ => (),
                };
            }
            if quit {
                log!("Exiting main TUI loop...");
                break;
            }
        }

        task_set.abort_all();
        while let Some(res) = task_set.join_next().await {
            if let Err(e) = res {
                eprintln!("Error in TUI background task: {:?}", e);
            }
        }

        log!("TUI has shut down.");
        Ok(())
    }

    fn ui(
        &self,
        frame: &mut Frame,
        tasks: &[TuiTask],
        selected_task: &mut TuiTask,
        state: &mut UiState,
        log: &mut LogEntry,
    ) {
        let area = frame.area();
        let [(top_height, bottom_height), (log_height, status_height)] = if state.show_log {
            [(70, 30), (67, 33)]
        } else {
            [(90, 10), (0, 100)]
        };
        let [top, bottom] = Layout::vertical([
            Constraint::Percentage(top_height),
            Constraint::Percentage(bottom_height),
        ])
        .areas(area);
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(top);
        let [log_area, status_area] = Layout::vertical([
            Constraint::Percentage(log_height),
            Constraint::Percentage(status_height),
        ])
        .areas(bottom);

        let [status, commands] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(status_area);

        if state.show_log {
            // let paragraph = Paragraph::new(log.join("\n"))
            //     .block(Block::bordered())
            //     .wrap(Wrap { trim: true });
            // frame.render_widget(paragraph, log_area);
            self.draw_logs(frame, log, log_area);
        }

        frame.render_widget(
            Paragraph::new(log.output().last().cloned().unwrap_or_default())
                .block(Block::bordered().title("Status")),
            status,
        );

        frame.render_widget(
            Paragraph::new("Commands: q=quit, ↑/k=up, ↓/j=down, Enter/r=start, s=stop")
                .block(Block::bordered().title("Commands")),
            commands,
        );

        // Left: Task List
        let items: Vec<ListItem> = tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let status = if task.status == TaskStatus::Running {
                    "● RUNNING"
                } else {
                    "○ STOPPED"
                };
                let style = if i == state.selected {
                    Style::default().fg(Color::Yellow).bold()
                } else {
                    Style::default()
                };
                ListItem::new(format!("{} {}", status, task.name)).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::bordered().title("Tasks (↑↓ to select, Enter/r to start, s to stop)"))
            .highlight_style(Style::default().fg(Color::Yellow));

        frame.render_widget(list, left);

        // Right: Selected Task Output
        self.draw_logs(frame, &mut selected_task.output, right);
        // let output = if selected_task.output.is_empty() {
        //     vec!["No output yet".to_string()]
        // } else {
        //     selected_task.output.clone()
        // };
        //
        // let text: Vec<Line> = output.iter().map(|l| Line::from(l.as_str())).collect();
        // let paragraph = Paragraph::new(Text::from(text))
        //     .block(Block::bordered().title(title))
        //     .scroll((state.vertical_scroll, 0));
        //
        // let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        //     .begin_symbol(Some("↑"))
        //     .end_symbol(Some("↓"));
        //
        // frame.render_stateful_widget(
        //     scrollbar,
        //     right.inner(ratatui::layout::Margin {
        //         vertical: 1,
        //         horizontal: 0,
        //     }),
        //     &mut state.vertical_scroll_state,
        // );
        // frame.render_widget(paragraph, right);
    }

    fn draw_logs(&self, f: &mut Frame, app: &mut LogEntry, area: Rect) {
        let chunks = Layout::vertical([Constraint::Fill(1)]).split(area);
        app.set_viewport_height(chunks[0].height);
        // Convert logs to Lines
        let lines: Vec<_> = app
            .logs
            .iter()
            .map(|l| ratatui::text::Line::from(l.as_str()))
            .collect();

        let max_scroll = lines.len().saturating_sub(1) as u16;
        let clamped_scroll = app.scroll.min(max_scroll);

        let paragraph = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Live Logs (tail -f mode)"),
            )
            .scroll((clamped_scroll, 0))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, chunks[0]);

        // Vertical scrollbar on the right
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));

        f.render_stateful_widget(
            scrollbar,
            chunks[0].inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut app.state,
        );
    }
}
