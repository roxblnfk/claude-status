//! GUI state: what has been read from the database and how to label it in the tray.

use anyhow::Result;
use claude_status_core::{
    Config, Db, Sample,
    install::{self, InstallStatus},
    pace::Overview,
    stats_cache::StatsCache,
    timefmt, tr, tr_args,
};

/// The span the history is shown over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
}

impl Range {
    pub fn secs(self) -> i64 {
        match self {
            Range::Day => 86_400,
            Range::Week => 7 * 86_400,
            Range::Month => 30 * 86_400,
        }
    }

    pub fn label(self) -> String {
        match self {
            Range::Day => tr("ui.range.day"),
            Range::Week => tr("ui.range.week"),
            Range::Month => tr("ui.range.month"),
        }
    }

    pub const ALL: [Range; 3] = [Range::Day, Range::Week, Range::Month];
}

pub struct AppState {
    pub config: Config,
    pub overview: Overview,
    /// Samples over the selected span, oldest first.
    pub history: Vec<Sample>,
    pub range: Range,
    pub stats: Option<StatsCache>,
    pub install: InstallStatus,
    /// The last read error — shown in the window, never fatal.
    pub error: Option<String>,
    pub refreshed_at: i64,
}

impl AppState {
    pub fn load() -> Self {
        let mut state = Self {
            config: Config::load_and_apply_language(),
            overview: Overview::default(),
            history: Vec::new(),
            range: Range::Week,
            stats: None,
            install: InstallStatus::Absent,
            error: None,
            refreshed_at: 0,
        };
        state.refresh();
        state
    }

    /// Re-reads the database and the Claude Code settings.
    ///
    /// An error is not fatal: the database may be locked by a writing hook and
    /// the settings may be temporarily invalid. We keep the previous data on
    /// screen and show the error text.
    pub fn refresh(&mut self) {
        let now = timefmt::now();
        self.refreshed_at = now;

        match self.reload(now) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(format!("{e:#}")),
        }

        self.install = install::status().unwrap_or(InstallStatus::Absent);
    }

    fn reload(&mut self, now: i64) -> Result<()> {
        let db = Db::open_default()?;
        self.history = db.samples_between(now - self.range.secs(), now)?;

        // Deliberately not derived from `history`: the summary needs the peak
        // reading of each window and the bounds of the weekly one, neither of
        // which a fixed time span is guaranteed to contain.
        self.overview = db.overview(now)?;
        self.stats = StatsCache::load().unwrap_or(None);
        Ok(())
    }

    /// The outer ring of the icon: the session limit.
    pub fn session_gauge(&self) -> Option<f64> {
        self.overview.five_hour.and_then(|w| w.live_used_pct())
    }

    /// The inner ring: how much of today's budget is gone.
    pub fn daily_gauge(&self) -> Option<f64> {
        self.overview.daily.map(|d| d.used_pct())
    }

    /// The tray label.
    ///
    /// Windows truncates a tray tooltip at 127 characters, so this carries only
    /// the essentials; the full breakdown lives in the window.
    pub fn tooltip(&self) -> String {
        let now = timefmt::now();
        let mut lines = vec![tr("tray.tooltip.title")];

        if let Some(w) = self.overview.five_hour {
            lines.push(match w.live_used_pct() {
                Some(pct) => tr_args(
                    "tray.tooltip.five_hour",
                    &[("pct", &format!("{pct:.0}")), ("reset", &timefmt::clock(w.resets_at))],
                ),
                None => tr("tray.tooltip.five_hour_expired"),
            });
        }
        if let Some(w) = self.overview.week {
            lines.push(match w.live_used_pct() {
                Some(pct) => tr_args(
                    "tray.tooltip.week",
                    &[("pct", &format!("{pct:.0}")), ("reset", &timefmt::date(w.resets_at))],
                ),
                None => tr("tray.tooltip.week_expired"),
            });
        }
        if let Some(d) = self.overview.daily {
            lines.push(tr_args(
                "tray.tooltip.today",
                &[
                    ("spent", &format!("{:.1}", d.spent_pct)),
                    ("allowance", &format!("{:.1}", d.allowance_pct)),
                ],
            ));
        }

        match self.overview.staleness_secs(now) {
            None => lines.push(tr("tray.tooltip.no_data")),
            Some(staleness) if staleness > 3600 => {
                lines.push(tr_args("tray.tooltip.stale", &[("age", &timefmt::duration(staleness))]));
            }
            Some(_) => {}
        }

        truncate_chars(&lines.join("\n"), 127)
    }
}

/// Truncates at a character boundary rather than a byte one.
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => s[..idx].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_status_core::{pace::DailyBudget, statusline::SEVEN_DAY_SECS};

    fn overview_with(five: Option<f64>, week: Option<f64>, now: i64) -> Overview {
        let sample = Sample {
            id: 1,
            ts: now,
            last_seen_ts: now,
            five_pct: five,
            five_resets_at: five.map(|_| now + 3600),
            week_pct: week,
            week_resets_at: week.map(|_| now + SEVEN_DAY_SECS / 2),
            ..Sample::default()
        };
        Overview::from_samples(&[sample], now)
    }

    fn state_with(overview: Overview) -> AppState {
        AppState {
            config: Config::default(),
            overview,
            history: Vec::new(),
            range: Range::Week,
            stats: None,
            install: InstallStatus::Absent,
            error: None,
            refreshed_at: 0,
        }
    }

    #[test]
    fn tooltip_lists_both_windows() {
        let now = timefmt::now();
        let state = state_with(overview_with(Some(52.0), Some(41.0), now));
        let tip = state.tooltip();
        assert!(tip.contains("52%"), "{tip}");
        assert!(tip.contains("41%"), "{tip}");
        assert_eq!(tip.lines().count(), 3, "title and the two windows: {tip}");
    }

    #[test]
    fn tooltip_reports_absence_of_data() {
        let state = state_with(Overview::default());
        assert_eq!(state.tooltip().lines().count(), 2, "{}", state.tooltip());
    }

    #[test]
    fn tooltip_hides_the_percentage_of_a_window_that_has_reset() {
        let now = timefmt::now();
        // Both windows closed an hour ago: the readings describe windows that
        // no longer exist, so the figures must not be quoted as current.
        let sample = Sample {
            id: 1,
            ts: now - 7200,
            last_seen_ts: now - 7200,
            five_pct: Some(36.0),
            five_resets_at: Some(now - 3600),
            week_pct: Some(41.0),
            week_resets_at: Some(now - 3600),
            ..Sample::default()
        };
        let tip = state_with(Overview::from_samples(&[sample], now)).tooltip();
        assert!(!tip.contains("36%"), "{tip}");
        assert!(!tip.contains("41%"), "{tip}");
    }

    #[test]
    fn tooltip_fits_the_windows_limit() {
        let now = timefmt::now();
        let state = state_with(overview_with(Some(99.9), Some(99.9), now));
        assert!(state.tooltip().chars().count() <= 127);
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // Cyrillic takes two bytes per character — slicing by byte would panic.
        assert_eq!(truncate_chars("абвгд", 3), "абв");
        assert_eq!(truncate_chars("абв", 10), "абв");
    }

    #[test]
    fn the_outer_gauge_is_the_session_the_inner_one_is_today() {
        let now = timefmt::now();
        let mut overview = overview_with(Some(52.0), Some(41.0), now);
        overview.daily = Some(DailyBudget {
            spent_pct: 3.0,
            allowance_pct: 12.0,
            estimated: false,
        });
        let state = state_with(overview);

        assert_eq!(state.session_gauge(), Some(52.0));
        assert_eq!(state.daily_gauge(), Some(25.0));
    }

    #[test]
    fn the_inner_gauge_is_absent_without_a_daily_budget() {
        let now = timefmt::now();
        let state = state_with(overview_with(Some(52.0), Some(41.0), now));
        assert_eq!(state.daily_gauge(), None);
    }
}
