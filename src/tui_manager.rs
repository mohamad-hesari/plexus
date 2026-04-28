use futures::StreamExt;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, EventStream, KeyCode},
    prelude::*,
    widgets::*,
};
use std::{io, ops::Add, sync::Arc};
use tokio::{
    sync::{Mutex, RwLock},
    time::Instant,
};
use tracing::{Event, debug, error, field::Visit, info};
use tracing_subscriber::{Layer, layer::Context};

use crate::{
    app::App,
    app_state::{AppState, StateEvent, TaskState, TaskStatus},
    emit,
    task_manager::TaskManager,
};

pub struct TuiTracingLayer;

impl TuiTracingLayer {
    pub fn new() -> Self {
        Self
    }
}

impl<S> Layer<S> for TuiTracingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = LogVisitor::new();
        event.record(&mut visitor);

        let message = visitor.message;
        let level = *event.metadata().level();

        tokio::spawn(async move {
            App::instance().state.add_log(level, message).await;
        });
    }
}

struct LogVisitor {
    message: String,
}

impl LogVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
        }
    }
}

impl Visit for LogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        }
    }
}

pub trait TuiManager {
    fn start_tui(&self) -> impl Future<Output = ()> + Send;
}

impl TuiManager for App {
    async fn start_tui(&self) {
        info!("Starting TUI...");
        let state = Arc::clone(&self.state);
        self.state
            .spawn(async move {
                color_eyre::install().unwrap();
                let terminal = Arc::new(Mutex::new( ratatui::init()));
                let pure_tasks = Arc::new(state.get_tasks().await);
                let ui_state = Arc::new(RwLock::new(TuiState {
                    selected: 0,
                    frame_count: 0,
                    show_log: false,
                    total_tasks: 0,
                    log_entry: LogEntry::new(),
                    output_entry: LogEntry::new(),
                    tasks: vec![],
                }));

                let mut t = terminal.lock().await;
                render(&mut t, Arc::clone(&ui_state), Arc::clone(&state), Arc::clone(&pure_tasks)).await;
                drop(t);

                let mut rx_status_change = state.get_status_change_receiver();
                // let sx_key_clone = Arc::new(state.get_status_change_sender());
                let mut reader = EventStream::new();
                let terminal_clone = Arc::clone(&terminal);
                let ui_state_clone = Arc::clone(&ui_state);
                let state_clone = Arc::clone(&state);
                let pure_tasks_clone = Arc::clone(&pure_tasks);
                state.spawn(async move {
                    loop {
                        if !state_clone.is_running() {
                            debug!("TUI status change listener detected app is no longer running, exiting...");
                            break;
                        }
                        rx_status_change.changed().await.unwrap();
                        debug!("Received task status change event, marking TUI as dirty...");
                        let mut t = terminal_clone.lock().await;
                        render(&mut t, Arc::clone(&ui_state_clone), Arc::clone(&state_clone), Arc::clone(&pure_tasks_clone)).await;
                        drop(t);
                    }
                }).await;
                let state= Arc::clone(&state);
                // let (sx,mut rx) = tokio::sync::mpsc::unbounded_channel::<event::KeyEvent>();
                loop {
                    if !state.is_running() {
                        debug!("TUI event loop detected app is no longer running, exiting...");
                        break;
                    }
                    let maybe_event = reader.next().await;

                    let start = Instant::now();
                    if let Some(Ok(event::Event::Key(key_event))) = maybe_event {
                        // Handle directly. If this feels slow, it's because of the WRITE lock in handle_key
                        if handle_key_event(key_event, Arc::clone(&ui_state)).await {
                            // Instead of spawning, just trigger the render here directly
                            let mut t = terminal.lock().await;
                            render(&mut t, Arc::clone(&ui_state), Arc::clone(&state), Arc::clone(&pure_tasks)).await;
                        }
                    }
                        debug!("TUI event loop iteration took {:?} ms", start.elapsed().as_millis());
                    // tokio::select! {
                    //     // running = state.is_running_blocking() => {
                    //     //     if !running {
                    //     //         debug!("TUI event loop detected app is no longer running, exiting...");
                    //     //         break;
                    //     //     }
                    //     // }
                    //     // _ = rx_status_change.changed() => {
                    //     //     rx_status_change.borrow_and_update();
                    //     //     debug!("Received task status change event, marking TUI as dirty...");
                    //     //     render(&mut terminal, Arc::clone(&ui_state), Arc::clone(&state), Arc::clone(&pure_tasks)).await;
                    //     // }
                    //     maybe_event = reader.next() => {
                    //         if let Some(Ok(event::Event::Key(key_event))) = maybe_event {
                    //             // Handle directly. If this feels slow, it's because of the WRITE lock in handle_key_event.
                    //             if handle_key_event(key_event, Arc::clone(&ui_state)).await {
                    //                 // Instead of spawning, just trigger the render here directly
                    //                 let mut t = terminal.lock().await;
                    //                 render(&mut t, Arc::clone(&ui_state), Arc::clone(&state), Arc::clone(&pure_tasks)).await;
                    //             }
                    //         }
                    //     }
                    //     // maybe_event = reader.next() => {
                    //     //     info!("********** Received terminal event: {:?}", maybe_event);
                    //     //     if let Some(Ok(event::Event::Key(key_event))) = maybe_event &&
                    //     //            let Err(e) = sx.send(key_event) {
                    //     //         error!("Failed to send key event: {:?}", e);
                    //     //     }
                    //     // } 
                    //     // key_event = rx.recv() => {
                    //     //     debug!("Received internal TUI update signal");
                    //     //     if let Some(key_event) = key_event {
                    //     //         let sx_key_clone = Arc::clone(&sx_key_clone);
                    //     //         let ui_state = Arc::clone(&ui_state);
                    //     //         tokio::spawn(async move {
                    //     //             if handle_key_event(key_event, ui_state).await {
                    //     //                 debug!("Key event handled");
                    //     //                 if let Err(e) = sx_key_clone.send(None) {
                    //     //                     error!("Failed to send status change event: {:?}", e);
                    //     //                 } else {
                    //     //                     debug!("Status change event sent successfully");
                    //     //                 }
                    //     //             }
                    //     //         });
                    //     //     }
                    //     // }
                    // }
                }

                ratatui::restore();
            }).await;
    }
}

// static SPINERS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

async fn render(
    terminal: &mut DefaultTerminal,
    ui_state: Arc<RwLock<TuiState>>,
    state: Arc<AppState>,
    pure_tasks: Arc<Vec<String>>,
) {
    let logs = state.get_logs().await;
    let mut ui_state_lock = ui_state.write().await;
    debug!(
        "TUI state is dirty, refreshing UI... selected: {}",
        ui_state_lock.selected
    );
    ui_state_lock.log_entry.clear();
    logs.iter().for_each(|(level, line)| {
        ui_state_lock
            .log_entry
            .add_log(format!("[{}] {}", level, line))
    });
    ui_state_lock.output_entry.clear();
    let task_name = &pure_tasks[ui_state_lock.selected];
    let task_state = state.get_task_state(task_name).await;
    task_state
        .outputs()
        .read()
        .await
        .iter()
        .for_each(|line| ui_state_lock.output_entry.add_log(line.clone()));
    ui_state_lock.tasks.clear();
    for task in pure_tasks.iter() {
        let task_state = state.get_task_state(task).await;
        ui_state_lock.tasks.push(task_state);
    }
    ui_state_lock.total_tasks = pure_tasks.len();

    // let mut ui_state_lock = ui_state.write().await;
    ui_state_lock.frame_count = ui_state_lock.frame_count.wrapping_add(1);
    debug!("Trying to draw a UI...");
    let draw_result = terminal.try_draw(render_ui(&mut ui_state_lock));
    drop(ui_state_lock);

    if let Err(e) = draw_result {
        error!("Error drawing TUI: {:?}", e);
    } else {
        debug!("We draw UI successfully");
    }
}

fn render_ui(state: &mut TuiState) -> impl FnOnce(&mut Frame) -> io::Result<()> {
    move |frame| {
        // let spiners = ["⣾", "⣷", "⣯", "⣟", "⣻", "⣽", "⣾", "⣷"];
        // { "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏" }
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
            draw_logs(frame, &mut state.log_entry, log_area);
        }

        frame.render_widget(
            Paragraph::new(state.log_entry.logs().last().cloned().unwrap_or_default())
                .block(Block::bordered().title("Status")),
            status,
        );

        frame.render_widget(
            Paragraph::new("Commands: q=quit, ↑/k=up, ↓/j=down, Enter/r=start, s=stop")
                .block(Block::bordered().title("Commands")),
            commands,
        );

        // Left: Task List
        let items: Vec<ListItem> = state
            .tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let status = if *task.status() == TaskStatus::Running {
                    // format!("{} RUNNING", SPINERS[state.frame_count % SPINERS.len()])
                    " RUNNING".to_string()
                } else if *task.status() == TaskStatus::Finished {
                    " Finished".to_string()
                } else {
                    " IDLE".to_string()
                };
                let style = if i == state.selected {
                    Style::default().fg(Color::Yellow).bold()
                } else if *task.status() == TaskStatus::Running {
                    Style::default().fg(Color::Green).bold()
                } else if *task.status() == TaskStatus::Finished {
                    Style::default().fg(Color::White).dim()
                } else {
                    Style::default()
                };
                ListItem::new(format!("{} {}", status, task.name())).style(style)
            })
            .collect();

        let list = List::new(items)
            .block(Block::bordered().title("Tasks (↑↓ to select, Enter/r to start, s to stop)"))
            .highlight_style(Style::default().fg(Color::Yellow));

        frame.render_widget(list, left);

        // Right: Selected Task Output
        draw_logs(frame, &mut state.output_entry, right);
        io::Result::Ok(())
    }
}

fn draw_logs(f: &mut Frame, app: &mut LogEntry, area: Rect) {
    let chunks = Layout::vertical([Constraint::Fill(1)]).split(area);
    app.set_viewport_height(chunks[0].height);
    // Convert logs to Lines
    let lines: Vec<_> = app
        .logs
        .iter()
        .map(|l| ratatui::text::Line::from(l.as_str()).style(Style::default().fg(Color::White)))
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

struct TuiState {
    selected: usize,
    show_log: bool,
    total_tasks: usize,
    log_entry: LogEntry,
    output_entry: LogEntry,
    tasks: Vec<TaskState>,
    frame_count: usize,
}

async fn handle_key_event(key_event: event::KeyEvent, state: Arc<RwLock<TuiState>>) -> bool {
    debug!("Handling key event: {:?}", key_event);
    match key_event.code {
        KeyCode::Char('q') => {
            debug!("Received quit command. Shutting down TUI...");
            emit!(StateEvent::Quit);
            false
        }
        KeyCode::Char('l') => {
            let mut state = state.write().await;
            state.show_log = !state.show_log;
            drop(state);
            true
        }
        KeyCode::Char('c') => {
            let mut log = state.write().await;
            log.log_entry.clear();
            drop(log);
            true
        }
        KeyCode::Char('r') | KeyCode::Enter => {
            let task_name = {
                let state = state.read().await;
                let task_name = state.tasks[state.selected].name().to_string();
                drop(state);
                task_name
            };
            App::instance().stop_task(&task_name).await;
            App::instance().run_task(&task_name).await;
            true
        }
        KeyCode::Char('s') => {
            let (name, status) = {
                let state = state.read().await;
                let selected_task = &state.tasks[state.selected];
                let result = (
                    selected_task.name().to_string(),
                    selected_task.status().clone(),
                );
                drop(state);
                result
            };
            if status == TaskStatus::Running {
                App::instance().stop_task(&name).await;
            } else {
                App::instance().run_task(&name).await;
            }
            true
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let mut state = state.write().await;
            state.selected = state.selected.saturating_sub(1).max(0);
            drop(state);
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let mut state = state.write().await;
            state.selected = state.selected.add(1).min(state.total_tasks - 1);
            drop(state);
            true
        }
        KeyCode::PageDown | KeyCode::PageUp | KeyCode::End | KeyCode::Home | KeyCode::Char('f') => {
            let mut state = state.write().await;
            if state.show_log {
                match key_event.code {
                    KeyCode::PageDown => state.log_entry.scroll_down(10),
                    KeyCode::PageUp => state.log_entry.scroll_up(10),

                    KeyCode::End => {
                        state.log_entry.follow = true;
                        state.log_entry.scroll_to_bottom();
                    }
                    KeyCode::Home => {
                        state.log_entry.follow = false;
                        state.log_entry.scroll = 0;
                    }
                    KeyCode::Char('f') => {
                        state.log_entry.follow = !state.log_entry.follow;
                        if state.log_entry.follow {
                            state.log_entry.scroll_to_bottom();
                        }
                    }
                    _ => (),
                }
            } else {
                match key_event.code {
                    KeyCode::PageDown => state.output_entry.scroll_down(10),
                    KeyCode::PageUp => state.output_entry.scroll_up(10),
                    KeyCode::End => {
                        state.output_entry.follow = true;
                        state.output_entry.scroll_to_bottom();
                    }
                    KeyCode::Home => {
                        state.output_entry.follow = false;
                        state.output_entry.scroll = 0;
                    }
                    KeyCode::Char('f') => {
                        state.output_entry.follow = !state.output_entry.follow;
                        if state.output_entry.follow {
                            state.output_entry.scroll_to_bottom();
                        }
                    }
                    _ => (),
                }
            }
            drop(state);
            true
        }
        _ => false,
    }
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
    pub fn logs(&self) -> Vec<String> {
        self.logs.clone()
    }
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
