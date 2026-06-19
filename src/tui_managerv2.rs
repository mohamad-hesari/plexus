use chrono::{DateTime, Utc};
use futures::StreamExt;
use ratatui::{
  Frame,
  layout::{Alignment, Constraint, Direction, Layout, Rect},
  style::{Color, Modifier, Style, Stylize},
  symbols::scrollbar,
  text::{Line, Span, Text},
  widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
  },
};
use std::{
  io,
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
  },
};
use tokio::{sync::RwLock, task::JoinSet};
use tracing::{debug, info};

use crossterm::{
  event::{self, DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode},
  execute,
};

use crate::task_managerv2::{self, InternalTaskStatus, TaskWithDetails};

static QUIT_CONFIRM_SECONDS: i64 = 5;
static FRAME_RATE: u64 = 1000 / 15; // 15 FPS

struct State {
  selected_task_id: Option<String>,
  scroll: usize, // first visible line
  viewport_height: usize,
  follow: bool,
  state: ScrollbarState,
  logs: Vec<String>,
  last_key_log: Option<String>,
  command_index: usize,
  quit: Option<DateTime<Utc>>,
}

impl State {
  // pub fn clear(&mut self) {
  //   self.logs.clear();
  // }
  //
  // pub fn scroll(&self) -> usize {
  //   self.scroll
  // }

  pub fn logs(&self) -> Vec<String> {
    self.logs[self.scroll..(self.scroll + self.viewport_height).min(self.logs.len())].to_vec()
  }

  pub fn set_logs(&mut self, logs: Vec<String>) {
    self.logs = logs;
    if self.follow {
      self.scroll = self.max_scroll();
    }
    self.update_state();
  }

  pub fn new() -> Self {
    Self {
      scroll: 0,
      logs: Vec::new(),
      viewport_height: 0,
      follow: true,
      state: ScrollbarState::default(),
      selected_task_id: None,
      last_key_log: None,
      command_index: 0,
      quit: None,
    }
  }

  fn max_scroll(&self) -> usize {
    self.logs.len().saturating_sub(self.viewport_height)
  }

  fn clamp_scroll(&mut self) {
    let max = self.max_scroll();
    if self.scroll > max {
      self.scroll = max;
    }
  }

  fn update_state(&mut self) {
    let content_len = self.logs.len();

    let viewport = self.viewport_height.min(content_len);

    self.state = self
      .state
      .content_length(content_len)
      .viewport_content_length(viewport)
      .position(self.scroll.min(content_len.saturating_sub(viewport)));

    if self.scroll == self.max_scroll() {
      self.state = self
        .state
        .position(self.logs.len().saturating_sub(self.viewport_height));
    }
  }

  pub fn set_viewport_height(&mut self, height: u16) {
    self.viewport_height = height as usize;
    self.clamp_scroll();
    self.update_state();
  }

  // --- navigation ---

  pub fn up(&mut self) {
    self.follow = false;
    self.scroll = self.scroll.saturating_sub(1);
    self.update_state();
  }

  pub fn down(&mut self) {
    if self.scroll >= self.max_scroll() {
      self.follow = true;
    } else {
      self.scroll += 1;
    }
    self.clamp_scroll();
    self.update_state();
  }

  pub fn page_up(&mut self) {
    self.follow = false;
    self.scroll = self.scroll.saturating_sub(self.viewport_height);
    self.update_state();
  }

  pub fn page_down(&mut self) {
    self.scroll = (self.scroll + self.viewport_height).min(self.max_scroll());
    if self.scroll == self.max_scroll() {
      self.follow = true;
    }
    self.update_state();
  }

  pub fn home(&mut self) {
    self.follow = false;
    self.scroll = 0;
    self.update_state();
  }

  pub fn end(&mut self) {
    self.follow = true;
    self.scroll = self.max_scroll();
    self.update_state();
  }

  // pub fn visible_lines(&self) -> &[String] {
  //   let end = (self.scroll + self.viewport_height).min(self.logs.len());
  //   &self.logs[self.scroll..end]
  // }

  pub fn scrollbar_state(&mut self) -> &mut ScrollbarState {
    &mut self.state
  }
}

pub struct TuiManager {
  _task_manager: Arc<task_managerv2::TaskManager>,
  _is_running: Arc<AtomicBool>,
  _state: Arc<RwLock<State>>,
  _show_logs: AtomicBool,
}

static SPINERS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl TuiManager {
  pub fn new(task: Arc<task_managerv2::TaskManager>, is_running: Arc<AtomicBool>) -> Self {
    Self {
      _task_manager: task,
      _is_running: is_running,
      _state: Arc::new(RwLock::new(State::new())),
      _show_logs: AtomicBool::new(false),
    }
  }

  fn loading_widget(&self, frame: &mut ratatui::Frame, frame_count: usize) {
    let vertical_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Percentage(40), // Top spacing
        Constraint::Length(3),      // Middle area for your text
        Constraint::Percentage(40), // Bottom spacing
      ])
      .split(frame.area());
    let spinner_index = frame_count % SPINERS.len(); // Change spinner every 5 frames

    let text = format!("Loading... {}", SPINERS[spinner_index]);
    let paragraph = Paragraph::new(text)
      .alignment(Alignment::Center) // Centers text horizontally
      .block(Block::default().borders(Borders::NONE)); // Add borders if you want a box

    frame.render_widget(paragraph, vertical_chunks[1]);
  }

  fn task_widget(
    &self,
    task: &TaskWithDetails,
    selected_id: &str,
    frame_count: usize,
    max_name_length: usize,
  ) -> ListItem<'_> {
    let spinner_index = frame_count % SPINERS.len();
    let task_name = task._name.clone();
    let space = max_name_length + 4;
    let mut spans = vec![
      Span::styled(
        if task._id == selected_id {
          format!("> ")
        } else {
          format!("  ")
        },
        Style::default().fg(Color::Green),
      ),
      Span::styled(format!("{task_name:<space$}"), Style::default()),
    ];
    let mut commands = task._commands.values().cloned().collect::<Vec<_>>();
    commands.sort_by(|a, b| a._command.to_string().cmp(&b._command.to_string()));
    commands.iter().for_each(|task_cmd| {
      let (color, symbol) =
        if task_cmd._status.is_running() || task_cmd._status.is_starting() || task_cmd._status.is_stopping() {
          (Some(Color::Yellow), SPINERS[spinner_index])
        } else if task_cmd._status.is_failed() {
          (Some(Color::Red), "✗")
        } else if task_cmd._status.is_successed() {
          (Some(Color::Green), "✓")
        } else if task_cmd._status.is_stopped() {
          (Some(Color::LightRed), "■")
        } else {
          (None, " ")
        };
      let style = if let Some(c) = color {
        Style::default().fg(c)
      } else {
        Style::default()
      }
      .add_modifier(Modifier::BOLD);
      spans.push(Span::styled(format!("{} {}", symbol, task_cmd._command), style));
      spans.push(Span::raw(" "));
    });
    // spans.push(Span::styled(
    //   format!(" ({})", task.last_run_time.format("%Y-%m-%d %H:%M:%S.3f")),
    //   Style::default().fg(Color::DarkGray),
    // ));
    ListItem::new(Line::from(spans)).style(if task._id == selected_id {
      Style::default().add_modifier(Modifier::REVERSED)
    } else {
      Style::default()
    })
  }

  fn apply_dim_overlay(&self, frame: &mut Frame, area: Rect) {
    let buffer = frame.buffer_mut();

    for y in area.top()..area.bottom() {
      for x in area.left()..area.right() {
        let Some(cell) = buffer.cell_mut((x, y)) else {
          continue;
        };

        cell.modifier.insert(Modifier::DIM);
        cell.modifier.remove(Modifier::BOLD);
      }
    }
  }

  fn apply_terminal_padding(&self, area: Rect, padding_x: u16, padding_y: u16) -> Rect {
    // 1. Slice off the top and bottom padding
    let vertical_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Length(padding_y), // Top padding
        Constraint::Min(0),            // Main usable height
        Constraint::Length(padding_y), // Bottom padding
      ])
      .split(area);

    // 2. Take the middle vertical row and slice off the left and right padding
    let horizontal_chunks = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([
        Constraint::Length(padding_x), // Left padding
        Constraint::Min(0),            // Main usable width
        Constraint::Length(padding_x), // Right padding
      ])
      .split(vertical_chunks[1]);

    // Return the perfectly nested middle inner box
    horizontal_chunks[1]
  }

  fn draw_logs_dialog(&self, frame: &mut Frame, area: Rect, state: &mut State, tasks: &Vec<TaskWithDetails>) {
    let sidebar_area = {
      let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);
      let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(10), Constraint::Min(0)])
        .split(vertical_chunks[0]);
      horizontal_chunks
    };

    let task = tasks
      .iter()
      .find(|t| Some(&t._id) == state.selected_task_id.as_ref())
      .unwrap();

    let title = {
      let mut spans = vec![Span::styled(format!("{} commands: ", task._name), Style::default())];
      for (idx, cmd) in task._commands.values().clone().into_iter().enumerate() {
        let style = if idx == state.command_index {
          Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
            .reversed()
        } else {
          Style::default().add_modifier(Modifier::DIM)
        };
        spans.push(Span::styled(format!("{}", cmd._command), style));
        spans.push(Span::raw(" "));
      }
      Line::from(spans)
    };

    frame.render_widget(Clear, sidebar_area[1]);
    self.apply_dim_overlay(frame, sidebar_area[0]); // Dim the main area to make the sidebar pop
    self.draw_scrollable_logs(frame, state, sidebar_area[1], title);
  }

  fn exact_text_centered_rect(&self, text_len: u16, parent_area: Rect) -> (Rect, Vec<Rect>) {
    // 1. Box width needs to be text length + 2 cells for left/right borders
    let box_width = text_len + 2;
    // 2. Box height (e.g., 3 lines high: 1 line text + top border + bottom border)
    let box_height = 5;

    // Center vertically
    let vertical_padding = parent_area.height.saturating_sub(box_height) / 2;
    let vertical_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Length(vertical_padding),
        Constraint::Length(box_height),
        Constraint::Min(0),
      ])
      .split(parent_area);

    // Center horizontally using the exact calculated width
    let horizontal_padding = parent_area.width.saturating_sub(box_width) / 2;
    let horizontal_chunks = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([
        Constraint::Length(horizontal_padding),
        Constraint::Length(box_width),
        Constraint::Min(0),
      ])
      .split(vertical_chunks[1]);

    (
      horizontal_chunks[1],
      vec![
        vertical_chunks[0],
        vertical_chunks[2],
        horizontal_chunks[0],
        horizontal_chunks[2],
      ],
    )
  }

  fn draw_quit_dialog(&self, frame: &mut ratatui::Frame, area: Rect, state: &mut State) {
    let time_elapsed = state
      .quit
      .unwrap()
      .signed_duration_since(Utc::now())
      .num_seconds()
      .abs();

    let seconds_remaining = QUIT_CONFIRM_SECONDS.saturating_sub(time_elapsed);
    let text = format!(
      "[q / Ctrl+C] Quit │ [Esc] Cancel (Auto-resume in {}s)",
      seconds_remaining
    );
    let text_len = text.chars().count() as u16;
    let (centered_area, others) = self.exact_text_centered_rect(text_len, area);

    let dialog_lines = vec![
      // Line 1: Header title indicator line
      Line::from(vec![
        Span::raw("  ").yellow(),
        Span::raw("Quit Application?").white().bold(),
      ]),
      // Line 2: Empty spacer block line for breathing room inside the container
      Line::from(""),
      // Line 3: High-contrast key combinations mapped to actions
      Line::from(vec![
        Span::raw(" Press "),
        Span::raw("q").red().bold(),
        Span::raw(" or "),
        Span::raw("Ctrl+C").red().bold(),
        Span::raw(" to exit. Press "),
        Span::raw("Esc").green().bold(),
        Span::raw(" or wait "),
        Span::raw(format!("{}s", seconds_remaining)).yellow().bold(),
        Span::raw(" to resume."),
      ]),
    ];

    let paragraph = Paragraph::new(dialog_lines).block(
      Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .border_type(BorderType::Thick),
    );
    for other in others {
      self.apply_dim_overlay(frame, other);
    }
    frame.render_widget(Clear, centered_area);
    frame.render_widget(paragraph, centered_area);
  }

  fn draw_scrollable_logs(&self, f: &mut Frame, app: &mut State, area: Rect, title: Line) {
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

    let paragraph = Paragraph::new(Text::from(lines))
      .block(
        Block::default()
          .borders(Borders::ALL)
          .border_type(BorderType::Plain)
          .style(Style::default())
          .title(title),
      )
      .wrap(ratatui::widgets::Wrap { trim: false })
      // IMPORTANT: use your scroll, not scrollbar state
      .scroll((0, 0));
    // NO wrap!

    f.render_widget(paragraph, area);

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(scrollbar::VERTICAL);

    f.render_stateful_widget(scrollbar, area, app.scrollbar_state());
  }

  fn main_widget(
    &self,
    tasks: &Vec<TaskWithDetails>,
    state: &State,
    frame: &mut ratatui::Frame,
    area: Rect,
    frame_count: usize,
  ) {
    let main_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Min(0), Constraint::Length(1)])
      .split(area);
    let max_name_length = tasks.iter().map(|t| t._name.len()).max().unwrap_or(0);
    let vertical_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Percentage(20), Constraint::Min(0)])
      .split(main_chunks[0]);
    let selected_task_id = if let Some(ref id) = state.selected_task_id {
      id.clone()
    } else {
      String::new()
    };
    let running_task_items: Vec<_> = tasks
      .iter()
      .filter(|t| t._status == InternalTaskStatus::Running)
      .map(|t| self.task_widget(&t, &selected_task_id, frame_count, max_name_length))
      .collect::<Vec<_>>();
    let list = List::new(running_task_items).block(Block::default());
    frame.render_widget(list, vertical_chunks[0]);
    let finished_task_items: Vec<_> = tasks
      .iter()
      .filter(|t| t._status == InternalTaskStatus::Other)
      .map(|t| self.task_widget(&t, &selected_task_id, frame_count, max_name_length))
      .collect();
    let list = List::new(finished_task_items).block(Block::default());
    frame.render_widget(list, vertical_chunks[1]);

    let help_text = if self._show_logs.load(Ordering::Relaxed) {
      "q Quit │ Esc Close │ 󰹹 j/k Navigate │  h/l Switch Output │ 󰄳 Home/End/Pg".to_string()
    } else {
      [
        "q Quit",
        " 󰹹 j/k Navigate",
        " Enter Show output",
        " r Restart",
        " s Stop/start",
        " S Force stop",
        " R Restart all",
      ]
      .join(" |")
    };

    let status_bar = Paragraph::new(format!(
      "{} | Status: Ready | last key: {}",
      help_text,
      if let Some(key) = state.last_key_log.as_ref() {
        key.clone()
      } else {
        "None".to_string()
      }
    ));
    frame.render_widget(status_bar, main_chunks[1]);
  }

  pub async fn main_loop(&self) {
    let mut stdout = io::stdout();
    execute!(stdout, EnableMouseCapture).unwrap();
    color_eyre::install().unwrap();
    let mut terminal = ratatui::init();
    let mut loading = true;
    let frame_count = Arc::new(AtomicUsize::new(0));

    info!("Starting TUI main loop");

    loop {
      if !self._is_running.load(std::sync::atomic::Ordering::Relaxed) {
        info!("Exit signal received, breaking TUI main loop");
        break;
      }

      if loading && self._task_manager.is_loading().await {
        let fr = frame_count.load(std::sync::atomic::Ordering::Relaxed);
        terminal
          .try_draw(|frame| {
            self.loading_widget(frame, fr);
            io::Result::Ok(())
          })
          .unwrap();
        frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(FRAME_RATE)).await;
        continue;
      } else {
        loading = false;
      }

      let tasks = self._task_manager.get_store().get_all_tasks_with_details().await;
      let fr = frame_count.load(std::sync::atomic::Ordering::Relaxed);
      let clone_tasks = tasks.clone();
      if self._show_logs.load(Ordering::Relaxed) {
        let mut state = self._state.write().await;
        if let Some(st) = state.selected_task_id.as_ref() {
          debug!("Currently showing logs for task ID: {}", st);
          let task = tasks.iter().find(|t| t._id == *st);
          if let Some(task) = task {
            debug!("Found task for logs: {} with name: {}", task._id, task._name);
            let (_, cmd) = task
              ._commands
              .values()
              .into_iter()
              .enumerate()
              .find(|(idx, _)| idx == &state.command_index)
              .unwrap_or((0, &task._commands.values().next().unwrap()));
            let logs = self._task_manager.get_store().get_logs(&cmd._id).await;
            state.set_logs(logs);
          }
        }
      }
      let mut state = self._state.write().await;
      let show_logs = self._show_logs.load(Ordering::Relaxed);
      let draw_result = terminal.try_draw(move |frame| {
        let area = self.apply_terminal_padding(frame.area(), 1, 1);
        self.main_widget(&tasks, &state, frame, area, fr);
        if show_logs {
          debug!("Showing logs for task: {}", show_logs);
          self.draw_logs_dialog(frame, area, &mut state, &tasks);
        }
        if let Some(ref quit_time) = state.quit {
          if Utc::now().signed_duration_since(*quit_time).num_seconds() >= QUIT_CONFIRM_SECONDS {
            state.quit = None;
          } else {
            self.draw_quit_dialog(frame, area, &mut state);
          }
        }
        drop(state);
        io::Result::Ok(())
      });
      frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

      if let Err(e) = draw_result {
        eprintln!("Error drawing UI: {:?}", e);
        break;
      }

      let mut reader = EventStream::new();
      tokio_select!(
        biased,
        match .. {
          .. if let event = reader.next() => {
            match event {
              Some(Ok(event)) => match event {
                Event::Key(key_event) => {
                  debug!("Key event: {:?}", key_event);
                  if key_event.code == KeyCode::Char('q')
                    || (key_event.code == KeyCode::Char('c')
                      && key_event.modifiers.contains(event::KeyModifiers::CONTROL))
                  {
                    let mut state = self._state.write().await;
                    let mut handled = false;
                    if let Some(quit_time) = state.quit {
                      if Utc::now().signed_duration_since(quit_time).num_seconds() < QUIT_CONFIRM_SECONDS {
                        debug!(
                          "'q' pressed again within {} seconds, confirming quit",
                          QUIT_CONFIRM_SECONDS
                        );
                        self._is_running.store(false, Ordering::Relaxed);
                        handled = true;
                      }
                    }
                    if !handled {
                      debug!("'q' pressed, starting quit timer");
                      state.quit = Some(Utc::now());
                    }
                  } else {
                    let mut handled = false;
                    if key_event.code == KeyCode::Esc {
                      let mut state = self._state.write().await;
                      if let Some(_) = state.quit {
                        debug!("ESC pressed, canceling quit");
                        state.quit = None;
                        handled = true;
                      }
                      drop(state);
                    }
                    if !handled {
                      if self._show_logs.load(Ordering::Relaxed) {
                        self.handle_logs_key(key_event, clone_tasks).await;
                      } else {
                        self.handle_main_key(key_event, clone_tasks).await;
                      }
                    }
                  }
                }
                _ => {}
              },
              Some(Err(e)) => {
                eprintln!("Error reading event: {:?}", e);
                break;
              }
              None => {
                // Stream ended
                break;
              }
            }
          }
          .. if let _ = tokio::time::sleep(std::time::Duration::from_millis(FRAME_RATE)) => {
            tokio::task::yield_now().await;
          }
        }
      );
    }

    debug!("Exiting TUI main loop, performing cleanup");
    ratatui::restore();
    execute!(stdout, DisableMouseCapture).unwrap();
  }

  async fn handle_logs_key(&self, key_event: event::KeyEvent, tasks: Vec<TaskWithDetails>) {
    debug!("Key event in logs view: {:?}", key_event);
    match key_event.code {
      KeyCode::Char('f') => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'f' to toggle follow mode"));
        state.follow = !state.follow;
        if state.follow {
          state.scroll = state.max_scroll();
        }
        state.update_state();
        drop(state);
      }
      KeyCode::Char('h') | KeyCode::Left => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'h' or Left arrow in logs view"));
        state.command_index = state.command_index.saturating_sub(1).max(0);
        drop(state);
      }
      KeyCode::Char('l') | KeyCode::Right => {
        let mut state = self._state.write().await;
        let task_id = if let Some(ref id) = state.selected_task_id {
          id.clone()
        } else {
          String::new()
        };
        let task = tasks.iter().find(|t| t._id == task_id);
        state.last_key_log = Some(format!("Pressed 'l' or Right arrow in logs view"));
        if let Some(task) = task {
          state.command_index = state
            .command_index
            .saturating_add(1)
            .min(task._commands.len().saturating_sub(1));
        }
        drop(state);
      }
      KeyCode::Char('k') | KeyCode::Up => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'k' or Up arrow in logs view"));
        state.up();
        drop(state);
      }
      KeyCode::Char('j') | KeyCode::Down => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'j' or Down arrow in logs view"));
        state.down();
        drop(state);
      }
      KeyCode::PageUp => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed Page Up in logs view"));
        state.page_up();
        drop(state);
      }
      KeyCode::PageDown => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed Page Down in logs view"));
        state.page_down();
        drop(state);
      }
      KeyCode::Home => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed Home in logs view"));
        state.home();
        drop(state);
      }
      KeyCode::End => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed End in logs view"));
        state.end();
        drop(state);
      }
      KeyCode::Esc => {
        self._show_logs.store(false, Ordering::Relaxed);
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed ESC to exit logs view"));
        // state.selected_task_id = None;
        drop(state);
      }
      _ => {
        debug!("Unhandled key event in logs view: {:?}", key_event);
      }
    }
  }

  async fn handle_main_key(&self, key_event: event::KeyEvent, tasks: Vec<TaskWithDetails>) {
    debug!("Key event: {:?}", key_event);
    async fn restart_all(tasks: Vec<TaskWithDetails>, task_manager: Arc<task_managerv2::TaskManager>) {
      let mut sets = JoinSet::new();
      for task in tasks.iter() {
        debug!("Restarting task ID: {} with name: {}", task._id, task._name);
        for cmd in task._commands.values() {
          debug!("Command: {} with status: {:?}", cmd._command, cmd._status);
          let task_id = task._id.clone();
          let cmd_id = cmd._id.clone();
          let task_manager = Arc::clone(&task_manager);
          sets.spawn(async move {
            task_manager.restart_command(&task_id, &cmd_id, false).await;
          });
        }
      }
      sets.join_all().await;
    }
    match key_event.code {
      KeyCode::Char('k') | KeyCode::Up => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'k' or Up arrow"));
        let idx = if let Some(selected_id) = state.selected_task_id.as_ref() {
          tasks.iter().position(|t| &t._id == selected_id)
        } else {
          Some(0)
        };
        if let Some(idx) = idx {
          state.selected_task_id = Some(tasks[(idx + tasks.len() - 1) % tasks.len()]._id.clone());
        } else {
          state.selected_task_id = Some(tasks[0]._id.clone());
        }
        drop(state);
      }
      KeyCode::Char('j') | KeyCode::Down => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'j' or Down arrow"));
        let idx = if let Some(selected_id) = state.selected_task_id.as_ref() {
          tasks.iter().position(|t| &t._id == selected_id)
        } else {
          None
        };
        if let Some(idx) = idx {
          state.selected_task_id = Some(tasks[(idx + 1) % tasks.len()]._id.clone());
        } else {
          state.selected_task_id = Some(tasks[0]._id.clone());
        }
        drop(state);
      }
      KeyCode::Esc => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed ESC to deselect task"));
        state.selected_task_id = None;
        drop(state);
      }
      KeyCode::Enter => {
        let mut state = self._state.write().await;
        let Some(ref selected_id) = state.selected_task_id else {
          debug!("No task selected, ignoring key event: {:?}", key_event);
          state.last_key_log = Some(format!("Pressed Enter but no task selected"));
          drop(state);
          return;
        };
        let id = selected_id.clone();
        state.last_key_log = Some(format!("Pressed Enter on task ID: {}", id.clone()));
        debug!("Enter pressed, showing logs for task ID: {}", id.clone());
        state.command_index = 0;
        self._show_logs.store(true, Ordering::Relaxed);
        drop(state);
      }
      KeyCode::Char('R') => {
        let mut state = self._state.write().await;
        state.last_key_log = Some(format!("Pressed 'R' to restart all tasks"));
        drop(state);
        restart_all(tasks, Arc::clone(&self._task_manager)).await;
      }
      KeyCode::Char('r') => {
        if key_event.modifiers.contains(event::KeyModifiers::SHIFT) {
          restart_all(tasks, Arc::clone(&self._task_manager)).await;
          return;
        }
        let state = self._state.write().await;
        let Some(ref selected_id) = state.selected_task_id else {
          debug!("No task selected, ignoring key event: {:?}", key_event);
          drop(state);
          return;
        };
        let id = selected_id.clone();
        debug!("Pressed 'r' to restart task ID: {}", id.clone());
        for task in tasks.iter() {
          if task._id == id {
            debug!("Found task to restart: {} with name: {}", task._id, task._name);
            for cmd in task._commands.values() {
              debug!("Command: {} with status: {:?}", cmd._command, cmd._status);
              self._task_manager.restart_command(&task._id, &cmd._id, false).await;
            }
            break;
          }
        }
        drop(state);
      }
      KeyCode::Char('s') => {
        let mut state = self._state.write().await;
        let Some(ref selected_id) = state.selected_task_id else {
          debug!("No task selected, ignoring key event: {:?}", key_event);
          drop(state);
          return;
        };
        let id = selected_id.clone();
        debug!("Pressed 's' to stop/start task ID: {}", id.clone());
        for task in tasks.iter() {
          if task._id == id {
            debug!("Found task to stop/start: {} with name: {}", task._id, task._name);
            if task
              ._commands
              .iter()
              .any(|(_, cmd)| cmd._status.is_starting() || cmd._status.is_stopping())
            {
              debug!("Task is currently running or stopping, stopping it...");
              let message = format!("Currently running or stopping, stopping task ID: {}", id.clone());
              state.last_key_log = Some(message);
              break;
            }
            let mut handled = false;
            for cmd in task._commands.values() {
              debug!("Command: {} with status: {:?}", cmd._command, cmd._status);
              if cmd._status.is_finished() {
                self._task_manager.start_command(&task._id, &cmd._id, false).await;
                handled = true;
              }
            }
            if handled {
              break;
            }
            for cmd in task._commands.values() {
              debug!("Command: {} with status: {:?}", cmd._command, cmd._status);
              if cmd._status.is_running() {
                self._task_manager.stop_command(&task._id, &cmd._id, false).await;
              }
            }
            break;
          }
        }
        drop(state);
      }
      KeyCode::Char('S') => {
        let state = self._state.write().await;
        let Some(ref selected_id) = state.selected_task_id else {
          debug!("No task selected, ignoring key event: {:?}", key_event);
          drop(state);
          return;
        };
        let id = selected_id.clone();
        debug!("Pressed 'S' to stop task ID: {}", id.clone());
        for task in tasks.iter() {
          if task._id == id {
            debug!("Found task to stop: {} with name: {}", task._id, task._name);
            for cmd in task._commands.values() {
              debug!("Command: {} with status: {:?}", cmd._command, cmd._status);
              if cmd._status.is_running() || cmd._status.is_starting() {
                self._task_manager.stop_command(&task._id, &cmd._id, false).await;
              }
            }
            break;
          }
        }
        drop(state);
      }
      // KeyCode::Char('r') => {
      // }
      _ => {
        debug!("Unhandled key event: {:?}", key_event);
      }
    }
  }
}
