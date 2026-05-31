//! Periodic trading loop with a market-hours guard.

mod market_hours;

pub use market_hours::{is_market_open, next_interval};

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::agent::TradingAgent;
use crate::tui::AppEvent;

/// Run the trading loop forever: tick every `interval_minutes`, and on each
/// tick run all agents sequentially if the US equity market is open.
///
/// Pass `simulate = true` to skip the market-hours gate (useful for
/// paper-trading outside regular session hours).
///
/// If `events` is provided, market-status, next-cycle, and per-strategy
/// updates are forwarded to the TUI. Cycle errors are logged but never
/// terminate the loop.
pub async fn run_trading_loop(
    agents: Vec<Arc<TradingAgent>>,
    interval_minutes: u64,
    simulate: bool,
    events: Option<mpsc::Sender<AppEvent>>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_minutes * 60));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        ticker.tick().await;
        let open = simulate || is_market_open(chrono::Utc::now());
        if let Some(tx) = &events {
            let _ = tx.send(AppEvent::MarketStatus(open)).await;
            let _ = tx
                .send(AppEvent::NextCycle(next_interval(interval_minutes)))
                .await;
        }

        if !open {
            tracing::debug!("market closed; skipping cycle");
            continue;
        }

        for (idx, agent) in agents.iter().enumerate() {
            if let Some(tx) = &events {
                let _ = tx.send(AppEvent::StrategyActive(idx)).await;
            }
            if let Err(e) = agent.run_cycle().await {
                tracing::error!(error = %e, "trading cycle failed");
                if let Some(tx) = &events {
                    let _ = tx.send(AppEvent::Error(e.to_string())).await;
                }
            }
        }
    }
}
