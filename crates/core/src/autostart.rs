//! Starting with the user session.
//!
//! Nothing is collected while the program is not running — the hook keeps
//! appending samples on its own, but the tray icon, the daily budget and the
//! direct request all need the application alive. Hence a way to have the
//! session start it.
//!
//! The state lives with the operating system, not in `config.toml`: a copy of
//! ours would drift the moment the entry is removed by anything else.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Told to the copy the session starts, so that it goes straight to the tray
/// instead of opening a window over whatever the user is doing at login.
pub const TRAY_FLAG: &str = "--tray";

/// Name the entry is filed under, on every platform that needs one.
const ENTRY: &str = "claude-status";

/// Whether the application is registered to start with the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Off,
    On,
    /// Registered, but pointing at something other than the running binary —
    /// what a moved or renamed copy leaves behind.
    Elsewhere { path: String },
}

impl State {
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On)
    }
}

/// Reads what the session is currently set to start.
pub fn state() -> Result<State> {
    Ok(classify(registered()?.as_deref(), &own_exe()?))
}

/// Registers or removes the entry.
pub fn set(on: bool) -> Result<()> {
    if on { enable(&own_exe()?) } else { disable() }
}

fn own_exe() -> Result<PathBuf> {
    std::env::current_exe().with_context(|| crate::tr("error.own_exe"))
}

fn classify(registered: Option<&str>, exe: &Path) -> State {
    let Some(path) = registered else {
        return State::Off;
    };
    if same_file(Path::new(path), exe) {
        State::On
    } else {
        State::Elsewhere { path: path.to_string() }
    }
}

/// Compares two paths by what they resolve to.
///
/// A plain string comparison would call `bin/claude-status` and
/// `bin/../bin/claude-status` different programs; canonicalising is only
/// possible for a file that still exists, which is exactly the case where the
/// answer matters.
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The executable, quoted — the path routinely contains spaces.
fn quoted(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Pulls the program out of a command line, dropping the arguments.
fn program(value: &str) -> String {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return rest[..end].to_string();
    }
    value.split_whitespace().next().unwrap_or(value).to_string()
}

#[cfg(windows)]
use windows::{disable, enable, registered};

/// The autostart key of the current user.
///
/// `HKEY_CURRENT_USER` rather than the machine-wide key: this needs no
/// elevation, and a usage monitor is a per-user thing anyway.
#[cfg(windows)]
mod windows {
    use super::*;
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn registered() -> Result<Option<String>> {
        let run = match RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY) {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| crate::tr("error.autostart_read")),
        };
        match run.get_value::<String, _>(ENTRY) {
            Ok(value) => Ok(Some(program(&value))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| crate::tr("error.autostart_read")),
        }
    }

    pub fn enable(exe: &Path) -> Result<()> {
        let (run, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .with_context(|| crate::tr("error.autostart_write"))?;
        run.set_value(ENTRY, &format!("{} {TRAY_FLAG}", quoted(exe)))
            .with_context(|| crate::tr("error.autostart_write"))
    }

    pub fn disable() -> Result<()> {
        let run = match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(
            RUN_KEY,
            winreg::enums::KEY_ALL_ACCESS,
        ) {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| crate::tr("error.autostart_write")),
        };
        match run.delete_value(ENTRY) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| crate::tr("error.autostart_write")),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
use xdg::{disable, enable, registered};

/// `~/.config/autostart/claude-status.desktop`, as freedesktop.org specifies.
#[cfg(all(unix, not(target_os = "macos")))]
mod xdg {
    use super::*;

    fn path() -> Result<PathBuf> {
        let base = dirs::config_dir().with_context(|| crate::tr("error.data_dir"))?;
        Ok(base.join("autostart").join(format!("{ENTRY}.desktop")))
    }

    pub fn registered() -> Result<Option<String>> {
        match std::fs::read_to_string(path()?) {
            Ok(text) => Ok(desktop_exec(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| crate::tr("error.autostart_read")),
        }
    }

    pub fn enable(exe: &Path) -> Result<()> {
        let path = path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| crate::tr("error.autostart_write"))?;
        }
        std::fs::write(&path, desktop_entry(exe))
            .with_context(|| crate::tr("error.autostart_write"))
    }

    pub fn disable() -> Result<()> {
        match std::fs::remove_file(path()?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| crate::tr("error.autostart_write")),
        }
    }
}

/// The desktop entry, written whole rather than patched: it is ours alone.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=claude-status\n\
         Comment=Claude Code usage limits in the tray\n\
         Exec={} {TRAY_FLAG}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        quoted(exe)
    )
}

#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn desktop_exec(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Exec="))
        .map(program)
}

#[cfg(target_os = "macos")]
use launchd::{disable, enable, registered};

/// `~/Library/LaunchAgents/<label>.plist` — how macOS starts a user agent.
#[cfg(target_os = "macos")]
mod launchd {
    use super::*;

    fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().with_context(|| crate::tr("error.home_dir"))?;
        Ok(home.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist")))
    }

    pub fn registered() -> Result<Option<String>> {
        match std::fs::read_to_string(path()?) {
            Ok(text) => Ok(plist_program(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| crate::tr("error.autostart_read")),
        }
    }

    pub fn enable(exe: &Path) -> Result<()> {
        let path = path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| crate::tr("error.autostart_write"))?;
        }
        std::fs::write(&path, plist(exe)).with_context(|| crate::tr("error.autostart_write"))
    }

    pub fn disable() -> Result<()> {
        match std::fs::remove_file(path()?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| crate::tr("error.autostart_write")),
        }
    }
}

/// Reverse-DNS, as launchd expects a label to be.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const LABEL: &str = "dev.roxblnfk.claude-status";

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>{TRAY_FLAG}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        exe.display()
    )
}

/// The first `ProgramArguments` entry: the program, before its flags.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn plist_program(text: &str) -> Option<String> {
    let array = text.split_once("<array>")?.1;
    let value = array.split_once("<string>")?.1.split_once("</string>")?.0;
    Some(value.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_registered_is_off() {
        assert_eq!(classify(None, Path::new("/opt/claude-status")), State::Off);
    }

    #[test]
    fn the_running_binary_is_on() {
        let exe = Path::new("/opt/claude-status");
        assert_eq!(classify(Some("/opt/claude-status"), exe), State::On);
    }

    /// A copy left behind by a move must not read as "off": the session would
    /// still be starting it, and switching the checkbox on would look like a
    /// no-op while quietly replacing a different entry.
    #[test]
    fn a_stale_entry_is_reported_rather_than_hidden() {
        let state = classify(Some("/old/claude-status"), Path::new("/opt/claude-status"));
        assert_eq!(state, State::Elsewhere { path: "/old/claude-status".into() });
        assert!(!state.is_on());
    }

    #[test]
    fn a_quoted_command_line_yields_the_program_alone() {
        assert_eq!(program(r#""C:\Program Files\cs\claude-status.exe" --tray"#), r"C:\Program Files\cs\claude-status.exe");
        assert_eq!(program("/usr/bin/claude-status --tray"), "/usr/bin/claude-status");
        assert_eq!(program("  /usr/bin/claude-status  "), "/usr/bin/claude-status");
    }

    #[test]
    fn the_desktop_entry_round_trips() {
        let exe = Path::new("/home/u/.local/bin/claude status");
        let text = desktop_entry(exe);
        assert_eq!(desktop_exec(&text).as_deref(), Some("/home/u/.local/bin/claude status"));
        assert!(text.contains(TRAY_FLAG), "the session must start it hidden: {text}");
    }

    #[test]
    fn a_desktop_file_without_an_exec_line_registers_nothing() {
        assert_eq!(desktop_exec("[Desktop Entry]\nType=Application\n"), None);
    }

    #[test]
    fn the_plist_round_trips() {
        let exe = Path::new("/Applications/claude-status");
        let text = plist(exe);
        assert_eq!(plist_program(&text).as_deref(), Some("/Applications/claude-status"));
        assert!(text.contains(TRAY_FLAG), "{text}");
    }

    #[test]
    fn an_empty_plist_registers_nothing() {
        assert_eq!(plist_program("<plist></plist>"), None);
    }
}
