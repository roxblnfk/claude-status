//! Working out how fast the limit is being spent.
//!
//! Claude Code reports only the percentage of a window consumed, never absolute
//! tokens, so the whole budget is computed in percent. The "even pace" is the
//! diagonal from 0 % at the start of the window to 100 % at the reset; the
//! deviation from it answers "am I spending too fast?".

use crate::db::Sample;
use crate::statusline::{FIVE_HOUR_SECS, SEVEN_DAY_SECS};

const SECS_PER_DAY: f64 = 86_400.0;

/// State of one limit window, evaluated at a point in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowState {
    /// Consumed, 0..100.
    pub used_pct: f64,
    /// When the window resets, unix seconds.
    pub resets_at: i64,
    /// Window length in seconds.
    pub duration_secs: i64,
    /// The moment the calculation is made for.
    pub now: i64,
}

impl WindowState {
    pub fn new(used_pct: f64, resets_at: i64, duration_secs: i64, now: i64) -> Self {
        Self { used_pct, resets_at, duration_secs, now }
    }

    pub fn started_at(&self) -> i64 {
        self.resets_at - self.duration_secs
    }

    /// The window should already have reset — the data is stale and must not be
    /// presented as current.
    pub fn is_expired(&self) -> bool {
        self.now >= self.resets_at
    }

    /// Usage as it may be shown to the user.
    ///
    /// `None` once the window has reset: the recorded percentage belongs to a
    /// window that no longer exists, and how much of the new one is spent stays
    /// unknown until Claude Code reports again.
    pub fn live_used_pct(&self) -> Option<f64> {
        (!self.is_expired()).then_some(self.used_pct)
    }

    pub fn remaining_secs(&self) -> i64 {
        (self.resets_at - self.now).max(0)
    }

    pub fn elapsed_secs(&self) -> i64 {
        (self.now - self.started_at()).clamp(0, self.duration_secs)
    }

    /// How much of the window has elapsed, 0..1.
    pub fn elapsed_fraction(&self) -> f64 {
        if self.duration_secs <= 0 {
            return 1.0;
        }
        self.elapsed_secs() as f64 / self.duration_secs as f64
    }

    /// How much would have been spent at a perfectly even pace.
    pub fn expected_pct(&self) -> f64 {
        self.elapsed_fraction() * 100.0
    }

    /// Deviation from the even pace: `> 0` means spending faster than budget.
    pub fn deviation_pct(&self) -> f64 {
        self.used_pct - self.expected_pct()
    }

    /// The same deviation expressed as time: how far along the window the even
    /// pace would have to run to reach what is already spent.
    ///
    /// "A day ahead of schedule" is something a person can act on; the same gap
    /// in percentage points is not.
    pub fn deviation_secs(&self) -> i64 {
        (self.deviation_pct() / 100.0 * self.duration_secs as f64) as i64
    }

    pub fn remaining_pct(&self) -> f64 {
        (100.0 - self.used_pct).max(0.0)
    }

    /// How many percent of the window may be spent per day to land exactly on
    /// the reset. `None` when less than a minute remains — over such a horizon
    /// a daily rate becomes a meaninglessly large number.
    pub fn allowance_per_day_pct(&self) -> Option<f64> {
        let remaining_days = self.remaining_secs() as f64 / SECS_PER_DAY;
        if remaining_days < 1.0 / 1440.0 {
            return None;
        }
        Some(self.remaining_pct() / remaining_days)
    }

    /// How many percent may be spent between now and `until` while staying on
    /// the even pace. An `until` beyond the window is clamped to the reset.
    pub fn allowance_until(&self, until: i64) -> Option<f64> {
        let per_day = self.allowance_per_day_pct()?;
        let span = (until.min(self.resets_at) - self.now).max(0) as f64;
        Some(per_day * span / SECS_PER_DAY)
    }

    /// Usage at the reset if `burn_pct_per_day` holds.
    pub fn projected_used_at_reset(&self, burn_pct_per_day: f64) -> f64 {
        let days_left = self.remaining_secs() as f64 / SECS_PER_DAY;
        self.used_pct + burn_pct_per_day * days_left
    }

    /// When 100 % is reached at `burn_pct_per_day`.
    ///
    /// `None` if the pace is zero or the limit would only run out after the
    /// window resets — at such a pace the ceiling is simply never reached.
    pub fn exhausted_at(&self, burn_pct_per_day: f64) -> Option<i64> {
        if burn_pct_per_day <= 0.0 {
            return None;
        }
        let secs_to_full = self.remaining_pct() / burn_pct_per_day * SECS_PER_DAY;
        if !secs_to_full.is_finite() {
            return None;
        }
        let at = self.now + secs_to_full as i64;
        (at < self.resets_at).then_some(at)
    }
}

/// A measured spending rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Burn {
    /// Percent of the window per day.
    pub pct_per_day: f64,
    /// The span the rate was averaged over. Kept so the figure can be shown
    /// with the observation it rests on.
    pub span_secs: i64,
}

/// The shortest span a rate may be averaged over.
///
/// Claude Code reports whole percents, so over a few minutes the smallest step
/// it can report — a single point — already extrapolates to hundreds of percent
/// a day. Such a number says nothing about the days ahead.
const MIN_BURN_SPAN_SECS: i64 = 1800;

/// Actual spending rate of the weekly window.
///
/// Measured within a single window: if it reset in between (`week_resets_at`
/// changed), earlier points are discarded — otherwise a reset from 100 % to 0 %
/// would read as a negative pace.
pub fn week_burn(samples: &[Sample]) -> Option<Burn> {
    burn(samples, |s| s.week_pct, |s| s.week_resets_at)
}

/// Actual spending rate of the five-hour window.
pub fn five_hour_burn(samples: &[Sample]) -> Option<Burn> {
    burn(samples, |s| s.five_pct, |s| s.five_resets_at)
}

fn burn(
    samples: &[Sample],
    pct: impl Fn(&Sample) -> Option<f64>,
    resets_at: impl Fn(&Sample) -> Option<i64>,
) -> Option<Burn> {
    let last = samples.iter().rev().find(|s| pct(s).is_some())?;
    let window = resets_at(last);

    // Only points from the current window: percentages of the previous one are
    // not comparable.
    let first = samples
        .iter()
        .find(|s| pct(s).is_some() && resets_at(s) == window)
        .filter(|s| s.id != last.id)?;

    // The span runs to the last moment the newest reading was still confirmed,
    // not to the moment it first appeared. Ending it at the appearance would
    // cut off the idle stretch that followed — the very time during which
    // nothing was spent — and inflate the rate several times over.
    let span_secs = last.seen_until() - first.ts;
    if span_secs < MIN_BURN_SPAN_SECS {
        return None;
    }
    let delta = pct(last)? - pct(first)?;
    Some(Burn { pct_per_day: delta / span_secs as f64 * SECS_PER_DAY, span_secs })
}

/// Full summary of both windows at `now`.
#[derive(Debug, Clone, Default)]
pub struct Overview {
    pub five_hour: Option<WindowState>,
    pub week: Option<WindowState>,
    pub week_opus: Option<WindowState>,
    /// Actual spending rate of the weekly window.
    pub week_burn: Option<Burn>,
    /// When the sample was taken — tells how fresh the data is.
    pub sampled_at: Option<i64>,
}

impl Overview {
    /// Builds the summary from the authoritative current state plus the points
    /// the pace is measured between.
    ///
    /// The two come from different rows on purpose: with several Claude Code
    /// sessions writing, the state has to be assembled per window (see
    /// [`crate::db::Db::current_sample`]) while the pace needs two points in
    /// time.
    pub fn new(current: &Sample, pace_samples: &[Sample], now: i64) -> Self {
        Self {
            five_hour: window_of(current.five_pct, current.five_resets_at, FIVE_HOUR_SECS, now),
            week: window_of(current.week_pct, current.week_resets_at, SEVEN_DAY_SECS, now),
            week_opus: window_of(current.opus_pct, current.opus_resets_at, SEVEN_DAY_SECS, now),
            week_burn: week_burn(pace_samples),
            sampled_at: Some(current.seen_until()),
        }
    }

    /// Builds the summary from a plain slice, treating its last element as the
    /// current state.
    pub fn from_samples(samples: &[Sample], now: i64) -> Self {
        match samples.last() {
            Some(last) => Self::new(last, samples, now),
            None => Self::default(),
        }
    }

    /// How stale the data is, in seconds.
    pub fn staleness_secs(&self, now: i64) -> Option<i64> {
        self.sampled_at.map(|ts| (now - ts).max(0))
    }
}

fn window_of(pct: Option<f64>, resets_at: Option<i64>, dur: i64, now: i64) -> Option<WindowState> {
    Some(WindowState::new(pct?, resets_at?, dur, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    /// A weekly window that started at t=0 and resets after 7 days.
    fn week(used_pct: f64, now: i64) -> WindowState {
        WindowState::new(used_pct, SEVEN_DAY_SECS, SEVEN_DAY_SECS, now)
    }

    #[test]
    fn expected_pct_follows_the_diagonal() {
        assert!((week(0.0, 0).expected_pct() - 0.0).abs() < 1e-9);
        assert!((week(0.0, 3 * DAY).expected_pct() - 300.0 / 7.0).abs() < 1e-9);
        assert!((week(0.0, SEVEN_DAY_SECS).expected_pct() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn deviation_flags_overspending() {
        // After one day the budget is ~14.3 %, but 50 % is gone — well ahead.
        let w = week(50.0, DAY);
        assert!(w.deviation_pct() > 35.0, "overspending: {}", w.deviation_pct());

        // After six days only 20 % is gone — plenty of headroom.
        let w = week(20.0, 6 * DAY);
        assert!(w.deviation_pct() < -60.0, "underspending: {}", w.deviation_pct());
    }

    #[test]
    fn deviation_converts_to_the_span_it_covers() {
        // One day in, 30 % spent while the diagonal allows 1/7: the surplus of
        // ~15.7 pp is what the even pace would spend in about 1.1 days.
        let w = week(30.0, DAY);
        let expected = (w.deviation_pct() / 100.0 * SEVEN_DAY_SECS as f64) as i64;
        assert_eq!(w.deviation_secs(), expected);
        assert!(w.deviation_secs() > DAY, "{}", w.deviation_secs());

        // Underspending points the other way.
        assert!(week(20.0, 6 * DAY).deviation_secs() < 0);
    }

    #[test]
    fn allowance_spreads_remainder_over_remaining_days() {
        // One day in, 30 % spent, six days left: 70 / 6 ≈ 11.67 % per day.
        let w = week(30.0, DAY);
        let per_day = w.allowance_per_day_pct().unwrap();
        assert!((per_day - 70.0 / 6.0).abs() < 1e-6, "{per_day}");

        // Half a day gets half the daily budget.
        let half = w.allowance_until(DAY + DAY / 2).unwrap();
        assert!((half - per_day / 2.0).abs() < 1e-6, "{half}");
    }

    #[test]
    fn allowance_is_none_right_before_reset() {
        let w = week(30.0, SEVEN_DAY_SECS - 10);
        assert!(w.allowance_per_day_pct().is_none());
    }

    #[test]
    fn allowance_until_clamps_to_reset() {
        let w = week(50.0, 6 * DAY);
        // Asking for three days ahead while the window closes in one: the
        // answer must not exceed the whole remaining limit.
        let budget = w.allowance_until(9 * DAY).unwrap();
        assert!(budget <= w.remaining_pct() + 1e-6, "{budget}");
    }

    #[test]
    fn projection_and_exhaustion() {
        // 40 % spent, six days left, 20 %/day — the ceiling will be hit.
        let w = week(40.0, DAY);
        assert!((w.projected_used_at_reset(20.0) - 160.0).abs() < 1e-6);

        let at = w.exhausted_at(20.0).expect("60 % at 20 %/day lasts three days");
        assert_eq!(at, DAY + 3 * DAY);

        // At 5 %/day the reset arrives first.
        assert!(w.exhausted_at(5.0).is_none());
        assert!(w.exhausted_at(0.0).is_none());
    }

    #[test]
    fn expired_window_is_detected() {
        assert!(!week(50.0, SEVEN_DAY_SECS - 1).is_expired());
        assert!(week(50.0, SEVEN_DAY_SECS).is_expired());
        // The percentage belongs to the window that has just gone.
        assert_eq!(week(50.0, SEVEN_DAY_SECS - 1).live_used_pct(), Some(50.0));
        assert_eq!(week(50.0, SEVEN_DAY_SECS).live_used_pct(), None);
    }

    fn sample(id: i64, ts: i64, week_pct: f64, week_resets_at: i64) -> Sample {
        Sample {
            id,
            ts,
            last_seen_ts: ts,
            week_pct: Some(week_pct),
            week_resets_at: Some(week_resets_at),
            ..Sample::default()
        }
    }

    #[test]
    fn burn_rate_from_two_points() {
        // 10 % over half a day => 20 %/day.
        let samples =
            vec![sample(1, 0, 10.0, SEVEN_DAY_SECS), sample(2, DAY / 2, 20.0, SEVEN_DAY_SECS)];
        let burn = week_burn(&samples).unwrap();
        assert!((burn.pct_per_day - 20.0).abs() < 1e-6, "{burn:?}");
        assert_eq!(burn.span_secs, DAY / 2);
    }

    #[test]
    fn burn_rate_ignores_points_from_a_previous_window() {
        // The first two points belong to the previous window (90 %), then it reset.
        let samples = vec![
            sample(1, 0, 90.0, SEVEN_DAY_SECS),
            sample(2, DAY, 95.0, SEVEN_DAY_SECS),
            sample(3, 2 * DAY, 2.0, 2 * SEVEN_DAY_SECS),
            sample(4, 3 * DAY, 12.0, 2 * SEVEN_DAY_SECS),
        ];
        let burn = week_burn(&samples).unwrap();
        assert!((burn.pct_per_day - 10.0).abs() < 1e-6, "pace uses the new window only: {burn:?}");
    }

    #[test]
    fn burn_rate_runs_to_the_last_confirmation_not_the_first_sighting() {
        // One point gained in four minutes, then two hours of the same reading
        // being confirmed over and over. Measuring only the four minutes would
        // read as 360 %/day; across everything observed it is about 11.
        let mut last = sample(2, 245, 60.0, SEVEN_DAY_SECS);
        last.last_seen_ts = 7568;
        let samples = vec![sample(1, 0, 59.0, SEVEN_DAY_SECS), last];

        let burn = week_burn(&samples).unwrap();
        assert_eq!(burn.span_secs, 7568);
        assert!((burn.pct_per_day - 11.4).abs() < 0.1, "{burn:?}");
    }

    #[test]
    fn burn_rate_needs_a_long_enough_span() {
        // Whole percents over four minutes extrapolate to nonsense.
        let samples =
            vec![sample(1, 0, 59.0, SEVEN_DAY_SECS), sample(2, 245, 60.0, SEVEN_DAY_SECS)];
        assert!(week_burn(&samples).is_none());
    }

    #[test]
    fn burn_rate_needs_two_points() {
        assert!(week_burn(&[]).is_none());
        assert!(week_burn(&[sample(1, 0, 10.0, SEVEN_DAY_SECS)]).is_none());
    }

    #[test]
    fn overview_reports_staleness() {
        let samples = vec![sample(1, 100, 10.0, SEVEN_DAY_SECS)];
        let overview = Overview::from_samples(&samples, 400);
        assert_eq!(overview.staleness_secs(400), Some(300));
        assert!(overview.week.is_some());
        assert!(overview.five_hour.is_none(), "the sample had no five-hour window");
    }
}
