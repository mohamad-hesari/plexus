use crossterm::event::MouseEventKind;
use futures::StreamExt;
use ratatui::{
  DefaultTerminal,
  crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, EventStream, KeyCode,
  },
  crossterm::execute,
  prelude::*,
  symbols::scrollbar,
  widgets::*,
};
use std::{io, ops::Add, sync::Arc};
use tokio::sync::{Mutex, RwLock};
use tracing::{Event, debug, error, field::Visit, info, trace};
use tracing_subscriber::{Layer, layer::Context};

use crate::{
  app::App,
  app_state::{AppState, StateEvent, TaskState, TaskStatus},
  emit,
  log_view::LogView,
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
  fn record_debug(
    &mut self,
    field: &tracing::field::Field,
    value: &dyn std::fmt::Debug,
  ) {
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
    let mut stdout = io::stdout();
    execute!(stdout, EnableMouseCapture).unwrap();
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
                    log_entry: LogView::new(false),
                    output_entry: LogView::new(true),
                    tasks: vec![],
                }));

                let mut t = terminal.lock().await;
                render(&mut t, Arc::clone(&ui_state), Arc::clone(&state), Arc::clone(&pure_tasks)).await;
                drop(t);

                let mut rx_status_change = state.get_status_change_receiver();
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

                    let mut t = terminal_clone.lock().await;
                    render(&mut t, Arc::clone(&ui_state_clone), Arc::clone(&state_clone), Arc::clone(&pure_tasks_clone)).await;
                    drop(t);

                    tokio::select! {
                        _ = rx_status_change.changed() => {
                            rx_status_change.borrow_and_update();
                            trace!("Received initial task status change event during TUI setup, marking TUI as dirty...");
                        }
                        maybe_event = reader.next() => {
                            if let Some(Ok(event::Event::Key(key_event))) = maybe_event {
                                let _ = handle_key_event(key_event, Arc::clone(&ui_state)).await;
                            } else if let Some(Ok(event::Event::Mouse(mouse))) = maybe_event {
                                handle_mouse_event(mouse, Arc::clone(&ui_state)).await;
                            } else if let Some(Ok(event::Event::Resize(width, height))) = maybe_event {
                                debug!("Terminal resized to {}x{}", width, height);
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                            tokio::task::yield_now().await; // Yield to allow other tasks to run
                        }
                    }
                }

                ratatui::restore();
                execute!(stdout, DisableMouseCapture).unwrap();
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
  if pure_tasks.is_empty() {
    return;
  }
  let logs = state.get_logs().await;
  let mut ui_state_lock = ui_state.write().await;
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
  let draw_result = terminal.try_draw(render_ui(&mut ui_state_lock));
  drop(ui_state_lock);

  if let Err(e) = draw_result {
    error!("Error drawing TUI: {:?}", e);
  }
}

fn render_ui(
  state: &mut TuiState,
) -> impl FnOnce(&mut Frame) -> io::Result<()> {
  move |frame| {
    let area = frame.area();
    let [(top_height, bottom_height), _] = if state.show_log {
      [(70, 30), (100, 0)]
    } else {
      [(100, 0), (0, 100)]
    };
    let [top, bottom] = Layout::vertical([
      Constraint::Percentage(top_height),
      Constraint::Percentage(bottom_height),
    ])
    .areas(area);
    let [left, right] = Layout::horizontal([
      Constraint::Percentage(35),
      Constraint::Percentage(65),
    ])
    .areas(top);

    if state.show_log {
      draw_logs(
        frame,
        &mut state.log_entry,
        // log_area,
        bottom,
        "Logs",
        state.box_selected == BoxSelected::Logs,
      );
    }

    // Left: Task List
    let items: Vec<ListItem> = state
      .tasks
      .iter()
      .enumerate()
      .map(|(i, task)| {
        let status = if *task.status() == TaskStatus::Running {
          format!("{} RUNNING", SPINERS[state.frame_count % SPINERS.len()])
        } else if *task.status() == TaskStatus::Finished {
          " Finished".to_string()
        } else if *task.status() == TaskStatus::Failed {
          " ERROR".to_string()
        } else if *task.status() == TaskStatus::Stopped {
          " STOPPED".to_string()
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
        } else if *task.status() == TaskStatus::Stopped {
          Style::default().fg(Color::Gray).dim()
        } else {
          Style::default().fg(Color::DarkGray).dim()
        };
        ListItem::new(format!("{} {}", status, task.name())).style(style)
      })
      .collect();

    let task_selected = state.box_selected == BoxSelected::Tasks;
    let list =
      List::new(items).highlight_style(Style::default().fg(Color::Yellow));

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
      Layout::vertical([Constraint::Min(0), Constraint::Length(9)])
        .areas(tasks_block_area);

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

fn draw_logs(
  f: &mut Frame,
  app: &mut LogView,
  area: Rect,
  title: &str,
  selected: bool,
) {
  // Correct viewport: subtract block borders ONLY
  let viewport_height = area.height.saturating_sub(2);
  app.set_viewport_height(viewport_height);

  let logs = app.logs();
  let lines: Vec<_> = logs
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

  let final_title = if selected {
    format!("{} (f: follow, ↑/PgUp, ↓/PgDn, Home, End)", title)
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
    // IMPORTANT: use your scroll, not scrollbar state
    .scroll((0, 0));
  // NO wrap!

  f.render_widget(paragraph, area);

  let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
    .symbols(scrollbar::VERTICAL);

  f.render_stateful_widget(scrollbar, area, app.scrollbar_state());
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
  log_entry: LogView,
  output_entry: LogView,
  tasks: Vec<TaskState>,
  frame_count: usize,
}

async fn handle_mouse_event(
  mouse: event::MouseEvent,
  state: Arc<RwLock<TuiState>>,
) {
  trace!("Handling mouse event: {:?}", mouse);
  let mut state = state.write().await;
  match mouse.kind {
    MouseEventKind::ScrollUp => {
      if state.box_selected == BoxSelected::Logs {
        state.log_entry.up();
      } else if state.box_selected == BoxSelected::Output {
        state.output_entry.up();
      } else if state.box_selected == BoxSelected::Tasks {
        state.selected = state.selected.saturating_sub(1);
      }
    }
    MouseEventKind::ScrollDown => {
      if state.box_selected == BoxSelected::Logs {
        state.log_entry.down();
      } else if state.box_selected == BoxSelected::Output {
        state.output_entry.down();
      } else if state.box_selected == BoxSelected::Tasks {
        state.selected = state.selected.add(1).min(state.total_tasks - 1);
      }
    }
    // MouseEventKind::Down(MouseButton::Left) => {
    //     // mouse.row and mouse.column tell you where the click happened.
    //     // You'll need to calculate if this row matches your list's position.
    // }
    _ => {}
  }
  drop(state);
}

async fn handle_key_event(
  key_event: event::KeyEvent,
  state: Arc<RwLock<TuiState>>,
) -> bool {
  debug!("Handling key event: {:?}", key_event);
  match key_event.code {
    KeyCode::Char('q') => {
      debug!("Received quit command. Shutting down TUI...");
      emit!(StateEvent::Quit);
      false
    }
    KeyCode::Char('x') => {
      emit!(StateEvent::Failed);
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
      App::instance().state.clear_logs().await;
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
      debug!("Toggling task '{}', current status: {:?}", name, status);
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
        state.selected = state.selected.saturating_sub(1);
      } else if state.box_selected == BoxSelected::Logs {
        state.log_entry.up();
      } else if state.box_selected == BoxSelected::Output {
        state.output_entry.up();
      }
      drop(state);
      true
    }
    KeyCode::Down | KeyCode::Char('j') => {
      let mut state = state.write().await;
      if state.box_selected == BoxSelected::Tasks {
        state.selected = state.selected.add(1).min(state.total_tasks - 1);
      } else if state.box_selected == BoxSelected::Logs {
        state.log_entry.down();
      } else if state.box_selected == BoxSelected::Output {
        state.output_entry.down();
      }
      drop(state);
      true
    }
    KeyCode::PageDown
    | KeyCode::PageUp
    | KeyCode::End
    | KeyCode::Home
    | KeyCode::Char('f') => {
      info!("Handling scroll-related key event: {:?}", key_event.code);
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
        KeyCode::PageDown => entry.page_down(),
        KeyCode::PageUp => entry.page_up(),

        KeyCode::End => {
          entry.end();
        }
        KeyCode::Home => {
          entry.home();
        }
        KeyCode::Char('f') => {
          // entry.follow = !entry.follow;
          // if entry.follow {
          //     entry.scroll_to_bottom();
          // }
        }
        _ => (),
      }

      drop(state);
      true
    }
    _ => false,
  }
}
