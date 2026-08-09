//! Updating in place from the GitHub releases.
//!
//! The program is distributed as one file people download by hand, so without
//! this there is no way to learn that a newer one exists. Checking is a
//! deliberate act — a button — rather than something that happens on startup:
//! a usage monitor has no business talking to the network unasked.
//!
//! Nothing here decides *when* to run; the caller does, on a thread of its own,
//! because both steps block on the network.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{tr, tr_args};

/// Where the releases live.
pub const REPO: &str = "roxblnfk/claude-status";

/// The triple this binary was built for, recorded by `build.rs`. Release assets
/// carry it in their names.
pub const TARGET: &str = env!("CLAUDE_STATUS_TARGET");

/// GitHub refuses requests without one.
const USER_AGENT: &str = concat!("claude-status/", env!("CARGO_PKG_VERSION"));

/// Generous: a release asset is some megabytes over whatever link the user has.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// Refuses anything absurd before it reaches memory.
const MAX_ASSET_BYTES: u64 = 200 * 1024 * 1024;

/// A three-part version, compared part by part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(u64, u64, u64);

impl Version {
    /// Parses `1.2.3`, with or without a leading `v`. Anything trailing the
    /// patch number — `-rc1`, build metadata — is ignored rather than refused:
    /// the comparison only ever needs the three numbers.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let mut parts = text.split(['.', '-', '+']);
        let mut next = || parts.next()?.parse::<u64>().ok();
        Some(Self(next()?, next()?, next()?))
    }

    /// The version this binary was built as.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self(0, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A newer release, and the file to fetch from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub version: Version,
    pub url: String,
    /// What GitHub says the asset weighs; the download is checked against it.
    pub size: u64,
}

/// Asks GitHub for the latest release. `None` means there is nothing newer.
pub fn check() -> Result<Option<Update>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = get(&url, API_TIMEOUT)?;
    let release: Release =
        serde_json::from_slice(&body).with_context(|| tr("update.error.unreadable"))?;
    Ok(newer_than(&release, Version::current(), TARGET))
}

/// Picks the asset for this platform out of a release, if it is an upgrade.
///
/// Split from [`check`] so the choice can be tested without a network.
fn newer_than(release: &Release, current: Version, target: &str) -> Option<Update> {
    let version = Version::parse(&release.tag_name)?;
    if version <= current {
        return None;
    }
    let asset = pick_asset(&release.assets, target)?;
    Some(Update {
        version,
        url: asset.browser_download_url.clone(),
        size: asset.size,
    })
}

/// The bare executable for this platform.
///
/// The archives are for people; self-update takes the loose binary beside them,
/// which needs no unpacking and so no archive format support.
fn pick_asset<'a>(assets: &'a [Asset], target: &str) -> Option<&'a Asset> {
    assets
        .iter()
        .find(|a| a.name.contains(target) && !is_archive(&a.name))
}

fn is_archive(name: &str) -> bool {
    [".zip", ".tar.gz", ".sha256"].iter().any(|ext| name.ends_with(ext))
}

/// Downloads the update and puts it in place of the running binary.
///
/// Returns the path that was replaced. The new version takes effect on the next
/// start — a process cannot swap out its own image.
pub fn install(update: &Update) -> Result<PathBuf> {
    let exe = std::env::current_exe().with_context(|| tr("error.own_exe"))?;
    let payload = get(&update.url, DOWNLOAD_TIMEOUT)?;

    // The size GitHub reports is the cheapest guard there is against a
    // truncated download; the magic number catches a proxy that served an error
    // page with a 200.
    if payload.len() as u64 != update.size {
        bail!(tr_args(
            "update.error.size",
            &[("got", &payload.len().to_string()), ("expected", &update.size.to_string())]
        ));
    }
    if !looks_executable(&payload) {
        bail!(tr("update.error.not_a_binary"));
    }

    replace(&exe, &payload)?;
    Ok(exe)
}

/// Whether the bytes begin like a program for some platform we ship to.
fn looks_executable(bytes: &[u8]) -> bool {
    const MAGIC: [&[u8]; 4] = [
        b"MZ",                   // PE
        b"\x7fELF",              // ELF
        &[0xcf, 0xfa, 0xed, 0xfe], // Mach-O, 64-bit little-endian
        &[0xca, 0xfe, 0xba, 0xbe], // Mach-O universal
    ];
    MAGIC.iter().any(|m| bytes.starts_with(m))
}

/// Swaps the file under the running program.
///
/// Written beside the target rather than into a temporary directory: the final
/// move has to stay on one filesystem to be a rename and not a copy, and a copy
/// could be interrupted half-written. Windows will not let a running image be
/// overwritten, but it will let it be renamed out of the way — the displaced
/// file is cleaned up by [`clean_leftovers`] on the next start.
fn replace(exe: &Path, payload: &[u8]) -> Result<()> {
    let staged = staged_path(exe);
    write_executable(&staged, payload)?;

    let displaced = displaced_path(exe);
    let _ = std::fs::remove_file(&displaced);
    if cfg!(windows) {
        std::fs::rename(exe, &displaced).with_context(|| {
            tr_args("update.error.replace", &[("path", &exe.display().to_string())])
        })?;
    }

    if let Err(e) = std::fs::rename(&staged, exe) {
        // Put the old one back rather than leave the user with no program.
        if cfg!(windows) {
            let _ = std::fs::rename(&displaced, exe);
        }
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| {
            tr_args("update.error.replace", &[("path", &exe.display().to_string())])
        });
    }
    Ok(())
}

fn write_executable(path: &Path, payload: &[u8]) -> Result<()> {
    std::fs::write(path, payload).with_context(|| {
        tr_args("error.write_file", &[("path", &path.display().to_string())])
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).with_context(
            || tr_args("error.write_file", &[("path", &path.display().to_string())]),
        )?;
    }
    Ok(())
}

fn staged_path(exe: &Path) -> PathBuf {
    with_suffix(exe, "new")
}

fn displaced_path(exe: &Path) -> PathBuf {
    with_suffix(exe, "old")
}

/// `claude-status.exe` → `claude-status.exe.old`.
///
/// Appended rather than substituted for the extension: replacing it would give
/// `claude-status.old`, which on Windows collides with nothing but reads like a
/// different program.
fn with_suffix(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.as_os_str().to_os_string();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

/// Removes what a previous update left behind. Called at startup, where the
/// displaced image is no longer in use and can finally go.
pub fn clean_leftovers() {
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::fs::remove_file(displaced_path(&exe));
    let _ = std::fs::remove_file(staged_path(&exe));
}

/// One GET, following redirects, with the body read into memory.
fn get(url: &str, timeout: Duration) -> Result<Vec<u8>> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(USER_AGENT)
        .build()
        .new_agent();

    let mut response = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| tr("update.error.unreachable"))?;

    if response.status() != 200 {
        bail!(tr_args("update.error.status", &[("code", &response.status().to_string())]));
    }

    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_ASSET_BYTES)
        .read_to_end(&mut body)
        .with_context(|| tr("update.error.unreachable"))?;
    Ok(body)
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, names: &[&str]) -> Release {
        Release {
            tag_name: tag.to_string(),
            assets: names
                .iter()
                .map(|name| Asset {
                    name: (*name).to_string(),
                    browser_download_url: format!("https://example.test/{name}"),
                    size: 100,
                })
                .collect(),
        }
    }

    #[test]
    fn versions_parse_with_or_without_the_tag_prefix() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse(" 10.0.11 "), Some(Version(10, 0, 11)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("nightly"), None);
    }

    /// Text after the patch number is not a reason to give up on the numbers
    /// before it.
    #[test]
    fn versions_ignore_what_follows_the_patch_number() {
        assert_eq!(Version::parse("v1.2.3-rc1"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("1.2.3+build7"), Some(Version(1, 2, 3)));
    }

    #[test]
    fn versions_compare_part_by_part() {
        assert!(Version(1, 10, 0) > Version(1, 9, 9), "not a string comparison");
        assert!(Version(2, 0, 0) > Version(1, 99, 99));
        assert!(Version(1, 2, 3) == Version(1, 2, 3));
    }

    #[test]
    fn an_older_or_equal_release_is_not_an_update() {
        let target = "x86_64-pc-windows-msvc";
        let assets = ["claude-status-x86_64-pc-windows-msvc.exe"];
        assert_eq!(newer_than(&release("v1.2.0", &assets), Version(1, 2, 0), target), None);
        assert_eq!(newer_than(&release("v1.1.9", &assets), Version(1, 2, 0), target), None);
    }

    #[test]
    fn a_newer_release_yields_the_asset_for_this_platform() {
        let update = newer_than(
            &release(
                "v1.3.0",
                &[
                    "claude-status-v1.3.0-x86_64-unknown-linux-gnu",
                    "claude-status-v1.3.0-x86_64-pc-windows-msvc.exe",
                ],
            ),
            Version(1, 2, 0),
            "x86_64-pc-windows-msvc",
        )
        .expect("an update");

        assert_eq!(update.version, Version(1, 3, 0));
        assert!(update.url.ends_with("x86_64-pc-windows-msvc.exe"), "{}", update.url);
    }

    /// The archives are for people to download; taking one would hand the
    /// installer a zip where it expects a program.
    #[test]
    fn archives_and_checksums_are_passed_over() {
        let target = "x86_64-pc-windows-msvc";
        let only_archives = release(
            "v1.3.0",
            &[
                "claude-status-v1.3.0-x86_64-pc-windows-msvc.zip",
                "claude-status-v1.3.0-x86_64-pc-windows-msvc.zip.sha256",
            ],
        );
        assert_eq!(newer_than(&only_archives, Version(1, 2, 0), target), None);
    }

    /// A release built before the loose binary was published, or one that a
    /// platform's build failed for, must read as "nothing to install" rather
    /// than grabbing another platform's file.
    #[test]
    fn a_release_without_an_asset_for_us_is_no_update() {
        let elsewhere = release("v1.3.0", &["claude-status-v1.3.0-aarch64-apple-darwin"]);
        assert_eq!(newer_than(&elsewhere, Version(1, 2, 0), "x86_64-pc-windows-msvc"), None);
    }

    #[test]
    fn an_error_page_is_not_mistaken_for_a_program() {
        assert!(looks_executable(b"MZ\x90\x00"));
        assert!(looks_executable(b"\x7fELF\x02"));
        assert!(!looks_executable(b"<!DOCTYPE html>"));
        assert!(!looks_executable(b""));
    }

    #[test]
    fn the_staged_and_displaced_names_sit_beside_the_binary() {
        let exe = Path::new("/opt/cs/claude-status.exe");
        assert_eq!(staged_path(exe), Path::new("/opt/cs/claude-status.exe.new"));
        assert_eq!(displaced_path(exe), Path::new("/opt/cs/claude-status.exe.old"));
    }

    /// The whole point of the dance: the replacement lands exactly where the
    /// old one was, so the registered `statusLine.command` keeps working.
    #[test]
    fn replacing_puts_the_new_bytes_at_the_old_path() {
        let dir = std::env::temp_dir().join(format!("cs-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("claude-status-test");
        std::fs::write(&exe, b"old").unwrap();

        replace(&exe, b"MZnew").unwrap();

        assert_eq!(std::fs::read(&exe).unwrap(), b"MZnew");
        assert!(!staged_path(&exe).exists(), "the staging file is moved, not copied");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
