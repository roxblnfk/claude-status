//! Asking Claude Code itself for the current limits.
//!
//! The status line is the cheap source, but it only reaches commands started by
//! the terminal client — a session hosted by an editor over the agent SDK never
//! renders one, and its limits never leave the process. Claude Code does answer
//! a control request for the same data it shows under `/usage`, so a short-lived
//! instance can be asked directly.
//!
//! It is not cheap: a few seconds and a few hundred megabytes for the duration.
//! Nothing here decides *when* to run — see [`crate::probe::due`].

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::config::ProbeConfig;
use crate::pace::Overview;
use crate::tr;

/// The session id samples from the probe are filed under.
///
/// Deduplication compares a reading with the previous one *from the same
/// session*; giving the probe a lane of its own keeps it from colliding with
/// the running Claude Code sessions.
pub const SOURCE: &str = "claude-status-probe";

/// Environment override for the Claude Code executable.
pub const BINARY_ENV: &str = "CLAUDE_STATUS_CLAUDE_BIN";

/// The control request Claude Code answers with the data behind `/usage`.
///
/// The SDK wraps it in a method whose name shouts that the API is unstable, so
/// a failure here is expected to be survivable rather than fatal.
const REQUEST: &str =
    r#"{"type":"control_request","request_id":"claude-status","request":{"subtype":"get_usage"}}"#;

/// One limit window as the probe reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    pub used_pct: f64,
    pub resets_at: i64,
}

/// What a probe brought back.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// The weekly cap that applies to one model, with the name of that model.
    /// The status line never carried this one at all.
    pub scoped: Option<(String, Window)>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.five_hour.is_none() && self.seven_day.is_none() && self.scoped.is_none()
    }
}

/// Whether the heavy probe is worth running.
///
/// The rule is "only when the cheap source has nothing to say": while the hook
/// keeps delivering readings that describe windows still open, spawning a whole
/// Claude Code costs seconds and hundreds of megabytes to learn nothing.
pub fn due(cfg: &ProbeConfig, overview: &Overview, last_probe_ts: i64, now: i64) -> bool {
    if !cfg.enabled {
        return false;
    }
    if now - last_probe_ts < cfg.interval_secs() as i64 {
        return false;
    }

    let Some(age) = overview.staleness_secs(now) else {
        return true; // nothing has ever been collected
    };
    if age > cfg.fresh_secs as i64 {
        return true;
    }
    // A reading can be seconds old and still describe a window that closed
    // hours ago: an idle session keeps resending what it last saw.
    [overview.five_hour, overview.week].iter().flatten().any(|w| w.is_expired())
}

/// Runs one probe. Blocks for as long as Claude Code takes to start.
pub fn run(timeout: Duration) -> Result<Usage> {
    let exe = binary();
    let mut child = spawn(&exe)?;

    child
        .stdin
        .take()
        .context("stdin of the probe was not piped")?
        .write_all(format!("{REQUEST}\n").as_bytes())
        .context("could not send the control request")?;

    let stdout = child.stdout.take().context("stdout of the probe was not piped")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            // Claude Code narrates startup on the same stream; the answer is
            // the only line we care about.
            if !line.contains("control_response") {
                continue;
            }
            let _ = tx.send(line);
            return;
        }
    });

    let answer = rx.recv_timeout(timeout);
    // The instance has served its purpose either way; it would otherwise sit
    // waiting for input that is never coming.
    let _ = child.kill();
    let _ = child.wait();

    let line = answer.map_err(|_| anyhow::anyhow!(tr("probe.error.timeout")))?;
    parse(&line)
}

fn spawn(exe: &PathBuf) -> Result<Child> {
    let mut command = Command::new(exe);
    command
        .args(["--print", "--input-format", "stream-json", "--output-format", "stream-json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    command.spawn().with_context(|| tr("probe.error.spawn"))
}

/// Keeps the probe from flashing a console window.
///
/// Claude Code is a console program, so Windows gives it a window of its own
/// whatever the parent is — a black rectangle appearing and vanishing every
/// quarter of an hour. All three streams are redirected anyway, so nothing is
/// lost by denying it one.
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// Where to find Claude Code.
///
/// The native installer drops a file with no extension, which Windows will not
/// resolve from `PATH` on its own, so the known location is tried explicitly
/// before falling back to whatever `PATH` offers.
fn binary() -> PathBuf {
    if let Ok(path) = std::env::var(BINARY_ENV)
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    if let Some(home) = dirs::home_dir() {
        for name in ["claude.exe", "claude"] {
            let candidate = home.join(".local").join("bin").join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("claude")
}

fn parse(line: &str) -> Result<Usage> {
    let envelope: Envelope =
        serde_json::from_str(line).with_context(|| tr("probe.error.unreadable"))?;

    if envelope.response.subtype.as_deref() != Some("success") {
        bail!(envelope.response.error.unwrap_or_else(|| tr("probe.error.refused")));
    }
    let Some(limits) = envelope.response.response.and_then(|r| r.rate_limits) else {
        bail!(tr("probe.error.no_limits"));
    };

    let scoped = limits
        .limits
        .iter()
        .filter(|l| l.kind.as_deref() == Some("weekly_scoped"))
        // Several models can be capped; the one in force is the one to show.
        .find(|l| l.is_active.unwrap_or(false))
        .and_then(|l| {
            let name = l.scope.as_ref()?.model.as_ref()?.display_name.clone()?;
            Some((name, window(l.percent, l.resets_at.as_deref())?))
        });

    Ok(Usage {
        five_hour: limits.five_hour.and_then(Raw::into_window),
        seven_day: limits.seven_day.and_then(Raw::into_window),
        scoped,
    })
}

fn window(pct: Option<f64>, resets_at: Option<&str>) -> Option<Window> {
    Some(Window { used_pct: pct?, resets_at: unix(resets_at?)? })
}

/// The probe speaks RFC 3339 where the status line speaks unix seconds.
fn unix(text: &str) -> Option<i64> {
    OffsetDateTime::parse(text, &Rfc3339).ok().map(|t| t.unix_timestamp())
}

#[derive(Deserialize)]
struct Envelope {
    response: ResponseEnvelope,
}

#[derive(Deserialize)]
struct ResponseEnvelope {
    subtype: Option<String>,
    error: Option<String>,
    response: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    rate_limits: Option<Limits>,
}

#[derive(Deserialize)]
struct Limits {
    five_hour: Option<Raw>,
    seven_day: Option<Raw>,
    #[serde(default)]
    limits: Vec<Entry>,
}

#[derive(Deserialize)]
struct Raw {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

impl Raw {
    fn into_window(self) -> Option<Window> {
        window(self.utilization, self.resets_at.as_deref())
    }
}

#[derive(Deserialize)]
struct Entry {
    kind: Option<String>,
    percent: Option<f64>,
    resets_at: Option<String>,
    is_active: Option<bool>,
    scope: Option<Scope>,
}

#[derive(Deserialize)]
struct Scope {
    model: Option<Model>,
}

#[derive(Deserialize)]
struct Model {
    display_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Sample;

    /// Trimmed to the fields that are read, in the shape the probe returns.
    const ANSWER: &str = r#"{"type":"control_response","response":{"subtype":"success",
        "request_id":"claude-status","response":{
        "session":{"total_cost_usd":0},"subscription_type":"team","rate_limits_available":true,
        "rate_limits":{
          "five_hour":{"utilization":16,"resets_at":"2026-08-09T17:40:00.386415+00:00"},
          "seven_day":{"utilization":63,"resets_at":"2026-08-10T15:00:00.386472+00:00"},
          "seven_day_opus":null,
          "limits":[
            {"kind":"session","percent":16,"resets_at":"2026-08-09T17:40:00.386415+00:00",
             "scope":null,"is_active":false},
            {"kind":"weekly_all","percent":63,"resets_at":"2026-08-10T15:00:00.386472+00:00",
             "scope":null,"is_active":false},
            {"kind":"weekly_scoped","percent":79,"resets_at":"2026-08-10T15:00:00.386691+00:00",
             "scope":{"model":{"id":null,"display_name":"Fable"}},"is_active":true}
          ]}}}}"#;

    #[test]
    fn reads_the_windows_and_the_scoped_cap() {
        let usage = parse(ANSWER).unwrap();

        let five = usage.five_hour.unwrap();
        assert_eq!(five.used_pct, 16.0);
        assert_eq!(five.resets_at, 1786297200, "RFC 3339 becomes unix seconds");
        assert_eq!(usage.seven_day.unwrap().used_pct, 63.0);

        let (model, scoped) = usage.scoped.unwrap();
        assert_eq!(model, "Fable", "the per-model cap the status line never carried");
        assert_eq!(scoped.used_pct, 79.0);
    }

    #[test]
    fn an_inactive_scoped_cap_is_not_taken() {
        let quiet = ANSWER.replace(r#""is_active":true"#, r#""is_active":false"#);
        assert!(parse(&quiet).unwrap().scoped.is_none());
    }

    #[test]
    fn a_refusal_is_an_error_not_an_empty_reading() {
        let refused = r#"{"type":"control_response","response":{"subtype":"error",
            "request_id":"claude-status","error":"get_usage is not supported in this context"}}"#;
        let message = parse(refused).unwrap_err().to_string();
        assert!(message.contains("not supported"), "{message}");
    }

    #[test]
    fn a_session_without_a_subscription_reports_no_limits() {
        let bare = r#"{"type":"control_response","response":{"subtype":"success",
            "request_id":"claude-status","response":{"rate_limits":null}}}"#;
        assert!(parse(bare).is_err());
    }

    fn overview_at(age_secs: i64, resets_in: i64, now: i64) -> Overview {
        let sample = Sample {
            id: 1,
            ts: now - age_secs,
            last_seen_ts: now - age_secs,
            five_pct: Some(10.0),
            five_resets_at: Some(now + resets_in),
            ..Sample::default()
        };
        Overview::from_samples(&[sample], now)
    }

    fn config() -> ProbeConfig {
        ProbeConfig { enabled: true, interval_secs: 900, fresh_secs: 300 }
    }

    #[test]
    fn fresh_readings_keep_the_probe_asleep() {
        let now = 10_000_000;
        let overview = overview_at(60, 3600, now);
        assert!(!due(&config(), &overview, 0, now), "the cheap source is doing its job");
    }

    #[test]
    fn stale_readings_wake_it() {
        let now = 10_000_000;
        assert!(due(&config(), &overview_at(3600, 3600, now), 0, now));
    }

    #[test]
    fn a_closed_window_wakes_it_however_fresh_the_reading() {
        let now = 10_000_000;
        // Recorded a second ago, but about a window that ended an hour back.
        assert!(due(&config(), &overview_at(1, -3600, now), 0, now));
    }

    #[test]
    fn the_interval_is_respected_even_with_nothing_collected() {
        let now = 10_000_000;
        let empty = Overview::default();
        assert!(due(&config(), &empty, now - 901, now));
        assert!(!due(&config(), &empty, now - 899, now), "too soon after the last probe");
    }

    #[test]
    fn disabled_means_never() {
        let now = 10_000_000;
        let cfg = ProbeConfig { enabled: false, ..config() };
        assert!(!due(&cfg, &Overview::default(), 0, now));
    }
}
