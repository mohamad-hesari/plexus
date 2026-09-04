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
  collections::HashMap,
  io::{self, Write},
  sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
  },
};
use tokio::{sync::RwLock, task::JoinSet};
use tracing::{debug, info, warn};
use unicode_width::UnicodeWidthChar;

use crossterm::{
  event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, MouseEvent, MouseEventKind,
  },
  execute,
};

use crate::task_managerv2::{self, InternalTaskStatus, TaskCommand, TaskStatus, TaskWithDetails, sorted_commands};

static QUIT_CONFIRM_SECONDS: i64 = 5;
static FRAME_RATE: u64 = 1000 / 15; // 15 FPS
/// Rows moved per wheel notch when real mouse capture is on.
static WHEEL_ROWS: usize = 3;
/// Columns a tab character expands to before wrapping measures the line.
static TAB_WIDTH: usize = 4;

/// DECSET 1007, "alternate scroll". While the alternate screen is active the terminal turns
/// wheel notches into cursor key presses, so the wheel scrolls without us capturing the
/// mouse. That is what keeps the terminal's own click-drag text selection working.
static ALTERNATE_SCROLL_ON: &str = "\x1b[?1007h";
static ALTERNATE_SCROLL_OFF: &str = "\x1b[?1007l";

// ---------------------------------------------------------------------------
// Focus
// ---------------------------------------------------------------------------

/// Which window owns the keyboard. Kept as a stack so closing a window restores the keys
/// (and the status bar) of whatever was underneath it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
  Tasks,
  Logs,
  QuitConfirm,
}

/// Placeholder in a keymap, swapped for the live mouse mode when the bar is rendered.
const MOUSE_LABEL: &str = "\u{0}mouse";

impl Focus {
  fn keymap(&self) -> &'static [(&'static str, &'static str)] {
    match self {
      // Most useful first: a narrow terminal wraps the tail onto another row rather than
      // hiding it, so ordering is about reading comfort, not about what survives.
      Focus::Tasks => &[
        ("Enter", "Output"),
        ("j/k", "Move"),
        ("r", "Restart"),
        ("s", "Stop/start"),
        ("S", "Force stop"),
        ("R", "Restart all"),
        ("m", MOUSE_LABEL),
        ("q", "Quit"),
      ],
      Focus::Logs => &[
        ("Esc", "Close"),
        ("h/l", "Tabs"),
        ("j/k", "Scroll"),
        ("PgUp/PgDn", "Page"),
        ("Home/End", "Jump"),
        ("f", "Follow"),
        ("s", "Stop/start"),
        ("r", "Restart"),
        ("S", "Force stop"),
        ("m", MOUSE_LABEL),
      ],
      Focus::QuitConfirm => &[("q/Ctrl+C", "Quit"), ("Esc", "Cancel")],
    }
  }
}

// ---------------------------------------------------------------------------
// Log pane
// ---------------------------------------------------------------------------

/// One scrollable log view, owned by a single tab.
///
/// Scroll offsets here count *rendered rows*, not source lines. The two are not the same
/// once a line is wider than the pane, and conflating them is what used to leave the tail
/// of the log permanently below the bottom border: the widget was handed N source lines
/// that needed more than N rows to draw, and silently clipped the overflow.
struct LogPane {
  rows: Vec<(String, Color)>,
  /// How many source lines have already been wrapped into `rows`.
  raw_len: usize,
  wrap_width: u16,
  scroll: usize,
  viewport_height: usize,
  follow: bool,
  sb: ScrollbarState,
}

/// Hard-wrap one source line into chunks no wider than `width` display columns.
///
/// Hard wrap rather than word wrap: log lines are mostly paths, JSON and stack traces where
/// breaking on spaces just wastes columns.
fn wrap_line(line: &str, width: u16) -> Vec<String> {
  let width = width.max(1) as usize;
  let mut out = Vec::new();
  let mut current = String::new();
  let mut current_width = 0usize;

  for ch in line.chars() {
    if ch == '\r' {
      continue;
    }
    let (text, w) = if ch == '\t' {
      let pad = TAB_WIDTH - (current_width % TAB_WIDTH);
      (" ".repeat(pad), pad)
    } else {
      (ch.to_string(), ch.width().unwrap_or(0))
    };
    if current_width + w > width && current_width > 0 {
      out.push(std::mem::take(&mut current));
      current_width = 0;
    }
    current.push_str(&text);
    current_width += w;
  }
  // An empty source line still occupies one row.
  out.push(current);
  out
}

fn line_color(line: &str) -> Color {
  if line.starts_with("[ERR]") {
    Color::Red
  } else if line.starts_with("[WARN]") {
    Color::Yellow
  } else if line.starts_with("[INFO]") {
    Color::Green
  } else {
    Color::White
  }
}

impl LogPane {
  fn new() -> Self {
    Self {
      rows: Vec::new(),
      raw_len: 0,
      wrap_width: 0,
      scroll: 0,
      viewport_height: 0,
      follow: true,
      sb: ScrollbarState::default(),
    }
  }

  /// Bring the pane in line with the current log contents and pane geometry.
  ///
  /// Only newly arrived source lines get wrapped, so the cost is proportional to new
  /// output rather than to the size of the whole log. A width change forces a full rewrap,
  /// which is the only time we pay for the backlog.
  fn sync(&mut self, logs: &[String], inner_width: u16, viewport_height: u16) {
    if inner_width != self.wrap_width {
      self.wrap_width = inner_width;
      self.rows.clear();
      self.raw_len = 0;
    }
    // The log store is append-only, but a task restart can truncate it.
    if logs.len() < self.raw_len {
      self.rows.clear();
      self.raw_len = 0;
    }
    for line in &logs[self.raw_len..] {
      let color = line_color(line);
      for row in wrap_line(line, inner_width) {
        self.rows.push((row, color));
      }
    }
    self.raw_len = logs.len();

    self.viewport_height = viewport_height as usize;
    if self.follow {
      self.scroll = self.max_scroll();
    } else {
      self.clamp();
    }
    self.update_scrollbar();
  }

  fn max_scroll(&self) -> usize {
    self.rows.len().saturating_sub(self.viewport_height)
  }

  fn clamp(&mut self) {
    self.scroll = self.scroll.min(self.max_scroll());
  }

  fn update_scrollbar(&mut self) {
    self.sb = self
      .sb
      .content_length(self.rows.len())
      .viewport_content_length(self.viewport_height)
      .position(self.scroll);
  }

  fn visible(&self) -> &[(String, Color)] {
    let end = (self.scroll + self.viewport_height).min(self.rows.len());
    &self.rows[self.scroll.min(end)..end]
  }

  fn up(&mut self, n: usize) {
    self.follow = false;
    self.scroll = self.scroll.saturating_sub(n);
    self.update_scrollbar();
  }

  fn down(&mut self, n: usize) {
    self.scroll = (self.scroll + n).min(self.max_scroll());
    // Reaching the bottom by scrolling re-arms follow, which is what you want after
    // catching up with a live task.
    self.follow = self.scroll == self.max_scroll();
    self.update_scrollbar();
  }

  fn page_up(&mut self) {
    let step = self.viewport_height.max(1);
    self.up(step);
  }

  fn page_down(&mut self) {
    let step = self.viewport_height.max(1);
    self.down(step);
  }

  fn home(&mut self) {
    self.follow = false;
    self.scroll = 0;
    self.update_scrollbar();
  }

  fn end(&mut self) {
    self.follow = true;
    self.scroll = self.max_scroll();
    self.update_scrollbar();
  }

  fn toggle_follow(&mut self) {
    self.follow = !self.follow;
    if self.follow {
      self.scroll = self.max_scroll();
    }
    self.update_scrollbar();
  }
}

// ---------------------------------------------------------------------------
// Logs window
// ---------------------------------------------------------------------------

/// The output window. Tabs are the commands of one task, and every tab keeps its own
/// scroll position and follow flag, so switching away and back does not lose your place.
struct LogsWindow {
  task_id: String,
  tab: usize,
  panes: HashMap<String, LogPane>,
}

impl LogsWindow {
  fn new(task_id: String) -> Self {
    Self {
      task_id,
      tab: 0,
      panes: HashMap::new(),
    }
  }

  fn pane_mut(&mut self, command_id: &str) -> &mut LogPane {
    self.panes.entry(command_id.to_string()).or_insert_with(LogPane::new)
  }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct State {
  selected_task_id: Option<String>,
  focus: Vec<Focus>,
  logs: Option<LogsWindow>,
  last_key_log: Option<String>,
  quit: Option<DateTime<Utc>>,
}

impl State {
  fn new() -> Self {
    Self {
      selected_task_id: None,
      focus: vec![Focus::Tasks],
      logs: None,
      last_key_log: None,
      quit: None,
    }
  }

  fn focus(&self) -> Focus {
    *self.focus.last().unwrap_or(&Focus::Tasks)
  }

  fn push_focus(&mut self, f: Focus) {
    if self.focus() != f {
      self.focus.push(f);
    }
  }

  fn pop_focus(&mut self) {
    if self.focus.len() > 1 {
      self.focus.pop();
    }
  }
}

/// Everything `draw_logs_dialog` needs beyond the frame and the state.
struct LogsDraw<'a> {
  area: Rect,
  task: Option<&'a TaskWithDetails>,
  logs: &'a [String],
  frame_count: usize,
  status_height: u16,
}

/// The command a log tab points at, or None if the task or tab no longer exists.
fn tab_command(task: &TaskWithDetails, tab: usize) -> Option<&TaskCommand> {
  let cmds = sorted_commands(&task._commands);
  cmds.get(tab).copied()
}

pub struct TuiManager {
  _task_manager: Arc<task_managerv2::TaskManager>,
  _is_running: Arc<AtomicBool>,
  _state: Arc<RwLock<State>>,
  /// Whether the user wants real mouse capture. Off by default so the terminal keeps its
  /// own text selection; the wheel still works through alternate scroll.
  _mouse_wanted: AtomicBool,
}

static SPINERS: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Colour and glyph for a command status, shared by the task rows and the log tabs.
fn status_glyph(status: &TaskStatus, frame_count: usize) -> (Option<Color>, &'static str) {
  if status.is_running() || status.is_starting() || status.is_stopping() {
    (Some(Color::Yellow), SPINERS[frame_count % SPINERS.len()])
  } else if status.is_failed() {
    (Some(Color::Red), "✗")
  } else if status.is_successed() {
    (Some(Color::Green), "✓")
  } else if status.is_stopped() {
    (Some(Color::LightRed), "■")
  } else {
    (None, " ")
  }
}

/// What a key asked us to do to a command. Every one of these is dispatched onto its own
/// task: `stop_command` polls for up to five seconds, which would otherwise freeze the
/// render loop for that whole time.
#[derive(Clone, Copy)]
enum CmdAction {
  Start,
  Stop,
  ForceStop,
  Restart,
}

impl TuiManager {
  pub fn new(task: Arc<task_managerv2::TaskManager>, is_running: Arc<AtomicBool>) -> Self {
    Self::with_mouse(task, is_running, false)
  }

  pub fn with_mouse(task: Arc<task_managerv2::TaskManager>, is_running: Arc<AtomicBool>, mouse: bool) -> Self {
    Self {
      _task_manager: task,
      _is_running: is_running,
      _state: Arc::new(RwLock::new(State::new())),
      _mouse_wanted: AtomicBool::new(mouse),
    }
  }

  fn spawn_action(&self, task_id: String, command_id: String, action: CmdAction) {
    let tm = Arc::clone(&self._task_manager);
    tokio::spawn(async move {
      match action {
        CmdAction::Start => {
          tm.start_command(&task_id, &command_id, false).await;
        }
        CmdAction::Stop => {
          tm.stop_command(&task_id, &command_id, false).await;
        }
        CmdAction::ForceStop => {
          tm.stop_command(&task_id, &command_id, true).await;
        }
        CmdAction::Restart => {
          tm.restart_command(&task_id, &command_id, false).await;
        }
      }
    });
  }

  /// Stop a running command, start a finished one.
  fn toggle_command(&self, task_id: &str, cmd: &TaskCommand) -> String {
    if cmd._status.is_stopping() || cmd._status.is_starting() {
      return format!("{} is already {}", cmd._command, cmd._status);
    }
    if cmd._status.is_running() {
      self.spawn_action(task_id.to_string(), cmd._id.clone(), CmdAction::Stop);
      format!("Stopping {}", cmd._command)
    } else {
      self.spawn_action(task_id.to_string(), cmd._id.clone(), CmdAction::Start);
      format!("Starting {}", cmd._command)
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
    let task_name = task._name.clone();
    let space = max_name_length + 4;
    let mut spans = vec![
      Span::styled(
        if task._id == selected_id { "> " } else { "  " },
        Style::default().fg(Color::Green),
      ),
      Span::styled(format!("{task_name:<space$}"), Style::default()),
    ];
    for task_cmd in sorted_commands(&task._commands) {
      let (color, symbol) = status_glyph(&task_cmd._status, frame_count);
      let style = if let Some(c) = color {
        Style::default().fg(c)
      } else {
        Style::default()
      }
      .add_modifier(Modifier::BOLD);
      spans.push(Span::styled(format!("{} {}", symbol, task_cmd._command), style));
      spans.push(Span::raw(" "));
    }
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

  /// The rect the output window occupies inside `area`, and the strip left of it.
  /// `status_height` has to match what `main_widget` reserved, or the bar gets covered.
  fn logs_dialog_areas(&self, area: Rect, status_height: u16) -> (Rect, Rect) {
    let vertical_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Min(0), Constraint::Length(status_height)])
      .split(area);
    let horizontal_chunks = Layout::default()
      .direction(Direction::Horizontal)
      .constraints([Constraint::Percentage(10), Constraint::Min(0)])
      .split(vertical_chunks[0]);
    (horizontal_chunks[0], horizontal_chunks[1])
  }

  fn draw_logs_dialog(&self, frame: &mut Frame, state: &mut State, ctx: LogsDraw<'_>) {
    let LogsDraw {
      area,
      task,
      logs,
      frame_count,
      status_height,
    } = ctx;
    let (side, dialog) = self.logs_dialog_areas(area, status_height);
    frame.render_widget(Clear, dialog);
    self.apply_dim_overlay(frame, side);

    let Some(task) = task else {
      // The task vanished from the store while its window was open.
      let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .title("output");
      frame.render_widget(
        Paragraph::new("This task is no longer available. Press Esc to close.").block(block),
        dialog,
      );
      return;
    };

    let cmds = sorted_commands(&task._commands);
    let title = {
      let mut spans = vec![Span::styled(
        format!(" {} ", task._name),
        Style::default().add_modifier(Modifier::BOLD),
      )];
      if cmds.is_empty() {
        spans.push(Span::styled("(no commands)", Style::default().add_modifier(Modifier::DIM)));
      }
      for (idx, cmd) in cmds.iter().enumerate() {
        let (color, glyph) = status_glyph(&cmd._status, frame_count);
        let base = if idx == state.logs.as_ref().map_or(0, |w| w.tab) {
          Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
            .reversed()
        } else {
          Style::default().add_modifier(Modifier::DIM)
        };
        let glyph_style = match color {
          Some(c) if idx != state.logs.as_ref().map_or(0, |w| w.tab) => base.fg(c),
          _ => base,
        };
        spans.push(Span::styled(format!(" {} {} ", glyph, cmd._command), glyph_style));
        spans.push(Span::raw(" "));
      }
      Line::from(spans)
    };

    let Some(cmd) = cmds.get(state.logs.as_ref().map_or(0, |w| w.tab)) else {
      let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .title(title);
      frame.render_widget(Paragraph::new("").block(block), dialog);
      return;
    };
    let cmd_id = cmd._id.clone();

    let Some(window) = state.logs.as_mut() else {
      return;
    };
    let pane = window.pane_mut(&cmd_id);

    // Borders eat one column and one row on each side. Every number below is in rendered
    // rows, so the slice we hand the widget always fits exactly.
    let inner_width = dialog.width.saturating_sub(2);
    let viewport_height = dialog.height.saturating_sub(2);
    pane.sync(logs, inner_width, viewport_height);

    let lines: Vec<Line> = pane
      .visible()
      .iter()
      .map(|(row, color)| Line::from(row.as_str()).style(Style::default().fg(*color)))
      .collect();

    // No .wrap(): the rows were already wrapped, and letting the widget wrap again is what
    // pushed the last lines off the bottom.
    let paragraph = Paragraph::new(Text::from(lines)).block(
      Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .style(Style::default())
        .title(title),
    );
    frame.render_widget(paragraph, dialog);

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight).symbols(scrollbar::VERTICAL);
    frame.render_stateful_widget(scrollbar, dialog, &mut pane.sb);
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

  fn draw_quit_dialog(&self, frame: &mut ratatui::Frame, area: Rect, quit_at: DateTime<Utc>) {
    let time_elapsed = quit_at.signed_duration_since(Utc::now()).num_seconds().abs();

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
        Span::raw("  ").yellow(),
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

  /// Every shortcut of the focused window, packed into as many rows as the width needs.
  ///
  /// The whole point of the bar is to tell you what the open window responds to, so it
  /// wraps rather than truncating: ten shortcuts do not fit on one 100-column row.
  fn status_rows(&self, state: &State, width: u16) -> Vec<Line<'static>> {
    let mouse_label = if self._mouse_wanted.load(Ordering::Relaxed) {
      "Mouse: capture"
    } else {
      "Mouse: select"
    };

    let mut entries: Vec<(String, String)> = state
      .focus()
      .keymap()
      .iter()
      .map(|(key, action)| {
        let action = if *action == MOUSE_LABEL { mouse_label } else { *action };
        (key.to_string(), action.to_string())
      })
      .collect();
    if let Some(last) = state.last_key_log.as_ref() {
      entries.push((String::new(), last.clone()));
    }

    let width = width.max(1) as usize;
    let sep = " │ ";
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    for (key, action) in entries {
      let label_width = key.chars().count() + if key.is_empty() { 0 } else { 1 } + action.chars().count();
      let sep_width = if spans.is_empty() { 0 } else { sep.len() };
      if !spans.is_empty() && used + sep_width + label_width > width {
        rows.push(Line::from(std::mem::take(&mut spans)));
        used = 0;
      }
      if !spans.is_empty() {
        spans.push(Span::styled(sep, Style::default().fg(Color::DarkGray)));
        used += sep_width;
      }
      if key.is_empty() {
        spans.push(Span::styled(action, Style::default().fg(Color::DarkGray)));
      } else {
        spans.push(Span::styled(
          key,
          Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::raw(action));
      }
      used += label_width;
    }
    if !spans.is_empty() {
      rows.push(Line::from(spans));
    }
    rows
  }

  fn status_height(&self, state: &State, width: u16) -> u16 {
    self.status_rows(state, width).len().clamp(1, 4) as u16
  }

  fn main_widget(
    &self,
    tasks: &[TaskWithDetails],
    state: &State,
    frame: &mut ratatui::Frame,
    area: Rect,
    frame_count: usize,
    status_height: u16,
  ) {
    let main_chunks = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Min(0), Constraint::Length(status_height)])
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
      .map(|t| self.task_widget(t, &selected_task_id, frame_count, max_name_length))
      .collect::<Vec<_>>();
    let list = List::new(running_task_items).block(Block::default());
    frame.render_widget(list, vertical_chunks[0]);
    let finished_task_items: Vec<_> = tasks
      .iter()
      .filter(|t| t._status == InternalTaskStatus::Other)
      .map(|t| self.task_widget(t, &selected_task_id, frame_count, max_name_length))
      .collect();
    let list = List::new(finished_task_items).block(Block::default());
    frame.render_widget(list, vertical_chunks[1]);

    frame.render_widget(
      Paragraph::new(self.status_rows(state, main_chunks[1].width)),
      main_chunks[1],
    );
  }

  pub async fn main_loop(&self) {
    if let Err(e) = color_eyre::install() {
      warn!("Could not install the color_eyre hooks: {}", e);
    }
    let mut terminal = ratatui::init();
    let mut stdout = io::stdout();
    // Enable alternate scroll only after ratatui has switched to the alternate screen.
    if let Err(e) = write!(stdout, "{}", ALTERNATE_SCROLL_ON).and_then(|_| stdout.flush()) {
      warn!("Could not enable alternate scroll: {}", e);
    }
    let mut mouse_capture_on = false;
    let mut loading = true;
    let frame_count = Arc::new(AtomicUsize::new(0));
    // One reader for the whole session. Building a new EventStream per frame drops events
    // and churns the crossterm reader lock 15 times a second.
    let mut reader = EventStream::new();

    info!("Starting TUI main loop");

    loop {
      if !self._is_running.load(std::sync::atomic::Ordering::Relaxed) {
        info!("Exit signal received, breaking TUI main loop");
        break;
      }

      // Apply a pending mouse-capture toggle before drawing so the status bar and the
      // terminal agree.
      let want_mouse = self._mouse_wanted.load(Ordering::Relaxed);
      if want_mouse != mouse_capture_on {
        let res = if want_mouse {
          execute!(stdout, EnableMouseCapture)
        } else {
          execute!(stdout, DisableMouseCapture)
        };
        match res {
          Ok(()) => mouse_capture_on = want_mouse,
          Err(e) => {
            warn!("Could not change mouse capture: {}", e);
            self._mouse_wanted.store(mouse_capture_on, Ordering::Relaxed);
          }
        }
      }

      if loading && self._task_manager.is_loading().await {
        let fr = frame_count.load(std::sync::atomic::Ordering::Relaxed);
        if let Err(e) = terminal.try_draw(|frame| {
          self.loading_widget(frame, fr);
          io::Result::Ok(())
        }) {
          warn!("Error drawing the loading screen: {}", e);
        }
        frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(FRAME_RATE)).await;
        continue;
      } else {
        loading = false;
      }

      let tasks = self._task_manager.get_store().get_all_tasks_with_details().await;
      let fr = frame_count.load(std::sync::atomic::Ordering::Relaxed);
      let clone_tasks = tasks.clone();

      // The draw closure is synchronous, so the log text for the active tab has to be
      // fetched first.
      let mut state = self._state.write().await;
      let show_logs = state.logs.is_some();
      let mut logs: Vec<String> = Vec::new();
      let mut logs_task_index: Option<usize> = None;
      if let Some(idx) = state
        .logs
        .as_ref()
        .and_then(|w| tasks.iter().position(|t| t._id == w.task_id))
      {
        logs_task_index = Some(idx);
        let count = tasks[idx]._commands.len();
        let tab = {
          let window = state.logs.as_mut().expect("checked above");
          window.tab = window.tab.min(count.saturating_sub(1));
          window.tab
        };
        if let Some(cmd) = tab_command(&tasks[idx], tab) {
          logs = self._task_manager.get_store().get_logs(&cmd._id).await;
        }
      }

      let draw_result = terminal.try_draw(|frame| {
        let area = self.apply_terminal_padding(frame.area(), 1, 1);
        let status_height = self.status_height(&state, area.width);
        self.main_widget(&tasks, &state, frame, area, fr, status_height);
        if show_logs {
          let ctx = LogsDraw {
            area,
            task: logs_task_index.map(|i| &tasks[i]),
            logs: &logs,
            frame_count: fr,
            status_height,
          };
          self.draw_logs_dialog(frame, &mut state, ctx);
        }
        if let Some(quit_time) = state.quit {
          if Utc::now().signed_duration_since(quit_time).num_seconds() >= QUIT_CONFIRM_SECONDS {
            state.quit = None;
            state.pop_focus();
          } else {
            self.draw_quit_dialog(frame, area, quit_time);
          }
        }
        io::Result::Ok(())
      });
      drop(state);
      frame_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

      if let Err(e) = draw_result {
        warn!("Error drawing UI: {:?}", e);
        break;
      }

      tokio_select!(
        biased,
        match .. {
          .. if let event = reader.next() => {
            match event {
              Some(Ok(event)) => match event {
                Event::Key(key_event) => {
                  self.handle_key(key_event, clone_tasks).await;
                }
                Event::Mouse(mouse_event) => {
                  self.handle_mouse(mouse_event, clone_tasks).await;
                }
                _ => {}
              },
              Some(Err(e)) => {
                warn!("Error reading event: {:?}", e);
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
    if mouse_capture_on {
      let _ = execute!(stdout, DisableMouseCapture);
    }
    let _ = write!(stdout, "{}", ALTERNATE_SCROLL_OFF).and_then(|_| stdout.flush());
    ratatui::restore();
  }

  /// Quit confirmation and the Esc unwind live here because they cut across every window.
  async fn handle_key(&self, key_event: KeyEvent, tasks: Vec<TaskWithDetails>) {
    debug!("Key event: {:?}", key_event);
    let is_quit_key = key_event.code == KeyCode::Char('q')
      || (key_event.code == KeyCode::Char('c') && key_event.modifiers.contains(event::KeyModifiers::CONTROL));

    if is_quit_key {
      let mut state = self._state.write().await;
      if let Some(quit_time) = state.quit
        && Utc::now().signed_duration_since(quit_time).num_seconds() < QUIT_CONFIRM_SECONDS
      {
        debug!("Quit key pressed again within {}s, confirming", QUIT_CONFIRM_SECONDS);
        self._is_running.store(false, Ordering::Relaxed);
        return;
      }
      debug!("Quit key pressed, starting quit timer");
      state.quit = Some(Utc::now());
      state.push_focus(Focus::QuitConfirm);
      return;
    }

    if key_event.code == KeyCode::Char('m') {
      let now = !self._mouse_wanted.load(Ordering::Relaxed);
      self._mouse_wanted.store(now, Ordering::Relaxed);
      let mut state = self._state.write().await;
      state.last_key_log = Some(format!(
        "Mouse {}",
        if now {
          "capture on (terminal selection off)"
        } else {
          "capture off (terminal selection on)"
        }
      ));
      return;
    }

    let focus = {
      let state = self._state.read().await;
      state.focus()
    };

    match focus {
      Focus::QuitConfirm => {
        if key_event.code == KeyCode::Esc {
          let mut state = self._state.write().await;
          debug!("Esc pressed, canceling quit");
          state.quit = None;
          state.pop_focus();
        }
      }
      Focus::Logs => self.handle_logs_key(key_event, tasks).await,
      Focus::Tasks => self.handle_main_key(key_event, tasks).await,
    }
  }

  async fn handle_mouse(&self, mouse_event: MouseEvent, tasks: Vec<TaskWithDetails>) {
    let mut state = self._state.write().await;
    match mouse_event.kind {
      MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
        let up = mouse_event.kind == MouseEventKind::ScrollUp;
        match state.focus() {
          Focus::Logs => {
            let Some(task) = tasks.iter().find(|t| Some(&t._id) == state.logs.as_ref().map(|w| &w.task_id)) else {
              return;
            };
            let Some(cmd) = tab_command(task, state.logs.as_ref().map_or(0, |w| w.tab)) else {
              return;
            };
            let cmd_id = cmd._id.clone();
            let Some(window) = state.logs.as_mut() else {
              return;
            };
            let pane = window.pane_mut(&cmd_id);
            if up {
              pane.up(WHEEL_ROWS);
            } else {
              pane.down(WHEEL_ROWS);
            }
          }
          Focus::Tasks => {
            drop(state);
            self.move_selection(if up { -1 } else { 1 }, tasks).await;
          }
          Focus::QuitConfirm => {}
        }
      }
      _ => {}
    }
  }

  /// Move the task-list selection by `delta`, wrapping around.
  async fn move_selection(&self, delta: i32, tasks: Vec<TaskWithDetails>) {
    if tasks.is_empty() {
      return;
    }
    let mut state = self._state.write().await;
    let len = tasks.len() as i32;
    let current = state
      .selected_task_id
      .as_ref()
      .and_then(|id| tasks.iter().position(|t| &t._id == id));
    let next = match current {
      Some(idx) => (((idx as i32 + delta) % len) + len) % len,
      None => 0,
    };
    state.selected_task_id = Some(tasks[next as usize]._id.clone());
  }

  async fn handle_logs_key(&self, key_event: KeyEvent, tasks: Vec<TaskWithDetails>) {
    debug!("Key event in logs view: {:?}", key_event);
    let mut state = self._state.write().await;

    if key_event.code == KeyCode::Esc {
      state.last_key_log = Some("Closed the output window".to_string());
      state.logs = None;
      state.pop_focus();
      return;
    }

    let Some(task) = tasks
      .iter()
      .find(|t| Some(&t._id) == state.logs.as_ref().map(|w| &w.task_id))
    else {
      return;
    };
    let cmds = sorted_commands(&task._commands);
    let tab = state.logs.as_ref().map_or(0, |w| w.tab).min(cmds.len().saturating_sub(1));
    let task_id = task._id.clone();
    let current = cmds.get(tab).map(|c| (c._id.clone(), c._command.to_string(), c._status.clone()));

    // Tab switching and the per-command actions do not touch the pane, so handle them
    // before borrowing one.
    match key_event.code {
      KeyCode::Char('h') | KeyCode::Left | KeyCode::BackTab => {
        if let Some(window) = state.logs.as_mut() {
          window.tab = tab.saturating_sub(1);
        }
        state.last_key_log = Some("Previous tab".to_string());
        return;
      }
      KeyCode::Char('l') | KeyCode::Right | KeyCode::Tab => {
        if let Some(window) = state.logs.as_mut() {
          window.tab = (tab + 1).min(cmds.len().saturating_sub(1));
        }
        state.last_key_log = Some("Next tab".to_string());
        return;
      }
      KeyCode::Char('s') => {
        if let Some(cmd) = cmds.get(tab) {
          let msg = self.toggle_command(&task_id, cmd);
          state.last_key_log = Some(msg);
        }
        return;
      }
      KeyCode::Char('S') => {
        if let Some((cmd_id, name, _)) = current.clone() {
          self.spawn_action(task_id, cmd_id, CmdAction::ForceStop);
          state.last_key_log = Some(format!("Force stopping {}", name));
        }
        return;
      }
      KeyCode::Char('r') => {
        if let Some((cmd_id, name, _)) = current.clone() {
          self.spawn_action(task_id, cmd_id, CmdAction::Restart);
          state.last_key_log = Some(format!("Restarting {}", name));
        }
        return;
      }
      _ => {}
    }

    let Some((cmd_id, _, _)) = current else {
      return;
    };
    let Some(window) = state.logs.as_mut() else {
      return;
    };
    window.tab = tab;
    let pane = window.pane_mut(&cmd_id);
    let msg = match key_event.code {
      KeyCode::Char('f') => {
        pane.toggle_follow();
        format!("Follow {}", if pane.follow { "on" } else { "off" })
      }
      KeyCode::Char('k') | KeyCode::Up => {
        pane.up(1);
        "Scroll up".to_string()
      }
      KeyCode::Char('j') | KeyCode::Down => {
        pane.down(1);
        "Scroll down".to_string()
      }
      KeyCode::PageUp => {
        pane.page_up();
        "Page up".to_string()
      }
      KeyCode::PageDown => {
        pane.page_down();
        "Page down".to_string()
      }
      KeyCode::Home => {
        pane.home();
        "Top".to_string()
      }
      KeyCode::End => {
        pane.end();
        "Bottom".to_string()
      }
      _ => {
        debug!("Unhandled key event in logs view: {:?}", key_event);
        return;
      }
    };
    state.last_key_log = Some(msg);
  }

  async fn handle_main_key(&self, key_event: KeyEvent, tasks: Vec<TaskWithDetails>) {
    debug!("Key event: {:?}", key_event);
    match key_event.code {
      KeyCode::Char('k') | KeyCode::Up => {
        self.move_selection(-1, tasks).await;
        self._state.write().await.last_key_log = Some("Previous task".to_string());
      }
      KeyCode::Char('j') | KeyCode::Down => {
        self.move_selection(1, tasks).await;
        self._state.write().await.last_key_log = Some("Next task".to_string());
      }
      KeyCode::Esc => {
        let mut state = self._state.write().await;
        state.last_key_log = Some("Deselected".to_string());
        state.selected_task_id = None;
      }
      KeyCode::Enter => {
        let mut state = self._state.write().await;
        let Some(id) = state.selected_task_id.clone() else {
          state.last_key_log = Some("No task selected".to_string());
          return;
        };
        debug!("Enter pressed, showing logs for task ID: {}", id);
        let name = tasks
          .iter()
          .find(|t| t._id == id)
          .map(|t| t._name.clone())
          .unwrap_or_else(|| id.clone());
        state.last_key_log = Some(format!("Opened output for {}", name));
        state.logs = Some(LogsWindow::new(id));
        state.push_focus(Focus::Logs);
      }
      KeyCode::Char('R') => {
        self._state.write().await.last_key_log = Some("Restarting every task".to_string());
        let tm = Arc::clone(&self._task_manager);
        // Restarting everything can take seconds per command; keep it off the render loop.
        tokio::spawn(async move {
          let mut sets = JoinSet::new();
          for task in tasks.iter() {
            for cmd in task._commands.values() {
              let task_id = task._id.clone();
              let cmd_id = cmd._id.clone();
              let tm = Arc::clone(&tm);
              sets.spawn(async move {
                tm.restart_command(&task_id, &cmd_id, false).await;
              });
            }
          }
          sets.join_all().await;
        });
      }
      KeyCode::Char('r') => {
        let mut state = self._state.write().await;
        let Some(id) = state.selected_task_id.clone() else {
          return;
        };
        if let Some(task) = tasks.iter().find(|t| t._id == id) {
          for cmd in task._commands.values() {
            self.spawn_action(id.clone(), cmd._id.clone(), CmdAction::Restart);
          }
          state.last_key_log = Some(format!("Restarting {}", task._name));
        }
      }
      KeyCode::Char('s') => {
        let mut state = self._state.write().await;
        let Some(id) = state.selected_task_id.clone() else {
          return;
        };
        let Some(task) = tasks.iter().find(|t| t._id == id) else {
          return;
        };
        if task
          ._commands
          .values()
          .any(|cmd| cmd._status.is_starting() || cmd._status.is_stopping())
        {
          state.last_key_log = Some(format!("{} is mid-transition, ignoring", task._name));
          return;
        }
        // Restart everything that has finished; failing that, stop what is running.
        let mut started = false;
        for cmd in sorted_commands(&task._commands) {
          if cmd._status.is_finished() {
            self.spawn_action(id.clone(), cmd._id.clone(), CmdAction::Start);
            started = true;
          }
        }
        if started {
          state.last_key_log = Some(format!("Starting {}", task._name));
          return;
        }
        for cmd in sorted_commands(&task._commands) {
          if cmd._status.is_running() {
            self.spawn_action(id.clone(), cmd._id.clone(), CmdAction::Stop);
          }
        }
        state.last_key_log = Some(format!("Stopping {}", task._name));
      }
      KeyCode::Char('S') => {
        let mut state = self._state.write().await;
        let Some(id) = state.selected_task_id.clone() else {
          return;
        };
        if let Some(task) = tasks.iter().find(|t| t._id == id) {
          for cmd in task._commands.values() {
            if cmd._status.is_running() || cmd._status.is_starting() {
              self.spawn_action(id.clone(), cmd._id.clone(), CmdAction::ForceStop);
            }
          }
          state.last_key_log = Some(format!("Force stopping {}", task._name));
        }
      }
      _ => {
        debug!("Unhandled key event: {:?}", key_event);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn pane_with(logs: &[&str], width: u16, height: u16) -> (LogPane, Vec<String>) {
    let logs: Vec<String> = logs.iter().map(|s| s.to_string()).collect();
    let mut pane = LogPane::new();
    pane.sync(&logs, width, height);
    (pane, logs)
  }

  #[test]
  fn wraps_by_display_width() {
    assert_eq!(wrap_line("abcdef", 3), vec!["abc", "def"]);
    assert_eq!(wrap_line("abcd", 4), vec!["abcd"]);
    // An empty line still takes a row.
    assert_eq!(wrap_line("", 10), vec![""]);
    // Wide characters count as two columns.
    assert_eq!(wrap_line("日本語", 4), vec!["日本", "語"]);
  }

  #[test]
  fn tabs_expand_before_measuring() {
    assert_eq!(wrap_line("\tx", 8), vec!["    x"]);
  }

  #[test]
  fn follow_shows_the_real_last_row() {
    // Three source lines, each wrapping to two rows, in a viewport four rows tall. The
    // old logical-line maths put scroll at 3 - 4 = 0 and drew the first four of six rows,
    // hiding the tail. Row maths puts it at 6 - 4 = 2.
    let (pane, _) = pane_with(&["aaaabbbb", "ccccdddd", "eeeeffff"], 4, 4);
    assert_eq!(pane.rows.len(), 6);
    assert_eq!(pane.scroll, 2);
    let visible: Vec<&str> = pane.visible().iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(visible, vec!["cccc", "dddd", "eeee", "ffff"]);
    // The last visible row really is the last row of the log.
    assert_eq!(visible.last(), Some(&pane.rows.last().unwrap().0.as_str()));
  }

  #[test]
  fn visible_never_exceeds_the_viewport() {
    let (pane, _) = pane_with(&["aaaabbbbcccc", "dddd"], 4, 2);
    assert_eq!(pane.rows.len(), 4);
    assert_eq!(pane.visible().len(), 2);
  }

  #[test]
  fn end_reaches_the_bottom_and_home_the_top() {
    let (mut pane, _) = pane_with(&["aaaabbbb", "ccccdddd"], 4, 2);
    pane.home();
    assert_eq!(pane.scroll, 0);
    assert!(!pane.follow);
    pane.end();
    assert_eq!(pane.scroll, pane.max_scroll());
    assert_eq!(pane.scroll, 2);
    assert!(pane.follow);
  }

  #[test]
  fn a_paused_pane_keeps_its_offset_as_output_arrives() {
    let mut logs: Vec<String> = vec!["aaaa".into(), "bbbb".into(), "cccc".into(), "dddd".into()];
    let mut pane = LogPane::new();
    pane.sync(&logs, 4, 2);
    pane.up(1); // stop following
    let parked = pane.scroll;
    logs.push("eeee".into());
    pane.sync(&logs, 4, 2);
    assert_eq!(pane.scroll, parked, "new output must not yank a paused view");
    assert_eq!(pane.rows.len(), 5);
  }

  #[test]
  fn scrolling_back_to_the_bottom_re_arms_follow() {
    let logs: Vec<String> = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let mut pane = LogPane::new();
    pane.sync(&logs, 10, 2);
    pane.up(2);
    assert!(!pane.follow);
    pane.down(2);
    assert!(pane.follow);
    assert_eq!(pane.scroll, pane.max_scroll());
  }

  #[test]
  fn resizing_rewraps_and_keeps_the_view_valid() {
    let logs: Vec<String> = vec!["aaaabbbb".into(), "ccccdddd".into()];
    let mut pane = LogPane::new();
    pane.sync(&logs, 4, 4);
    assert_eq!(pane.rows.len(), 4);
    pane.sync(&logs, 8, 4);
    assert_eq!(pane.rows.len(), 2);
    assert!(pane.scroll <= pane.max_scroll());
  }

  #[test]
  fn a_truncated_log_resets_the_pane() {
    let logs: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
    let mut pane = LogPane::new();
    pane.sync(&logs, 10, 2);
    assert_eq!(pane.rows.len(), 3);
    pane.sync(&logs[..1], 10, 2);
    assert_eq!(pane.rows.len(), 1);
    assert_eq!(pane.scroll, 0);
  }

  #[test]
  fn focus_stack_restores_the_previous_keymap() {
    let mut state = State::new();
    assert_eq!(state.focus(), Focus::Tasks);
    state.push_focus(Focus::Logs);
    assert_eq!(state.focus(), Focus::Logs);
    assert_ne!(Focus::Logs.keymap(), Focus::Tasks.keymap());
    state.pop_focus();
    assert_eq!(state.focus(), Focus::Tasks);
    // The base window can never be popped off.
    state.pop_focus();
    assert_eq!(state.focus(), Focus::Tasks);
  }
}
