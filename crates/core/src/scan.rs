//! Counting tokens from the session logs Claude Code writes.
//!
//! `~/.claude/stats-cache.json` carries the same figures already aggregated,
//! and reading it is free — but Claude Code stopped recomputing it, and the
//! window then showed a week-old picture with no way to tell why. The logs
//! under `~/.claude/projects` are the ground truth: every assistant message is
//! there with its model and its token counts, written as it happens.
//!
//! The cost is that there are hundreds of megabytes of them, so this is not
//! something to run on a timer. Two things keep it cheap: logs are append-only,
//! so only the tail of a file that grew is read, and a line is parsed only once
//! it looks like it carries usage at all.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::Db;
use crate::{paths, timefmt, tr_args};

/// One assistant message worth counting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The id the API gave the message. Identity across logs: resuming a
    /// session copies the history into a new file, and only this tells the copy
    /// from a message that actually happened twice.
    pub id: String,
    /// The directory the session ran in, taken from `cwd` in the log.
    pub project: String,
    /// Local day, `YYYY-MM-DD`.
    pub date: String,
    pub model: String,
    /// Whether a dispatched subagent spent this rather than the session itself.
    /// Taken from where the log sits, which is the only place it is said.
    pub agent: bool,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// What a scan did, for the line the window shows afterwards.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Logs that had grown since the last scan.
    pub logs_read: usize,
    /// Logs left alone because neither size nor mtime had moved.
    pub logs_skipped: usize,
    /// Assistant messages found in the logs, copies included.
    pub parsed: usize,
    /// Messages counted — the ones not already seen in another log.
    pub messages: usize,
    /// Bytes actually parsed.
    pub bytes: u64,
}

/// Reads every session log that has changed and folds it into the database.
///
/// Returns `Ok(Report::default())` when there is no `projects` directory at all
/// — a Claude Code that has never run leaves none, which is not an error.
pub fn run(db: &mut Db) -> Result<Report> {
    let root = paths::claude_projects()?;
    if !root.is_dir() {
        return Ok(Report::default());
    }

    let mut report = Report::default();
    let progress = db.scan_progress()?;
    let mut projects = HashMap::new();

    for log in logs(&root) {
        let Ok(meta) = log.metadata() else { continue };
        let key = relative(&root, &log);
        let mtime = modified_secs(&meta);
        let size = meta.len() as i64;

        // A log that has neither grown nor been touched holds nothing new.
        // Hundreds of these are old sessions that will never change again.
        let seen = progress.get(&key).copied().unwrap_or((0, 0));
        if seen.0 >= size && seen.1 == mtime {
            report.logs_skipped += 1;
            continue;
        }

        // Reading a shrunken log from the start is safe rather than clever:
        // messages already counted are recognised by id and add nothing.
        let from = if seen.0 <= size { seen.0 } else { 0 };
        let (messages, offset) = read_from(&log, from, &key, &mut projects)?;

        report.parsed += messages.len();
        report.messages += db.record_log_scan(&key, offset, mtime, &messages)?;
        report.bytes += (offset - from).max(0) as u64;
        report.logs_read += 1;
    }

    db.set_last_scan_ts(timefmt::now())?;
    Ok(report)
}

/// Every `*.jsonl` anywhere under the projects directory.
///
/// The walk has to go all the way down rather than one level: a session that
/// spawned subagents keeps their logs in `<session>/subagents/`, and on this
/// machine that is 189 logs out of 321 — the majority of the tokens spent.
fn logs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => pending.push(path),
                Ok(_) if path.extension().is_some_and(|e| e == "jsonl") => found.push(path),
                _ => {}
            }
        }
    }
    found
}

/// The project a session belongs to: the repository above its working
/// directory.
///
/// Claude Code files a session by the directory it started in, so a session
/// begun in `runtime/html-report` inside a checkout reads as a project of its
/// own — one repository was split across a dozen entries. The `.git` above it
/// is what puts them back together. A checkout that no longer exists keeps the
/// path it had: the statistics outlive it on purpose.
fn project_of(cwd: &str, cache: &mut HashMap<String, String>) -> String {
    if let Some(known) = cache.get(cwd) {
        return known.clone();
    }
    let root = Path::new(cwd)
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map_or_else(|| cwd.to_string(), |dir| dir.to_string_lossy().into_owned());
    cache.insert(cwd.to_string(), root.clone());
    root
}

/// Parses a log from `offset`, returning the messages and where reading stopped.
///
/// Stopping short of a partial line matters: the log is appended to while this
/// runs, and a half-written line resumed from the wrong offset would corrupt
/// every line after it.
fn read_from(
    path: &Path,
    offset: i64,
    key: &str,
    projects: &mut HashMap<String, String>,
) -> Result<(Vec<Message>, i64)> {
    // `<session>/subagents/agent-*.jsonl` is the whole of the marking; nothing
    // inside the log says which side of the dispatch it belongs to.
    let agent = key.contains("/subagents/");
    let file = std::fs::File::open(path)
        .with_context(|| tr_args("error.read_file", &[("path", &path.display().to_string())]))?;
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset.max(0) as u64))?;

    let mut messages = Vec::new();
    let mut consumed = offset.max(0);
    let mut line = String::new();
    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => n,
            // A log can hold anything a tool printed, invalid UTF-8 included;
            // one bad line is not a reason to abandon the file.
            Err(_) => break,
        };
        if !line.ends_with('\n') {
            break;
        }
        consumed += read as i64;

        if let Some(mut message) = parse_line(&line) {
            message.project = project_of(&message.project, projects);
            message.agent = agent;
            messages.push(message);
        }
    }
    Ok((messages, consumed))
}

/// A log line, once it turns out to be an assistant message with usage.
fn parse_line(line: &str) -> Option<Message> {
    // The cheap test first: most of the bulk is tool output and user turns,
    // and running serde over all of it would cost more than everything else
    // here put together.
    if !line.contains("\"usage\"") {
        return None;
    }

    let entry: Entry = serde_json::from_str(line).ok()?;
    if entry.entry_type.as_deref() != Some("assistant") {
        return None;
    }
    let message = entry.message?;
    let usage = message.usage?;
    let ts = OffsetDateTime::parse(entry.timestamp.as_deref()?, &Rfc3339).ok()?;

    // `<synthetic>` stands for messages Claude Code composed itself — an
    // interrupted turn, a rendered error. They carry no tokens and no model.
    let model = message.model?;
    if model.starts_with('<') {
        return None;
    }

    Some(Message {
        id: message.id?,
        project: entry.cwd.unwrap_or_default(),
        date: timefmt::day_key(ts.unix_timestamp()),
        model,
        // Set by the caller, which is what knows where the log came from.
        agent: false,
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cache_read_input_tokens,
        cache_write: usage.cache_creation_input_tokens,
    })
}

/// Path of a log relative to the projects directory — its identity in the
/// database, and short enough to read in a debugger.
fn relative(root: &Path, log: &Path) -> String {
    log.strip_prefix(root).unwrap_or(log).to_string_lossy().replace('\\', "/")
}

fn modified_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Whether a scan is worth running now: at most once a day, unless asked for.
///
/// Hundreds of megabytes go through it, so the daily bound is the point. A
/// database that has never been scanned is the exception — until it has, the
/// window has nothing per-model to show at all.
pub fn due(last_scan_ts: i64, now: i64) -> bool {
    const DAY: i64 = 86_400;
    last_scan_ts <= 0 || now - last_scan_ts >= DAY
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    message: Option<Msg>,
}

#[derive(Deserialize)]
struct Msg {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line in the shape the logs actually carry, trimmed to what is read.
    fn line(id: &str, model: &str, ts: &str, input: i64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","cwd":"D:\\git\\demo","sessionId":"s",
                "message":{{"id":"{id}","model":"{model}","usage":{{"input_tokens":{input},
                "output_tokens":7,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}}}}"#
        )
        .replace('\n', "")
    }

    #[test]
    fn reads_an_assistant_message() {
        let m = parse_line(&line("msg_1", "claude-opus-5", "2026-08-12T09:00:00.000Z", 3))
            .expect("a message with usage is counted");
        assert_eq!(m.id, "msg_1");
        assert_eq!(m.model, "claude-opus-5");
        assert_eq!(m.project, r"D:\git\demo");
        assert_eq!((m.input, m.output, m.cache_read, m.cache_write), (3, 7, 100, 20));
    }

    /// User turns and tool results make up most of the bulk and carry no usage.
    #[test]
    fn skips_everything_that_is_not_an_assistant_message() {
        assert!(parse_line(r#"{"type":"user","message":{"content":"hi"}}"#).is_none());
        assert!(parse_line(r#"{"type":"assistant","message":{"id":"x"}}"#).is_none());
        assert!(parse_line("not json at all").is_none());
    }

    /// A summary line mentions `usage` without being a message; the cheap
    /// pre-filter lets it through and the parse has to reject it.
    #[test]
    fn a_line_that_merely_mentions_usage_is_not_counted() {
        assert!(parse_line(r#"{"type":"summary","summary":"about \"usage\" limits"}"#).is_none());
    }

    #[test]
    fn a_scan_is_due_once_a_day() {
        assert!(!due(1_000_000, 1_000_000 + 3600));
        assert!(due(1_000_000, 1_000_000 + 86_400));
        assert!(due(0, 0), "nothing scanned yet");
    }
}
