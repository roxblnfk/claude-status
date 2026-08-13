//! Reading `~/.claude/stats-cache.json` — the aggregates Claude Code keeps.
//!
//! Only the lifetime counters are taken from it now. Its daily breakdown used
//! to feed the plots, until Claude Code quietly stopped recomputing the file
//! and it stood a week stale; the tokens are counted from the session logs
//! instead (see [`crate::scan`]). These two counters are what the logs cannot
//! give: they reach back past the oldest session still on disk.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::{paths, tr_args};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsCache {
    #[serde(default)]
    pub total_sessions: i64,
    #[serde(default)]
    pub first_session_date: Option<String>,
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

}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed fragment of a real `stats-cache.json`. The fields left in are
    /// there to prove the rest of the file is ignored rather than choked on.
    const SAMPLE: &str = r#"{
        "version": 5,
        "lastComputedDate": "2026-08-05",
        "dailyActivity": [
            { "date": "2025-12-30", "messageCount": 349, "sessionCount": 1, "toolCallCount": 82 }
        ],
        "dailyModelTokens": [
            { "date": "2026-05-10", "tokensByModel": { "claude-opus-4-7": 16704257 } }
        ],
        "modelUsage": {
            "claude-opus-5": { "inputTokens": 98677, "outputTokens": 9112097 }
        },
        "totalSessions": 425,
        "totalMessages": 1000,
        "firstSessionDate": "2025-12-22T14:34:41.654920Z",
        "hourCounts": { "9": 12 }
    }"#;

    #[test]
    fn reads_the_lifetime_counters() {
        let cache: StatsCache = serde_json::from_str(SAMPLE).expect("the real format parses");
        assert_eq!(cache.total_sessions, 425);
        assert_eq!(cache.first_session_date.as_deref(), Some("2025-12-22T14:34:41.654920Z"));
    }

    #[test]
    fn tolerates_unknown_and_missing_fields() {
        let cache: StatsCache = serde_json::from_str(r#"{"brandNew":true}"#).unwrap();
        assert_eq!(cache.total_sessions, 0);
        assert!(cache.first_session_date.is_none());
    }
}
