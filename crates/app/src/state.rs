//! GUI state: what has been read from the database and how to label it in the tray.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use claude_status_core::{
    Config, Db, Sample, Totals, autostart, db,
    install::{self, InstallStatus},
    pace::Overview,
    probe, scan,
    stats_cache::StatsCache,
    timefmt, tr, tr_args, update,
};

/// How long a window of time is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
    /// Everything counted so far. Has no length, so it cannot be stepped
    /// through — there is nothing on either side of it.
    All,
}

impl Range {
    pub fn secs(self) -> Option<i64> {
        match self {
            Range::Day => Some(86_400),
            Range::Week => Some(7 * 86_400),
            Range::Month => Some(30 * 86_400),
            Range::All => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            Range::Day => tr("ui.range.day"),
            Range::Week => tr("ui.range.week"),
            Range::Month => tr("ui.range.month"),
            Range::All => tr("ui.range.all"),
        }
    }

    pub const ALL: [Range; 4] = [Range::Day, Range::Week, Range::Month, Range::All];
}

/// The window a tab is looking at: a length, and how far back from now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub range: Range,
    /// 0 is the window ending now, 1 the one before it, and so on. Stepping
    /// back is what makes last month reachable without a date picker.
    pub back: i64,
}

impl Period {
    pub fn new(range: Range) -> Self {
        Self { range, back: 0 }
    }

    /// The window in unix seconds.
    pub fn bounds(self, now: i64) -> (i64, i64) {
        match self.range.secs() {
            Some(secs) => (now - (self.back + 1) * secs, now - self.back * secs),
            None => (0, now),
        }
    }

    /// The same window as the day keys the aggregates are stored under.
    pub fn days(self, now: i64) -> (String, String) {
        if self.range == Range::All {
            return (db::Span::ALL.from.to_string(), db::Span::ALL.to.to_string());
        }
        let (from, to) = self.bounds(now);
        (timefmt::day_key(from), timefmt::day_key(to))
    }

    /// `13.07 — 13.08`, or the single date when the window is a day.
    pub fn label(self, now: i64) -> String {
        if self.range == Range::All {
            return String::new();
        }
        let (from, to) = self.bounds(now);
        if self.range == Range::Day {
            return timefmt::date(to);
        }
        format!("{} — {}", timefmt::date(from), timefmt::date(to))
    }

    /// Whether stepping forward is possible: the newest window is now.
    pub fn at_present(self) -> bool {
        self.back == 0
    }

    pub fn steppable(self) -> bool {
        self.range != Range::All
    }
}

pub struct AppState {
    pub config: Config,
    pub overview: Overview,
    /// Samples over the selected span, oldest first.
    pub history: Vec<Sample>,
    /// What the history tab is looking at — every plot on it, not just the
    /// limits one.
    pub period: Period,
    /// What the models tab is looking at. Kept apart: one is for watching the
    /// week go by, the other for adding up a month.
    pub models_period: Period,
    /// What Claude Code aggregated itself. Only the all-time counters are read
    /// from it now — they reach back past the oldest session log still on disk.
    pub stats: Option<StatsCache>,
    /// Tokens counted from the session logs: `(date, model, tokens)`.
    pub tokens_per_day: Vec<(String, String, i64)>,
    /// `(date, sessions, messages)` from the same source.
    pub activity_per_day: Vec<(String, i64, i64)>,
    /// All-time totals per model and per project, busiest first.
    pub models: Vec<Totals>,
    pub projects: Vec<Totals>,
    /// The project the models table is narrowed to, picked from that list.
    pub project: Option<String>,
    /// Everything in the models window summed, for the subagent share.
    pub usage_totals: Totals,
    /// The first day the counted history covers, and when it was last counted.
    pub counted_since: Option<String>,
    pub last_scan_ts: i64,
    pub install: InstallStatus,
    /// Whether the session starts us, read from the operating system rather
    /// than mirrored in the configuration — a mirror would drift.
    pub autostart: autostart::State,
    /// The last read error — shown in the window, never fatal.
    pub error: Option<String>,
    pub refreshed_at: i64,
    /// Which model the weekly scoped cap applies to, when it has been reported.
    pub scoped_model: Option<String>,
    /// Shared with the probe thread: a probe takes seconds, and the window has
    /// to keep drawing meanwhile.
    probe: Arc<Mutex<ProbeSlot>>,
    /// Outcome of the last probe, for the settings screen.
    pub probe_message: Option<Result<String, String>>,
    /// Shared with the scan thread: counting hundreds of megabytes of logs
    /// takes seconds, and the window has to keep drawing.
    scan: Arc<Mutex<ScanSlot>>,
    /// Outcome of the last scan, for the models screen.
    pub scan_message: Option<Result<String, String>>,
    /// Shared with the update thread, for the same reason.
    update: Arc<Mutex<UpdateStage>>,
    /// Set by the settings screen, acted on by the paint loop: only that loop
    /// can tell the difference between quitting and hiding in the tray.
    pub restart_requested: bool,
}

/// Where self-update has got to. The settings button reads its label from this.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UpdateStage {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(update::Update),
    Downloading,
    /// Installed and waiting for a restart to take effect.
    Installed(update::Version),
    Failed(String),
}

/// The probe thread's side of the fence.
#[derive(Default)]
struct ProbeSlot {
    running: bool,
    outcome: Option<Result<String, String>>,
}

/// The scan thread's side of the same fence.
#[derive(Default)]
struct ScanSlot {
    running: bool,
    outcome: Option<Result<String, String>>,
}

impl AppState {
    pub fn load() -> Self {
        let mut state = Self {
            config: Config::load_and_apply_language(),
            overview: Overview::default(),
            history: Vec::new(),
            // A month either way: a week of daily bars is too few to see a
            // shape in, and the limits plot holds a month of samples at most.
            period: Period::new(Range::Month),
            models_period: Period::new(Range::Month),
            stats: None,
            tokens_per_day: Vec::new(),
            activity_per_day: Vec::new(),
            models: Vec::new(),
            projects: Vec::new(),
            project: None,
            usage_totals: Totals::default(),
            counted_since: None,
            last_scan_ts: 0,
            install: InstallStatus::Absent,
            autostart: autostart::State::Off,
            error: None,
            refreshed_at: 0,
            scoped_model: None,
            probe: Arc::new(Mutex::new(ProbeSlot::default())),
            probe_message: None,
            scan: Arc::new(Mutex::new(ScanSlot::default())),
            scan_message: None,
            update: Arc::new(Mutex::new(UpdateStage::Idle)),
            restart_requested: false,
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

        self.collect_probe();
        self.collect_scan();
        match self.reload(now) {
            Ok(()) => self.error = None,
            Err(e) => self.error = Some(format!("{e:#}")),
        }

        self.install = install::status().unwrap_or(InstallStatus::Absent);
        self.autostart = autostart::state().unwrap_or(autostart::State::Off);
        self.maybe_probe(now);
        if scan::due(self.last_scan_ts, now) {
            self.start_scan();
        }
    }

    fn reload(&mut self, now: i64) -> Result<()> {
        let db = Db::open_default()?;
        let (from, to) = self.period.bounds(now);
        self.history = db.samples_between(from, to)?;

        // Deliberately not derived from `history`: the summary needs the peak
        // reading of each window and the bounds of the weekly one, neither of
        // which a fixed time span is guaranteed to contain.
        self.overview = db.overview(now)?;
        self.stats = StatsCache::load().unwrap_or(None);
        self.scoped_model = db.scoped_model().unwrap_or(None);

        // The daily plots and both tables come from the logs Claude Code
        // writes, counted into this database — its own aggregates stopped
        // being recomputed and stood a week stale with nothing saying so.
        let (plot_from, plot_to) = self.period.days(now);
        let plotted = db::Span::new(&plot_from, &plot_to);
        self.tokens_per_day = db.tokens_per_day(plotted)?;
        self.activity_per_day = db.activity_per_day(plotted)?;

        let (table_from, table_to) = self.models_period.days(now);
        let tabled = db::Span::new(&table_from, &table_to);
        let project = self.project.as_deref();
        self.models = db.totals_by_model(tabled, project)?;
        self.projects = db.totals_by_project(tabled)?;
        self.usage_totals = db.overall_totals(tabled, project)?;

        self.counted_since = db.first_counted_day()?;
        self.last_scan_ts = db.last_scan_ts()?;
        Ok(())
    }

    /// Picks up what the scan thread left behind.
    fn collect_scan(&mut self) {
        let Ok(mut slot) = self.scan.lock() else { return };
        if let Some(outcome) = slot.outcome.take() {
            self.scan_message = Some(outcome);
        }
    }

    /// Throws away what was counted and counts it again from scratch.
    ///
    /// Every scan already reads every log — the incremental part only skips
    /// files that have not moved. This is for when the stored numbers are in
    /// doubt, and it costs the days whose logs Claude Code has since deleted.
    pub fn rescan_everything(&mut self) {
        if let Ok(db) = Db::open_default() {
            let _ = db.forget_counted_usage();
        }
        self.start_scan();
    }

    /// Counts the session logs on a thread of its own.
    ///
    /// Runs itself once a day and on the button: the first pass reads every log
    /// there is — 400 MB and half a minute on this machine — and later ones only
    /// the tails that grew, which is under a second.
    pub fn start_scan(&mut self) {
        let Ok(mut slot) = self.scan.lock() else { return };
        if slot.running {
            return;
        }
        slot.running = true;
        drop(slot);

        let shared = Arc::clone(&self.scan);
        std::thread::spawn(move || {
            let outcome = scan_once();
            if let Ok(mut slot) = shared.lock() {
                slot.running = false;
                slot.outcome = Some(outcome);
            }
        });
    }

    /// Narrows the models table to one project, or widens it back.
    ///
    /// Re-reads rather than filtering what is already loaded: the totals are a
    /// `GROUP BY` away in the database, and a table of a dozen rows is not
    /// worth keeping a second copy of in memory.
    pub fn select_project(&mut self, project: Option<String>) {
        if self.project == project {
            return;
        }
        self.project = project;
        self.refresh();
    }

    /// Whether a scan is running right now.
    pub fn scanning(&self) -> bool {
        self.scan.lock().is_ok_and(|slot| slot.running)
    }

    /// Picks up what the probe thread left behind.
    fn collect_probe(&mut self) {
        let Ok(mut slot) = self.probe.lock() else { return };
        if let Some(outcome) = slot.outcome.take() {
            self.probe_message = Some(outcome);
        }
    }

    /// Starts a probe if the cheap source has stopped saying anything useful.
    fn maybe_probe(&mut self, now: i64) {
        let last = Db::open_default().and_then(|db| db.last_probe_ts()).unwrap_or(0);
        if probe::due(&self.config.probe, &self.overview, last, now) {
            self.start_probe();
        }
    }

    /// Runs a probe regardless of the interval — the button in the settings.
    pub fn start_probe(&mut self) {
        let Ok(mut slot) = self.probe.lock() else { return };
        if slot.running {
            return;
        }
        slot.running = true;
        drop(slot);

        let shared = Arc::clone(&self.probe);
        std::thread::spawn(move || {
            let outcome = probe_once();
            if let Ok(mut slot) = shared.lock() {
                slot.running = false;
                slot.outcome = Some(outcome);
            }
        });
    }

    /// Whether a probe is running right now.
    pub fn probing(&self) -> bool {
        self.probe.lock().is_ok_and(|slot| slot.running)
    }

    /// What the update button should say.
    pub fn update_stage(&self) -> UpdateStage {
        self.update.lock().map(|stage| stage.clone()).unwrap_or_default()
    }

    /// Asks GitHub whether there is anything newer.
    pub fn check_for_update(&mut self) {
        self.spawn_update(UpdateStage::Checking, |_| match update::check() {
            Ok(Some(found)) => UpdateStage::Available(found),
            Ok(None) => UpdateStage::UpToDate,
            Err(e) => UpdateStage::Failed(format!("{e:#}")),
        });
    }

    /// Downloads the update found by the check and puts it in place.
    pub fn download_update(&mut self) {
        let UpdateStage::Available(found) = self.update_stage() else {
            return;
        };
        self.spawn_update(UpdateStage::Downloading, move |_| match update::install(&found) {
            Ok(_) => UpdateStage::Installed(found.version),
            Err(e) => UpdateStage::Failed(format!("{e:#}")),
        });
    }

    /// Runs `work` on a thread, holding `busy` in the meantime.
    ///
    /// Both steps talk to the network for seconds at a time; on the paint
    /// thread that would be a frozen window.
    fn spawn_update(
        &mut self,
        busy: UpdateStage,
        work: impl FnOnce(()) -> UpdateStage + Send + 'static,
    ) {
        {
            let Ok(mut stage) = self.update.lock() else { return };
            if matches!(*stage, UpdateStage::Checking | UpdateStage::Downloading) {
                return;
            }
            *stage = busy;
        }

        let shared = Arc::clone(&self.update);
        std::thread::spawn(move || {
            let outcome = work(());
            if let Ok(mut stage) = shared.lock() {
                *stage = outcome;
            }
        });
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

/// One probe, start to stored.
///
/// Claude Code needs a few seconds to come up; beyond half a minute something
/// is wrong and waiting longer helps nobody.
fn probe_once() -> Result<String, String> {
    let fail = |e: anyhow::Error| format!("{e:#}");

    let usage = probe::run(Duration::from_secs(30)).map_err(fail)?;
    let db = Db::open_default().map_err(fail)?;
    let now = timefmt::now();
    db.record_probe(&usage, now).map_err(fail)?;
    // Stamped whatever the reading turned out to be: a probe that found
    // nothing new still cost the same seconds and must not repeat at once.
    db.set_last_probe_ts(now).map_err(fail)?;

    Ok(tr("probe.updated"))
}

/// Counts the session logs into the database, on the scan thread.
fn scan_once() -> Result<String, String> {
    let fail = |e: anyhow::Error| format!("{e:#}");

    let mut db = Db::open_default().map_err(fail)?;
    let report = scan::run(&mut db).map_err(fail)?;

    Ok(tr_args(
        "models.scan.done",
        &[
            ("messages", &report.messages.to_string()),
            ("logs", &report.logs_read.to_string()),
        ],
    ))
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
            // A month either way: a week of daily bars is too few to see a
            // shape in, and the limits plot holds a month of samples at most.
            period: Period::new(Range::Month),
            models_period: Period::new(Range::Month),
            stats: None,
            tokens_per_day: Vec::new(),
            activity_per_day: Vec::new(),
            models: Vec::new(),
            projects: Vec::new(),
            project: None,
            usage_totals: Totals::default(),
            counted_since: None,
            last_scan_ts: 0,
            install: InstallStatus::Absent,
            autostart: autostart::State::Off,
            error: None,
            refreshed_at: 0,
            scoped_model: None,
            probe: Arc::new(Mutex::new(ProbeSlot::default())),
            probe_message: None,
            scan: Arc::new(Mutex::new(ScanSlot::default())),
            scan_message: None,
            update: Arc::new(Mutex::new(UpdateStage::Idle)),
            restart_requested: false,
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
