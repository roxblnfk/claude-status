//! Interface language selection.
//!
//! Translations live in `locales/app.yml` and are compiled in by the
//! `rust_i18n::i18n!` macro. `rust-i18n` resolves `t!` through a per-crate
//! helper, so every crate that translates has to invoke the macro itself; the
//! selected locale, however, is global and set here once.

use serde::{Deserialize, Serialize};

/// Languages the interface is translated into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Follow the operating system.
    #[default]
    Auto,
    En,
    Ru,
}

/// Locale used when the system language is not one we translate into.
pub const FALLBACK: &str = "en";

/// Overrides both the configuration and the system language.
const LANG_ENV: &str = "CLAUDE_STATUS_LANG";

impl Language {
    /// Locale code, resolving [`Language::Auto`] against the system language.
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Ru => "ru",
            Language::Auto => system_language().unwrap_or(FALLBACK),
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "auto" => Some(Language::Auto),
            "en" => Some(Language::En),
            "ru" => Some(Language::Ru),
            _ => None,
        }
    }

    /// Every selectable value, in the order the settings list shows them.
    pub const ALL: [Language; 3] = [Language::Auto, Language::En, Language::Ru];
}

/// System language, if we have a translation for it.
///
/// `sys_locale` yields tags like `ru-RU`, so only the primary subtag matters.
fn system_language() -> Option<&'static str> {
    let locale = sys_locale::get_locale()?;
    let primary = locale.split(['-', '_']).next()?.to_ascii_lowercase();
    match primary.as_str() {
        "ru" => Some("ru"),
        "en" => Some("en"),
        _ => None,
    }
}

/// Applies the language globally.
///
/// The environment variable wins over the argument: it is the escape hatch for
/// running the hook under a different language than the GUI is configured with.
pub fn apply(language: Language) -> &'static str {
    let code = std::env::var(LANG_ENV)
        .ok()
        .and_then(|value| Language::from_code(value.trim()))
        .unwrap_or(language)
        .code();

    rust_i18n::set_locale(code);
    code
}

/// Serialises tests that switch the locale.
///
/// The selected locale is global, and `cargo test` runs tests in parallel, so
/// without this a test asserting on English text can observe a locale another
/// test just switched to.
#[cfg(test)]
pub(crate) fn test_guard(language: Language) -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Deliberately bypasses `apply`: CLAUDE_STATUS_LANG in the environment
    // would otherwise override what the test asked for.
    rust_i18n::set_locale(language.code());
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_languages_map_to_codes() {
        assert_eq!(Language::En.code(), "en");
        assert_eq!(Language::Ru.code(), "ru");
    }

    #[test]
    fn auto_resolves_to_a_translated_locale() {
        // Whatever the machine's locale is, we must end up with a locale we
        // actually ship, never an untranslated tag like `de`.
        let code = Language::Auto.code();
        assert!(["en", "ru"].contains(&code), "unexpected locale: {code}");
    }

    #[test]
    fn codes_round_trip() {
        for language in Language::ALL {
            let code = match language {
                Language::Auto => "auto",
                Language::En => "en",
                Language::Ru => "ru",
            };
            assert_eq!(Language::from_code(code), Some(language));
        }
        assert_eq!(Language::from_code("klingon"), None);
    }

    #[test]
    fn default_is_auto() {
        assert_eq!(Language::default(), Language::Auto);
    }
}
