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

/// Which scrollable panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedPanel {
    Strategy,
    Reasoning,
    Logs,
}

/// The full TUI state, mutated as [`AppEvent`](super::AppEvent)s arrive.
pub struct App {
    pub strategies: Vec<StrategyConfig>,
    pub active_strategy_idx: usize,
    pub mode: RunMode,
    pub status: AppStatus,
    pub portfolio: Option<PortfolioSnapshot>,
    pub log_buffer: VecDeque<LogEntry>,
    pub last_cycle: Option<DateTime<Utc>>,
    pub next_cycle: Option<DateTime<Utc>>,
    pub market_open: bool,
    pub should_quit: bool,
    /// Latest reasoning text from the LLM (why it traded or held).
    pub latest_reasoning: Option<String>,
    /// Which panel receives scroll key events.
    pub focused_panel: FocusedPanel,
    /// Vertical scroll offset for the Strategy panel.
    pub strategy_scroll: u16,
    /// Vertical scroll offset for the Reasoning panel.
    pub reasoning_scroll: u16,
    /// Vertical scroll offset for the Logs panel.
    pub logs_scroll: u16,
}

const MAX_LOGS: usize = 20;

impl App {
    pub fn new(strategies: Vec<StrategyConfig>, mode: RunMode) -> Self {
        Self {
            strategies,
            active_strategy_idx: 0,
            mode,
            status: AppStatus::Running,
            portfolio: None,
            log_buffer: VecDeque::with_capacity(MAX_LOGS),
            last_cycle: None,
            next_cycle: None,
            market_open: false,
            should_quit: false,
            latest_reasoning: None,
            focused_panel: FocusedPanel::Reasoning,
            strategy_scroll: 0,
            reasoning_scroll: 0,
            logs_scroll: 0,
        }
    }

    pub fn scroll_up(&mut self) {
        match self.focused_panel {
            FocusedPanel::Strategy  => self.strategy_scroll  = self.strategy_scroll.saturating_sub(1),
            FocusedPanel::Reasoning => self.reasoning_scroll = self.reasoning_scroll.saturating_sub(1),
            FocusedPanel::Logs      => self.logs_scroll      = self.logs_scroll.saturating_sub(1),
        }
    }

    pub fn scroll_down(&mut self) {
        match self.focused_panel {
            FocusedPanel::Strategy  => self.strategy_scroll  = self.strategy_scroll.saturating_add(1),
            FocusedPanel::Reasoning => self.reasoning_scroll = self.reasoning_scroll.saturating_add(1),
            FocusedPanel::Logs      => self.logs_scroll      = self.logs_scroll.saturating_add(1),
        }
    }

    pub fn cycle_focus(&mut self) {
        self.focused_panel = match self.focused_panel {
            FocusedPanel::Strategy  => FocusedPanel::Reasoning,
            FocusedPanel::Reasoning => FocusedPanel::Logs,
            FocusedPanel::Logs      => FocusedPanel::Strategy,
        };
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
