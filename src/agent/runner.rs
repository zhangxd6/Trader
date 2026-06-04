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
    full_conversation: bool,
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
        full_conversation: bool,
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
            full_conversation,
            audit,
            events,
            sim_portfolio,
        }
    }

    pub fn strategy_name(&self) -> &str {
        &self.strategy.name
    }

    /// Run one decision cycle: prompt the LLM, let it call tools through the
    /// safety layer, then audit and report the result.
    pub async fn run_cycle(&self) -> Result<()> {
        let cycle_id = Uuid::new_v4();
        self.emit(LogLevel::Cycle, format!("starting cycle {cycle_id}"))
            .await;
        self.send(AppEvent::CycleStarted).await;

        let system_prompt: String = self.build_system_prompt();
        let user_message: String = self.build_user_message();

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
                // Simulated orders were actually executed (in the virtual portfolio);
                // only true dry-run blocks and safety rejections are Safety-level.
                let was_executed = call.result
                    .get("simulated")
                    .and_then(|v| v.as_bool())
                    == Some(true)
                    || !call.intercepted;
                if was_executed { LogLevel::Order } else { LogLevel::Safety }
            } else {
                LogLevel::Mcp
            };
            let msg = if self.executor.is_order_tool(&call.tool) {
                order_log_message(&call.tool, &call.arguments, &call.result)
            } else {
                format!("{} {}", call.tool, summarize(&call.arguments))
            };
            self.emit(level, msg).await;
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

        // Record an equity-curve snapshot after every cycle.
        if let Some(handle) = &self.sim_portfolio {
            handle.write().await.record_snapshot();
        }

        // Refresh the TUI portfolio panel.
        if let Some(snapshot) = self.portfolio_snapshot().await {
            self.send(AppEvent::PortfolioUpdate(snapshot)).await;
        }

        // Persist the audit entry.
        let conversation = if self.full_conversation {
            Some(result.conversation.clone())
        } else {
            None
        };
        let entry = AuditEntry {
            cycle_id,
            timestamp: Utc::now(),
            strategy_name: self.strategy.name.clone(),
            mode: self.mode.label().to_string(),
            dry_run: self.dry_run,
            system_prompt: system_prompt.clone(),
            user_message: user_message.clone(),
            final_response: result.final_response.clone(),
            iterations: result.iterations,
            tool_calls: result.tool_calls_made,
            orders_attempted: result.orders_attempted,
            conversation,
        };
        self.audit.log(&entry).await?;

        let display_reasoning = if !result.full_reasoning.is_empty() {
            &result.full_reasoning
        } else {
            &result.final_response
        };
        if !display_reasoning.is_empty() {
            self.emit(LogLevel::Llm, truncate(&result.final_response, 120))
                .await;
            // Send full reasoning to TUI panel, or print to stdout in headless mode.
            if self.events.is_some() {
                self.send(AppEvent::Reasoning(display_reasoning.to_string())).await;
            } else {
                println!("\n─── REASONING ───────────────────────────────────────");
                println!("{display_reasoning}");
                println!("─────────────────────────────────────────────────────\n");
            }
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
             - Stop-loss:     SELL a position only if its current market price (from live quotes)\n\
               is confirmed to be more than {stop:.1}% below your average cost. Do NOT sell\n\
               based on assumed or missing price data.\n\
             - Take-profit:   SELL a position only if current market price is confirmed to be\n\
               more than {take:.1}% above your average cost.\n\
             - Max positions: {maxpos} simultaneous holdings\n\
             - Min confidence: {minconf:.2} — if you are not confident, HOLD.\n\
             - Buy filters:   {filters}\n\
             - Per-trade cap and position-size caps are enforced; oversized orders are rejected.\n\n\
             === JUDGMENT RULES (apply your reasoning) ===\n{judgment}\n\n\
             === INSTRUCTIONS ===\n\
             - {scope_instruction}\n\
             {industry_instruction}\
             - TOOL CALL BUDGET: you have at most 8 tool calls this cycle. Plan ahead:\n\
               1. get_portfolio (1 call)\n\
               2. get_equity_quotes or get_stock_fundamentals for each open position AND candidate (1-3 calls)\n\
               3. Place one order only if rules clearly support it, otherwise HOLD (0-1 calls)\n\
               Do NOT call the same tool twice with the same arguments.\n\
             - ALWAYS fetch live quotes for any open position before evaluating stop-loss or take-profit.\n\
             - When price data is unavailable or uncertain, default to HOLD.\n\
             - HOLD is always acceptable; only trade when the rules clearly and confidently support it.\n\
             - When you place an order, include the symbol, quantity, and (if known) price.\n\
             - When finished, STOP calling tools and write your summary in 2-4 sentences.",
            mode = self.mode.label(),
            name = self.strategy.name,
            description = self.strategy.description,
            stop = s.stop_loss_pct,
            take = s.take_profit_pct,
            maxpos = s.max_positions,
            minconf = s.min_confidence,
            filters = format_buy_filters(&s.buy_filters),
            judgment = judgment,
            scope_instruction = format_scope_instruction(&self.strategy.watchlist),
            industry_instruction = format_industry_instruction(&self.strategy.industries),
        )
    }

    fn build_user_message(&self) -> String {
        let scope = match (
            self.strategy.watchlist.is_empty(),
            self.strategy.industries.is_empty(),
        ) {
            (false, _) => format!("the watchlist {:?}", self.strategy.watchlist),
            (true, false) => format!(
                "stocks in the {} sector(s) — use the available tools to discover candidates",
                self.strategy.industries.join(", ")
            ),
            (true, true) => "any symbols you deem appropriate".to_string(),
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
                .map(|pos| {
                    // No live price available between cycles; use avg_cost as proxy.
                    let cost_total = pos.avg_cost * pos.quantity;
                    PositionRow {
                        symbol: pos.symbol.clone(),
                        quantity: pos.quantity,
                        avg_cost: pos.avg_cost,
                        current_price: pos.avg_cost,
                        gain_usd: 0.0,
                        pnl_pct: if cost_total > 0.0 { 0.0 } else { 0.0 },
                    }
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

/// How the LLM should select trade candidates (watchlist vs. open universe).
fn format_scope_instruction(watchlist: &[String]) -> String {
    if watchlist.is_empty() {
        "You may trade any symbol available through the tools.".to_string()
    } else {
        format!("Only trade symbols on the watchlist: {watchlist:?}")
    }
}

/// Extra instruction injected when the strategy targets specific industries.
fn format_industry_instruction(industries: &[String]) -> String {
    if industries.is_empty() {
        return String::new();
    }
    format!(
        "- Your focus sectors are: {}. \
         Use the available search and discovery tools (e.g. search_stocks, \
         get_sector_stocks, or similar) to identify strong candidates within \
         these industries before selecting which to trade.\n",
        industries.join(", ")
    )
}

/// Format the buy filters into a one-line human summary.
fn format_buy_filters(f: &BuyFilters) -> String {
    let mut parts = Vec::new();
    if let Some(pe) = f.max_pe_ratio {
        parts.push(format!("P/E < {pe}"));
    }
    if let Some(h) = f.max_price_vs_52w_high_pct {
        let min_pct_below = 100.0 - h;
        parts.push(format!(
            "pct_from_52w_high >= {min_pct_below:.1}% \
             (i.e. price must be at least {min_pct_below:.1}% BELOW the 52-week high; \
             get_stock_fundamentals returns pct_from_52w_high directly — use that field, \
             do NOT recompute it)"
        ));
    }
    if let Some(v) = f.min_volume_ratio {
        parts.push(format!(
            "volume_ratio >= {v:.2} \
             (get_stock_fundamentals returns volume_ratio directly — use that field)"
        ));
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
                    let quantity = first_f64(item, &["quantity", "qty", "shares"]).unwrap_or(0.0);
                    let avg_cost = first_f64(item, &["average_buy_price", "avg_cost", "cost_basis", "average_price"]).unwrap_or(0.0);
                    let current_price = first_f64(item, &["current_price", "price", "last_price"]).unwrap_or(avg_cost);
                    let gain_usd = (current_price - avg_cost) * quantity;
                    let pnl_pct = if avg_cost > 0.0 { (current_price - avg_cost) / avg_cost * 100.0 } else { 0.0 };
                    Some(PositionRow {
                        symbol,
                        quantity,
                        avg_cost,
                        current_price,
                        gain_usd,
                        pnl_pct,
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

/// Rich one-line message for an order tool call, e.g. "BUY ANET $125 (simulated)".
fn order_log_message(tool: &str, args: &Value, result: &Value) -> String {
    let side = args.get("side").and_then(Value::as_str)
        .or_else(|| if tool.to_lowercase().contains("buy") { Some("buy") }
                    else if tool.to_lowercase().contains("sell") { Some("sell") }
                    else { None })
        .unwrap_or("order")
        .to_uppercase();

    let symbol = args.get("symbol").and_then(Value::as_str).unwrap_or("?");

    let amount = args.get("dollar_amount").and_then(Value::as_str)
        .map(|s| format!("${s}"))
        .or_else(|| args.get("quantity").and_then(Value::as_f64).map(|q| format!("{q:.4} sh")))
        .unwrap_or_default();

    let status = if result.get("simulated").and_then(Value::as_bool) == Some(true) {
        let price = result.get("price").and_then(Value::as_f64)
            .map(|p| format!(" @ ${p:.2}"))
            .unwrap_or_default();
        format!("simulated{price}")
    } else if result.get("status").and_then(Value::as_str) == Some("dry_run") {
        "dry-run blocked".to_string()
    } else if result.get("error").is_some() {
        format!("rejected: {}", truncate(&result["error"].to_string(), 40))
    } else {
        "placed".to_string()
    };

    format!("{side} {symbol} {amount} ({status})")
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
