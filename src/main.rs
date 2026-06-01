//! LLM-driven Robinhood agentic trading agent — binary entry point.

mod agent;
mod audit;
mod cli;
mod config;
mod error;
mod llm;
mod mcp;
mod research;
mod safety;
mod scheduler;
mod simulation;
mod tui;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use serde_json::json;
use tokio::sync::{mpsc, RwLock};
use tracing_subscriber::EnvFilter;

use crate::audit::AuditLogger;
use crate::agent::{LiveExecutor, TradingAgent};
use crate::cli::{Cli, Command};
use crate::research::ResearchExecutor;
use crate::config::AppConfig;
use crate::llm::ToolExecutor;
use crate::mcp::{McpTool, RobinhoodMcpClient};
use crate::safety::SafetyValidator;
use crate::simulation::{SimulatedPortfolio, SimulationExecutor};
use crate::tui::{App, AppEvent, RunMode};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();
    let _guard = init_tracing(cli.verbose);

    let config = AppConfig::load(&cli.config)
        .with_context(|| format!("loading config from {}", cli.config.display()))?;

    match &cli.command {
        Command::Auth => cmd_auth(&config).await,
        Command::Tools => cmd_tools(&config).await,
        Command::Portfolio => cmd_portfolio(&config).await,
        Command::Quotes => cmd_quotes(&config).await,
        Command::Research { symbols, query, news_items } => {
            cmd_research(&config, symbols.clone(), query.clone(), *news_items).await
        }
        Command::Once => run_live(&config, cli.dry_run, RunStyle::Once).await,
        Command::Run { tui } => {
            let style = if *tui { RunStyle::Tui } else { RunStyle::Loop };
            run_live(&config, cli.dry_run, style).await
        }
        Command::Tui => run_live(&config, cli.dry_run, RunStyle::Tui).await,
        Command::Simulate {
            once,
            tui,
            status,
            reset,
            chart,
            csv,
        } => {
            cmd_simulate(&config, *once, *tui, *status, *reset, *chart, csv.as_deref()).await
        }
    }
}

/// How a live/dry-run session should execute.
enum RunStyle {
    Once,
    Loop,
    Tui,
}

/// Initialise tracing to a daily-rolling file (never stdout, to keep the TUI
/// clean). Returns the worker guard which must be kept alive.
fn init_tracing(verbose: u8) -> tracing_appender::non_blocking::WorkerGuard {
    let level = match verbose {
        0 => "trader=info",
        1 => "trader=debug",
        _ => "trader=trace",
    };
    let file_appender = tracing_appender::rolling::daily("./logs", "trader.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level)))
        .with_writer(non_blocking)
        .with_ansi(false)
        .json()
        .try_init();
    guard
}

/// Connect to the Robinhood MCP server.
async fn connect_mcp(config: &AppConfig) -> Result<Arc<RobinhoodMcpClient>> {
    let client = RobinhoodMcpClient::new(&config.robinhood.mcp_url, &config.robinhood.api_token)?;
    client
        .connect()
        .await
        .with_context(|| format!("connecting to MCP at {}", config.robinhood.mcp_url))?;
    Ok(Arc::new(client))
}

async fn cmd_auth(config: &AppConfig) -> Result<()> {
    let mcp = connect_mcp(config).await?;
    let tools = mcp.list_tools().await?;
    println!(
        "✓ Connected to {} — {} tools available.",
        config.robinhood.mcp_url,
        tools.len()
    );
    Ok(())
}

async fn cmd_tools(config: &AppConfig) -> Result<()> {
    let mcp = connect_mcp(config).await?;
    let tools = mcp.list_tools().await?;
    println!("Robinhood MCP tools ({}):\n", tools.len());
    for t in &tools {
        println!("  {:<28} {}", t.name, t.description);
    }
    Ok(())
}

async fn cmd_portfolio(config: &AppConfig) -> Result<()> {
    let mcp = connect_mcp(config).await?;
    let tools = mcp.list_tools().await?;
    let tool = find_tool(&tools, &["portfolio", "position", "account"])
        .context("no portfolio-like tool found on the MCP server")?;
    let result = mcp.call_tool(&tool, json!({})).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn cmd_quotes(config: &AppConfig) -> Result<()> {
    let mcp = connect_mcp(config).await?;
    let tools = mcp.list_tools().await?;
    let tool = find_tool(&tools, &["quote", "price", "market"])
        .context("no quote-like tool found on the MCP server")?;
    let symbols = config.watchlist_upper();
    // Try a few common argument shapes for the quote tool.
    let result = match mcp.call_tool(&tool, json!({ "symbols": symbols })).await {
        Ok(v) => v,
        Err(_) => {
            mcp.call_tool(&tool, json!({ "symbol": symbols.join(",") }))
                .await?
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

/// Build a live/dry-run trading agent and run it in the requested style.
async fn run_live(config: &AppConfig, force_dry: bool, style: RunStyle) -> Result<()> {
    let dry_run = config.risk.dry_run || force_dry;
    let mode = if dry_run { RunMode::DryRun } else { RunMode::Live };

    let mcp = connect_mcp(config).await?;
    let tools = mcp.list_tools().await?;
    let inner: Arc<dyn ToolExecutor> = Arc::new(LiveExecutor::new(mcp.clone()));

    drive(config, style, mode, dry_run, tools, inner, None).await
}

/// Handle the `simulate` subcommand and its flags.
async fn cmd_simulate(
    config: &AppConfig,
    once: bool,
    tui: bool,
    status: bool,
    reset: bool,
    chart: bool,
    csv: Option<&std::path::Path>,
) -> Result<()> {
    let path = std::path::Path::new(&config.simulation.portfolio_path);

    if reset {
        SimulatedPortfolio::reset(path, config.simulation.starting_cash)?;
        println!(
            "Reset virtual portfolio to ${:.2} at {}",
            config.simulation.starting_cash,
            path.display()
        );
        return Ok(());
    }

    if let Some(out) = csv {
        let p = SimulatedPortfolio::load_or_init(path, config.simulation.starting_cash)?;
        p.export_csv(out)
            .with_context(|| format!("writing CSV to {}", out.display()))?;
        println!("Exported {} snapshots to {}", p.value_history.len(), out.display());
        return Ok(());
    }

    if status {
        let p = SimulatedPortfolio::load_or_init(path, config.simulation.starting_cash)?;
        print_sim_status(&p, chart);
        return Ok(());
    }

    let portfolio = Arc::new(RwLock::new(SimulatedPortfolio::load_or_init(
        path,
        config.simulation.starting_cash,
    )?));

    let mcp = connect_mcp(config).await?;
    let tools = mcp.list_tools().await?;
    let inner: Arc<dyn ToolExecutor> =
        Arc::new(SimulationExecutor::new(mcp.clone(), portfolio.clone()));

    let style = if tui {
        RunStyle::Tui
    } else if once {
        RunStyle::Once
    } else {
        RunStyle::Loop
    };

    // Simulation always executes orders virtually; it is never a real-money dry-run.
    drive(config, style, RunMode::Simulate, false, tools, inner, Some(portfolio)).await
}

/// Shared driver: assemble one agent per strategy and run them once, in a loop,
/// or with a TUI.
async fn drive(
    config: &AppConfig,
    style: RunStyle,
    mode: RunMode,
    dry_run: bool,
    tools: Vec<McpTool>,
    inner: Arc<dyn ToolExecutor>,
    sim_portfolio: Option<Arc<RwLock<SimulatedPortfolio>>>,
) -> Result<()> {
    let audit = Arc::new(AuditLogger::open(&config.audit.log_dir, &config.audit.log_file).await?);
    let llm = llm::build_provider(&config.llm);

    let (events_tx, events_rx) = mpsc::channel::<AppEvent>(256);
    let with_tui = matches!(style, RunStyle::Tui);
    let agent_events = if with_tui { Some(events_tx.clone()) } else { None };

    let simulate = matches!(mode, RunMode::Simulate);
    let default_interval = config.scheduler.interval_minutes;

    // Wrap the inner executor with research tools (news + web search).
    let research_inner: Arc<dyn ToolExecutor> = Arc::new(ResearchExecutor::new(
        inner.clone(),
        config.research.brave_api_key.clone(),
    ));
    // Extend the tool list so the LLM knows about web_search and get_stock_news.
    let mut all_tools = tools;
    all_tools.extend(research::research_tools());

    // One agent per strategy, each with its own SafetyValidator and interval.
    let agents: Vec<(Arc<TradingAgent>, u64)> = config
        .strategies
        .iter()
        .map(|strategy| {
            let interval = strategy.interval_minutes.unwrap_or(default_interval);
            let validator: Arc<dyn ToolExecutor> = Arc::new(SafetyValidator::new(
                research_inner.clone(),
                config.risk.clone(),
                strategy,
            ));
            let agent = Arc::new(TradingAgent::new(
                llm.clone(),
                validator,
                all_tools.clone(),
                strategy.clone(),
                mode,
                dry_run,
                audit.clone(),
                agent_events.clone(),
                sim_portfolio.clone(),
            ));
            (agent, interval)
        })
        .collect();

    match style {
        RunStyle::Once => {
            for (agent, _) in &agents {
                agent.run_cycle().await?;
            }
            println!("Cycle complete. See {} for the audit trail.", config.audit.log_file);
        }
        RunStyle::Loop => {
            println!(
                "Starting trading loop ({}, {} {}). Ctrl-C to stop.",
                mode.label(),
                agents.len(),
                if agents.len() == 1 { "strategy" } else { "strategies" },
            );
            for (agent, interval) in &agents {
                println!("  • {} — every {} min", agent.strategy_name(), interval);
            }
            // Spawn one independent loop per strategy.
            let handles: Vec<_> = agents
                .into_iter()
                .map(|(agent, interval)| {
                    tokio::spawn(async move {
                        scheduler::run_trading_loop(vec![agent], interval, simulate, None).await;
                    })
                })
                .collect();
            for h in handles {
                let _ = h.await;
            }
        }
        RunStyle::Tui => {
            let app = App::new(config.strategies.clone(), mode);
            // Spawn one independent loop per strategy; all share the event sender.
            let mut loop_handles = Vec::new();
            for (agent, interval) in agents {
                let tx = events_tx.clone();
                loop_handles.push(tokio::spawn(async move {
                    scheduler::run_trading_loop(vec![agent], interval, simulate, Some(tx)).await;
                }));
            }
            drop(events_tx);
            tui::run_tui(app, events_rx).await?;
            for h in loop_handles {
                h.abort();
            }
        }
    }
    Ok(())
}

fn print_sim_status(p: &SimulatedPortfolio, show_chart: bool) {
    let total = p.cash_usd + p.market_value(|_| None);
    let ret = if p.starting_cash > 0.0 {
        (total - p.starting_cash) / p.starting_cash * 100.0
    } else {
        0.0
    };
    println!("=== Simulation Portfolio ===");
    println!("Started:    {}", p.created_at.format("%Y-%m-%d"));
    println!("Cash:       ${:.2}", p.cash_usd);
    println!("Positions:  {}", p.positions.len());
    for pos in p.positions.values() {
        println!(
            "  {:<6} {:.2} sh @ ${:.2}",
            pos.symbol, pos.quantity, pos.avg_cost
        );
    }
    println!("Total (cost-basis): ${total:.2}");
    println!("Return:     {ret:+.2}% vs ${:.2} start", p.starting_cash);

    if show_chart {
        println!("\n=== Equity Curve ===");
        println!("{}", p.render_chart(60));
    }

    println!("\nRecent trades:");
    if p.trade_log.is_empty() {
        println!("  (none yet)");
    }
    for t in p.trade_log.iter().rev().take(10) {
        let pnl = t
            .pnl
            .map(|v| format!(", P&L: {v:+.2}"))
            .unwrap_or_default();
        println!(
            "  {}  {:<4} {:<6} {:.2} @ ${:.2}{}",
            t.timestamp.format("%Y-%m-%d %H:%M"),
            t.side.to_uppercase(),
            t.symbol,
            t.quantity,
            t.price,
            pnl
        );
    }
}

/// Gather news + web context for the given symbols and ask the LLM to produce
/// an actionable research report. Uses a read-only executor chain (no safety
/// validator) since no orders are placed.
async fn cmd_research(
    config: &AppConfig,
    symbols: Vec<String>,
    query: Option<String>,
    news_items: usize,
) -> Result<()> {
    let mcp = connect_mcp(config).await?;
    let mcp_tools = mcp.list_tools().await?;

    let live: Arc<dyn ToolExecutor> = Arc::new(LiveExecutor::new(mcp));
    let executor: Arc<dyn ToolExecutor> = Arc::new(ResearchExecutor::new(
        live,
        config.research.brave_api_key.clone(),
    ));

    let mut tools = mcp_tools;
    tools.extend(research::research_tools());

    let llm = llm::build_provider(&config.llm);

    let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
    let symbols_list = if symbols_upper.is_empty() {
        "any relevant stocks".to_string()
    } else {
        symbols_upper.join(", ")
    };
    let search_query = query.unwrap_or_else(|| {
        if symbols_upper.is_empty() {
            "stock market news today".to_string()
        } else {
            format!("{} stock news analysis outlook", symbols_upper.join(" "))
        }
    });

    let system_prompt = "You are a financial research analyst. Gather information using the \
        available tools, then produce a concise, actionable research report. \
        Focus on facts and clear trading implications. Do not place any orders."
        .to_string();

    let user_message = format!(
        "Research these symbols: [{symbols_list}]. \
         Search context: \"{search_query}\".\n\n\
         Steps:\n\
         1. For each symbol, call `get_stock_news` (max_items: {news_items}).\n\
         2. Call `web_search` with \"{search_query}\" (max_results: 5).\n\
         3. Fetch current quotes via the MCP quote tool.\n\
         4. Write a 300-500 word report covering: price action, news sentiment, \
         key themes, trading implication (bullish/bearish/neutral), risks and \
         upcoming catalysts."
    );

    println!("Researching: {symbols_list}");
    println!("Query: {search_query}\n");

    let result = llm
        .run_agent_loop(&system_prompt, &user_message, &tools, executor.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("═══════════════════ Research Report ═══════════════════\n");
    println!("{}", result.final_response);
    println!("\n═══════════════════════════════════════════════════════");
    println!("({} tool calls, {} iterations)", result.tool_calls_made.len(), result.iterations);
    Ok(())
}

/// Find a tool whose name contains any of the keywords.
fn find_tool(tools: &[McpTool], keywords: &[&str]) -> Option<String> {
    tools
        .iter()
        .find(|t| {
            let n = t.name.to_lowercase();
            keywords.iter().any(|k| n.contains(k))
        })
        .map(|t| t.name.clone())
}
