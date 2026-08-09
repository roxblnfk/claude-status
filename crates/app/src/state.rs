//! GUI state: what has been read from the database and how to label it in the tray.

use anyhow::Result;
use claude_status_core::{
    Config, Db, Sample,
    config::TrayRing,
    install::{self, InstallStatus},
    pace::{Overview, WindowState},
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

    /// The window the icon ring shows.
    pub fn ring_window(&self) -> Option<WindowState> {
        match self.config.tray.ring {
            TrayRing::FiveHour => self.overview.five_hour,
            TrayRing::Week => self.overview.week,
        }
    }

    /// The other window — the dot in the centre of the icon.
    pub fn dot_window(&self) -> Option<WindowState> {
        match self.config.tray.ring {
            TrayRing::FiveHour => self.overview.week,
            TrayRing::Week => self.overview.five_hour,
        }
    }

    /// The tray label.
    ///
    /// Windows truncates a tray tooltip at 127 characters, so this carries only
    /// the essentials; the full breakdown lives in the window.
    pub fn tooltip(&self) -> String {
        let now = timefmt::now();
        let mut lines = vec![tr("tray.tooltip.title")];

        if let Some(w) = self.overview.five_hour {
            lines.push(tr_args(
                "tray.tooltip.five_hour",
                &[
                    ("pct", &format!("{:.0}", w.used_pct)),
                    ("reset", &timefmt::clock(w.resets_at)),
                ],
            ));
        }
        if let Some(w) = self.overview.week {
            lines.push(tr_args(
                "tray.tooltip.week",
                &[
                    ("pct", &format!("{:.0}", w.used_pct)),
                    ("reset", &timefmt::date(w.resets_at)),
                ],
            ));
            if let Some(per_day) = w.allowance_per_day_pct() {
                lines.push(tr_args(
                    "tray.tooltip.allowance",
                    &[("pct", &format!("{per_day:.1}"))],
                ));
            }
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
    use claude_status_core::statusline::SEVEN_DAY_SECS;

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
        assert_eq!(tip.lines().count(), 4, "title, two windows and the budget: {tip}");
    }

    #[test]
    fn tooltip_reports_absence_of_data() {
        let state = state_with(Overview::default());
        assert_eq!(state.tooltip().lines().count(), 2, "{}", state.tooltip());
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
    fn ring_and_dot_follow_the_setting() {
        let now = timefmt::now();
        let mut state = state_with(overview_with(Some(52.0), Some(41.0), now));

        state.config.tray.ring = TrayRing::FiveHour;
        assert_eq!(state.ring_window().unwrap().used_pct, 52.0);
        assert_eq!(state.dot_window().unwrap().used_pct, 41.0);

        state.config.tray.ring = TrayRing::Week;
        assert_eq!(state.ring_window().unwrap().used_pct, 41.0);
        assert_eq!(state.dot_window().unwrap().used_pct, 52.0);
    }
}
