//! Terminal UI: a three-panel live view of strategy, portfolio, and logs.

mod app;
mod events;
mod ui;

pub use app::{App, LogEntry, LogLevel, PortfolioSnapshot, PositionRow, RunMode};
pub use events::{run_tui, AppEvent};
