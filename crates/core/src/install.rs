//! Registering the hook in `~/.claude/settings.json`.
//!
//! `rate_limits` are in no file on disk — Claude Code hands them only to the
//! command in `statusLine.command`, feeding it JSON on stdin. Without this
//! registration there is nothing to collect limit statistics with.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::{paths, tr_args};

/// Name of the hook binary, without an extension.
pub const HOOK_BIN: &str = "claude-status-hook";

/// What is currently registered in the Claude Code settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    /// No `statusLine` is configured.
    Absent,
    /// Our hook is registered.
    Ours { command: String },
    /// Somebody else's command is registered — overwriting it silently is not
    /// acceptable.
    Foreign { command: String },
}

impl InstallStatus {
    pub fn is_ours(&self) -> bool {
        matches!(self, Self::Ours { .. })
    }
}

/// Path to the hook binary: looked up next to the current executable.
pub fn hook_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().with_context(|| crate::tr("error.own_exe"))?;
    let dir = exe.parent().with_context(|| crate::tr("error.exe_parent"))?;
    let candidate = dir.join(format!("{HOOK_BIN}{}", std::env::consts::EXE_SUFFIX));
    if !candidate.exists() {
        bail!(tr_args(
            "error.hook_missing",
            &[
                ("hook", &candidate.display().to_string()),
                ("exe", &exe.display().to_string()),
            ]
        ));
    }
    Ok(candidate)
}

/// Builds the value for `statusLine.command`.
///
/// Claude Code runs the string through a shell, so the path is always quoted.
/// On Windows the separators are switched to `/`: that way the string does not
/// depend on whether `cmd`, PowerShell or Git Bash receives it — the latter two
/// treat a backslash inside quotes as an escape.
pub fn command_string(hook: &Path) -> String {
    let path = hook.to_string_lossy().replace('\\', "/");
    format!("\"{path}\"")
}

/// Reads the current registration state.
pub fn status() -> Result<InstallStatus> {
    let settings = read_settings()?;
    Ok(classify(&settings))
}

fn classify(settings: &Map<String, Value>) -> InstallStatus {
    let Some(command) = settings
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
    else {
        return InstallStatus::Absent;
    };
    if command.contains(HOOK_BIN) {
        InstallStatus::Ours { command: command.to_string() }
    } else {
        InstallStatus::Foreign { command: command.to_string() }
    }
}

/// Registers the hook in the Claude Code settings.
///
/// `refresh_interval` makes Claude Code re-run the command every N seconds on
/// top of the event-driven updates — otherwise samples go stale during long
/// pauses. `force` permits overwriting a third-party command.
pub fn install(refresh_interval: Option<u64>, force: bool) -> Result<PathBuf> {
    let hook = hook_path()?;
    let mut settings = read_settings()?;

    match classify(&settings) {
        InstallStatus::Foreign { command } if !force => {
            bail!(tr_args("error.foreign_statusline", &[("command", &command)]));
        }
        InstallStatus::Foreign { .. } => save_previous(settings.get("statusLine"))?,
        _ => {}
    }

    let mut entry = json!({ "type": "command", "command": command_string(&hook) });
    if let Some(secs) = refresh_interval {
        entry["refreshInterval"] = json!(secs.max(1));
    }
    settings.insert("statusLine".into(), entry);

    write_settings(&settings)?;
    Ok(hook)
}

/// Removes our hook from the settings, restoring the previous command if any.
pub fn uninstall() -> Result<()> {
    let mut settings = read_settings()?;
    match classify(&settings) {
        InstallStatus::Absent => return Ok(()),
        InstallStatus::Foreign { command } => {
            bail!(tr_args("error.not_ours", &[("command", &command)]));
        }
        InstallStatus::Ours { .. } => {}
    }

    match load_previous()? {
        Some(previous) => settings.insert("statusLine".into(), previous),
        None => settings.remove("statusLine"),
    };
    write_settings(&settings)
}

fn read_settings() -> Result<Map<String, Value>> {
    let path = paths::claude_settings()?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => {
            return Err(e).with_context(|| {
                tr_args("error.read_file", &[("path", &path.display().to_string())])
            });
        }
    };
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| tr_args("error.parse_file", &[("path", &path.display().to_string())]))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => bail!(tr_args("error.not_an_object", &[("path", &path.display().to_string())])),
    }
}

/// Writes the settings, taking a backup first.
fn write_settings(settings: &Map<String, Value>) -> Result<()> {
    let path = paths::claude_settings()?;

    if path.exists() {
        let backup = paths::ensure_data_dir()?.join("settings.json.bak");
        std::fs::copy(&path, &backup).with_context(|| {
            tr_args("error.backup", &[("path", &backup.display().to_string())])
        })?;
    }

    let mut raw = serde_json::to_string_pretty(&Value::Object(settings.clone()))?;
    raw.push('\n');
    std::fs::write(&path, raw)
        .with_context(|| tr_args("error.write_file", &[("path", &path.display().to_string())]))
}

/// File holding the third-party `statusLine` setting we displaced.
fn previous_path() -> Result<PathBuf> {
    Ok(paths::ensure_data_dir()?.join("previous-statusline.json"))
}

fn save_previous(value: Option<&Value>) -> Result<()> {
    let Some(value) = value else { return Ok(()) };
    std::fs::write(previous_path()?, serde_json::to_string_pretty(value)?)
        .with_context(|| crate::tr("error.save_previous"))
}

fn load_previous() -> Result<Option<Value>> {
    let path = previous_path()?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => {
            let value = serde_json::from_str(&raw).with_context(|| {
                tr_args("error.parse_file", &[("path", &path.display().to_string())])
            })?;
            // Restored — the file is no longer needed, otherwise the next
            // uninstall would resurrect a long-obsolete command.
            let _ = std::fs::remove_file(&path);
            Ok(Some(value))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            Err(e).with_context(|| tr_args("error.read_file", &[("path", &path.display().to_string())]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(command: &str) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("statusLine".into(), json!({ "type": "command", "command": command }));
        map
    }

    #[test]
    fn classifies_absent_settings() {
        assert_eq!(classify(&Map::new()), InstallStatus::Absent);

        let mut only_model = Map::new();
        only_model.insert("model".into(), json!("opus"));
        assert_eq!(classify(&only_model), InstallStatus::Absent);
    }

    #[test]
    fn recognises_our_own_command() {
        let status = classify(&settings_with("\"C:/tools/claude-status-hook.exe\""));
        assert!(status.is_ours(), "{status:?}");
    }

    #[test]
    fn recognises_foreign_command() {
        let status = classify(&settings_with("~/.claude/statusline.sh"));
        assert!(matches!(status, InstallStatus::Foreign { .. }), "{status:?}");
        assert!(!status.is_ours());
    }

    #[test]
    fn command_string_is_quoted_and_slash_separated() {
        let cmd = command_string(Path::new(r"C:\Program Files\cs\claude-status-hook.exe"));
        assert_eq!(cmd, "\"C:/Program Files/cs/claude-status-hook.exe\"");
        assert!(cmd.starts_with('"') && cmd.ends_with('"'), "a path with spaces must be quoted");
    }

    #[test]
    fn unix_paths_survive_untouched() {
        let cmd = command_string(Path::new("/usr/local/bin/claude-status-hook"));
        assert_eq!(cmd, "\"/usr/local/bin/claude-status-hook\"");
    }
}
