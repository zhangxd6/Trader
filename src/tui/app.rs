//! TUI application state.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};

use crate::config::StrategyConfig;

/// How the agent is operating, shown in the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Live,
    DryRun,
    Simulate,
}

impl RunMode {
    pub fn label(self) -> &'static str {
        match self {
            RunMode::Live => "LIVE",
            RunMode::DryRun => "DRY-RUN",
            RunMode::Simulate => "SIMULATE",
        }
    }
}

/// Severity / category of a log line, used for colour-coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Cycle,
    Mcp,
    Llm,
    Safety,
    Order,
    Info,
    Error,
}

impl LogLevel {
    pub fn tag(self) -> &'static str {
        match self {
            LogLevel::Cycle => "CYCLE",
            LogLevel::Mcp => "MCP",
            LogLevel::Llm => "LLM",
            LogLevel::Safety => "SAFETY",
            LogLevel::Order => "ORDER",
            LogLevel::Info => "INFO",
            LogLevel::Error => "ERROR",
        }
    }
}

/// One rendered log line.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
}

/// A single position row in the portfolio panel.
#[derive(Debug, Clone)]
pub struct PositionRow {
    pub symbol: String,
    pub quantity: f64,
    pub price: f64,
    pub pnl_pct: f64,
}

/// A snapshot of the portfolio for display.
#[derive(Debug, Clone, Default)]
pub struct PortfolioSnapshot {
    pub cash: f64,
    pub market_value: f64,
    pub total_value: f64,
    pub return_pct: f64,
    pub positions: Vec<PositionRow>,
}

/// Overall run status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppStatus {
    Running,
    Paused,
    Error(String),
}

/// The full TUI state, mutated as [`AppEvent`](super::AppEvent)s arrive.
pub struct App {
    pub strategy: StrategyConfig,
    pub mode: RunMode,
    pub status: AppStatus,
    pub portfolio: Option<PortfolioSnapshot>,
    pub log_buffer: VecDeque<LogEntry>,
    pub last_cycle: Option<DateTime<Utc>>,
    pub next_cycle: Option<DateTime<Utc>>,
    pub market_open: bool,
    pub should_quit: bool,
}

const MAX_LOGS: usize = 20;

impl App {
    pub fn new(strategy: StrategyConfig, mode: RunMode) -> Self {
        Self {
            strategy,
            mode,
            status: AppStatus::Running,
            portfolio: None,
            log_buffer: VecDeque::with_capacity(MAX_LOGS),
            last_cycle: None,
            next_cycle: None,
            market_open: false,
            should_quit: false,
        }
    }

    /// Append a log line, keeping only the most recent [`MAX_LOGS`].
    pub fn push_log(&mut self, entry: LogEntry) {
        if self.log_buffer.len() == MAX_LOGS {
            self.log_buffer.pop_front();
        }
        self.log_buffer.push_back(entry);
    }

    pub fn toggle_pause(&mut self) {
        self.status = match self.status {
            AppStatus::Paused => AppStatus::Running,
            _ => AppStatus::Paused,
        };
    }
}
