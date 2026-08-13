//! Shared core of `claude-status`: parsing Claude Code data, storing limit
//! samples and working out how fast they are being spent.
//!
//! Live subscription limits are not stored anywhere on disk — Claude Code hands
//! them only to the command configured as `statusLine.command`, feeding it JSON
//! on stdin (see [`statusline`]). What gets registered there ([`install`]) is
//! `claude-status hook` — the same binary as the window, under an argument —
//! which parses that JSON and appends samples to SQLite ([`db`]).

rust_i18n::i18n!("../../locales", fallback = "en");

pub mod autostart;
pub mod config;
pub mod db;
pub mod i18n;
pub mod install;
pub mod model_override;
pub mod pace;
pub mod paths;
pub mod probe;
pub mod render;
pub mod scan;
pub mod settings;
pub mod stats_cache;
pub mod statusline;
pub mod timefmt;
pub mod update;

pub use config::Config;
pub use db::{Db, Sample, Totals, Written};
pub use i18n::Language;
pub use pace::{Overview, WindowState};
pub use statusline::StatuslineInput;

/// Translates a key using the current locale.
///
/// `rust-i18n` expands `t!` through a helper generated in the crate that called
/// `i18n!`, so downstream crates cannot use the macro directly against our
/// translations. They call this instead.
pub fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

/// Translates a key, substituting `%{name}` placeholders.
///
/// Takes the arguments as a slice rather than through a macro so that callers
/// in other crates need no macro machinery of their own.
pub fn tr_args(key: &str, args: &[(&str, &str)]) -> String {
    let template = rust_i18n::t!(key).into_owned();
    args.iter().fold(template, |acc, (name, value)| {
        acc.replace(&format!("%{{{name}}}"), value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    #[test]
    fn translations_differ_between_locales() {
        {
            let _guard = i18n::test_guard(Language::En);
            assert_eq!(tr("overview.card.week"), "Week");
        }
        let _guard = i18n::test_guard(Language::Ru);
        assert_eq!(tr("overview.card.week"), "Неделя");
    }

    #[test]
    fn arguments_are_substituted() {
        let _guard = i18n::test_guard(Language::En);
        let text = tr_args("ui.refreshed_at", &[("time", "12:30")]);
        assert_eq!(text, "updated 12:30");
    }

    #[test]
    fn unknown_keys_are_visible_rather_than_silent() {
        let _guard = i18n::test_guard(Language::En);
        // rust-i18n echoes the key back; an untranslated string must be obvious
        // in the UI instead of rendering as an empty label.
        //
        // Held in a variable rather than written out: the scan that checks
        // every literal key exists would otherwise flag this deliberate one.
        let absent = "no.such.key";
        assert!(tr(absent).contains(absent));
    }
}
