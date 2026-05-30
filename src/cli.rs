//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// LLM-driven Robinhood agentic trading agent.
#[derive(Parser, Debug)]
#[command(name = "trader", version, about)]
pub struct Cli {
    /// Path to the strategy/config YAML file.
    #[arg(short, long, default_value = "config/strategy.yaml", env = "TRADER_CONFIG")]
    pub config: PathBuf,

    /// Force dry-run regardless of the config setting.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run the scheduled trading loop (headless).
    Run {
        /// Launch the live TUI dashboard.
        #[arg(long)]
        tui: bool,
    },
    /// Execute a single trading cycle and exit.
    Once,
    /// Launch the TUI dashboard with the scheduled trading loop.
    Tui,
    /// Paper-trade with real market data against a virtual portfolio.
    Simulate {
        /// Run one cycle and exit.
        #[arg(long)]
        once: bool,
        /// Launch the TUI dashboard.
        #[arg(long)]
        tui: bool,
        /// Print the virtual portfolio and exit.
        #[arg(long)]
        status: bool,
        /// Reset the virtual portfolio to the starting cash and exit.
        #[arg(long)]
        reset: bool,
    },
    /// List the tools advertised by the Robinhood MCP server.
    Tools,
    /// Print the current portfolio via the MCP server.
    Portfolio,
    /// Print quotes for the watchlist symbols.
    Quotes,
    /// Verify the MCP connection and exit.
    Auth,
}
