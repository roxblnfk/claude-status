//! Parsing the JSON Claude Code feeds to `statusLine.command` on stdin.
//!
//! The schema is documented at <https://code.claude.com/docs/en/statusline>.
//! Almost everything is optional: `rate_limits` appears only for Claude.ai
//! subscribers and only after the first API response in a session, and
//! `context_window.current_usage` goes empty again after `/compact`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StatuslineInput {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub model: Option<Model>,
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub context_window: Option<ContextWindow>,
    #[serde(default)]
    pub exceeds_200k_tokens: Option<bool>,
    #[serde(default)]
    pub fast_mode: Option<bool>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub thinking: Option<Thinking>,
    #[serde(default)]
    pub rate_limits: Option<RateLimits>,
    #[serde(default)]
    pub output_style: Option<Named>,
    #[serde(default)]
    pub agent: Option<Named>,
    #[serde(default)]
    pub vim: Option<Vim>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Model {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Named {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Vim {
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Effort {
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Thinking {
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Workspace {
    #[serde(default)]
    pub current_dir: Option<String>,
    #[serde(default)]
    pub project_dir: Option<String>,
    #[serde(default)]
    pub git_worktree: Option<String>,
    #[serde(default)]
    pub repo: Option<Repo>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Repo {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Cost {
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub total_duration_ms: Option<i64>,
    #[serde(default)]
    pub total_api_duration_ms: Option<i64>,
    #[serde(default)]
    pub total_lines_added: Option<i64>,
    #[serde(default)]
    pub total_lines_removed: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ContextWindow {
    #[serde(default)]
    pub total_input_tokens: Option<i64>,
    #[serde(default)]
    pub total_output_tokens: Option<i64>,
    #[serde(default)]
    pub context_window_size: Option<i64>,
    #[serde(default)]
    pub used_percentage: Option<f64>,
    #[serde(default)]
    pub remaining_percentage: Option<f64>,
    #[serde(default)]
    pub current_usage: Option<CurrentUsage>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CurrentUsage {
    #[serde(default)]
    pub input_tokens: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<i64>,
}

/// Claude.ai subscription limit windows.
///
/// Only `five_hour` and `seven_day` are documented, but the payload carries
/// more: `/usage` shows a separate weekly bar for individual models. Rather
/// than guessing their field names, everything unrecognised is collected into
/// [`RateLimits::extra`] so a new window shows up the day Anthropic adds it.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RateLimits {
    #[serde(default)]
    pub five_hour: Option<Window>,
    #[serde(default)]
    pub seven_day: Option<Window>,
    /// Every other window, keyed by its JSON field name.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Window>,
}

impl RateLimits {
    /// Extra windows in a stable order, skipping entries that carry no data.
    ///
    /// `serde(flatten)` also catches fields that merely look like a window, so
    /// anything without a percentage is dropped rather than shown as a blank row.
    pub fn extra_windows(&self) -> Vec<(&str, Window)> {
        self.extra
            .iter()
            .filter(|(_, w)| w.used_percentage.is_some())
            .map(|(name, w)| (name.as_str(), *w))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub struct Window {
    /// Share of the limit consumed, 0..100.
    #[serde(default)]
    pub used_percentage: Option<f64>,
    /// When the window resets, unix seconds.
    #[serde(default)]
    pub resets_at: Option<i64>,
}

impl Window {
    /// Start of the window: the reset moment minus its duration.
    pub fn started_at(&self, duration_secs: i64) -> Option<i64> {
        self.resets_at.map(|r| r - duration_secs)
    }
}

/// Length of the short window — 5 hours.
pub const FIVE_HOUR_SECS: i64 = 5 * 3600;
/// Length of the weekly window — 7 days.
pub const SEVEN_DAY_SECS: i64 = 7 * 24 * 3600;

impl StatuslineInput {
    pub fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    pub fn model_id(&self) -> Option<&str> {
        self.model.as_ref()?.id.as_deref()
    }

    pub fn model_name(&self) -> Option<&str> {
        let model = self.model.as_ref()?;
        model.display_name.as_deref().or(model.id.as_deref())
    }

    pub fn five_hour(&self) -> Option<Window> {
        self.rate_limits.as_ref()?.five_hour
    }

    pub fn seven_day(&self) -> Option<Window> {
        self.rate_limits.as_ref()?.seven_day
    }

    /// Undocumented windows, such as the weekly cap on an individual model.
    pub fn extra_windows(&self) -> Vec<(&str, Window)> {
        self.rate_limits.as_ref().map(RateLimits::extra_windows).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample from the documentation — a minimal guarantee that the schema
    /// has not drifted.
    const DOC_SAMPLE: &str = r#"{
        "cwd": "/current/working/directory",
        "session_id": "abc123",
        "model": { "id": "claude-opus-5", "display_name": "Opus" },
        "context_window": {
            "total_input_tokens": 15500,
            "total_output_tokens": 1200,
            "context_window_size": 200000,
            "used_percentage": 8,
            "remaining_percentage": 92,
            "current_usage": {
                "input_tokens": 8500,
                "output_tokens": 1200,
                "cache_creation_input_tokens": 5000,
                "cache_read_input_tokens": 2000
            }
        },
        "cost": { "total_cost_usd": 0.01234, "total_duration_ms": 45000 },
        "rate_limits": {
            "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
            "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 }
        }
    }"#;

    #[test]
    fn parses_documented_sample() {
        let input = StatuslineInput::parse(DOC_SAMPLE).expect("documented sample parses");
        assert_eq!(input.model_name(), Some("Opus"));
        assert_eq!(input.five_hour().unwrap().used_percentage, Some(23.5));
        assert_eq!(input.seven_day().unwrap().resets_at, Some(1738857600));
    }

    #[test]
    fn tolerates_missing_and_unknown_fields() {
        let input = StatuslineInput::parse(r#"{"session_id":"x","brand_new_field":42}"#)
            .expect("unknown fields are ignored");
        assert!(input.rate_limits.is_none());
        assert!(input.five_hour().is_none());
    }

    /// The payload carries more windows than the documentation lists, so any
    /// unrecognised one must survive parsing instead of being dropped.
    #[test]
    fn collects_undocumented_windows() {
        let json = r#"{
            "rate_limits": {
                "five_hour":      { "used_percentage": 25.0, "resets_at": 100 },
                "seven_day":      { "used_percentage": 59.0, "resets_at": 200 },
                "seven_day_opus": { "used_percentage": 79.0, "resets_at": 200 },
                "seven_day_some_future_model": { "used_percentage": 12.0, "resets_at": 200 }
            }
        }"#;
        let input = StatuslineInput::parse(json).unwrap();

        assert_eq!(input.five_hour().unwrap().used_percentage, Some(25.0));
        assert_eq!(input.seven_day().unwrap().used_percentage, Some(59.0));

        let extra = input.extra_windows();
        assert_eq!(extra.len(), 2, "both unknown windows are kept: {extra:?}");
        assert_eq!(extra[0].0, "seven_day_opus");
        assert_eq!(extra[0].1.used_percentage, Some(79.0));
        assert_eq!(extra[1].0, "seven_day_some_future_model");
    }

    #[test]
    fn extra_windows_ignore_entries_without_a_percentage() {
        let json = r#"{"rate_limits":{"five_hour":{"used_percentage":1.0},"junk":{"resets_at":5}}}"#;
        let input = StatuslineInput::parse(json).unwrap();
        assert!(input.extra_windows().is_empty(), "{:?}", input.extra_windows());
    }

    #[test]
    fn window_start_is_reset_minus_duration() {
        let w = Window { used_percentage: Some(50.0), resets_at: Some(1_000_000) };
        assert_eq!(w.started_at(SEVEN_DAY_SECS), Some(1_000_000 - SEVEN_DAY_SECS));
    }
}
