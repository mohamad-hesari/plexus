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

pub struct TuiTracingLayer {
    handle: tokio::runtime::Handle,
}

impl Default for TuiTracingLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiTracingLayer {
    pub fn new() -> Self {
        Self {
            handle: tokio::runtime::Handle::current(),
        }
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

        self.handle.spawn(async move {
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
        color_eyre::install().unwrap();
        let terminal = Arc::new(Mutex::new(ratatui::init()));
        let pure_tasks = Arc::new(state.get_tasks().await);
        self.state
            .spawn(async move {
                let ui_state = Arc::new(RwLock::new(TuiState {
                    selected: 0,
                    box_selected: BoxSelected::Tasks,
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

                loop {
                    if !state_clone.is_running() {
                        debug!("TUI initialization detected app is no longer running, exiting...");
                        break;
                    }

                    let start = Instant::now();

                    let mut t = terminal_clone.lock().await;
                    render(&mut t, Arc::clone(&ui_state_clone), Arc::clone(&state_clone), Arc::clone(&pure_tasks_clone)).await;
                    drop(t);

                    debug!("TUI render took {:?} ms", start.elapsed().as_millis());

                    tokio::select! {
                        _ = rx_status_change.changed() => {
                            rx_status_change.borrow_and_update();
                            debug!("Received initial task status change event during TUI setup, marking TUI as dirty...");
                        }
                        maybe_event = reader.next() => {
                            if let Some(Ok(event::Event::Key(key_event))) = maybe_event {
                                // Handle directly. If this feels slow, it's because of the WRITE lock in handle_key
                                if handle_key_event(key_event, Arc::clone(&ui_state)).await {
                                    // Instead of spawning, just trigger the render here directly
                                    // let mut t = terminal_clone.lock().await;
                                    // render(&mut t, Arc::clone(&ui_state), Arc::clone(&state_clone), Arc::clone(&pure_tasks_clone)).await;
                                    // drop(t);
                                }
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                            tokio::task::yield_now().await; // Yield to allow other tasks to run
                        }
                    }
                }

                // state.spawn(async move {
                //     loop {
                //         if !state_clone.is_running() {
                //             debug!("TUI status change listener detected app is no longer running, exiting...");
                //             break;
                //         }
                //         rx_status_change.changed().await.unwrap();
                //         debug!("Received task status change event, marking TUI as dirty...");
                //         let mut t = terminal_clone.lock().await;
                //         render(&mut t, Arc::clone(&ui_state_clone), Arc::clone(&state_clone), Arc::clone(&pure_tasks_clone)).await;
                //         drop(t);
                //     }
                // }).await;
                // let state= Arc::clone(&state);
                // // let (sx,mut rx) = tokio::sync::mpsc::unbounded_channel::<event::KeyEvent>();
                // loop {
                //     if !state.is_running() {
                //         debug!("TUI event loop detected app is no longer running, exiting...");
                //         break;
                //     }
                //     let maybe_event = reader.next().await;
                //
                //     let start = Instant::now();
                //     if let Some(Ok(event::Event::Key(key_event))) = maybe_event {
                //         // Handle directly. If this feels slow, it's because of the WRITE lock in handle_key
                //         if handle_key_event(key_event, Arc::clone(&ui_state)).await {
                //             // Instead of spawning, just trigger the render here directly
                //             let mut t = terminal.lock().await;
                //             render(&mut t, Arc::clone(&ui_state), Arc::clone(&state), Arc::clone(&pure_tasks)).await;
                //         }
                //     }
                //     debug!("TUI event loop iteration took {:?} ms", start.elapsed().as_millis());
                // }

                ratatui::restore();
            }).await;
    }
}

static SPINERS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
            draw_logs(
                frame,
                &mut state.log_entry,
                log_area,
                "Logs",
                state.box_selected == BoxSelected::Logs,
            );
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
                    format!("{} RUNNING", SPINERS[state.frame_count % SPINERS.len()])
                    // " RUNNING".to_string()
                } else if *task.status() == TaskStatus::Finished {
                    " Finished".to_string()
                } else if *task.status() == TaskStatus::Failed {
                    " ERROR".to_string()
                } else {
                    " IDLE".to_string()
                };
                let style = if i == state.selected {
                    Style::default().fg(Color::Yellow).bold()
                } else if *task.status() == TaskStatus::Running {
                    Style::default().fg(Color::Green).bold()
                } else if *task.status() == TaskStatus::Finished {
                    Style::default().fg(Color::White).dim()
                } else if *task.status() == TaskStatus::Failed {
                    Style::default().fg(Color::Red).bold()
                } else {
                    Style::default()
                };
                ListItem::new(format!("{} {}", status, task.name())).style(style)
            })
            .collect();

        let task_selected = state.box_selected == BoxSelected::Tasks;
        // let title = if state.tasks.is_empty() {
        //     "Tasks".to_string()
        // } else {
        //     "Tasks (↑↓, r to restart, s to stop/start, tab to switch box)".to_string()
        // };
        let list = List::new(items)
            // .block(
            //     Block::bordered()
            //         .border_type(if task_selected {
            //             BorderType::Thick
            //         } else {
            //             BorderType::Plain
            //         })
            //         .style(if task_selected {
            //             Style::default().fg(Color::Yellow)
            //         } else {
            //             Style::default()
            //         })
            //         .title(title),
            // )
            .highlight_style(Style::default().fg(Color::Yellow));

        let tasks_block = Block::default()
            .borders(Borders::ALL)
            .border_type(if task_selected {
                BorderType::Thick
            } else {
                BorderType::Plain
            })
            .style(if task_selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            })
            .title("Tasks");

        let tasks_block_area = tasks_block.inner(left);
        frame.render_widget(tasks_block, left);

        let [tasks_list_area, help_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(9)]).areas(tasks_block_area);

        frame.render_widget(list, tasks_list_area);
        let help_text = [
            "Commands:",
            "  ↑/k: Move up",
            "  ↓/j: Move down",
            "  r/Enter: Restart selected task",
            "  s: Stop/start selected task",
            "  Tab: Switch between boxes",
            "  l: Toggle logs",
            "  c: Clear logs",
            "  q: Quit",
        ]
        .iter()
        .map(|s| ratatui::text::Line::from(*s))
        .collect::<Vec<_>>();
        let help_paragraph = Paragraph::new(Text::from(help_text))
            .style(Style::default().fg(Color::White).rapid_blink());
        frame.render_widget(help_paragraph, help_area);

        // frame.render_widget(list, tasks_block);

        let output_title = if state.tasks.is_empty() {
            "Output".to_string()
        } else {
            format!(
                "Output - {}",
                state
                    .tasks
                    .get(state.selected)
                    .map(|t| t.name())
                    .unwrap_or_default()
            )
        };
        // Right: Selected Task Output
        draw_logs(
            frame,
            &mut state.output_entry,
            right,
            &output_title,
            state.box_selected == BoxSelected::Output,
        );
        io::Result::Ok(())
    }
}

fn draw_logs(f: &mut Frame, app: &mut LogEntry, area: Rect, title: &str, selected: bool) {
    let chunks = Layout::vertical([Constraint::Fill(1)]).split(area);
    app.set_viewport_height(chunks[0].height);
    // Convert logs to Lines
    let lines: Vec<_> = app
        .logs
        .iter()
        .map(|l| {
            let color = if l.starts_with("[ERR]") {
                Color::Red
            } else if l.starts_with("[WARN]") {
                Color::Yellow
            } else if l.starts_with("[INFO]") {
                Color::Green
            } else {
                Color::White
            };
            ratatui::text::Line::from(l.as_str()).style(Style::default().fg(color))
        })
        .collect();

    let max_scroll = lines.len().saturating_sub(1) as u16;
    let clamped_scroll = app.scroll.min(max_scroll);

    let final_title = if selected {
        format!(
            "{} (f: follow, ↑/PageUp: scroll up, ↓/PageDown: scroll down, Home: top, End: bottom)",
            title
        )
    } else {
        title.to_string()
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(if selected {
                    BorderType::Thick
                } else {
                    BorderType::Plain
                })
                .style(if selected {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                })
                .title(final_title),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoxSelected {
    Tasks,
    Logs,
    Output,
}

struct TuiState {
    selected: usize,
    box_selected: BoxSelected,
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
            if !state.show_log && state.box_selected == BoxSelected::Logs {
                state.box_selected = BoxSelected::Tasks;
            }
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
        KeyCode::Tab => {
            let mut state = state.write().await;
            if state.box_selected == BoxSelected::Tasks {
                state.box_selected = BoxSelected::Output;
            } else if state.box_selected == BoxSelected::Output {
                if state.show_log {
                    state.box_selected = BoxSelected::Logs;
                } else {
                    state.box_selected = BoxSelected::Tasks;
                }
            } else {
                state.box_selected = BoxSelected::Tasks;
            }
            drop(state);
            true
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let mut state = state.write().await;
            if state.box_selected == BoxSelected::Tasks {
                state.selected = state.selected.saturating_sub(1).max(0);
            } else if state.box_selected == BoxSelected::Logs {
                state.log_entry.scroll_up(1);
            } else if state.box_selected == BoxSelected::Output {
                state.output_entry.scroll_up(1);
            }
            drop(state);
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let mut state = state.write().await;
            if state.box_selected == BoxSelected::Tasks {
                state.selected = state.selected.add(1).min(state.total_tasks - 1);
            } else if state.box_selected == BoxSelected::Logs {
                state.log_entry.scroll_down(1);
            } else if state.box_selected == BoxSelected::Output {
                state.output_entry.scroll_down(1);
            }
            drop(state);
            true
        }
        KeyCode::PageDown | KeyCode::PageUp | KeyCode::End | KeyCode::Home | KeyCode::Char('f') => {
            let mut state = state.write().await;
            let entry = if state.box_selected == BoxSelected::Logs {
                &mut state.log_entry
            } else if state.box_selected == BoxSelected::Output {
                &mut state.output_entry
            } else {
                drop(state);
                return false;
            };

            match key_event.code {
                KeyCode::PageDown => entry.scroll_down(10),
                KeyCode::PageUp => entry.scroll_up(10),

                KeyCode::End => {
                    entry.follow = true;
                    entry.scroll_to_bottom();
                }
                KeyCode::Home => {
                    entry.follow = false;
                    entry.scroll = 0;
                }
                KeyCode::Char('f') => {
                    entry.follow = !entry.follow;
                    if entry.follow {
                        entry.scroll_to_bottom();
                    }
                }
                _ => (),
            }

            // if state.show_log && state.box_selected == BoxSelected::Logs {
            //     match key_event.code {
            //         KeyCode::PageDown => state.log_entry.scroll_down(10),
            //         KeyCode::PageUp => state.log_entry.scroll_up(10),
            //
            //         KeyCode::End => {
            //             state.log_entry.follow = true;
            //             state.log_entry.scroll_to_bottom();
            //         }
            //         KeyCode::Home => {
            //             state.log_entry.follow = false;
            //             state.log_entry.scroll = 0;
            //         }
            //         KeyCode::Char('f') => {
            //             state.log_entry.follow = !state.log_entry.follow;
            //             if state.log_entry.follow {
            //                 state.log_entry.scroll_to_bottom();
            //             }
            //         }
            //         _ => (),
            //     }
            // } else if state.box_selected == BoxSelected::Output {
            //     match key_event.code {
            //         KeyCode::PageDown => state.output_entry.scroll_down(10),
            //         KeyCode::PageUp => state.output_entry.scroll_up(10),
            //         KeyCode::End => {
            //             state.output_entry.follow = true;
            //             state.output_entry.scroll_to_bottom();
            //         }
            //         KeyCode::Home => {
            //             state.output_entry.follow = false;
            //             state.output_entry.scroll = 0;
            //         }
            //         KeyCode::Char('f') => {
            //             state.output_entry.follow = !state.output_entry.follow;
            //             if state.output_entry.follow {
            //                 state.output_entry.scroll_to_bottom();
            //             }
            //         }
            //         _ => (),
            //     }
            // }
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
