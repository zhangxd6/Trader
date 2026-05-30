//! TUI event types and the async render/input loop.

use std::io;
use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::tui::app::{App, AppStatus, LogEntry, LogLevel, PortfolioSnapshot};
use crate::tui::ui;

/// Events sent from the trading loop to the TUI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    PortfolioUpdate(PortfolioSnapshot),
    Log(LogEntry),
    CycleStarted,
    CycleComplete { executed: usize, held: usize },
    NextCycle(DateTime<Utc>),
    MarketStatus(bool),
    Error(String),
}

impl App {
    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::PortfolioUpdate(p) => self.portfolio = Some(p),
            AppEvent::Log(entry) => self.push_log(entry),
            AppEvent::CycleStarted => {
                self.last_cycle = Some(Utc::now());
                if self.status != AppStatus::Paused {
                    self.status = AppStatus::Running;
                }
            }
            AppEvent::CycleComplete { executed, held } => {
                self.push_log(LogEntry {
                    timestamp: Utc::now(),
                    level: LogLevel::Info,
                    message: format!("cycle complete: {executed} placed, {held} held"),
                });
            }
            AppEvent::NextCycle(t) => self.next_cycle = Some(t),
            AppEvent::MarketStatus(open) => self.market_open = open,
            AppEvent::Error(e) => {
                self.push_log(LogEntry {
                    timestamp: Utc::now(),
                    level: LogLevel::Error,
                    message: e.clone(),
                });
                self.status = AppStatus::Error(e);
            }
        }
    }
}

/// Run the TUI event loop until the user quits. Drains `rx` for trading-loop
/// events and polls the terminal for keypresses.
pub async fn run_tui(mut app: App, mut rx: mpsc::Receiver<AppEvent>) -> io::Result<()> {
    let mut terminal = setup_terminal()?;
    let mut reader = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    loop {
        // Drain all pending trading-loop events.
        while let Ok(event) = rx.try_recv() {
            app.handle_event(event);
        }

        terminal.draw(|f| ui::render(f, &app))?;

        if app.should_quit {
            break;
        }

        tokio::select! {
            maybe_event = reader.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                            KeyCode::Char('p') => app.toggle_pause(),
                            _ => {}
                        }
                    }
                }
            }
            _ = ticker.tick() => {}
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}

type Tui = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
