//! The [`TradingAgent`] runs one full decision cycle and records the outcome.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::audit::{AuditEntry, AuditLogger};
use crate::config::{BuyFilters, StrategyConfig};
use crate::error::Result;
use crate::llm::{DynLlmProvider, ToolExecutor};
use crate::mcp::{McpTool, RobinhoodMcpClient};
use crate::simulation::SimulatedPortfolio;
use crate::tui::{AppEvent, LogEntry, LogLevel, PortfolioSnapshot, PositionRow, RunMode};

/// Thin [`ToolExecutor`] that forwards every call to the live MCP server.
pub struct LiveExecutor {
    mcp: Arc<RobinhoodMcpClient>,
}

impl LiveExecutor {
    pub fn new(mcp: Arc<RobinhoodMcpClient>) -> Self {
        Self { mcp }
    }
}

#[async_trait]
impl ToolExecutor for LiveExecutor {
    async fn execute(&self, name: &str, args: Value) -> Result<Value> {
        self.mcp.call_tool(name, args).await
    }
}

/// Orchestrates a single trading cycle end-to-end.
pub struct TradingAgent {
    llm: DynLlmProvider,
    executor: Arc<dyn ToolExecutor>,
    tools: Vec<McpTool>,
    strategy: StrategyConfig,
    mode: RunMode,
    dry_run: bool,
    audit: Arc<AuditLogger>,
    events: Option<mpsc::Sender<AppEvent>>,
    /// For simulation mode: the virtual portfolio, used to render the TUI.
    sim_portfolio: Option<Arc<RwLock<SimulatedPortfolio>>>,
}

impl TradingAgent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm: DynLlmProvider,
        executor: Arc<dyn ToolExecutor>,
        tools: Vec<McpTool>,
        strategy: StrategyConfig,
        mode: RunMode,
        dry_run: bool,
        audit: Arc<AuditLogger>,
        events: Option<mpsc::Sender<AppEvent>>,
        sim_portfolio: Option<Arc<RwLock<SimulatedPortfolio>>>,
    ) -> Self {
        Self {
            llm,
            executor,
            tools,
            strategy,
            mode,
            dry_run,
            audit,
            events,
            sim_portfolio,
        }
    }

    /// Run one decision cycle: prompt the LLM, let it call tools through the
    /// safety layer, then audit and report the result.
    pub async fn run_cycle(&self) -> Result<()> {
        let cycle_id = Uuid::new_v4();
        self.emit(LogLevel::Cycle, format!("starting cycle {cycle_id}"))
            .await;
        self.send(AppEvent::CycleStarted).await;

        let system_prompt = self.build_system_prompt();
        let user_message = self.build_user_message();

        self.emit(LogLevel::Llm, format!("querying {}", self.llm.name()))
            .await;

        let result = self
            .llm
            .run_agent_loop(
                &system_prompt,
                &user_message,
                &self.tools,
                self.executor.as_ref(),
            )
            .await?;

        // Surface each tool call and order to the TUI log.
        for call in &result.tool_calls_made {
            let level = if self.executor.is_order_tool(&call.tool) {
                // An intercepted order was blocked (dry-run) or rejected (safety).
                if call.intercepted {
                    LogLevel::Safety
                } else {
                    LogLevel::Order
                }
            } else {
                LogLevel::Mcp
            };
            self.emit(level, format!("{} {}", call.tool, summarize(&call.arguments)))
                .await;
        }
        let executed = result
            .orders_attempted
            .iter()
            .filter(|o| o.outcome == "placed")
            .count();
        let held = result.tool_calls_made.len().saturating_sub(executed);

        self.emit(
            LogLevel::Cycle,
            format!("complete — {executed} placed, {} attempted", result.orders_attempted.len()),
        )
        .await;
        self.send(AppEvent::CycleComplete { executed, held }).await;

        // Refresh the TUI portfolio panel.
        if let Some(snapshot) = self.portfolio_snapshot().await {
            self.send(AppEvent::PortfolioUpdate(snapshot)).await;
        }

        // Persist the audit entry.
        let entry = AuditEntry {
            cycle_id,
            timestamp: Utc::now(),
            strategy_name: self.strategy.name.clone(),
            mode: self.mode.label().to_string(),
            dry_run: self.dry_run,
            final_response: result.final_response.clone(),
            iterations: result.iterations,
            tool_calls: result.tool_calls_made,
            orders_attempted: result.orders_attempted,
        };
        self.audit.log(&entry).await?;

        if !result.final_response.is_empty() {
            self.emit(LogLevel::Llm, truncate(&result.final_response, 120))
                .await;
        }

        Ok(())
    }

    /// Build the system prompt from the hybrid strategy definition.
    fn build_system_prompt(&self) -> String {
        let s = &self.strategy.structured;
        let judgment = if self.strategy.rules.is_empty() {
            "  (none)".to_string()
        } else {
            self.strategy
                .rules
                .iter()
                .enumerate()
                .map(|(i, r)| format!("  {}. {}", i + 1, r))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "You are a disciplined trading agent with access to Robinhood trading tools.\n\
             Operating mode: {mode}.\n\n\
             STRATEGY: {name}\n{description}\n\n\
             === HARD RULES (enforced by the system; violations are rejected) ===\n\
             - Stop-loss:     SELL any position down more than {stop:.1}%\n\
             - Take-profit:   SELL any position up more than {take:.1}%\n\
             - Max positions: {maxpos} simultaneous holdings\n\
             - Min confidence: {minconf:.2} (do not act below this)\n\
             - Buy filters:   {filters}\n\
             - Per-trade cap and position-size caps are enforced; oversized orders are rejected.\n\n\
             === JUDGMENT RULES (apply your reasoning) ===\n{judgment}\n\n\
             === INSTRUCTIONS ===\n\
             - {watchlist_instruction}\n\
             - ALWAYS read the portfolio and fetch quotes with the available tools BEFORE ordering.\n\
             - HOLD is always acceptable; only trade when the rules clearly support it.\n\
             - When you place an order, include the symbol, quantity, and (if known) price.\n\
             - When finished, summarise what you did and why in 2-4 sentences.",
            mode = self.mode.label(),
            name = self.strategy.name,
            description = self.strategy.description,
            stop = s.stop_loss_pct,
            take = s.take_profit_pct,
            maxpos = s.max_positions,
            minconf = s.min_confidence,
            filters = format_buy_filters(&s.buy_filters),
            judgment = judgment,
            watchlist_instruction = if self.strategy.watchlist.is_empty() {
                "You may trade any symbol available through the tools. Use your judgment to select candidates.".to_string()
            } else {
                format!("Only trade symbols on the watchlist: {:?}", self.strategy.watchlist)
            },
        )
    }

    fn build_user_message(&self) -> String {
        let scope = if self.strategy.watchlist.is_empty() {
            "any symbols you deem appropriate".to_string()
        } else {
            format!("the watchlist {:?}", self.strategy.watchlist)
        };
        format!(
            "Current time: {}. Review my portfolio and {scope}, then execute the \
             trading strategy for this cycle. Apply all rules and place any warranted \
             orders using the tools.",
            Utc::now().format("%Y-%m-%d %H:%M UTC"),
        )
    }

    /// Build a portfolio snapshot for the TUI, if possible.
    async fn portfolio_snapshot(&self) -> Option<PortfolioSnapshot> {
        // Simulation: render from the virtual portfolio.
        if let Some(handle) = &self.sim_portfolio {
            let p = handle.read().await;
            let market_value = p.market_value(|_| None);
            let total = p.cash_usd + market_value;
            let positions = p
                .positions
                .values()
                .map(|pos| PositionRow {
                    symbol: pos.symbol.clone(),
                    quantity: pos.quantity,
                    price: pos.avg_cost,
                    pnl_pct: 0.0,
                })
                .collect();
            return Some(PortfolioSnapshot {
                cash: p.cash_usd,
                market_value,
                total_value: total,
                return_pct: if p.starting_cash > 0.0 {
                    (total - p.starting_cash) / p.starting_cash * 100.0
                } else {
                    0.0
                },
                positions,
            });
        }

        // Live: call a portfolio tool through the executor and parse it.
        let tool = self
            .tools
            .iter()
            .find(|t| t.name.to_lowercase().contains("portfolio"))?;
        let result = self.executor.execute(&tool.name, serde_json::json!({})).await.ok()?;
        Some(parse_portfolio_snapshot(&result))
    }

    async fn emit(&self, level: LogLevel, message: String) {
        match level {
            LogLevel::Error => tracing::error!(target: "cycle", "{message}"),
            LogLevel::Order => tracing::info!(target: "order", "{message}"),
            _ => tracing::info!(target: "cycle", "{message}"),
        }
        self.send(AppEvent::Log(LogEntry {
            timestamp: Utc::now(),
            level,
            message,
        }))
        .await;
    }

    async fn send(&self, event: AppEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event).await;
        }
    }
}

/// Format the buy filters into a one-line human summary.
fn format_buy_filters(f: &BuyFilters) -> String {
    let mut parts = Vec::new();
    if let Some(pe) = f.max_pe_ratio {
        parts.push(format!("P/E < {pe}"));
    }
    if let Some(h) = f.max_price_vs_52w_high_pct {
        parts.push(format!("price < {h}% of 52w high"));
    }
    if let Some(v) = f.min_volume_ratio {
        parts.push(format!("volume > {v}x avg"));
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

/// Best-effort extraction of a [`PortfolioSnapshot`] from a tool result.
fn parse_portfolio_snapshot(value: &Value) -> PortfolioSnapshot {
    let cash = first_f64(value, &["cash", "buying_power", "uninvested_cash"]).unwrap_or(0.0);
    let market_value = first_f64(value, &["market_value", "equity"]).unwrap_or(0.0);
    let total = first_f64(value, &["total_equity", "total_value", "account_value"])
        .unwrap_or(cash + market_value);
    let positions = value
        .get("positions")
        .or_else(|| value.get("holdings"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let symbol = item.get("symbol").and_then(Value::as_str)?.to_string();
                    Some(PositionRow {
                        symbol,
                        quantity: first_f64(item, &["quantity", "qty", "shares"]).unwrap_or(0.0),
                        price: first_f64(item, &["current_price", "price", "last_price"])
                            .unwrap_or(0.0),
                        pnl_pct: first_f64(item, &["unrealized_pnl_pct", "pnl_pct"]).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    PortfolioSnapshot {
        cash,
        market_value,
        total_value: total,
        return_pct: 0.0,
        positions,
    }
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

/// Compact one-line summary of tool arguments for the log panel.
fn summarize(args: &Value) -> String {
    let s = args.to_string();
    truncate(&s, 60)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
