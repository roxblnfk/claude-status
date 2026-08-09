//! Storage for limit samples.
//!
//! Written by the hook process (one per Claude Code session), read by the GUI.
//! Hence WAL and a `busy_timeout`: several processes work with the database at
//! the same time.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::paths;
use crate::statusline::StatuslineInput;
use crate::tr_args;

/// A snapshot of the limit state at a point in time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sample {
    pub id: i64,
    /// When this state was first observed.
    pub ts: i64,
    /// When it was last confirmed (percentages unchanged).
    pub last_seen_ts: i64,
    pub five_pct: Option<f64>,
    pub five_resets_at: Option<i64>,
    pub week_pct: Option<f64>,
    pub week_resets_at: Option<i64>,
    pub opus_pct: Option<f64>,
    pub opus_resets_at: Option<i64>,
    pub session_id: Option<String>,
    pub model_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub ctx_input: Option<i64>,
    pub ctx_output: Option<i64>,
    pub ctx_size: Option<i64>,
    pub version: Option<String>,
}

/// What writing a sample actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// Percentages changed — a new row was appended.
    Inserted,
    /// Same state — `last_seen_ts` of the last row was moved forward.
    Touched,
    /// The JSON carried no `rate_limits`; there is nothing to store.
    Skipped,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    /// Opens the database in the application data directory, applying the schema.
    pub fn open_default() -> Result<Self> {
        Self::open(paths::db_path()?)
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let conn = Connection::open(path)
            .with_context(|| tr_args("error.open_db", &[("path", &path.display().to_string())]))?;
        Self::from_conn(conn)
    }

    /// In-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        // WAL lets the hook write while the GUI reads; the timeout covers races
        // between several Claude Code sessions running at once.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS samples (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                ts             INTEGER NOT NULL,
                last_seen_ts   INTEGER NOT NULL,
                five_pct       REAL,
                five_resets_at INTEGER,
                week_pct       REAL,
                week_resets_at INTEGER,
                opus_pct       REAL,
                opus_resets_at INTEGER,
                session_id     TEXT,
                model_id       TEXT,
                cost_usd       REAL,
                ctx_input      INTEGER,
                ctx_output     INTEGER,
                ctx_size       INTEGER,
                version        TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_samples_ts ON samples(ts);

            -- Mirror of ~/.claude/stats-cache.json: tokens per day and model.
            CREATE TABLE IF NOT EXISTS daily_tokens (
                date   TEXT NOT NULL,
                model  TEXT NOT NULL,
                tokens INTEGER NOT NULL,
                PRIMARY KEY (date, model)
            );

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    /// Stores a sample taken from the statusline JSON.
    ///
    /// The hook runs on every assistant message, so identical states collapse:
    /// a new row appears only when the percentages or the window boundaries
    /// change.
    pub fn record(&self, input: &StatuslineInput, now: i64) -> Result<Written> {
        let five = input.five_hour();
        let week = input.seven_day();
        // The per-model weekly cap, whatever Anthropic calls it this release.
        // Only the first one is stored for now; the schema holds a single slot.
        let opus = input.extra_windows().first().map(|(_, w)| *w);

        // Without rate_limits a sample is pointless: either this is not a
        // Claude.ai subscription, or the session has had no API response yet.
        if five.is_none() && week.is_none() && opus.is_none() {
            return Ok(Written::Skipped);
        }

        let five_pct = five.and_then(|w| w.used_percentage);
        let five_resets_at = five.and_then(|w| w.resets_at);
        let week_pct = week.and_then(|w| w.used_percentage);
        let week_resets_at = week.and_then(|w| w.resets_at);
        let opus_pct = opus.and_then(|w| w.used_percentage);
        let opus_resets_at = opus.and_then(|w| w.resets_at);

        // Compared against this session's own previous sample, not the global
        // last one: with several Claude Code sessions running, their readings
        // interleave and nothing would ever look unchanged.
        if let Some(last) = self.latest_for_session(input.session_id.as_deref())?
            && same_state(&last, five_pct, five_resets_at, week_pct, week_resets_at, opus_pct)
        {
            self.conn.execute(
                "UPDATE samples SET last_seen_ts = ?1 WHERE id = ?2",
                params![now.max(last.last_seen_ts), last.id],
            )?;
            return Ok(Written::Touched);
        }

        let ctx = input.context_window.as_ref();
        self.conn.execute(
            r#"INSERT INTO samples (
                   ts, last_seen_ts, five_pct, five_resets_at, week_pct, week_resets_at,
                   opus_pct, opus_resets_at, session_id, model_id, cost_usd,
                   ctx_input, ctx_output, ctx_size, version
               ) VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"#,
            params![
                now,
                five_pct,
                five_resets_at,
                week_pct,
                week_resets_at,
                opus_pct,
                opus_resets_at,
                input.session_id,
                input.model_id(),
                input.cost.as_ref().and_then(|c| c.total_cost_usd),
                ctx.and_then(|c| c.total_input_tokens),
                ctx.and_then(|c| c.total_output_tokens),
                ctx.and_then(|c| c.context_window_size),
                input.version,
            ],
        )?;
        Ok(Written::Inserted)
    }

    /// The most recently stored sample.
    pub fn latest(&self) -> Result<Option<Sample>> {
        let sample = self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM samples ORDER BY id DESC LIMIT 1"),
                [],
                row_to_sample,
            )
            .optional()?;
        Ok(sample)
    }

    /// The most recent sample written by a given session.
    fn latest_for_session(&self, session_id: Option<&str>) -> Result<Option<Sample>> {
        let sample = self
            .conn
            .query_row(
                &format!(
                    "SELECT {COLUMNS} FROM samples WHERE session_id IS ?1 ORDER BY id DESC LIMIT 1"
                ),
                params![session_id],
                row_to_sample,
            )
            .optional()?;
        Ok(sample)
    }

    /// The true current state of every window.
    ///
    /// Several Claude Code sessions write here at once, and an idle one keeps
    /// repeating the reading it captured long ago — the newest row is therefore
    /// not the newest data. Within one window (same `resets_at`) usage only
    /// ever grows, so the highest reading is the current one.
    pub fn current_sample(&self) -> Result<Option<Sample>> {
        let Some(latest) = self.latest()? else {
            return Ok(None);
        };

        let (five_pct, five_resets_at) = self.window_peak("five_pct", "five_resets_at")?;
        let (week_pct, week_resets_at) = self.window_peak("week_pct", "week_resets_at")?;
        let (opus_pct, opus_resets_at) = self.window_peak("opus_pct", "opus_resets_at")?;

        Ok(Some(Sample {
            five_pct,
            five_resets_at,
            week_pct,
            week_resets_at,
            opus_pct,
            opus_resets_at,
            ..latest
        }))
    }

    /// Highest reading of a window within its latest boundary.
    fn window_peak(&self, pct: &str, resets_at: &str) -> Result<(Option<f64>, Option<i64>)> {
        let boundary: Option<i64> = self
            .conn
            .query_row(
                &format!("SELECT MAX({resets_at}) FROM samples WHERE {pct} IS NOT NULL"),
                [],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        let Some(boundary) = boundary else {
            return Ok((None, None));
        };

        let peak: Option<f64> = self
            .conn
            .query_row(
                &format!("SELECT MAX({pct}) FROM samples WHERE {resets_at} = ?1"),
                params![boundary],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        Ok((peak, Some(boundary)))
    }

    /// Samples within `[from, to]`, in chronological order.
    pub fn samples_between(&self, from: i64, to: i64) -> Result<Vec<Sample>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM samples WHERE ts BETWEEN ?1 AND ?2 ORDER BY ts"
        ))?;
        let rows = stmt.query_map(params![from, to], row_to_sample)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The two points the weekly pace is measured between.
    ///
    /// The hook runs on every assistant message, so reading the whole history
    /// just to compute a pace is out of the question — two points are enough
    /// for a linear estimate. They are the earliest reading of the current
    /// window and the moment it first reached its peak: taking the newest row
    /// instead would measure against whatever an idle session last repeated.
    pub fn burn_endpoints(&self) -> Result<Vec<Sample>> {
        let Some(current) = self.current_sample()? else {
            return Ok(Vec::new());
        };
        let Some(boundary) = current.week_resets_at else {
            return Ok(vec![current]);
        };

        let pick = |order: &str| -> Result<Option<Sample>> {
            Ok(self
                .conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM samples
                         WHERE week_pct IS NOT NULL AND week_resets_at IS ?1
                         ORDER BY {order} LIMIT 1"
                    ),
                    params![boundary],
                    row_to_sample,
                )
                .optional()?)
        };

        let first = pick("ts, id")?;
        // Among equal peaks the earliest one: that is when the level was
        // actually reached, and a later repeat would flatten the pace.
        let peak = pick("week_pct DESC, ts, id")?;

        Ok(match (first, peak) {
            (Some(first), Some(peak)) if first.id != peak.id => vec![first, peak],
            _ => vec![current],
        })
    }

    /// The summary every entry point shows: current state plus the pace.
    pub fn overview(&self, now: i64) -> Result<crate::pace::Overview> {
        let Some(current) = self.current_sample()? else {
            return Ok(crate::pace::Overview::default());
        };
        Ok(crate::pace::Overview::new(&current, &self.burn_endpoints()?, now))
    }

    /// Stores the daily token breakdown per model (from `stats-cache.json`).
    pub fn upsert_daily_tokens(&mut self, entries: &[(String, String, i64)]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO daily_tokens (date, model, tokens) VALUES (?1, ?2, ?3)
                 ON CONFLICT(date, model) DO UPDATE SET tokens = excluded.tokens",
            )?;
            for (date, model, tokens) in entries {
                stmt.execute(params![date, model, tokens])?;
            }
        }
        tx.commit()?;
        Ok(entries.len())
    }

    /// Daily tokens per model from `from` onwards (inclusive, `YYYY-MM-DD`).
    pub fn daily_tokens_since(&self, from: &str) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, model, tokens FROM daily_tokens WHERE date >= ?1 ORDER BY date, model",
        )?;
        let rows = stmt.query_map(params![from], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Wipes the collected history.
    ///
    /// Only the samples we gathered ourselves: `daily_tokens` mirrors
    /// `stats-cache.json` and would come straight back, and `meta` holds
    /// bookkeeping rather than statistics.
    pub fn clear_samples(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM samples", [])?)
    }

    /// Copies the database next to itself, returning the backup path.
    ///
    /// Taken before wiping the history: the samples cannot be recomputed from
    /// anything — Claude Code does not keep them — so a misfired reset would
    /// otherwise be final.
    pub fn backup(&self) -> Result<PathBuf> {
        let path = paths::ensure_data_dir()?.join("usage.sqlite3.bak");
        // `VACUUM INTO` refuses to overwrite, and it is the only way to get a
        // consistent copy while WAL writers are active.
        let _ = std::fs::remove_file(&path);

        let target = path.to_string_lossy().replace('\'', "''");
        self.conn.execute(&format!("VACUUM INTO '{target}'"), [])?;
        Ok(path)
    }

    /// Deletes samples older than `before`. Returns the number of rows removed.
    pub fn prune(&self, before: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM samples WHERE ts < ?1", params![before])?)
    }

    /// Prunes the history at most once a day.
    ///
    /// The hook runs on every assistant message, so issuing a `DELETE` on each
    /// invocation is pointless. `retention_days <= 0` disables pruning entirely.
    pub fn maybe_prune(&self, retention_days: i64, now: i64) -> Result<usize> {
        if retention_days <= 0 {
            return Ok(0);
        }
        const DAY: i64 = 86_400;

        let last: Option<i64> = self.meta_get("last_prune_ts")?.and_then(|v| v.parse().ok());
        if last.is_some_and(|ts| now - ts < DAY) {
            return Ok(0);
        }

        let removed = self.prune(now - retention_days * DAY)?;
        self.meta_set("last_prune_ts", &now.to_string())?;
        Ok(removed)
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

const COLUMNS: &str = "id, ts, last_seen_ts, five_pct, five_resets_at, week_pct, week_resets_at, \
                       opus_pct, opus_resets_at, session_id, model_id, cost_usd, \
                       ctx_input, ctx_output, ctx_size, version";

fn row_to_sample(row: &rusqlite::Row<'_>) -> rusqlite::Result<Sample> {
    Ok(Sample {
        id: row.get(0)?,
        ts: row.get(1)?,
        last_seen_ts: row.get(2)?,
        five_pct: row.get(3)?,
        five_resets_at: row.get(4)?,
        week_pct: row.get(5)?,
        week_resets_at: row.get(6)?,
        opus_pct: row.get(7)?,
        opus_resets_at: row.get(8)?,
        session_id: row.get(9)?,
        model_id: row.get(10)?,
        cost_usd: row.get(11)?,
        ctx_input: row.get(12)?,
        ctx_output: row.get(13)?,
        ctx_size: row.get(14)?,
        version: row.get(15)?,
    })
}

fn same_state(
    last: &Sample,
    five_pct: Option<f64>,
    five_resets_at: Option<i64>,
    week_pct: Option<f64>,
    week_resets_at: Option<i64>,
    opus_pct: Option<f64>,
) -> bool {
    eq_pct(last.five_pct, five_pct)
        && last.five_resets_at == five_resets_at
        && eq_pct(last.week_pct, week_pct)
        && last.week_resets_at == week_resets_at
        && eq_pct(last.opus_pct, opus_pct)
}

/// Percentages arrive as f64 with one decimal; compare with a tolerance.
fn eq_pct(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 1e-6,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(five: f64, week: f64, resets: i64) -> StatuslineInput {
        let json = format!(
            r#"{{"session_id":"s","model":{{"id":"claude-opus-5"}},
                 "rate_limits":{{"five_hour":{{"used_percentage":{five},"resets_at":{resets}}},
                                 "seven_day":{{"used_percentage":{week},"resets_at":{resets}}}}}}}"#
        );
        StatuslineInput::parse(&json).unwrap()
    }

    #[test]
    fn inserts_then_collapses_identical_states() {
        let db = Db::open_in_memory().unwrap();

        assert_eq!(db.record(&input(10.0, 20.0, 999), 100).unwrap(), Written::Inserted);
        assert_eq!(db.record(&input(10.0, 20.0, 999), 160).unwrap(), Written::Touched);
        assert_eq!(db.record(&input(10.0, 20.0, 999), 220).unwrap(), Written::Touched);

        let last = db.latest().unwrap().unwrap();
        assert_eq!(last.ts, 100, "first observation of the state is not moved");
        assert_eq!(last.last_seen_ts, 220, "confirmation updates last_seen_ts");
        assert_eq!(db.samples_between(0, 1000).unwrap().len(), 1);
    }

    #[test]
    fn insert_on_percentage_change() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        assert_eq!(db.record(&input(10.5, 20.0, 999), 160).unwrap(), Written::Inserted);
        assert_eq!(db.samples_between(0, 1000).unwrap().len(), 2);
    }

    #[test]
    fn insert_on_window_reset() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(90.0, 20.0, 999), 100).unwrap();
        // Same percentage, but the window moved — a new window, not the same state.
        assert_eq!(db.record(&input(90.0, 20.0, 1999), 160).unwrap(), Written::Inserted);
    }

    #[test]
    fn skips_input_without_rate_limits() {
        let db = Db::open_in_memory().unwrap();
        let bare = StatuslineInput::parse(r#"{"session_id":"s"}"#).unwrap();
        assert_eq!(db.record(&bare, 100).unwrap(), Written::Skipped);
        assert!(db.latest().unwrap().is_none());
    }

    #[test]
    fn daily_tokens_roundtrip_and_upsert() {
        let mut db = Db::open_in_memory().unwrap();
        db.upsert_daily_tokens(&[("2026-08-01".into(), "claude-opus-5".into(), 100)]).unwrap();
        db.upsert_daily_tokens(&[("2026-08-01".into(), "claude-opus-5".into(), 250)]).unwrap();

        let rows = db.daily_tokens_since("2026-08-01").unwrap();
        assert_eq!(rows, vec![("2026-08-01".into(), "claude-opus-5".into(), 250)]);
    }

    /// Builds a sample as reported by a specific session.
    fn session_input(session: &str, five: f64, week: f64, resets: i64) -> StatuslineInput {
        let json = format!(
            r#"{{"session_id":"{session}",
                 "rate_limits":{{"five_hour":{{"used_percentage":{five},"resets_at":{resets}}},
                                 "seven_day":{{"used_percentage":{week},"resets_at":{resets}}}}}}}"#
        );
        StatuslineInput::parse(&json).unwrap()
    }

    /// The real failure this logic exists for: an idle Claude Code session
    /// keeps repeating a stale reading every minute, so the newest row is not
    /// the newest data.
    #[test]
    fn current_state_ignores_a_stale_session() {
        let db = Db::open_in_memory().unwrap();

        db.record(&session_input("busy", 25.0, 59.0, 999), 100).unwrap();
        // The idle session reports last, and reports much less.
        db.record(&session_input("idle", 3.0, 57.0, 999), 200).unwrap();

        assert_eq!(db.latest().unwrap().unwrap().five_pct, Some(3.0), "newest row is the stale one");

        let current = db.current_sample().unwrap().unwrap();
        assert_eq!(current.five_pct, Some(25.0), "the higher reading wins within a window");
        assert_eq!(current.week_pct, Some(59.0));
    }

    #[test]
    fn current_state_prefers_the_newer_window_over_a_higher_old_one() {
        let db = Db::open_in_memory().unwrap();
        // The previous window ended at 95 %; the new one has barely started.
        db.record(&session_input("s", 95.0, 90.0, 999), 100).unwrap();
        db.record(&session_input("s", 2.0, 1.0, 5000), 200).unwrap();

        let current = db.current_sample().unwrap().unwrap();
        assert_eq!(current.five_pct, Some(2.0), "a reset window is not shadowed by the old peak");
        assert_eq!(current.five_resets_at, Some(5000));
    }

    #[test]
    fn deduplication_is_per_session() {
        let db = Db::open_in_memory().unwrap();

        db.record(&session_input("a", 10.0, 20.0, 999), 100).unwrap();
        db.record(&session_input("b", 25.0, 30.0, 999), 110).unwrap();
        // Both sessions repeat themselves: neither should add a row.
        assert_eq!(db.record(&session_input("a", 10.0, 20.0, 999), 200).unwrap(), Written::Touched);
        assert_eq!(db.record(&session_input("b", 25.0, 30.0, 999), 210).unwrap(), Written::Touched);

        assert_eq!(db.samples_between(0, 1000).unwrap().len(), 2, "one row per session");
    }

    #[test]
    fn overview_uses_the_authoritative_reading() {
        let db = Db::open_in_memory().unwrap();
        db.record(&session_input("busy", 25.0, 59.0, 10_000_000), 100).unwrap();
        db.record(&session_input("idle", 3.0, 57.0, 10_000_000), 200).unwrap();

        let overview = db.overview(300).unwrap();
        assert_eq!(overview.five_hour.unwrap().used_pct, 25.0);
        assert_eq!(overview.week.unwrap().used_pct, 59.0);
    }

    #[test]
    fn burn_endpoints_returns_bounds_of_the_current_window() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.record(&input(20.0, 30.0, 999), 200).unwrap();
        db.record(&input(30.0, 40.0, 999), 300).unwrap();

        let bounds = db.burn_endpoints().unwrap();
        assert_eq!(bounds.len(), 2);
        assert_eq!(bounds[0].ts, 100);
        assert_eq!(bounds[1].ts, 300);
    }

    #[test]
    fn burn_endpoints_ignores_the_previous_window() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(90.0, 95.0, 999), 100).unwrap();
        // The window reset: bounds must be computed within the new one only.
        db.record(&input(1.0, 2.0, 1999), 200).unwrap();
        db.record(&input(5.0, 6.0, 1999), 300).unwrap();

        let bounds = db.burn_endpoints().unwrap();
        assert_eq!(bounds.len(), 2);
        assert_eq!(bounds[0].ts, 200, "a point from the previous window is excluded");
        assert_eq!(bounds[1].ts, 300);
    }

    #[test]
    fn burn_endpoints_on_empty_db() {
        assert!(Db::open_in_memory().unwrap().burn_endpoints().unwrap().is_empty());
    }

    #[test]
    fn prune_drops_old_samples() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.record(&input(30.0, 40.0, 999), 500).unwrap();
        assert_eq!(db.prune(300).unwrap(), 1);
        assert_eq!(db.samples_between(0, 10_000).unwrap().len(), 1);
    }

    #[test]
    fn maybe_prune_runs_at_most_once_a_day() {
        const DAY: i64 = 86_400;
        let db = Db::open_in_memory().unwrap();
        let now = 100 * DAY;
        db.record(&input(10.0, 20.0, 999), now - 40 * DAY).unwrap();
        db.record(&input(30.0, 40.0, 999), now).unwrap();

        assert_eq!(db.maybe_prune(30, now).unwrap(), 1, "the first run prunes");
        assert_eq!(db.maybe_prune(30, now + 3600).unwrap(), 0, "an hour later it does not");
        assert_eq!(db.maybe_prune(30, now + DAY + 1).unwrap(), 0, "nothing left to prune");
    }

    #[test]
    fn maybe_prune_disabled_by_zero_retention() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 0).unwrap();
        assert_eq!(db.maybe_prune(0, 10_000 * 86_400).unwrap(), 0);
        assert_eq!(db.samples_between(0, i64::MAX).unwrap().len(), 1);
    }

    #[test]
    fn backup_produces_a_readable_copy() {
        let dir = std::env::temp_dir().join(format!("cs-backup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: single-threaded test process; the variable steers `paths`.
        unsafe { std::env::set_var("CLAUDE_STATUS_DIR", &dir) };

        let db = Db::open(dir.join("usage.sqlite3")).unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();

        // Taking a backup twice must not fail on the existing file.
        db.backup().unwrap();
        let backup = db.backup().unwrap();

        db.clear_samples().unwrap();
        assert!(db.latest().unwrap().is_none(), "the live database is empty");

        let restored = Db::open(&backup).unwrap();
        assert_eq!(
            restored.latest().unwrap().unwrap().five_pct,
            Some(10.0),
            "the copy still holds what was wiped"
        );

        unsafe { std::env::remove_var("CLAUDE_STATUS_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Resetting the statistics must stay narrow: it wipes the samples and
    /// leaves every other piece of state alone, including the bookkeeping that
    /// nothing else would restore.
    #[test]
    fn clear_wipes_samples_but_keeps_the_token_mirror() {
        let mut db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.upsert_daily_tokens(&[("2026-08-01".into(), "claude-opus-5".into(), 100)]).unwrap();
        db.meta_set("last_prune_ts", "42").unwrap();

        assert_eq!(db.clear_samples().unwrap(), 1);
        assert!(db.latest().unwrap().is_none());
        assert!(db.current_sample().unwrap().is_none());
        assert_eq!(db.daily_tokens_since("2026-01-01").unwrap().len(), 1, "mirror survives");
        assert_eq!(db.meta_get("last_prune_ts").unwrap().as_deref(), Some("42"), "meta survives");
    }

    #[test]
    fn meta_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.meta_get("k").unwrap(), None);
        db.meta_set("k", "v1").unwrap();
        db.meta_set("k", "v2").unwrap();
        assert_eq!(db.meta_get("k").unwrap().as_deref(), Some("v2"));
    }
}
