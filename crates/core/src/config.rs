//! Application settings. Edited from the GUI, stored in `config.toml` next to
//! the database.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::i18n::Language;
use crate::{paths, tr_args};

/// Default status line template.
pub const DEFAULT_TEMPLATE: &str =
    "{model} · ctx {ctx_pct}% · 5h {five_bar} {five_pct}% (⟳{five_reset}) · week {week_pct}% {pace}";

/// Ready-made templates to pick from: `(translation key, template)`.
///
/// The first entry matches [`DEFAULT_TEMPLATE`]. Every placeholder used here
/// must be known to [`crate::render`] — a test enforces that, since a typo
/// would otherwise pass straight through into the rendered line.
pub const PRESETS: &[(&str, &str)] = &[
    ("preset.default", DEFAULT_TEMPLATE),
    ("preset.compact", "5h {five_pct}% · week {week_pct}%"),
    ("preset.bars", "5h {five_bar} {five_pct}% · week {week_bar} {week_pct}%"),
    (
        "preset.budget",
        "week {week_pct}% · today {today_left} left · budget {daily} · pace {burn}",
    ),
    (
        "preset.until_reset",
        "5h {five_pct}% ({five_left} left) · week {week_pct}% ({week_left} left)",
    ),
    (
        "preset.detailed",
        "{model} {effort} · ctx {ctx_pct}% · 5h {five_bar} {five_pct}% ⟳{five_reset} · \
         week {week_bar} {week_pct}% ⟳{week_reset} {pace} · {cost}",
    ),
    ("preset.minimal", "{five_pct}/{week_pct}"),
];

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub statusline: StatuslineConfig,
    pub budget: BudgetConfig,
    pub storage: StorageConfig,
    pub tray: TrayConfig,
    pub ui: UiConfig,
    pub probe: ProbeConfig,
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct StatuslineConfig {
    /// Whether to print the line. A disabled hook still records samples.
    pub enabled: bool,
    /// Template with placeholders, see [`crate::render`].
    pub template: String,
    /// Width of the textual progress bars, in characters.
    pub bar_width: usize,
    /// Whether to colourise the output with ANSI codes.
    pub colors: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct BudgetConfig {
    /// Target share of the weekly limit per day, %. `None` — even, 100/7 ≈ 14.3.
    pub target_daily_pct: Option<f64>,
    /// Deviation above which running ahead counts as overspending, pp.
    pub warn_deviation_pct: f64,
    /// Deviation above which overspending counts as critical, pp.
    pub critical_deviation_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct StorageConfig {
    /// How many days to keep samples. `0` — never delete.
    pub retention_days: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct TrayConfig {
    /// How often the GUI re-reads the database, in seconds.
    pub refresh_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct UiConfig {
    /// Interface language.
    pub language: Language,
}

/// Asking Claude Code directly when the status line has nothing to offer.
///
/// Each run starts a short-lived Claude Code — seconds and hundreds of
/// megabytes — so the defaults are deliberately lazy: never more often than
/// `interval_secs`, and only when the collected readings have gone stale.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ProbeConfig {
    pub enabled: bool,
    /// Floor between two runs.
    pub interval_secs: u64,
    /// Readings younger than this are trusted and no run is made.
    pub fresh_secs: u64,
}

/// Below this the probe would cost more than the answer is worth; Claude Code
/// throttles its own usage cache by the same five minutes.
pub const MIN_PROBE_INTERVAL_SECS: u64 = 300;

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct DebugConfig {
    /// File the hook appends the raw statusline JSON to.
    ///
    /// The schema keeps growing — the per-model weekly caps `/usage` shows are
    /// still undocumented — and the hook's environment belongs to Claude Code,
    /// so `CLAUDE_STATUS_DUMP` cannot be set for an already running session.
    /// This is the way in that does not require restarting anything.
    pub dump_path: Option<String>,
}

impl Default for StatuslineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            template: DEFAULT_TEMPLATE.to_string(),
            bar_width: 8,
            colors: true,
        }
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            target_daily_pct: None,
            warn_deviation_pct: 10.0,
            critical_deviation_pct: 25.0,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self { retention_days: 180 }
    }
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self { refresh_secs: 20 }
    }
}

impl Default for ProbeConfig {
    fn default() -> Self {
        Self { enabled: true, interval_secs: 900, fresh_secs: 300 }
    }
}

impl ProbeConfig {
    /// The interval actually used, whatever the file says.
    pub fn interval_secs(&self) -> u64 {
        self.interval_secs.max(MIN_PROBE_INTERVAL_SECS)
    }
}

impl BudgetConfig {
    /// Target daily share of the weekly limit, %.
    pub fn target_daily_pct(&self) -> f64 {
        self.target_daily_pct.unwrap_or(100.0 / 7.0)
    }
}

impl Config {
    /// Reads the configuration, falling back to defaults when the file is absent.
    ///
    /// The hook runs on every assistant message and must not fail because of a
    /// broken configuration file, so a parse error is not treated as fatal.
    pub fn load() -> Result<Self> {
        let path = paths::config_path()?;
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(toml::from_str(&raw).with_context(|| {
                tr_args("error.parse_file", &[("path", &path.display().to_string())])
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| {
                tr_args("error.read_file", &[("path", &path.display().to_string())])
            }),
        }
    }

    /// Reads the configuration, silently falling back to defaults on any error.
    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_default()
    }

    /// Reads the configuration and applies its language globally.
    ///
    /// Every entry point needs both, and doing them separately invites loading
    /// the configuration but forgetting to switch the locale.
    pub fn load_and_apply_language() -> Self {
        let config = Self::load_or_default();
        crate::i18n::apply(config.ui.language);
        config
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_path()?;
        let raw = toml::to_string_pretty(self).with_context(|| crate::tr("error.serialize_config"))?;
        std::fs::write(&path, raw)
            .with_context(|| tr_args("error.write_file", &[("path", &path.display().to_string())]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_target_is_a_seventh_of_the_week() {
        assert!((BudgetConfig::default().target_daily_pct() - 100.0 / 7.0).abs() < 1e-9);
    }

    #[test]
    fn explicit_target_wins() {
        let cfg = BudgetConfig { target_daily_pct: Some(25.0), ..BudgetConfig::default() };
        assert_eq!(cfg.target_daily_pct(), 25.0);
    }

    #[test]
    fn roundtrips_through_toml() {
        let mut cfg = Config::default();
        cfg.statusline.template = "{week_pct}".into();
        cfg.tray.refresh_secs = 45;
        cfg.ui.language = Language::Ru;

        let raw = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(toml::from_str::<Config>(&raw).unwrap(), cfg);
    }

    #[test]
    fn partial_toml_fills_in_defaults() {
        let cfg: Config = toml::from_str("[statusline]\nbar_width = 3\n").unwrap();
        assert_eq!(cfg.statusline.bar_width, 3);
        assert_eq!(cfg.statusline.template, DEFAULT_TEMPLATE, "the rest comes from defaults");
        assert_eq!(cfg.storage.retention_days, StorageConfig::default().retention_days);
        assert_eq!(cfg.ui.language, Language::Auto);
    }

    #[test]
    fn language_survives_a_roundtrip_by_name() {
        let raw = "[ui]\nlanguage = \"ru\"\n";
        assert_eq!(toml::from_str::<Config>(raw).unwrap().ui.language, Language::Ru);
    }
}
