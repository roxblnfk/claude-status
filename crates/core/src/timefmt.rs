//! Local-time formatting and human-readable durations.

use std::sync::OnceLock;

use time::{OffsetDateTime, UtcOffset};

use crate::tr_args;

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Local offset from UTC.
///
/// `time` refuses to determine it once the process has more than one thread, so
/// the value is computed once and cached. Call [`init_local_offset`] early in
/// `main`, before the GUI starts — otherwise the offset stays at UTC.
pub fn local_offset() -> UtcOffset {
    *LOCAL_OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

/// Determines and caches the local time zone. Call before spawning threads.
pub fn init_local_offset() {
    let _ = local_offset();
}

fn local(ts: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(ts)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .to_offset(local_offset())
}

/// Current time, unix seconds.
pub fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

/// `18:40`
pub fn clock(ts: i64) -> String {
    let t = local(ts);
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// `12.08`
pub fn date(ts: i64) -> String {
    let t = local(ts);
    format!("{:02}.{:02}", t.day(), t.month() as u8)
}

/// `12.08 18:40`
pub fn datetime(ts: i64) -> String {
    format!("{} {}", date(ts), clock(ts))
}

/// `2026-08-09` — the day key used by aggregates.
pub fn day_key(ts: i64) -> String {
    let t = local(ts);
    format!("{:04}-{:02}-{:02}", t.year(), t.month() as u8, t.day())
}

/// Start of the local day containing `ts`.
pub fn start_of_local_day(ts: i64) -> i64 {
    let t = local(ts);
    let seconds_into_day = t.hour() as i64 * 3600 + t.minute() as i64 * 60 + t.second() as i64;
    ts - seconds_into_day
}

/// End of the local day containing `ts` (midnight of the next day).
pub fn end_of_local_day(ts: i64) -> i64 {
    start_of_local_day(ts) + 86_400
}

/// Parses a `YYYY-MM-DD` key into days since the Unix epoch.
///
/// Daily aggregates from Claude Code are keyed by date string; plots need a
/// number on the X axis, and days-since-epoch keeps consecutive days one unit
/// apart across month and year boundaries.
pub fn parse_day_key(key: &str) -> Option<i64> {
    let mut parts = key.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    let date = time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    Some(date.to_julian_day() as i64 - UNIX_EPOCH_JULIAN_DAY)
}

/// Julian day number of 1970-01-01. `time` does not expose it as a constant;
/// a test pins it against the crate's own conversion.
const UNIX_EPOCH_JULIAN_DAY: i64 = 2_440_588;

/// Formats days since the Unix epoch as `12.08`.
pub fn format_day_number(days: i64) -> String {
    let julian = days + UNIX_EPOCH_JULIAN_DAY;
    match i32::try_from(julian).ok().and_then(|j| time::Date::from_julian_day(j).ok()) {
        Some(date) => format!("{:02}.{:02}", date.day(), date.month() as u8),
        None => String::new(),
    }
}

/// Days since the Unix epoch for the local day containing `ts`.
///
/// Derived from the local calendar date rather than by dividing the timestamp:
/// east of Greenwich the local day starts before midnight UTC, and the division
/// would land on the previous day.
pub fn day_number(ts: i64) -> i64 {
    local(ts).date().to_julian_day() as i64 - UNIX_EPOCH_JULIAN_DAY
}

/// Compact duration: `3d 4h`, `2h 13m`, `45m`, `30s`.
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3600;
    let minutes = (secs % 3600) / 60;

    if days > 0 {
        tr_args(
            "time.days_hours",
            &[("days", &days.to_string()), ("hours", &hours.to_string())],
        )
    } else if hours > 0 {
        tr_args(
            "time.hours_minutes",
            &[("hours", &hours.to_string()), ("minutes", &format!("{minutes:02}"))],
        )
    } else if minutes > 0 {
        tr_args("time.minutes", &[("minutes", &minutes.to_string())])
    } else {
        tr_args("time.seconds", &[("seconds", &secs.to_string())])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{self, Language};

    #[test]
    fn duration_picks_the_right_granularity() {
        let _guard = i18n::test_guard(Language::En);
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(-5), "0s", "a negative interval is never shown");
        assert_eq!(duration(30), "30s");
        assert_eq!(duration(45 * 60), "45m");
        assert_eq!(duration(2 * 3600 + 13 * 60), "2h 13m");
        assert_eq!(duration(3 * 86_400 + 4 * 3600), "3d 4h");
    }

    #[test]
    fn duration_is_translated() {
        let _guard = i18n::test_guard(Language::Ru);
        assert_eq!(duration(3 * 86_400 + 4 * 3600), "3д 4ч");
    }

    #[test]
    fn day_boundaries_span_exactly_24h() {
        let ts = 1_770_000_000;
        let start = start_of_local_day(ts);
        assert_eq!(end_of_local_day(ts) - start, 86_400);
        assert!(start <= ts && ts < end_of_local_day(ts));
    }

    #[test]
    fn epoch_julian_day_constant_is_correct() {
        let epoch = time::Date::from_calendar_date(1970, time::Month::January, 1).unwrap();
        assert_eq!(epoch.to_julian_day() as i64, UNIX_EPOCH_JULIAN_DAY);
    }

    #[test]
    fn day_keys_round_trip() {
        assert_eq!(parse_day_key("1970-01-01"), Some(0));
        assert_eq!(parse_day_key("1970-01-02"), Some(1));

        // Consecutive days stay one apart across a month and a year boundary.
        let dec = parse_day_key("2025-12-31").unwrap();
        let jan = parse_day_key("2026-01-01").unwrap();
        assert_eq!(jan - dec, 1);

        assert_eq!(format_day_number(jan), "01.01");
        assert_eq!(format_day_number(dec), "31.12");
    }

    #[test]
    fn malformed_day_keys_are_rejected() {
        for key in ["", "2026", "2026-08", "2026-13-01", "2026-08-99", "not-a-date", "2026-08-09-1"]
        {
            assert_eq!(parse_day_key(key), None, "{key}");
        }
    }

    #[test]
    fn day_number_matches_the_parsed_key() {
        let ts = 1_770_000_000;
        assert_eq!(parse_day_key(&day_key(ts)), Some(day_number(ts)));
    }

    #[test]
    fn clock_and_date_are_stable_at_the_epoch_in_utc() {
        // Checks the format rather than the zone: CI may sit at any offset.
        let s = clock(0);
        assert_eq!(s.len(), 5, "{s}");
        assert_eq!(&s[2..3], ":");
        assert_eq!(day_key(0).len(), 10);
    }
}
