//! Reading `~/.claude/stats-cache.json` — the aggregates Claude Code maintains
//! itself.
//!
//! There are no limits in it, but it does carry a breakdown of tokens by day
//! and model plus an all-time `modelUsage`. It is the only source of per-model
//! figures: the `rate_limits` from statusline give a single window percentage.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::{paths, tr_args};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsCache {
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub last_computed_date: Option<String>,
    #[serde(default)]
    pub daily_activity: Vec<DailyActivity>,
    #[serde(default)]
    pub daily_model_tokens: Vec<DailyModelTokens>,
    #[serde(default)]
    pub model_usage: BTreeMap<String, ModelUsage>,
    #[serde(default)]
    pub total_sessions: i64,
    #[serde(default)]
    pub total_messages: i64,
    #[serde(default)]
    pub first_session_date: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyActivity {
    pub date: String,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub session_count: i64,
    #[serde(default)]
    pub tool_call_count: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyModelTokens {
    pub date: String,
    #[serde(default)]
    pub tokens_by_model: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_input_tokens: i64,
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
    #[serde(default)]
    pub web_search_requests: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl ModelUsage {
    /// All input tokens, including cache traffic.
    pub fn total_input(&self) -> i64 {
        self.input_tokens + self.cache_read_input_tokens + self.cache_creation_input_tokens
    }

    pub fn total(&self) -> i64 {
        self.total_input() + self.output_tokens
    }
}

impl StatsCache {
    /// Reads the cache from the Claude Code home directory.
    ///
    /// `Ok(None)` when the file does not exist: Claude Code creates it lazily.
    pub fn load() -> Result<Option<Self>> {
        let path = paths::claude_stats_cache()?;
        match std::fs::read_to_string(&path) {
            Ok(raw) => Ok(Some(serde_json::from_str(&raw).with_context(|| {
                tr_args("error.parse_file", &[("path", &path.display().to_string())])
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| {
                tr_args("error.read_file", &[("path", &path.display().to_string())])
            }),
        }
    }

    /// Flattens `dailyModelTokens` into `(date, model, tokens)` — the shape the
    /// `daily_tokens` table takes.
    pub fn flatten_daily_tokens(&self) -> Vec<(String, String, i64)> {
        self.daily_model_tokens
            .iter()
            .flat_map(|day| {
                day.tokens_by_model
                    .iter()
                    .map(|(model, tokens)| (day.date.clone(), model.clone(), *tokens))
            })
            .collect()
    }

    /// Total tokens across all models on a given day.
    pub fn tokens_on(&self, date: &str) -> i64 {
        self.daily_model_tokens
            .iter()
            .filter(|d| d.date == date)
            .flat_map(|d| d.tokens_by_model.values())
            .sum()
    }

    /// Models sorted by descending total tokens.
    pub fn models_by_usage(&self) -> Vec<(&str, &ModelUsage)> {
        let mut models: Vec<_> = self.model_usage.iter().map(|(k, v)| (k.as_str(), v)).collect();
        models.sort_by_key(|(_, usage)| std::cmp::Reverse(usage.total()));
        models
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed fragment of a real `stats-cache.json`.
    const SAMPLE: &str = r#"{
        "version": 5,
        "lastComputedDate": "2026-08-05",
        "dailyActivity": [
            { "date": "2025-12-30", "messageCount": 349, "sessionCount": 1, "toolCallCount": 82 }
        ],
        "dailyModelTokens": [
            { "date": "2026-05-10", "tokensByModel": { "claude-opus-4-7": 16704257 } },
            { "date": "2026-05-14", "tokensByModel": { "claude-opus-4-7": 100, "claude-sonnet-5": 50 } }
        ],
        "dailyModelTokensVersion": 1,
        "modelUsage": {
            "claude-opus-5": {
                "inputTokens": 98677, "outputTokens": 9112097,
                "cacheReadInputTokens": 1825317886, "cacheCreationInputTokens": 47472536,
                "webSearchRequests": 0, "costUSD": 0, "contextWindow": 0, "maxOutputTokens": 0
            },
            "claude-sonnet-5": {
                "inputTokens": 8482, "outputTokens": 139072,
                "cacheReadInputTokens": 4579280, "cacheCreationInputTokens": 368992,
                "webSearchRequests": 0, "costUSD": 0, "contextWindow": 0, "maxOutputTokens": 0
            }
        },
        "totalSessions": 425,
        "totalMessages": 1000,
        "longestSession": { "messages": 1 },
        "firstSessionDate": "2025-12-22T14:34:41.654920Z",
        "hourCounts": { "9": 12 }
    }"#;

    fn parse() -> StatsCache {
        serde_json::from_str(SAMPLE).expect("the real format parses")
    }

    #[test]
    fn parses_real_shape() {
        let cache = parse();
        assert_eq!(cache.version, 5);
        assert_eq!(cache.total_sessions, 425);
        assert_eq!(cache.daily_activity[0].message_count, 349);
        assert_eq!(cache.model_usage.len(), 2);
    }

    #[test]
    fn flattens_daily_tokens() {
        let flat = parse().flatten_daily_tokens();
        assert_eq!(flat.len(), 3);
        assert!(flat.contains(&("2026-05-14".into(), "claude-sonnet-5".into(), 50)));
    }

    #[test]
    fn sums_tokens_for_a_day() {
        assert_eq!(parse().tokens_on("2026-05-14"), 150);
        assert_eq!(parse().tokens_on("1999-01-01"), 0);
    }

    #[test]
    fn ranks_models_by_total_tokens() {
        let cache = parse();
        let ranked = cache.models_by_usage();
        assert_eq!(ranked[0].0, "claude-opus-5", "the busiest model comes first");

        let opus = ranked[0].1;
        assert_eq!(opus.total_input(), 98677 + 1825317886 + 47472536);
        assert_eq!(opus.total(), opus.total_input() + 9112097);
    }

    #[test]
    fn tolerates_unknown_and_missing_fields() {
        let cache: StatsCache = serde_json::from_str(r#"{"version":9,"brandNew":true}"#).unwrap();
        assert_eq!(cache.version, 9);
        assert!(cache.daily_model_tokens.is_empty());
    }
}
