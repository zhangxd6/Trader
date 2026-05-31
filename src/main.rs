//! LLM-driven Robinhood agentic trading agent — binary entry point.

mod agent;
mod audit;
mod cli;
mod config;
mod error;
mod llm;
mod mcp;
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
        } => {
            cmd_simulate(&config, *once, *tui, *status, *reset).await
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

    if status {
        let p = SimulatedPortfolio::load_or_init(path, config.simulation.starting_cash)?;
        print_sim_status(&p);
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

    // One agent per strategy, each with its own SafetyValidator (own watchlist).
    let agents: Vec<Arc<TradingAgent>> = config
        .strategies
        .iter()
        .map(|strategy| {
            let validator: Arc<dyn ToolExecutor> = Arc::new(SafetyValidator::new(
                inner.clone(),
                config.risk.clone(),
                strategy,
            ));
            Arc::new(TradingAgent::new(
                llm.clone(),
                validator,
                tools.clone(),
                strategy.clone(),
                mode,
                dry_run,
                audit.clone(),
                agent_events.clone(),
                sim_portfolio.clone(),
            ))
        })
        .collect();

    match style {
        RunStyle::Once => {
            for agent in &agents {
                agent.run_cycle().await?;
            }
            println!("Cycle complete. See {} for the audit trail.", config.audit.log_file);
        }
        RunStyle::Loop => {
            println!(
                "Starting trading loop ({}, {} {}, every {} min). Ctrl-C to stop.",
                mode.label(),
                agents.len(),
                if agents.len() == 1 { "strategy" } else { "strategies" },
                config.scheduler.interval_minutes
            );
            let simulate = matches!(mode, RunMode::Simulate);
            scheduler::run_trading_loop(agents, config.scheduler.interval_minutes, simulate, None).await;
        }
        RunStyle::Tui => {
            let app = App::new(config.strategies.clone(), mode);
            let interval = config.scheduler.interval_minutes;
            let simulate = matches!(mode, RunMode::Simulate);
            // The scheduler loop shares the same sender so the TUI also sees
            // market-status and next-cycle updates.
            let loop_tx = events_tx.clone();
            let handle = tokio::spawn(async move {
                scheduler::run_trading_loop(agents, interval, simulate, Some(loop_tx)).await;
            });
            // Drop our spare sender so the channel closes once the loop ends.
            drop(events_tx);
            tui::run_tui(app, events_rx).await?;
            handle.abort();
        }
    }
    Ok(())
}

fn print_sim_status(p: &SimulatedPortfolio) {
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
    println!("\nRecent trades:");
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
