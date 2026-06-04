//! Yahoo Finance fundamentals fetcher.
//!
//! Uses the v8 chart endpoint (no API key or crumb required) to retrieve
//! 52-week range, current price, average volume, and related metrics.

use reqwest::Client;
use serde_json::{json, Value};

use crate::error::{Result, TraderError};

const CHART_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Fetch key fundamentals for `symbol` from Yahoo Finance.
///
/// Returns a JSON object with:
/// - `symbol`, `current_price`
/// - `52w_high`, `52w_low`
/// - `pct_from_52w_high` — how far below the 52-week high the price is (positive = below)
/// - `avg_volume`, `volume_today`, `volume_ratio`
pub async fn get_stock_fundamentals(client: &Client, symbol: &str) -> Result<Value> {
    let url = format!(
        "{}/{}?interval=1d&range=1y&includePrePost=false",
        CHART_URL,
        symbol.to_uppercase()
    );

    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (compatible; trader-bot/1.0)")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| TraderError::Http(format!("Yahoo Finance request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(TraderError::Http(format!(
            "Yahoo Finance returned HTTP {}",
            resp.status()
        )));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| TraderError::Http(format!("Yahoo Finance JSON parse error: {e}")))?;

    let meta = body
        .pointer("/chart/result/0/meta")
        .ok_or_else(|| TraderError::Http(format!("no chart result for {symbol}")))?;

    let current_price = meta.get("regularMarketPrice").and_then(Value::as_f64).unwrap_or(0.0);
    let week52_high   = meta.get("fiftyTwoWeekHigh").and_then(Value::as_f64).unwrap_or(0.0);
    let week52_low    = meta.get("fiftyTwoWeekLow").and_then(Value::as_f64).unwrap_or(0.0);

    // Compute 52w high/low from actual OHLCV data if meta fields are missing.
    let (week52_high, week52_low) = if week52_high > 0.0 {
        (week52_high, week52_low)
    } else {
        let indicators = body.pointer("/chart/result/0/indicators/quote/0");
        let highs = indicators.and_then(|i| i.get("high")).and_then(Value::as_array);
        let lows  = indicators.and_then(|i| i.get("low")).and_then(Value::as_array);
        let h = highs.map(|arr| arr.iter().filter_map(Value::as_f64).fold(f64::NEG_INFINITY, f64::max)).unwrap_or(0.0);
        let l = lows.map(|arr| arr.iter().filter_map(Value::as_f64).fold(f64::INFINITY, f64::min)).unwrap_or(0.0);
        (h, l)
    };

    let pct_from_52w_high = if week52_high > 0.0 && current_price > 0.0 {
        (week52_high - current_price) / week52_high * 100.0
    } else {
        0.0
    };

    let avg_volume   = meta.get("regularMarketDayVolume").and_then(Value::as_f64).unwrap_or(0.0);
    let volume_today = avg_volume; // best available without a separate quote call

    // Compute true average volume from the daily series.
    let daily_volumes: Vec<f64> = body
        .pointer("/chart/result/0/indicators/quote/0/volume")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let avg_vol_1y = if daily_volumes.is_empty() {
        avg_volume
    } else {
        daily_volumes.iter().sum::<f64>() / daily_volumes.len() as f64
    };
    let volume_ratio = if avg_vol_1y > 0.0 { volume_today / avg_vol_1y } else { 0.0 };

    Ok(json!({
        "symbol": symbol.to_uppercase(),
        "current_price": round2(current_price),
        "52w_high": round2(week52_high),
        "52w_low": round2(week52_low),
        "pct_from_52w_high": round2(pct_from_52w_high),
        "pct_from_52w_high_meaning": format!(
            "current price is {:.2}% BELOW the 52-week high — larger = deeper dip. \
             Compare this number directly against the buy filter threshold (no arithmetic needed).",
            round2(pct_from_52w_high)
        ),
        "avg_volume_1y": avg_vol_1y as u64,
        "volume_today": volume_today as u64,
        "volume_ratio": round2(volume_ratio),
        "volume_ratio_meaning": format!(
            "today's volume is {:.2}x the 1-year daily average — compare directly against filter.",
            round2(volume_ratio)
        ),
    }))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
