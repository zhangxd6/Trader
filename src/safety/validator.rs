//! The safety validator: a [`ToolExecutor`] that enforces hard risk limits.
//!
//! Read-only tool calls are forwarded to the underlying executor (the live MCP
//! client or the simulation engine) and their results are observed to maintain
//! a lightweight view of the portfolio. Order-placement tool calls are
//! validated against the configured risk limits and either forwarded, blocked
//! (dry-run), or rejected (limit violation).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Utc};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::config::{RiskConfig, StrategyConfig};
use crate::error::{Result, TraderError};
use crate::llm::ToolExecutor;

/// A coarse view of the portfolio, learned by observing read-tool results.
#[derive(Debug, Clone, Default)]
pub struct PortfolioView {
    /// Total account equity in USD, if known.
    pub total_equity: Option<f64>,
    /// Available cash / buying power in USD, if known.
    pub cash: Option<f64>,
    /// Per-symbol market value in USD.
    pub position_value: HashMap<String, f64>,
}

/// Tracks how many orders have been placed today.
#[derive(Debug)]
pub struct DailyCounter {
    day: u32,
    count: u32,
}

impl Default for DailyCounter {
    fn default() -> Self {
        Self {
            day: Utc::now().ordinal(),
            count: 0,
        }
    }
}

impl DailyCounter {
    fn roll(&mut self, now: DateTime<Utc>) {
        if now.ordinal() != self.day {
            self.day = now.ordinal();
            self.count = 0;
        }
    }

    /// Current count after rolling over at day boundaries.
    pub fn current(&mut self) -> u32 {
        self.roll(Utc::now());
        self.count
    }

    fn increment(&mut self) {
        self.roll(Utc::now());
        self.count += 1;
    }
}

/// Enforces risk limits around an inner tool executor.
pub struct SafetyValidator {
    inner: Arc<dyn ToolExecutor>,
    risk: RiskConfig,
    watchlist: Vec<String>,
    counter: RwLock<DailyCounter>,
    portfolio: RwLock<PortfolioView>,
}

impl SafetyValidator {
    /// Wrap `inner` (live MCP or simulation) with risk enforcement.
    pub fn new(inner: Arc<dyn ToolExecutor>, risk: RiskConfig, strategy: &StrategyConfig) -> Self {
        let watchlist = strategy
            .watchlist
            .iter()
            .map(|s| s.trim().to_uppercase())
            .collect();
        Self {
            inner,
            risk,
            watchlist,
            counter: RwLock::new(DailyCounter::default()),
            portfolio: RwLock::new(PortfolioView::default()),
        }
    }

    fn is_read_portfolio(name: &str) -> bool {
        let n = name.to_lowercase();
        n.contains("portfolio") || n.contains("position") || n.contains("account")
    }

    /// Validate an order's arguments against the risk limits. Returns `Ok` if
    /// the order may proceed, or a [`TraderError::SafetyRejection`] otherwise.
    async fn validate_order(&self, name: &str, args: &Value) -> Result<()> {
        let symbol = first_str(args, &["symbol", "ticker", "instrument"])
            .map(|s| s.to_uppercase())
            .ok_or_else(|| reject("order is missing a symbol"))?;

        let side = first_str(args, &["side", "action", "direction"])
            .map(|s| s.to_lowercase())
            .unwrap_or_else(|| {
                let n = name.to_lowercase();
                if n.contains("sell") {
                    "sell".into()
                } else {
                    "buy".into()
                }
            });
        let is_buy = side.contains("buy");

        // Symbol must be on the watchlist (if a watchlist is configured).
        if !self.watchlist.is_empty() && !self.watchlist.contains(&symbol) {
            return Err(reject(&format!("{symbol} is not on the watchlist")));
        }

        // Buy/sell permission flags.
        if is_buy && !self.risk.allow_buys {
            return Err(reject("buys are disabled by risk config"));
        }
        if !is_buy && !self.risk.allow_sells {
            return Err(reject("sells are disabled by risk config"));
        }

        // Sell guard: can only sell a symbol we actually hold.
        // Skip the check if the portfolio has never been observed (no data yet).
        if !is_buy {
            let view = self.portfolio.read().await;
            let portfolio_known = !view.position_value.is_empty() || view.cash.is_some();
            if portfolio_known && !view.position_value.contains_key(&symbol) {
                return Err(reject(&format!(
                    "cannot sell {symbol}: no position found in portfolio"
                )));
            }
        }

        // Daily trade cap.
        let count = self.counter.write().await.current();
        if count >= self.risk.max_daily_trades {
            return Err(reject(&format!(
                "daily trade cap of {} reached",
                self.risk.max_daily_trades
            )));
        }

        // Trade value cap (best-effort: requires a price in the args).
        let quantity = first_f64(args, &["quantity", "qty", "shares", "amount"]);
        let price = first_f64(args, &["price", "limit_price", "last_price", "estimated_price"]);
        if let (Some(qty), Some(px)) = (quantity, price) {
            let value = qty * px;
            if value > self.risk.max_trade_usd {
                return Err(reject(&format!(
                    "trade value ${value:.2} exceeds cap of ${:.2}",
                    self.risk.max_trade_usd
                )));
            }

            // Buy-only checks that depend on the observed portfolio.
            if is_buy {
                let view = self.portfolio.read().await;

                // Position concentration cap.
                if let Some(total) = view.total_equity.filter(|t| *t > 0.0) {
                    let existing = view.position_value.get(&symbol).copied().unwrap_or(0.0);
                    let new_pct = (existing + value) / total;
                    if new_pct > self.risk.max_position_pct {
                        return Err(reject(&format!(
                            "{symbol} would reach {:.1}% of portfolio (cap {:.1}%)",
                            new_pct * 100.0,
                            self.risk.max_position_pct * 100.0
                        )));
                    }
                }

                // Minimum cash reserve: cash after the buy must stay above the
                // configured fraction of total equity.
                if self.risk.min_cash_reserve_pct > 0.0 {
                    if let (Some(cash), Some(total)) = (view.cash, view.total_equity) {
                        let reserve = total * self.risk.min_cash_reserve_pct;
                        if cash - value < reserve {
                            return Err(reject(&format!(
                                "buy would breach minimum cash reserve of ${reserve:.2}"
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Observe a portfolio read result to refresh the cached view.
    async fn observe_portfolio(&self, result: &Value) {
        let mut view = self.portfolio.write().await;
        if let Some(total) = first_f64(
            result,
            &["total_equity", "portfolio_equity", "equity", "total_value", "account_value"],
        ) {
            view.total_equity = Some(total);
        }
        if let Some(cash) = first_f64(result, &["cash", "buying_power", "uninvested_cash"]) {
            view.cash = Some(cash);
        }
        // Look for a positions array in common shapes.
        let positions = result
            .get("positions")
            .or_else(|| result.get("holdings"))
            .and_then(|p| {
                p.as_array()
                    .cloned()
                    .or_else(|| p.as_object().map(|o| o.values().cloned().collect()))
            });
        if let Some(items) = positions {
            for item in items {
                if let Some(sym) = first_str(&item, &["symbol", "ticker"]) {
                    let sym_upper = sym.to_uppercase();
                    // Record market value when available; fall back to quantity as a
                    // presence marker so the sell-guard knows we hold the position.
                    let val = first_f64(&item, &["market_value", "value", "equity"])
                        .or_else(|| first_f64(&item, &["quantity", "qty", "shares"]))
                        .unwrap_or(1.0); // any positive value marks the position as held
                    view.position_value.insert(sym_upper, val);
                }
            }
        }
    }
}

#[async_trait]
impl ToolExecutor for SafetyValidator {
    async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        // Order tools: validate, then forward / block.
        if self.is_order_tool(name) {
            self.validate_order(name, &args).await?;

            if self.risk.dry_run {
                tracing::warn!(tool = name, args = %args, "[DRY RUN] order blocked");
                return Ok(json!({
                    "status": "dry_run",
                    "message": "Order accepted by safety checks but not placed (dry_run = true).",
                    "echo": args,
                }));
            }

            let result = self.inner.execute(name, args).await?;
            self.counter.write().await.increment();
            return Ok(result);
        }

        // Read tools: forward and observe.
        let result = self.inner.execute(name, args).await?;
        if Self::is_read_portfolio(name) {
            self.observe_portfolio(&result).await;
        }
        Ok(result)
    }
}

fn reject(msg: &str) -> TraderError {
    TraderError::SafetyRejection(msg.to_string())
}

fn first_str(value: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = value.get(*k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn first_f64(value: &Value, keys: &[&str]) -> Option<f64> {
    for k in keys {
        match value.get(*k) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BuyFilters, StructuredRules};

    fn strategy() -> StrategyConfig {
        StrategyConfig {
            name: "T".into(),
            description: String::new(),
            watchlist: vec!["AAPL".into(), "MSFT".into()],
            industries: vec![],
            structured: StructuredRules {
                stop_loss_pct: 5.0,
                take_profit_pct: 15.0,
                max_positions: 3,
                min_confidence: 0.0,
                buy_filters: BuyFilters::default(),
            },
            rules: vec![],
            interval_minutes: None,
        }
    }

    fn risk(dry_run: bool) -> RiskConfig {
        RiskConfig {
            dry_run,
            max_trade_usd: 500.0,
            max_position_pct: 0.10,
            max_daily_trades: 5,
            min_cash_reserve_pct: 0.0,
            allow_buys: true,
            allow_sells: true,
        }
    }

    struct NoopInner;
    #[async_trait]
    impl ToolExecutor for NoopInner {
        async fn execute(&self, _name: &str, _args: Value) -> Result<Value> {
            Ok(json!({ "state": "filled" }))
        }
    }

    #[tokio::test]
    async fn dry_run_blocks_order() {
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(true), &strategy());
        let out = v
            .execute("place_order", json!({ "symbol": "AAPL", "quantity": 1, "price": 100 }))
            .await
            .unwrap();
        assert_eq!(out["status"], json!("dry_run"));
    }

    #[tokio::test]
    async fn rejects_off_watchlist_symbol() {
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &strategy());
        let err = v
            .execute("place_order", json!({ "symbol": "TSLA", "quantity": 1, "price": 100 }))
            .await;
        assert!(matches!(err, Err(TraderError::SafetyRejection(_))));
    }

    #[tokio::test]
    async fn rejects_oversized_trade() {
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &strategy());
        let err = v
            .execute("place_order", json!({ "symbol": "AAPL", "quantity": 10, "price": 100 }))
            .await;
        assert!(matches!(err, Err(TraderError::SafetyRejection(_))));
    }

    #[tokio::test]
    async fn allows_valid_trade_when_live() {
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &strategy());
        let out = v
            .execute("place_order", json!({ "symbol": "AAPL", "quantity": 1, "price": 100 }))
            .await
            .unwrap();
        assert_eq!(out["state"], json!("filled"));
    }

    #[tokio::test]
    async fn rejects_sell_of_unowned_symbol() {
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &strategy());
        // Seed the portfolio view with cash so it's considered "known".
        {
            let mut view = v.portfolio.write().await;
            view.cash = Some(1000.0);
            // AAPL is not added — simulates having no position in it.
        }
        let err = v
            .execute(
                "place_order",
                json!({ "symbol": "AAPL", "side": "sell", "quantity": 1, "price": 100 }),
            )
            .await;
        assert!(matches!(err, Err(TraderError::SafetyRejection(_))));
    }

    #[tokio::test]
    async fn allows_sell_of_owned_symbol() {
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &strategy());
        {
            let mut view = v.portfolio.write().await;
            view.cash = Some(500.0);
            view.position_value.insert("AAPL".into(), 100.0);
        }
        let out = v
            .execute(
                "place_order",
                json!({ "symbol": "AAPL", "side": "sell", "quantity": 1, "price": 100 }),
            )
            .await
            .unwrap();
        assert_eq!(out["state"], json!("filled"));
    }

    #[tokio::test]
    async fn allows_sell_when_portfolio_unknown() {
        // If we haven't observed the portfolio yet, don't block sells.
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &strategy());
        let out = v
            .execute(
                "place_order",
                json!({ "symbol": "AAPL", "side": "sell", "quantity": 1, "price": 100 }),
            )
            .await
            .unwrap();
        assert_eq!(out["state"], json!("filled"));
    }

    #[tokio::test]
    async fn empty_watchlist_allows_any_symbol() {
        let mut s = strategy();
        s.watchlist = vec![];
        let v = SafetyValidator::new(Arc::new(NoopInner), risk(false), &s);
        let out = v
            .execute("place_order", json!({ "symbol": "TSLA", "quantity": 1, "price": 100 }))
            .await
            .unwrap();
        assert_eq!(out["state"], json!("filled"));
    }
}
