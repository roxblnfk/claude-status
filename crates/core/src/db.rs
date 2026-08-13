//! Storage for limit samples.
//!
//! Written by the hook process (one per Claude Code session), read by the GUI.
//! Hence WAL and a `busy_timeout`: several processes work with the database at
//! the same time.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::paths;
use crate::statusline::{SEVEN_DAY_SECS, StatuslineInput};
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
    /// Which model the weekly cap in `opus_*` applies to, when it is known.
    /// The status line never says; the probe does.
    pub scoped_model: Option<String>,
}

impl Sample {
    /// The last moment this reading was known to still hold.
    pub fn seen_until(&self) -> i64 {
        self.last_seen_ts.max(self.ts)
    }
}

/// Inclusive range of local days the aggregates are queried over, `YYYY-MM-DD`.
///
/// Dates are stored as text, and `YYYY-MM-DD` compares the same way as a date,
/// so a range needs no conversion on either side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span<'a> {
    pub from: &'a str,
    pub to: &'a str,
}

impl<'a> Span<'a> {
    pub fn new(from: &'a str, to: &'a str) -> Self {
        Self { from, to }
    }

    /// Everything ever counted. The bounds sit outside any real date rather
    /// than being absent, which keeps one query where two would otherwise be.
    pub const ALL: Span<'static> = Span { from: "0000-00-00", to: "9999-99-99" };
}

/// Usage summed over one grouping — a model, or a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Totals {
    /// Whatever the rows were grouped by.
    pub name: String,
    pub messages: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// The part of the total that subagents spent.
    pub agent_tokens: i64,
}

impl Totals {
    /// Cache traffic counts: it is what the context costs to carry, and on a
    /// long session it dwarfs everything typed.
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    pub fn cache(&self) -> i64 {
        self.cache_read + self.cache_write
    }

    /// How much of it went to subagents, 0..100. A dispatched agent reads and
    /// writes on a budget of its own, and that can be most of what a session
    /// spends without anything on screen saying so.
    pub fn agent_share(&self) -> f64 {
        match self.total() {
            0 => 0.0,
            total => self.agent_tokens as f64 / total as f64 * 100.0,
        }
    }
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

/// Marks that the one-off rounding of stored boundaries has run.
const BOUNDARIES_ROUNDED: &str = "boundaries_rounded";

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

            -- Assistant messages already counted, by the id the API gave them.
            -- Resuming a session copies the history into a log of its own, so
            -- the same message comes back under a new file — in one project
            -- 2829 messages carried 1368 distinct ids. Counting per id rather
            -- than per line is what keeps the totals honest, and it makes a
            -- scan idempotent: re-reading a log adds nothing.
            CREATE TABLE IF NOT EXISTS counted_messages (
                id TEXT PRIMARY KEY
            );

            -- Token usage aggregated as it is read. The project comes from the
            -- `cwd` in the log, so it reads as a path rather than as the
            -- mangled directory name Claude Code files sessions under.
            -- `agent` splits what a dispatched subagent spent from what the
            -- session itself did: it is a dimension rather than a column of its
            -- own so that the share can be asked for over any slice — a day, a
            -- project, one model.
            CREATE TABLE IF NOT EXISTS usage_by_day (
                date        TEXT NOT NULL,
                project     TEXT NOT NULL,
                model       TEXT NOT NULL,
                agent       INTEGER NOT NULL,
                messages    INTEGER NOT NULL,
                input       INTEGER NOT NULL,
                output      INTEGER NOT NULL,
                cache_read  INTEGER NOT NULL,
                cache_write INTEGER NOT NULL,
                PRIMARY KEY (date, project, model, agent)
            );

            -- Which days a session log was active on, for the sessions-per-day
            -- count. A session spanning midnight belongs to both days.
            CREATE TABLE IF NOT EXISTS session_days (
                path TEXT NOT NULL,
                date TEXT NOT NULL,
                PRIMARY KEY (path, date)
            );

            -- How far each log has been read. Logs are append-only, so the
            -- next scan starts where the last one stopped instead of parsing
            -- hundreds of megabytes again.
            CREATE TABLE IF NOT EXISTS scanned_logs (
                path   TEXT PRIMARY KEY,
                offset INTEGER NOT NULL,
                mtime  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;

        // Added once the probe started reporting which model the weekly cap is
        // scoped to; `IF NOT EXISTS` has no counterpart for columns.
        if !self.has_column("samples", "scoped_model")? {
            self.conn.execute_batch("ALTER TABLE samples ADD COLUMN scoped_model TEXT;")?;
        }

        // A mirror of `stats-cache.json` that the plots once read. Counting the
        // logs replaced it; the empty table is left over in every database that
        // has been through an earlier version.
        self.conn.execute_batch("DROP TABLE IF EXISTS daily_tokens;")?;

        // `agent` joined the key of `usage_by_day` after the table shipped, and
        // a key cannot be widened in place. Everything in these four tables can
        // be read again from the logs, so the cheapest correct answer is to
        // drop them together and let the next scan refill them — half a minute,
        // once. Dropping `counted_messages` along with them is what makes that
        // work: left behind, it would call every message a repeat.
        if self.has_column("usage_by_day", "date")? && !self.has_column("usage_by_day", "agent")? {
            self.conn.execute_batch(
                "DROP TABLE usage_by_day;
                 DROP TABLE IF EXISTS counted_messages;
                 DROP TABLE IF EXISTS session_days;
                 DROP TABLE IF EXISTS scanned_logs;",
            )?;
            self.meta_set("last_scan_ts", "0")?;
            return self.migrate();
        }

        // Rows written before [`boundary`] existed still hold the raw seconds,
        // and one window scattered across three of them keeps reading wrong
        // until they are brought onto the same grain. Once is enough — the flag
        // is what keeps every later open from rewriting the whole table.
        if self.meta_get(BOUNDARIES_ROUNDED)?.is_none() {
            self.round_stored_boundaries()?;
            self.meta_set(BOUNDARIES_ROUNDED, "1")?;
        }
        Ok(())
    }

    /// Brings boundaries already on disk onto the minute [`boundary`] rounds to.
    fn round_stored_boundaries(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE samples SET
                 five_resets_at = (five_resets_at + 30) / 60 * 60,
                 week_resets_at = (week_resets_at + 30) / 60 * 60,
                 opus_resets_at = (opus_resets_at + 30) / 60 * 60
             WHERE five_resets_at % 60 <> 0
                OR week_resets_at % 60 <> 0
                OR opus_resets_at % 60 <> 0",
            [],
        )?)
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        Ok(names.any(|name| name.is_ok_and(|n| n == column)))
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
        let five_resets_at = boundary(five.and_then(|w| w.resets_at));
        let week_pct = week.and_then(|w| w.used_percentage);
        let week_resets_at = boundary(week.and_then(|w| w.resets_at));
        let opus_pct = opus.and_then(|w| w.used_percentage);
        let opus_resets_at = boundary(opus.and_then(|w| w.resets_at));

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

    /// Stores what a probe brought back.
    ///
    /// Filed under a session id of its own so that deduplication compares one
    /// probe with the previous probe rather than with whatever a Claude Code
    /// session happened to write in between.
    pub fn record_probe(&self, usage: &crate::probe::Usage, now: i64) -> Result<Written> {
        if usage.is_empty() {
            return Ok(Written::Skipped);
        }

        let five_pct = usage.five_hour.map(|w| w.used_pct);
        let five_resets_at = boundary(usage.five_hour.map(|w| w.resets_at));
        let week_pct = usage.seven_day.map(|w| w.used_pct);
        let week_resets_at = boundary(usage.seven_day.map(|w| w.resets_at));
        let scoped_model = usage.scoped.as_ref().map(|(model, _)| model.clone());
        let opus_pct = usage.scoped.as_ref().map(|(_, w)| w.used_pct);
        let opus_resets_at = boundary(usage.scoped.as_ref().map(|(_, w)| w.resets_at));

        if let Some(last) = self.latest_for_session(Some(crate::probe::SOURCE))?
            && same_state(&last, five_pct, five_resets_at, week_pct, week_resets_at, opus_pct)
        {
            self.conn.execute(
                "UPDATE samples SET last_seen_ts = ?1 WHERE id = ?2",
                params![now.max(last.last_seen_ts), last.id],
            )?;
            return Ok(Written::Touched);
        }

        self.conn.execute(
            r#"INSERT INTO samples (
                   ts, last_seen_ts, five_pct, five_resets_at, week_pct, week_resets_at,
                   opus_pct, opus_resets_at, session_id, scoped_model
               ) VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                now,
                five_pct,
                five_resets_at,
                week_pct,
                week_resets_at,
                opus_pct,
                opus_resets_at,
                crate::probe::SOURCE,
                scoped_model,
            ],
        )?;
        Ok(Written::Inserted)
    }

    /// When the session logs were last counted, unix seconds.
    pub fn last_scan_ts(&self) -> Result<i64> {
        Ok(self.meta_get("last_scan_ts")?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn set_last_scan_ts(&self, now: i64) -> Result<()> {
        self.meta_set("last_scan_ts", &now.to_string())
    }

    /// When the last probe ran, unix seconds.
    pub fn last_probe_ts(&self) -> Result<i64> {
        Ok(self.meta_get("last_probe_ts")?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub fn set_last_probe_ts(&self, now: i64) -> Result<()> {
        self.meta_set("last_probe_ts", &now.to_string())
    }

    /// The model the weekly scoped cap applies to, as last reported.
    pub fn scoped_model(&self) -> Result<Option<String>> {
        let name = self
            .conn
            .query_row(
                "SELECT scoped_model FROM samples
                 WHERE scoped_model IS NOT NULL ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(name)
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
        // Among equal peaks the one confirmed latest: the pace is averaged up
        // to the last observation, and the stretch where the level held is part
        // of the average rather than something to cut away.
        let peak = pick("week_pct DESC, last_seen_ts DESC, ts DESC, id DESC")?;

        Ok(match (first, peak) {
            (Some(first), Some(peak)) if first.id != peak.id => vec![first, peak],
            _ => vec![current],
        })
    }

    /// The weekly percentage the day beginning at `midnight` started from.
    ///
    /// The flag says the level was estimated rather than read: it is `false`
    /// when a reading from before midnight exists, or when the week itself
    /// began after midnight and therefore stood at zero.
    pub fn week_baseline(&self, resets_at: i64, midnight: i64) -> Result<Option<(f64, bool)>> {
        // The highest reading before midnight, for the same reason the current
        // state is a maximum: idle sessions keep repeating stale snapshots.
        let recorded: Option<f64> = self.conn.query_row(
            "SELECT MAX(week_pct) FROM samples
             WHERE week_resets_at IS ?1 AND ts <= ?2 AND week_pct IS NOT NULL",
            params![resets_at, midnight],
            |row| row.get(0),
        )?;
        if let Some(pct) = recorded {
            return Ok(Some((pct, false)));
        }

        let window_start = resets_at - SEVEN_DAY_SECS;
        if window_start >= midnight {
            return Ok(Some((0.0, false)));
        }

        // Nothing was recorded before midnight — collecting started later than
        // the week did. The best that can be said is that the usage seen at the
        // first reading accumulated evenly since the week began.
        let first: Option<(i64, f64)> = self
            .conn
            .query_row(
                "SELECT ts, week_pct FROM samples
                 WHERE week_resets_at IS ?1 AND week_pct IS NOT NULL
                 ORDER BY ts, id LIMIT 1",
                params![resets_at],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((ts, pct)) = first else { return Ok(None) };

        let observed = (ts - window_start) as f64;
        if observed <= 0.0 {
            return Ok(Some((0.0, true)));
        }
        let share = ((midnight - window_start) as f64 / observed).clamp(0.0, 1.0);
        Ok(Some((pct * share, true)))
    }

    /// The summary every entry point shows: current state, pace and today's
    /// share of the week.
    pub fn overview(&self, now: i64) -> Result<crate::pace::Overview> {
        let Some(current) = self.current_sample()? else {
            return Ok(crate::pace::Overview::default());
        };
        let mut overview = crate::pace::Overview::new(&current, &self.burn_endpoints()?, now);

        if let Some(week) = overview.week.filter(|w| !w.is_expired()) {
            let midnight = crate::timefmt::start_of_local_day(now);
            overview.daily = self.week_baseline(week.resets_at, midnight)?.map(
                |(at_midnight, estimated)| {
                    crate::pace::DailyBudget::new(&week, at_midnight, estimated, midnight)
                },
            );
        }
        Ok(overview)
    }

    /// How far every session log has been read: path → (offset, mtime).
    ///
    /// Taken in one query rather than per file: the answer decides which of a
    /// few hundred logs are worth opening at all, and a query each would cost
    /// more than reading the ones that changed.
    pub fn scan_progress(&self) -> Result<HashMap<String, (i64, i64)>> {
        let mut stmt = self.conn.prepare("SELECT path, offset, mtime FROM scanned_logs")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))
        })?;
        Ok(rows.collect::<Result<HashMap<_, _>, _>>()?)
    }

    /// Stores what one session log yielded. Returns the messages actually
    /// counted — the ones not seen in some other log before.
    pub fn record_log_scan(
        &mut self,
        path: &str,
        offset: i64,
        mtime: i64,
        messages: &[crate::scan::Message],
    ) -> Result<usize> {
        let mut counted = 0;
        let tx = self.conn.transaction()?;
        {
            let mut seen =
                tx.prepare("INSERT OR IGNORE INTO counted_messages (id) VALUES (?1)")?;
            let mut aggregate = tx.prepare(
                "INSERT INTO usage_by_day
                     (date, project, model, agent,
                      messages, input, output, cache_read, cache_write)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8)
                 ON CONFLICT(date, project, model, agent) DO UPDATE SET
                     messages    = messages + 1,
                     input       = input + excluded.input,
                     output      = output + excluded.output,
                     cache_read  = cache_read + excluded.cache_read,
                     cache_write = cache_write + excluded.cache_write",
            )?;
            // Only days that brought something new. A resumed session carries a
            // copy of the history it continues, and counting its days would add
            // that session to every day it merely quotes.
            let mut active =
                tx.prepare("INSERT OR IGNORE INTO session_days (path, date) VALUES (?1, ?2)")?;

            for m in messages {
                if seen.execute(params![m.id])? == 0 {
                    continue;
                }
                aggregate.execute(params![
                    m.date,
                    m.project,
                    m.model,
                    m.agent,
                    m.input,
                    m.output,
                    m.cache_read,
                    m.cache_write
                ])?;
                active.execute(params![path, m.date])?;
                counted += 1;
            }

            tx.execute(
                "INSERT INTO scanned_logs (path, offset, mtime) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET offset = excluded.offset, mtime = excluded.mtime",
                params![path, offset, mtime],
            )?;
        }
        tx.commit()?;
        Ok(counted)
    }

    /// Tokens per day and model within `span`, inclusive.
    pub fn tokens_per_day(&self, span: Span<'_>) -> Result<Vec<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, model, SUM(input + output + cache_read + cache_write)
             FROM usage_by_day WHERE date >= ?1 AND date <= ?2
             GROUP BY date, model ORDER BY date, model",
        )?;
        let rows = stmt
            .query_map(params![span.from, span.to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sessions and messages per day within `span`.
    pub fn activity_per_day(&self, span: Span<'_>) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.date,
                    (SELECT COUNT(*) FROM session_days s WHERE s.date = d.date),
                    SUM(d.messages)
             FROM usage_by_day d WHERE d.date >= ?1 AND d.date <= ?2
             GROUP BY d.date ORDER BY d.date",
        )?;
        let rows = stmt
            .query_map(params![span.from, span.to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Totals per model over `span`, busiest first. `project` narrows to one.
    pub fn totals_by_model(&self, span: Span<'_>, project: Option<&str>) -> Result<Vec<Totals>> {
        self.totals("model", span, project)
    }

    /// Totals per project over `span`, busiest first.
    pub fn totals_by_project(&self, span: Span<'_>) -> Result<Vec<Totals>> {
        self.totals("project", span, None)
    }

    fn totals(&self, column: &str, span: Span<'_>, project: Option<&str>) -> Result<Vec<Totals>> {
        // `?3 IS NULL OR project = ?3` rather than two statements: the filter is
        // the only difference between them, and SQLite plans it the same way.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {column}, SUM(messages), SUM(input), SUM(output),
                    SUM(cache_read), SUM(cache_write),
                    SUM(CASE WHEN agent THEN input + output + cache_read + cache_write
                             ELSE 0 END)
             FROM usage_by_day
             WHERE date >= ?1 AND date <= ?2 AND (?3 IS NULL OR project = ?3)
             GROUP BY {column}
             ORDER BY SUM(input + output + cache_read + cache_write) DESC"
        ))?;
        let rows = stmt.query_map(params![span.from, span.to, project], |r| {
            Ok(Totals {
                name: r.get(0)?,
                messages: r.get(1)?,
                input: r.get(2)?,
                output: r.get(3)?,
                cache_read: r.get(4)?,
                cache_write: r.get(5)?,
                agent_tokens: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One row summing everything in `span`, for the share subagents took.
    pub fn overall_totals(&self, span: Span<'_>, project: Option<&str>) -> Result<Totals> {
        let mut stmt = self.conn.prepare(
            "SELECT SUM(messages), SUM(input), SUM(output), SUM(cache_read), SUM(cache_write),
                    SUM(CASE WHEN agent THEN input + output + cache_read + cache_write
                             ELSE 0 END)
             FROM usage_by_day
             WHERE date >= ?1 AND date <= ?2 AND (?3 IS NULL OR project = ?3)",
        )?;
        let totals = stmt.query_row(params![span.from, span.to, project], |r| {
            Ok(Totals {
                name: String::new(),
                messages: r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                input: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                output: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                cache_read: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                cache_write: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                agent_tokens: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            })
        })?;
        Ok(totals)
    }

    /// Forgets everything counted, so the next scan reads every log afresh.
    ///
    /// The counted figures are not merely a cache: a day whose log Claude Code
    /// has since deleted lives here and nowhere else, and this drops it. Worth
    /// it only when the count itself is in doubt.
    pub fn forget_counted_usage(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM usage_by_day;
             DELETE FROM counted_messages;
             DELETE FROM session_days;
             DELETE FROM scanned_logs;",
        )?;
        self.set_last_scan_ts(0)
    }

    /// The day the counted history starts, if anything has been counted.
    pub fn first_counted_day(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT MIN(date) FROM usage_by_day", [], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// Wipes the collected history.
    ///
    /// Only the samples we gathered ourselves. The counted usage stays: the
    /// logs it came from are Claude Code's to delete, and once they are gone
    /// nothing could bring those figures back.
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
                       ctx_input, ctx_output, ctx_size, version, scoped_model";

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
        scoped_model: row.get(16)?,
    })
}

/// Window boundaries are stored rounded to the minute.
///
/// The status line reports the reset as whole seconds and always the same ones,
/// but the probe reads an RFC 3339 timestamp that Claude Code recomputes for
/// every answer, so one and the same reset arrives as 18:59:59, 19:00:00 or
/// 19:00:01. Stored verbatim, each variant reads as a window of its own:
/// [`Db::current_sample`] follows the highest boundary and lands in whichever
/// variant happens to be latest, blind to everything the other two hold — one
/// stray second showed the week at 14 % while it stood at 27 %.
///
/// Real boundaries fall on ten-minute marks, so a minute is a grain no genuine
/// reset can cross.
fn boundary(resets_at: Option<i64>) -> Option<i64> {
    resets_at.map(|ts| (ts + 30).div_euclid(60) * 60)
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

    fn message(id: &str, date: &str, project: &str, input: i64) -> crate::scan::Message {
        crate::scan::Message {
            id: id.into(),
            project: project.into(),
            date: date.into(),
            model: "claude-opus-5".into(),
            agent: false,
            input,
            output: 1,
            cache_read: 10,
            cache_write: 2,
        }
    }

    fn by_agent(id: &str, date: &str, project: &str, input: i64) -> crate::scan::Message {
        crate::scan::Message { agent: true, ..message(id, date, project, input) }
    }

    /// Resuming a session copies the history it continues into a log of its
    /// own, so the same ids arrive twice. Counting them twice would roughly
    /// double every figure the window shows.
    #[test]
    fn a_message_counted_once_is_never_counted_again() {
        let mut db = Db::open_in_memory().unwrap();
        let first = [message("m1", "2026-08-01", "demo", 100), message("m2", "2026-08-01", "demo", 5)];
        assert_eq!(db.record_log_scan("a.jsonl", 500, 7, &first).unwrap(), 2);

        // The resumed log repeats m2 and brings one message of its own.
        let second = [message("m2", "2026-08-01", "demo", 5), message("m3", "2026-08-02", "demo", 7)];
        assert_eq!(db.record_log_scan("b.jsonl", 900, 8, &second).unwrap(), 1, "only the new one");

        let totals = db.totals_by_model(Span::ALL, None).unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].messages, 3);
        assert_eq!(totals[0].input, 112, "the repeat added nothing");
        assert_eq!(totals[0].total(), 112 + 3 + 30 + 6);
    }

    /// What a dispatched subagent spends is the session's spending too, but it
    /// is worth telling apart: on some projects it is most of the bill.
    #[test]
    fn subagent_usage_is_counted_apart_from_the_session() {
        let mut db = Db::open_in_memory().unwrap();
        db.record_log_scan(
            "a.jsonl",
            1,
            1,
            &[message("m1", "2026-08-01", "demo", 100), by_agent("m2", "2026-08-01", "demo", 300)],
        )
        .unwrap();

        let overall = db.overall_totals(Span::ALL, None).unwrap();
        assert_eq!(overall.input, 400, "both sides are in the total");
        assert_eq!(overall.agent_tokens, 300 + 1 + 10 + 2);
        // 313 of the 426 tokens counted are the agent's.
        assert!((overall.agent_share() - 73.5).abs() < 0.1, "{}", overall.agent_share());

        // The split survives grouping: the model row carries it as well.
        let per_model = db.totals_by_model(Span::ALL, None).unwrap();
        assert_eq!(per_model[0].agent_tokens, overall.agent_tokens);
    }

    /// Re-reading a log from the start — what happens when one is truncated or
    /// rewritten — has to be harmless.
    #[test]
    fn rescanning_the_same_log_changes_nothing() {
        let mut db = Db::open_in_memory().unwrap();
        let messages = [message("m1", "2026-08-01", "demo", 100)];
        db.record_log_scan("a.jsonl", 500, 7, &messages).unwrap();
        assert_eq!(db.record_log_scan("a.jsonl", 500, 9, &messages).unwrap(), 0);

        assert_eq!(db.totals_by_model(Span::ALL, None).unwrap()[0].input, 100);
    }

    #[test]
    fn usage_is_grouped_by_day_project_and_model() {
        let mut db = Db::open_in_memory().unwrap();
        db.record_log_scan(
            "a.jsonl",
            10,
            1,
            &[
                message("m1", "2026-08-01", "alpha", 100),
                message("m2", "2026-08-01", "beta", 50),
                message("m3", "2026-08-02", "alpha", 7),
            ],
        )
        .unwrap();

        let per_day = db.tokens_per_day(Span::new("2026-08-01", "2026-08-31")).unwrap();
        assert_eq!(per_day.len(), 2, "one row per day, both projects summed");
        assert_eq!(per_day[0].2, 100 + 50 + 2 * (1 + 10 + 2));

        let projects = db.totals_by_project(Span::ALL).unwrap();
        assert_eq!(projects[0].name, "alpha", "the busier project comes first");
        assert_eq!(projects[0].input, 107);
        assert_eq!(projects[1].name, "beta");

        let within = db.totals_by_model(Span::ALL, Some("beta")).unwrap();
        assert_eq!(within.len(), 1);
        assert_eq!(within[0].input, 50);
    }

    /// Sessions are counted by the logs that brought something new that day, so
    /// a resumed log does not add itself to every day it merely quotes.
    #[test]
    fn activity_counts_sessions_and_messages_per_day() {
        let mut db = Db::open_in_memory().unwrap();
        db.record_log_scan("a.jsonl", 1, 1, &[message("m1", "2026-08-01", "demo", 1)]).unwrap();
        db.record_log_scan(
            "b.jsonl",
            1,
            1,
            &[message("m1", "2026-08-01", "demo", 1), message("m2", "2026-08-01", "demo", 1)],
        )
        .unwrap();

        let activity = db.activity_per_day(Span::new("2026-08-01", "2026-08-31")).unwrap();
        assert_eq!(activity, vec![("2026-08-01".to_string(), 2, 2)]);
    }

    /// Samples expire — a percentage from last month says nothing. The counted
    /// usage must not: Claude Code deletes its own logs, so pruning what was
    /// read out of them would destroy the only remaining copy.
    #[test]
    fn pruning_samples_leaves_the_counted_usage_alone() {
        let mut db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.record_log_scan("a.jsonl", 10, 1, &[message("m1", "2020-01-01", "demo", 100)]).unwrap();

        assert_eq!(db.prune(1_000).unwrap(), 1, "the old sample goes");
        assert_eq!(db.totals_by_model(Span::ALL, None).unwrap()[0].input, 100, "the count stays");
        assert_eq!(db.first_counted_day().unwrap().as_deref(), Some("2020-01-01"));
    }

    /// A database from the version where `usage_by_day` had no `agent` column.
    /// The key cannot be widened in place, so the counted tables are dropped
    /// and the next scan refills them — what must survive is everything else.
    #[test]
    fn a_database_without_the_agent_column_is_rebuilt() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.meta_set("last_prune_ts", "42").unwrap();
        db.conn
            .execute_batch(
                "DROP TABLE usage_by_day;
                 CREATE TABLE usage_by_day (
                     date TEXT NOT NULL, project TEXT NOT NULL, model TEXT NOT NULL,
                     messages INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL,
                     cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
                     PRIMARY KEY (date, project, model));
                 INSERT INTO usage_by_day VALUES ('2026-08-01', 'demo', 'opus', 1, 5, 1, 1, 1);
                 INSERT INTO counted_messages (id) VALUES ('m1');",
            )
            .unwrap();

        db.migrate().unwrap();

        assert!(db.has_column("usage_by_day", "agent").unwrap());
        assert!(db.totals_by_model(Span::ALL, None).unwrap().is_empty(), "the counts are dropped");
        assert_eq!(db.last_scan_ts().unwrap(), 0, "so the next scan reads every log again");
        assert_eq!(db.samples_between(0, 200).unwrap().len(), 1, "samples are untouched");
        assert_eq!(db.meta_get("last_prune_ts").unwrap().as_deref(), Some("42"));
    }

    #[test]
    fn scan_progress_remembers_where_each_log_stopped() {
        let mut db = Db::open_in_memory().unwrap();
        db.record_log_scan("a.jsonl", 500, 7, &[]).unwrap();
        db.record_log_scan("a.jsonl", 900, 8, &[]).unwrap();
        db.record_log_scan("b.jsonl", 4, 1, &[]).unwrap();

        let progress = db.scan_progress().unwrap();
        assert_eq!(progress.get("a.jsonl"), Some(&(900, 8)));
        assert_eq!(progress.get("b.jsonl"), Some(&(4, 1)));
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
        db.record(&session_input("s", 2.0, 1.0, 5040), 200).unwrap();

        let current = db.current_sample().unwrap().unwrap();
        assert_eq!(current.five_pct, Some(2.0), "a reset window is not shadowed by the old peak");
        assert_eq!(current.five_resets_at, Some(5040));
    }

    fn probe_usage(week_pct: f64, resets_at: i64) -> crate::probe::Usage {
        crate::probe::Usage {
            seven_day: Some(crate::probe::Window { used_pct: week_pct, resets_at }),
            ..Default::default()
        }
    }

    /// The probe reads the reset off a timestamp Claude Code recomputes for
    /// every answer, so the same boundary arrives a second either way. Kept
    /// apart, the stray second reads as a window of its own, and being the
    /// highest it becomes the current one — the week showed 12 % while it stood
    /// at 27 %.
    #[test]
    fn a_reset_that_drifts_by_a_second_stays_one_window() {
        let db = Db::open_in_memory().unwrap();
        db.record_probe(&probe_usage(12.0, 1_786_978_801), 100).unwrap();
        db.record_probe(&probe_usage(27.0, 1_786_978_799), 200).unwrap();

        let current = db.current_sample().unwrap().unwrap();
        assert_eq!(current.week_resets_at, Some(1_786_978_800));
        assert_eq!(current.week_pct, Some(27.0), "the stray second must not shadow the window");
    }

    /// The same drift on rows written before the rounding existed: the
    /// migration has to fold them back onto one boundary, or a database that
    /// has already been collecting keeps reading wrong.
    #[test]
    fn boundaries_already_stored_are_rounded_once() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO samples (ts, last_seen_ts, week_pct, week_resets_at,
                                      five_pct, five_resets_at)
                 VALUES (100, 100, 27.0, 1786978799, 4.0, 1786552201)",
                [],
            )
            .unwrap();

        // Pretend the row predates the migration, then let it run again.
        db.conn.execute("DELETE FROM meta WHERE key = ?1", params![BOUNDARIES_ROUNDED]).unwrap();
        db.migrate().unwrap();

        let stored = db.latest().unwrap().unwrap();
        assert_eq!(stored.week_resets_at, Some(1_786_978_800));
        assert_eq!(stored.five_resets_at, Some(1_786_552_200));
        assert_eq!(db.round_stored_boundaries().unwrap(), 0, "nothing is left to round");
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
    fn burn_endpoints_take_the_latest_confirmation_of_the_peak() {
        let db = Db::open_in_memory().unwrap();
        // The week stops at 40 while the five-hour window keeps moving, so the
        // peak is repeated by several rows. The pace has to span all of them:
        // the stretch where the week stood still is exactly what keeps the rate
        // from being read off a four-minute burst.
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.record(&input(20.0, 40.0, 999), 200).unwrap();
        db.record(&input(30.0, 40.0, 999), 300).unwrap();
        db.record(&input(40.0, 40.0, 999), 400).unwrap();

        let bounds = db.burn_endpoints().unwrap();
        assert_eq!(bounds[0].ts, 100);
        assert_eq!(bounds[1].ts, 400, "the peak is taken where it was last confirmed");
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

    /// A week that began at t=10_200, so the dates below sit inside it. The
    /// offset is a whole number of minutes because that is the grain
    /// boundaries are stored on.
    const WEEK_RESETS: i64 = SEVEN_DAY_SECS + 10_200;

    #[test]
    fn baseline_is_the_level_recorded_before_midnight() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, WEEK_RESETS), 20_000).unwrap();
        db.record(&input(20.0, 30.0, WEEK_RESETS), 40_000).unwrap();

        let (pct, estimated) = db.week_baseline(WEEK_RESETS, 30_000).unwrap().unwrap();
        assert_eq!(pct, 20.0);
        assert!(!estimated, "the level was read, not guessed");
    }

    #[test]
    fn baseline_is_zero_when_the_week_began_after_midnight() {
        let db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, WEEK_RESETS), 20_000).unwrap();

        // Midnight came before the window existed, so it started the day empty.
        let (pct, estimated) = db.week_baseline(WEEK_RESETS, 5_000).unwrap().unwrap();
        assert_eq!(pct, 0.0);
        assert!(!estimated);
    }

    #[test]
    fn baseline_is_estimated_when_collecting_started_late() {
        let db = Db::open_in_memory().unwrap();
        // The week began at 10_200, the first reading is at 49_800 and shows
        // 40 %. Midnight at 30_000 is halfway through that stretch, so about
        // half of the 40 % is assumed to have been there already.
        db.record(&input(10.0, 40.0, WEEK_RESETS), 49_800).unwrap();

        let (pct, estimated) = db.week_baseline(WEEK_RESETS, 30_000).unwrap().unwrap();
        assert!((pct - 20.0).abs() < 1e-6, "{pct}");
        assert!(estimated, "there was nothing to read it from");
    }

    #[test]
    fn baseline_is_absent_without_any_sample() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.week_baseline(WEEK_RESETS, 30_000).unwrap(), None);
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
    fn clear_wipes_samples_but_keeps_the_counted_usage() {
        let mut db = Db::open_in_memory().unwrap();
        db.record(&input(10.0, 20.0, 999), 100).unwrap();
        db.record_log_scan("a.jsonl", 10, 1, &[message("m1", "2026-08-01", "demo", 100)]).unwrap();
        db.meta_set("last_prune_ts", "42").unwrap();

        assert_eq!(db.clear_samples().unwrap(), 1);
        assert!(db.latest().unwrap().is_none());
        assert!(db.current_sample().unwrap().is_none());
        assert_eq!(db.totals_by_model(Span::ALL, None).unwrap().len(), 1, "counted usage survives");
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
