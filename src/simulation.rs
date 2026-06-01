//! Paper-trading simulation: virtual portfolio and a [`ToolExecutor`] that
//! intercepts order calls and applies them locally instead of to the broker.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::error::{Result, TraderError};
use crate::llm::ToolExecutor;
use crate::mcp::RobinhoodMcpClient;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    pub avg_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub timestamp: DateTime<Utc>,
    pub side: String,
    pub symbol: String,
    pub quantity: f64,
    pub price: f64,
    pub pnl: Option<f64>,
}

/// A point-in-time portfolio value snapshot recorded after each cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueSnapshot {
    pub timestamp: DateTime<Utc>,
    pub total_value: f64,
    pub cash: f64,
    pub positions_value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedPortfolio {
    pub starting_cash: f64,
    pub cash_usd: f64,
    pub created_at: DateTime<Utc>,
    pub positions: HashMap<String, Position>,
    pub trade_log: Vec<TradeRecord>,
    /// Equity curve: one snapshot per cycle (or per trade).
    #[serde(default)]
    pub value_history: Vec<ValueSnapshot>,
}

impl SimulatedPortfolio {
    fn new(starting_cash: f64) -> Self {
        Self {
            starting_cash,
            cash_usd: starting_cash,
            created_at: Utc::now(),
            positions: HashMap::new(),
            trade_log: Vec::new(),
            value_history: Vec::new(),
        }
    }

    /// Record a snapshot of the current portfolio value. Call this after every
    /// cycle (or after every trade) to build the equity curve. Uses cost-basis
    /// for open positions — no live price needed.
    pub fn record_snapshot(&mut self) {
        let positions_value = self.market_value(|_| None);
        self.value_history.push(ValueSnapshot {
            timestamp: Utc::now(),
            total_value: self.cash_usd + positions_value,
            cash: self.cash_usd,
            positions_value,
        });
    }

    /// Export the equity curve to a CSV file for external plotting (Excel,
    /// Python, etc.).
    pub fn export_csv(&self, path: &Path) -> Result<()> {
        let mut out = String::from("timestamp,total_value,cash,positions_value,return_pct\n");
        for s in &self.value_history {
            let ret = if self.starting_cash > 0.0 {
                (s.total_value - self.starting_cash) / self.starting_cash * 100.0
            } else {
                0.0
            };
            out.push_str(&format!(
                "{},{:.2},{:.2},{:.2},{:.4}\n",
                s.timestamp.format("%Y-%m-%dT%H:%M:%S"),
                s.total_value,
                s.cash,
                s.positions_value,
                ret,
            ));
        }
        std::fs::write(path, out)?;
        Ok(())
    }

    /// Render a multi-row ASCII equity curve to a String.
    /// `width` is the plot area width in characters (default 60).
    pub fn render_chart(&self, width: usize) -> String {
        let height: usize = 10;

        if self.value_history.len() < 2 {
            return "  No history yet — run at least two simulation cycles first.\n".to_string();
        }

        let values: Vec<f64> = self.value_history.iter().map(|s| s.total_value).collect();
        let n = values.len();
        let min_v = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_v = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = (max_v - min_v).max(1.0);

        // Map each data-point index to a grid column and row.
        let to_col = |i: usize| -> usize {
            if n == 1 { 0 } else {
                (i * (width - 1) / (n - 1)).min(width - 1)
            }
        };
        let to_row = |v: f64| -> usize {
            let norm = (v - min_v) / range;
            (height - 1).saturating_sub((norm * (height - 1) as f64) as usize)
        };

        // col_y[x] = row index for that column (last writer wins on collision).
        let mut col_y: Vec<Option<usize>> = vec![None; width];
        for (i, &v) in values.iter().enumerate() {
            col_y[to_col(i)] = Some(to_row(v));
        }

        // Build character grid.
        let mut grid: Vec<Vec<char>> = vec![vec![' '; width]; height];

        // Draw each plotted point and horizontal connectors at the same row.
        let mut last_col: Option<(usize, usize)> = None; // (col, row)
        for x in 0..width {
            if let Some(y) = col_y[x] {
                // Fill horizontal run from last point if same row.
                if let Some((lx, ly)) = last_col {
                    if ly == y {
                        for cx in lx..x {
                            grid[y][cx] = '─';
                        }
                    }
                }
                grid[y][x] = '●';
                last_col = Some((x, y));
            }
        }

        // Y-axis label width: room for "$10,000" style labels.
        let y_label = |row: usize| -> String {
            let v = max_v - (row as f64 / (height - 1) as f64) * range;
            if row == 0 || row == height / 2 || row == height - 1 {
                format!("${:>7.0}", v)
            } else {
                "        ".to_string()
            }
        };

        let mut lines: Vec<String> = Vec::with_capacity(height + 4);

        for row in 0..height {
            let axis = if row == height - 1 { '┼' } else { '┤' };
            let plot: String = grid[row].iter().collect();
            lines.push(format!("{} {} {}", y_label(row), axis, plot));
        }

        // X-axis bar.
        lines.push(format!("         └─{}", "─".repeat(width)));

        // Date labels.
        let first_ts = self.value_history.first()
            .map(|s| s.timestamp.format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let last_ts = self.value_history.last()
            .map(|s| s.timestamp.format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let gap = width.saturating_sub(first_ts.len() + last_ts.len());
        lines.push(format!("           {}{}{}", first_ts, " ".repeat(gap), last_ts));

        // Summary line.
        let current = values.last().copied().unwrap_or(self.starting_cash);
        let ret_pct = (current - self.starting_cash) / self.starting_cash * 100.0;
        let pnl = current - self.starting_cash;
        lines.push(format!(
            "\n  {} snapshots | {} trades | P&L: ${:+.2} ({:+.1}%) | current: ${:.2}",
            self.value_history.len(),
            self.trade_log.len(),
            pnl,
            ret_pct,
            current,
        ));

        lines.join("\n")
    }

    /// Load from disk or create a fresh portfolio if none exists.
    pub fn load_or_init(path: &Path, starting_cash: f64) -> Result<Self> {
        if path.exists() {
            let data = std::fs::read_to_string(path)?;
            let p: SimulatedPortfolio = serde_json::from_str(&data)
                .map_err(|e| TraderError::Simulation(format!("parsing portfolio: {e}")))?;
            return Ok(p);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let p = Self::new(starting_cash);
        p.save(path)?;
        Ok(p)
    }

    /// Overwrite the portfolio file with a fresh state.
    pub fn reset(path: &Path, starting_cash: f64) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let p = Self::new(starting_cash);
        p.save(path)
    }

    /// Compute the total market value of all positions using the supplied price
    /// lookup. Returns 0.0 for any symbol whose price cannot be resolved.
    pub fn market_value<F>(&self, price_fn: F) -> f64
    where
        F: Fn(&str) -> Option<f64>,
    {
        self.positions
            .values()
            .map(|pos| {
                let price = price_fn(&pos.symbol).unwrap_or(pos.avg_cost);
                pos.quantity * price
            })
            .sum()
    }

    fn save(&self, path: &Path) -> Result<()> {
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    /// Execute a virtual buy. Returns an error if there is insufficient cash.
    fn apply_buy(&mut self, symbol: &str, quantity: f64, price: f64) -> Result<()> {
        let cost = quantity * price;
        if cost > self.cash_usd {
            return Err(TraderError::Simulation(format!(
                "insufficient cash: need ${cost:.2}, have ${:.2}",
                self.cash_usd
            )));
        }
        self.cash_usd -= cost;
        let pos = self.positions.entry(symbol.to_string()).or_insert(Position {
            symbol: symbol.to_string(),
            quantity: 0.0,
            avg_cost: 0.0,
        });
        let total_qty = pos.quantity + quantity;
        pos.avg_cost = (pos.avg_cost * pos.quantity + price * quantity) / total_qty;
        pos.quantity = total_qty;
        self.trade_log.push(TradeRecord {
            timestamp: Utc::now(),
            side: "buy".to_string(),
            symbol: symbol.to_string(),
            quantity,
            price,
            pnl: None,
        });
        Ok(())
    }

    /// Execute a virtual sell. Returns an error if there are insufficient shares.
    fn apply_sell(&mut self, symbol: &str, quantity: f64, price: f64) -> Result<()> {
        let pos = self.positions.get(symbol).ok_or_else(|| {
            TraderError::Simulation(format!("no position in {symbol}"))
        })?;
        if quantity > pos.quantity {
            return Err(TraderError::Simulation(format!(
                "insufficient shares: need {quantity}, have {}",
                pos.quantity
            )));
        }
        let pnl = (price - pos.avg_cost) * quantity;
        let remaining = pos.quantity - quantity;
        self.cash_usd += quantity * price;
        if remaining < 1e-6 {
            self.positions.remove(symbol);
        } else {
            self.positions.get_mut(symbol).unwrap().quantity = remaining;
        }
        self.trade_log.push(TradeRecord {
            timestamp: Utc::now(),
            side: "sell".to_string(),
            symbol: symbol.to_string(),
            quantity,
            price,
            pnl: Some(pnl),
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SimulationExecutor
// ---------------------------------------------------------------------------

/// A [`ToolExecutor`] that intercepts order tools and applies them to a virtual
/// portfolio. All read-only tools are forwarded to the live MCP server.
pub struct SimulationExecutor {
    mcp: Arc<RobinhoodMcpClient>,
    portfolio: Arc<RwLock<SimulatedPortfolio>>,
    portfolio_path: std::path::PathBuf,
}

impl SimulationExecutor {
    pub fn new(
        mcp: Arc<RobinhoodMcpClient>,
        portfolio: Arc<RwLock<SimulatedPortfolio>>,
    ) -> Self {
        Self {
            mcp,
            portfolio,
            portfolio_path: std::path::PathBuf::from("./simulation/portfolio.json"),
        }
    }

    /// Fetch the current last-trade price for `symbol` via MCP.
    async fn fetch_price(&self, symbol: &str) -> Option<f64> {
        let result = self
            .mcp
            .call_tool("get_equity_quotes", serde_json::json!({"symbols": [symbol]}))
            .await
            .ok()?;

        // The quotes tool wraps results in content[0].text as JSON.
        let text = result
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())?;
        let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
        let price_str = parsed
            .pointer("/data/results/0/quote/last_trade_price")
            .and_then(|v| v.as_str())?;
        price_str.parse::<f64>().ok()
    }
}

#[async_trait]
impl ToolExecutor for SimulationExecutor {
    async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        if !self.is_order_tool(name) {
            return self.mcp.call_tool(name, args).await;
        }

        // Parse the order fields the Robinhood MCP uses.
        let symbol = args
            .get("symbol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();
        let side = args
            .get("side")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let mut quantity = extract_f64(&args, &["quantity", "qty", "shares"]).unwrap_or(0.0);
        let mut price = extract_f64(&args, &["price", "limit_price", "estimated_price"])
            .unwrap_or_else(|| extract_f64(&args, &["ask_price", "bid_price"]).unwrap_or(0.0));

        if symbol.is_empty() {
            return Err(TraderError::Simulation("order missing `symbol`".into()));
        }

        // Market orders don't carry a price — fetch the live quote.
        if price <= 0.0 {
            price = self.fetch_price(&symbol).await.unwrap_or(0.0);
        }
        if price <= 0.0 {
            return Err(TraderError::Simulation(format!(
                "order for {symbol} has no usable price in args and quote fetch failed"
            )));
        }

        // Dollar-amount orders (e.g. dollar_amount: "120") — compute fractional qty.
        if quantity <= 0.0 {
            if let Some(dollars) = extract_f64(&args, &["dollar_amount", "notional", "amount_usd"]) {
                quantity = (dollars / price * 10_000.0).round() / 10_000.0;
            }
        }
        if quantity <= 0.0 {
            return Err(TraderError::Simulation(format!(
                "order for {symbol} has no usable quantity in args"
            )));
        }

        let mut p = self.portfolio.write().await;
        match side.as_str() {
            "buy" => p.apply_buy(&symbol, quantity, price)?,
            "sell" => p.apply_sell(&symbol, quantity, price)?,
            other => {
                return Err(TraderError::Simulation(format!(
                    "unknown order side: `{other}`"
                )))
            }
        }
        p.record_snapshot();
        p.save(&self.portfolio_path)?;

        Ok(json!({
            "simulated": true,
            "symbol": symbol,
            "side": side,
            "quantity": quantity,
            "price": price,
            "cash_remaining": p.cash_usd,
            "message": format!(
                "Simulated {side} {quantity} {symbol} @ ${price:.2}. Cash remaining: ${:.2}",
                p.cash_usd
            )
        }))
    }
}

fn extract_f64(args: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        match args.get(*k) {
            Some(Value::Number(n)) => return n.as_f64(),
            Some(Value::String(s)) => {
                if let Ok(f) = s.parse::<f64>() {
                    return Some(f);
                }
            }
            _ => {}
        }
    }
    None
}
