//! Reading and writing `~/.claude/settings.json`.
//!
//! Two features put things in that file — the status line hook ([`crate::install`])
//! and the model overrides ([`crate::model_override`]) — and it is the user's
//! file, not ours: their own configuration sits in it alongside whatever we add.
//! Hence a whole-document read, modify, write rather than anything that rebuilds
//! the file from what we happen to know about, and a backup before every write.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::{paths, tr_args};

/// Reads the settings document.
///
/// A missing file is not an error — Claude Code creates it on demand, so a
/// first-time installation legitimately finds nothing. Neither is an empty one:
/// `serde_json` would refuse it, and it means the same as `{}`.
pub fn read() -> Result<Map<String, Value>> {
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

/// Writes the settings, taking a backup of what was there first.
///
/// One backup slot, overwritten each time: it exists so that a misfired button
/// is survivable, not as a history of the file.
pub fn write(settings: &Map<String, Value>) -> Result<()> {
    let path = paths::claude_settings()?;

    if path.exists() {
        let backup = backup_path()?;
        std::fs::copy(&path, &backup).with_context(|| {
            tr_args("error.backup", &[("path", &backup.display().to_string())])
        })?;
    }

    let mut raw = serde_json::to_string_pretty(&Value::Object(settings.clone()))?;
    raw.push('\n');
    std::fs::write(&path, raw)
        .with_context(|| tr_args("error.write_file", &[("path", &path.display().to_string())]))
}

/// Where the copy of the previous settings file goes.
pub fn backup_path() -> Result<PathBuf> {
    Ok(paths::ensure_data_dir()?.join("settings.json.bak"))
}

/// The object under `key`, created empty if it is absent.
///
/// Refuses to touch a key holding something that is not an object: overwriting
/// it would silently destroy a setting we do not understand.
pub(crate) fn object_mut<'a>(
    settings: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>> {
    let entry = settings.entry(key).or_insert_with(|| Value::Object(Map::new()));
    match entry {
        Value::Object(map) => Ok(map),
        _ => bail!(tr_args("error.not_an_object_key", &[("key", key)])),
    }
}
