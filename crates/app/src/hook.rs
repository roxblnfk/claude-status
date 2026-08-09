//! The `statusLine.command` hook: the only place live subscription limits are
//! visible.
//!
//! Claude Code runs the program with [`HOOK_ARG`](claude_status_core::install::HOOK_ARG)
//! on every assistant message and feeds it the session JSON on stdin. We append
//! the `rate_limits` snapshot to SQLite and print a status line built from the
//! configured template.
//!
//! The overriding requirement is not to disturb Claude Code. Whatever happens,
//! the process exits with code 0 and prints no garbage to stdout: a broken hook
//! would otherwise break the session's interface. `CLAUDE_STATUS_DEBUG=1`
//! enables diagnostics.

use std::io::{Read, Write};

use claude_status_core::{
    Config, Db, StatuslineInput,
    render::{self, RenderContext},
    timefmt,
};

/// Where to append the raw JSON from Claude Code, when set.
///
/// The statusline schema grows from release to release (the weekly Opus window,
/// for one, is still undocumented), so there has to be a way to see the source
/// data in full.
const DUMP_ENV: &str = "CLAUDE_STATUS_DUMP";
const DEBUG_ENV: &str = "CLAUDE_STATUS_DEBUG";

/// Reads one payload from stdin and prints one status line. Never fails.
pub fn run(config: &Config) {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        return;
    }

    dump_raw(&raw, config.debug.dump_path.as_deref());

    match collect(&raw, config) {
        Ok(Some(line)) => println!("{line}"),
        Ok(None) => {}
        Err(e) => debug(format_args!("claude-status hook: {e:#}")),
    }
}

/// Returns the line to print, or `None` when there is nothing to print.
fn collect(raw: &str, config: &Config) -> anyhow::Result<Option<String>> {
    let input = StatuslineInput::parse(raw)?;
    let now = timefmt::now();

    let db = Db::open_default()?;
    db.record(&input, now)?;
    let _ = db.maybe_prune(config.storage.retention_days, now);

    if !config.statusline.enabled {
        return Ok(None);
    }

    let overview = db.overview(now)?;
    let ctx = RenderContext {
        input: Some(&input),
        overview: &overview,
        config,
        now,
    };
    Ok(Some(render::render(&ctx)))
}

/// Appends the raw payload, if either the environment or the configuration
/// asks for it. The environment wins so a single run can be traced without
/// touching the configuration.
fn dump_raw(raw: &str, configured: Option<&str>) {
    let path = std::env::var(DUMP_ENV).ok().or_else(|| configured.map(str::to_string));
    let Some(path) = path else { return };
    if path.trim().is_empty() {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let _ = writeln!(file, "{}", raw.trim());
}

fn debug(args: std::fmt::Arguments<'_>) {
    if std::env::var(DEBUG_ENV).is_ok_and(|v| !v.is_empty() && v != "0") {
        eprintln!("{args}");
    }
}
