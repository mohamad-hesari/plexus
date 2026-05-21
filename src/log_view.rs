use ratatui::widgets::ScrollbarState;

#[derive(Debug)]
pub struct LogView {
  scroll: usize, // first visible line
  logs: Vec<String>,
  viewport_height: usize,
  follow: bool,
  state: ScrollbarState,
  debug: bool,
}

impl LogView {
  pub fn clear(&mut self) {
    self.logs.clear();
  }

  pub fn scroll(&self) -> usize {
    self.scroll
  }

  pub fn logs(&self) -> Vec<String> {
    self.logs
      [self.scroll..(self.scroll + self.viewport_height).min(self.logs.len())]
      .to_vec()
  }

  pub fn new(debug: bool) -> Self {
    Self {
      scroll: 0,
      logs: Vec::new(),
      viewport_height: 0,
      follow: true,
      state: ScrollbarState::default(),
      debug,
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

    // CRITICAL: avoid division edge case
    let viewport = self.viewport_height.min(content_len);
    // if content_len > viewport {
    //     viewport = content_len - viewport;
    //     if self.scroll > viewport {
    //         self.scroll = viewport;
    //     }
    // }

    if self.debug {
      // info!(
      //     "Updating scrollbar state: content_len={}, viewport={}, scroll={}, viewport_height={}, position={}",
      //     content_len,
      //     viewport,
      //     self.scroll,
      //     self.viewport_height,
      //     self.state.get_position()
      // );
    }

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

  // fn update_state(&mut self) {
  //     self.state = self
  //         .state
  //         .content_length(self.logs.len())
  //         .viewport_content_length(self.viewport_height)
  //         .position(self.scroll);
  // }

  pub fn set_viewport_height(&mut self, height: u16) {
    self.viewport_height = height as usize;
    self.clamp_scroll();
    self.update_state();
  }

  pub fn add_log(&mut self, line: String) {
    self.logs.push(line);

    if self.follow {
      self.scroll = self.max_scroll();
    }

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

  pub fn visible_lines(&self) -> &[String] {
    let end = (self.scroll + self.viewport_height).min(self.logs.len());
    &self.logs[self.scroll..end]
  }

  pub fn scrollbar_state(&mut self) -> &mut ScrollbarState {
    &mut self.state
  }
}

impl Default for LogView {
  fn default() -> Self {
    Self::new(false)
  }
}
