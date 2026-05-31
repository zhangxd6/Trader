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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedPortfolio {
    pub starting_cash: f64,
    pub cash_usd: f64,
    pub created_at: DateTime<Utc>,
    pub positions: HashMap<String, Position>,
    pub trade_log: Vec<TradeRecord>,
}

impl SimulatedPortfolio {
    fn new(starting_cash: f64) -> Self {
        Self {
            starting_cash,
            cash_usd: starting_cash,
            created_at: Utc::now(),
            positions: HashMap::new(),
            trade_log: Vec::new(),
        }
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
        let quantity = extract_f64(&args, &["quantity", "qty", "shares"]).unwrap_or(1.0);
        let price = extract_f64(&args, &["price", "limit_price", "estimated_price"])
            .unwrap_or_else(|| extract_f64(&args, &["ask_price", "bid_price"]).unwrap_or(0.0));

        if symbol.is_empty() {
            return Err(TraderError::Simulation("order missing `symbol`".into()));
        }
        if price <= 0.0 {
            return Err(TraderError::Simulation(format!(
                "order for {symbol} has no usable price in args"
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
