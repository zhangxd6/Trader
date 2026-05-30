//! US equity market-hours determination (regular session, ET).

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc, Weekday};
use chrono_tz::America::New_York;

/// Returns true if `now` falls within the regular NYSE/Nasdaq session
/// (Mon–Fri, 09:30–16:00 America/New_York).
///
/// This does not account for market holidays or early closes; the live broker
/// will reject out-of-session orders, and simulation simply trades on the
/// observed prices.
pub fn is_market_open(now: DateTime<Utc>) -> bool {
    let et = now.with_timezone(&New_York);
    if matches!(et.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let minutes = et.hour() as i32 * 60 + et.minute() as i32;
    let open = 9 * 60 + 30;
    let close = 16 * 60;
    (open..close).contains(&minutes)
}

use chrono::Timelike;

/// The UTC instant of the next interval boundary `interval_minutes` from now.
pub fn next_interval(interval_minutes: u64) -> DateTime<Utc> {
    Utc::now() + Duration::minutes(interval_minutes as i64)
}

/// Helper for constructing an ET instant in tests.
#[allow(dead_code)]
fn et(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
    New_York
        .with_ymd_and_hms(year, month, day, hour, min, 0)
        .single()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_during_session_on_weekday() {
        // Wednesday 2026-05-27, 10:00 ET.
        assert!(is_market_open(et(2026, 5, 27, 10, 0)));
    }

    #[test]
    fn closed_before_open() {
        assert!(!is_market_open(et(2026, 5, 27, 9, 0)));
    }

    #[test]
    fn closed_after_close() {
        assert!(!is_market_open(et(2026, 5, 27, 16, 1)));
    }

    #[test]
    fn closed_on_weekend() {
        // Saturday 2026-05-30.
        assert!(!is_market_open(et(2026, 5, 30, 12, 0)));
    }
}
