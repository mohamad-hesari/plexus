use futures::StreamExt;
use std::{
  io,
  sync::{Arc, atomic::AtomicBool},
};
use tracing::debug;

use crossterm::{
  event::{DisableMouseCapture, EnableMouseCapture, EventStream},
  execute,
};

use crate::task_managerv2;

pub struct TuiManager {
  _task_manager: Arc<task_managerv2::TaskManager>,
  _is_running: Arc<AtomicBool>,
}

impl TuiManager {
  pub fn new(task: Arc<task_managerv2::TaskManager>, is_running: Arc<AtomicBool>) -> Self {
    Self {
      _task_manager: task,
      _is_running: is_running,
    }
  }

  pub async fn main_loop(&self) {
    let mut stdout = io::stdout();
    execute!(stdout, EnableMouseCapture).unwrap();
    color_eyre::install().unwrap();
    let mut terminal = ratatui::init();
    loop {
      if !self._is_running.load(std::sync::atomic::Ordering::Relaxed) {
        break;
      }

      let draw_result = terminal.try_draw(|frame| {
        let area = frame.area();
        let block = ratatui::widgets::Block::default()
          .title("Task Manager")
          .borders(ratatui::widgets::Borders::ALL);
        frame.render_widget(block, area);
        io::Result::Ok(())
      });

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
                crossterm::event::Event::Key(key_event) => {
                  debug!("Key event: {:?}", key_event);
                  if key_event.code == crossterm::event::KeyCode::Char('q') {
                    debug!("'q' pressed, exiting...");
                    self._is_running.store(false, std::sync::atomic::Ordering::Relaxed);
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
          .. if let _ = tokio::signal::ctrl_c() => {
            debug!("Ctrl-C received, exiting...");
            break;
          }
          _ => {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            tokio::task::yield_now().await;
          }
        }
      );
    }

    ratatui::restore();
    execute!(stdout, DisableMouseCapture).unwrap();
  }
}
