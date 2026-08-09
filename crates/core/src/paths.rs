//! Locations of Claude Code files and of our own state.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::tr_args;

/// Claude Code home directory (`~/.claude`, or `$CLAUDE_CONFIG_DIR`).
pub fn claude_home() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let home = dirs::home_dir().with_context(|| crate::tr("error.home_dir"))?;
    Ok(home.join(".claude"))
}

/// Claude Code user settings, where `statusLine` is injected.
pub fn claude_settings() -> Result<PathBuf> {
    Ok(claude_home()?.join("settings.json"))
}

/// Claude Code aggregates: daily tokens per model, cumulative `modelUsage`.
pub fn claude_stats_cache() -> Result<PathBuf> {
    Ok(claude_home()?.join("stats-cache.json"))
}

/// Session transcripts (`~/.claude/projects/<encoded-cwd>/<session>.jsonl`).
pub fn claude_projects() -> Result<PathBuf> {
    Ok(claude_home()?.join("projects"))
}

/// Application data directory. Overridden by `CLAUDE_STATUS_DIR`.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_STATUS_DIR")
        && !dir.trim().is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let base = dirs::data_dir().with_context(|| crate::tr("error.data_dir"))?;
    Ok(base.join("claude-status"))
}

/// Application data directory, guaranteed to exist on disk.
pub fn ensure_data_dir() -> Result<PathBuf> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| tr_args("error.create_dir", &[("path", &dir.display().to_string())]))?;
    Ok(dir)
}

/// Database holding the limit samples.
pub fn db_path() -> Result<PathBuf> {
    Ok(ensure_data_dir()?.join("usage.sqlite3"))
}

/// Application configuration file.
pub fn config_path() -> Result<PathBuf> {
    Ok(ensure_data_dir()?.join("config.toml"))
}
