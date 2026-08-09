//! Registering the hook in `~/.claude/settings.json`.
//!
//! `rate_limits` are in no file on disk — Claude Code hands them only to the
//! command in `statusLine.command`, feeding it JSON on stdin. Without this
//! registration there is nothing to collect limit statistics with.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use crate::{paths, tr_args};

/// Name of the program, without an extension.
pub const BIN: &str = "claude-status";

/// Subcommand that turns the program into the status line hook.
///
/// It used to be a binary of its own, shipped beside the window; a download is
/// one file now, and the hook is this same program under an argument.
pub const HOOK_ARG: &str = "hook";

/// The separate hook binary of earlier releases, still recognised so that an
/// installation made by one can be spotted and replaced.
const LEGACY_HOOK_BIN: &str = "claude-status-hook";

/// What is currently registered in the Claude Code settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    /// No `statusLine` is configured.
    Absent,
    /// This very binary is registered.
    Ours { command: String },
    /// Ours by name, but not the binary now running: a copy that has been
    /// moved, or the separate hook binary from before the two were merged —
    /// which the new download no longer ships.
    Stale { command: String },
    /// Somebody else's command is registered — overwriting it silently is not
    /// acceptable.
    Foreign { command: String },
}

impl InstallStatus {
    /// Whether the registration is ours to replace or remove.
    pub fn is_ours(&self) -> bool {
        matches!(self, Self::Ours { .. } | Self::Stale { .. })
    }

    /// Whether it is registered *and* pointing at the running binary.
    pub fn is_current(&self) -> bool {
        matches!(self, Self::Ours { .. })
    }
}

/// The binary to register: the one running.
pub fn hook_path() -> Result<PathBuf> {
    std::env::current_exe().with_context(|| crate::tr("error.own_exe"))
}

/// Builds the value for `statusLine.command`.
///
/// Claude Code runs the string through a shell, so the path is always quoted.
/// On Windows the separators are switched to `/`: that way the string does not
/// depend on whether `cmd`, PowerShell or Git Bash receives it — the latter two
/// treat a backslash inside quotes as an escape.
pub fn command_string(exe: &Path) -> String {
    let path = exe.to_string_lossy().replace('\\', "/");
    format!("\"{path}\" {HOOK_ARG}")
}

/// Reads the current registration state.
pub fn status() -> Result<InstallStatus> {
    let settings = read_settings()?;
    Ok(classify(&settings, &hook_path()?))
}

fn classify(settings: &Map<String, Value>, exe: &Path) -> InstallStatus {
    let Some(command) = settings
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(Value::as_str)
    else {
        return InstallStatus::Absent;
    };
    let command = command.to_string();

    if !is_ours(&command) {
        return InstallStatus::Foreign { command };
    }
    if same_command(&command, &command_string(exe)) {
        InstallStatus::Ours { command }
    } else {
        InstallStatus::Stale { command }
    }
}

/// Whether the registered command runs this program in any of its shapes.
fn is_ours(command: &str) -> bool {
    let trimmed = command.trim_end();
    trimmed.contains(LEGACY_HOOK_BIN)
        || (trimmed.contains(BIN) && trimmed.ends_with(HOOK_ARG))
}

/// Windows paths differ in case without differing at all.
fn same_command(command: &str, wanted: &str) -> bool {
    let command = command.trim();
    if cfg!(windows) {
        command.eq_ignore_ascii_case(wanted)
    } else {
        command == wanted
    }
}

/// Registers the hook in the Claude Code settings.
///
/// `refresh_interval` makes Claude Code re-run the command every N seconds on
/// top of the event-driven updates — otherwise samples go stale during long
/// pauses. `force` permits overwriting a third-party command.
pub fn install(refresh_interval: Option<u64>, force: bool) -> Result<PathBuf> {
    let exe = hook_path()?;
    let mut settings = read_settings()?;

    match classify(&settings, &exe) {
        InstallStatus::Foreign { command } if !force => {
            bail!(tr_args("error.foreign_statusline", &[("command", &command)]));
        }
        InstallStatus::Foreign { .. } => save_previous(settings.get("statusLine"))?,
        _ => {}
    }

    let mut entry = json!({ "type": "command", "command": command_string(&exe) });
    if let Some(secs) = refresh_interval {
        entry["refreshInterval"] = json!(secs.max(1));
    }
    settings.insert("statusLine".into(), entry);

    write_settings(&settings)?;
    Ok(exe)
}

/// Removes our hook from the settings, restoring the previous command if any.
pub fn uninstall() -> Result<()> {
    let mut settings = read_settings()?;
    match classify(&settings, &hook_path()?) {
        InstallStatus::Absent => return Ok(()),
        InstallStatus::Foreign { command } => {
            bail!(tr_args("error.not_ours", &[("command", &command)]));
        }
        // A registration left by the separate hook binary is still ours to
        // clear away.
        InstallStatus::Ours { .. } | InstallStatus::Stale { .. } => {}
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

    fn exe() -> &'static Path {
        Path::new("/opt/cs/claude-status")
    }

    #[test]
    fn classifies_absent_settings() {
        assert_eq!(classify(&Map::new(), exe()), InstallStatus::Absent);

        let mut only_model = Map::new();
        only_model.insert("model".into(), json!("opus"));
        assert_eq!(classify(&only_model, exe()), InstallStatus::Absent);
    }

    #[test]
    fn recognises_our_own_command() {
        let settings = settings_with(&command_string(exe()));
        let status = classify(&settings, exe());
        assert_eq!(status, InstallStatus::Ours { command: "\"/opt/cs/claude-status\" hook".into() });
        assert!(status.is_current());
    }

    /// The download used to carry a second binary; a settings file still
    /// pointing at it must be recognised as ours and reported as needing to be
    /// registered again, not passed off as working.
    #[test]
    fn recognises_the_registration_left_by_the_separate_hook_binary() {
        let settings = settings_with("\"/opt/cs/claude-status-hook\"");
        let status = classify(&settings, exe());
        assert!(matches!(status, InstallStatus::Stale { .. }), "{status:?}");
        assert!(status.is_ours(), "ours to replace");
        assert!(!status.is_current(), "but not what is running");
    }

    #[test]
    fn a_copy_that_has_moved_is_stale_too() {
        let settings = settings_with("\"/somewhere/else/claude-status\" hook");
        assert!(matches!(classify(&settings, exe()), InstallStatus::Stale { .. }));
    }

    #[test]
    fn recognises_foreign_command() {
        let status = classify(&settings_with("~/.claude/statusline.sh"), exe());
        assert!(matches!(status, InstallStatus::Foreign { .. }), "{status:?}");
        assert!(!status.is_ours());
    }

    /// Our own name inside somebody else's script is not our registration: the
    /// command has to actually end in the hook argument.
    #[test]
    fn a_wrapper_script_is_not_ours() {
        let status = classify(&settings_with("~/bin/wrap-claude-status.sh --json"), exe());
        assert!(matches!(status, InstallStatus::Foreign { .. }), "{status:?}");
    }

    #[test]
    fn command_string_is_quoted_and_slash_separated() {
        let cmd = command_string(Path::new(r"C:\Program Files\cs\claude-status.exe"));
        assert_eq!(cmd, "\"C:/Program Files/cs/claude-status.exe\" hook");
        assert!(cmd.starts_with('"'), "a path with spaces must be quoted");
    }

    #[test]
    fn unix_paths_survive_untouched() {
        let cmd = command_string(Path::new("/usr/local/bin/claude-status"));
        assert_eq!(cmd, "\"/usr/local/bin/claude-status\" hook");
    }
}
