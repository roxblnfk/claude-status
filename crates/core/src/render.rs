//! Assembling the status line from a user-defined template.
//!
//! A template is plain text with `{name}` placeholders. An unknown placeholder
//! is left in the string verbatim: a typo in the template is then immediately
//! visible instead of silently vanishing.

use crate::config::Config;
use crate::pace::{Overview, WindowState};
use crate::statusline::StatuslineInput;
use crate::{timefmt, tr_args};

/// Substituted when data is missing.
pub const MISSING: &str = "—";

/// Every supported placeholder: `(name, description translation key)`.
///
/// Used by the settings screen to render the reference list.
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    ("{model}", "render.placeholder.model"),
    ("{model_id}", "render.placeholder.model_id"),
    ("{effort}", "render.placeholder.effort"),
    ("{session}", "render.placeholder.session"),
    ("{dir}", "render.placeholder.dir"),
    ("{cost}", "render.placeholder.cost"),
    ("{ctx_pct}", "render.placeholder.ctx_pct"),
    ("{ctx_bar}", "render.placeholder.ctx_bar"),
    ("{five_pct}", "render.placeholder.five_pct"),
    ("{five_bar}", "render.placeholder.five_bar"),
    ("{five_reset}", "render.placeholder.five_reset"),
    ("{five_left}", "render.placeholder.five_left"),
    ("{week_pct}", "render.placeholder.week_pct"),
    ("{week_bar}", "render.placeholder.week_bar"),
    ("{week_reset}", "render.placeholder.week_reset"),
    ("{week_left}", "render.placeholder.week_left"),
    ("{opus_pct}", "render.placeholder.opus_pct"),
    ("{opus_bar}", "render.placeholder.opus_bar"),
    ("{pace}", "render.placeholder.pace"),
    ("{daily}", "render.placeholder.daily"),
    ("{today_left}", "render.placeholder.today_left"),
    ("{burn}", "render.placeholder.burn"),
];

/// The data a line is assembled from.
pub struct RenderContext<'a> {
    /// JSON of the current invocation: model, context, cost.
    pub input: Option<&'a StatuslineInput>,
    /// State of the limit windows.
    pub overview: &'a Overview,
    pub config: &'a Config,
    /// Render time, unix seconds.
    pub now: i64,
}

/// Assembles the status line using the template from the configuration.
pub fn render(ctx: &RenderContext<'_>) -> String {
    render_template(&ctx.config.statusline.template, ctx)
}

/// Assembles a line from an arbitrary template — used by the GUI preview.
pub fn render_template(template: &str, ctx: &RenderContext<'_>) -> String {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        match tail.find('}') {
            Some(end) => {
                let name = &tail[1..end];
                match substitute(name, ctx) {
                    Some(value) => out.push_str(&value),
                    // Unknown placeholder: keep it verbatim.
                    None => out.push_str(&tail[..=end]),
                }
                rest = &tail[end + 1..];
            }
            // An unclosed brace leaves nothing more to substitute.
            None => {
                out.push_str(tail);
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

fn substitute(name: &str, ctx: &RenderContext<'_>) -> Option<String> {
    let cfg = ctx.config;
    let input = ctx.input;
    let ov = ctx.overview;
    let width = cfg.statusline.bar_width;

    let value = match name {
        "model" => opt(input.and_then(|i| i.model_name()).map(str::to_string)),
        "model_id" => opt(input.and_then(|i| i.model_id()).map(str::to_string)),
        "effort" => opt(input
            .and_then(|i| i.effort.as_ref())
            .and_then(|e| e.level.clone())),
        "session" => opt(input.and_then(|i| i.session_name.clone())),
        "dir" => opt(input.and_then(|i| i.cwd.as_deref()).map(basename)),
        "cost" => opt(input
            .and_then(|i| i.cost.as_ref())
            .and_then(|c| c.total_cost_usd)
            .map(|c| format!("${c:.2}"))),

        "ctx_pct" => {
            let pct = input
                .and_then(|i| i.context_window.as_ref())
                .and_then(|c| c.used_percentage);
            opt(pct.map(|p| colorize(format!("{p:.0}"), p, cfg)))
        }
        "ctx_bar" => {
            let pct = input
                .and_then(|i| i.context_window.as_ref())
                .and_then(|c| c.used_percentage);
            bar(pct.unwrap_or(0.0), width)
        }

        "five_pct" => window_pct(ov.five_hour, cfg),
        "five_bar" => bar(ov.five_hour.map_or(0.0, |w| w.used_pct), width),
        "five_reset" => opt(ov.five_hour.map(|w| timefmt::clock(w.resets_at))),
        "five_left" => opt(ov.five_hour.map(|w| timefmt::duration(w.remaining_secs()))),

        "week_pct" => window_pct(ov.week, cfg),
        "week_bar" => bar(ov.week.map_or(0.0, |w| w.used_pct), width),
        "week_reset" => opt(ov.week.map(|w| timefmt::date(w.resets_at))),
        "week_left" => opt(ov.week.map(|w| timefmt::duration(w.remaining_secs()))),

        "opus_pct" => window_pct(ov.week_opus, cfg),
        "opus_bar" => bar(ov.week_opus.map_or(0.0, |w| w.used_pct), width),

        "pace" => opt(ov.week.map(|w| pace_label(w.deviation_pct(), cfg))),
        "daily" => opt(ov
            .week
            .and_then(|w| w.allowance_per_day_pct())
            .map(|p| format!("{p:.1}%"))),
        "today_left" => opt(ov.daily.map(|d| format!("{:.1}%", d.remaining_pct()))),
        "burn" => opt(ov
            .week_burn
            .map(|b| tr_args("render.per_day", &[("value", &format!("{:.1}", b.pct_per_day))]))),

        _ => return None,
    };
    Some(value)
}

fn opt(value: Option<String>) -> String {
    value.unwrap_or_else(|| MISSING.to_string())
}

fn window_pct(window: Option<WindowState>, cfg: &Config) -> String {
    opt(window.map(|w| colorize(format!("{:.0}", w.used_pct), w.used_pct, cfg)))
}

/// Textual fill bar.
fn bar(pct: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = ((pct.clamp(0.0, 100.0) / 100.0) * width as f64).round() as usize;
    let mut s = String::with_capacity(width * 3);
    for i in 0..width {
        s.push(if i < filled { '▓' } else { '░' });
    }
    s
}

/// Deviation from the even pace: `+12` means running ahead (spending fast).
fn pace_label(deviation: f64, cfg: &Config) -> String {
    let arrow = if deviation > cfg.budget.warn_deviation_pct {
        '↑'
    } else if deviation < -cfg.budget.warn_deviation_pct {
        '↓'
    } else {
        '≈'
    };
    let text = format!("{arrow}{:+.0}", deviation);
    if !cfg.statusline.colors {
        return text;
    }
    let color = if deviation > cfg.budget.critical_deviation_pct {
        RED
    } else if deviation > cfg.budget.warn_deviation_pct {
        YELLOW
    } else {
        GREEN
    };
    format!("{color}{text}{RESET}")
}

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

fn colorize(text: String, pct: f64, cfg: &Config) -> String {
    if !cfg.statusline.colors {
        return text;
    }
    let color = if pct >= 80.0 {
        RED
    } else if pct >= 50.0 {
        YELLOW
    } else {
        GREEN
    };
    format!("{color}{text}{RESET}")
}

/// Last path segment, for both `/` and `\`.
fn basename(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|s| !s.is_empty())
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Sample;
    use crate::i18n::{self, Language};
    use crate::statusline::SEVEN_DAY_SECS;

    fn plain_config() -> Config {
        let mut cfg = Config::default();
        cfg.statusline.colors = false; // tests compare text, not ANSI
        cfg.statusline.bar_width = 4;
        cfg
    }

    fn overview(week_pct: f64, now: i64) -> Overview {
        let samples = vec![Sample {
            id: 1,
            ts: now,
            last_seen_ts: now,
            five_pct: Some(50.0),
            five_resets_at: Some(now + 3600),
            week_pct: Some(week_pct),
            week_resets_at: Some(now + SEVEN_DAY_SECS),
            ..Sample::default()
        }];
        Overview::from_samples(&samples, now)
    }

    fn ctx<'a>(cfg: &'a Config, ov: &'a Overview, now: i64) -> RenderContext<'a> {
        RenderContext { input: None, overview: ov, config: cfg, now }
    }

    #[test]
    fn substitutes_known_placeholders() {
        let cfg = plain_config();
        let ov = overview(30.0, 1_000_000);
        let out = render_template("5h {five_pct}% week {week_pct}%", &ctx(&cfg, &ov, 1_000_000));
        assert_eq!(out, "5h 50% week 30%");
    }

    #[test]
    fn unknown_placeholder_is_left_verbatim() {
        let cfg = plain_config();
        let ov = overview(30.0, 0);
        assert_eq!(render_template("a {nope} b", &ctx(&cfg, &ov, 0)), "a {nope} b");
    }

    #[test]
    fn unclosed_brace_does_not_panic() {
        let cfg = plain_config();
        let ov = overview(30.0, 0);
        assert_eq!(render_template("a {week_pct", &ctx(&cfg, &ov, 0)), "a {week_pct");
    }

    #[test]
    fn missing_data_renders_as_dash() {
        let cfg = plain_config();
        let ov = Overview::default();
        assert_eq!(render_template("{week_pct} {model}", &ctx(&cfg, &ov, 0)), "— —");
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(bar(0.0, 4), "░░░░");
        assert_eq!(bar(50.0, 4), "▓▓░░");
        assert_eq!(bar(100.0, 4), "▓▓▓▓");
        assert_eq!(bar(150.0, 4), "▓▓▓▓", "exceeding the limit keeps the width");
        assert_eq!(bar(50.0, 0), "");
    }

    #[test]
    fn pace_arrow_reflects_direction() {
        let cfg = plain_config();
        // The window has just started but 40 % is gone — well ahead.
        let ov = overview(40.0, 1_000_000);
        let out = render_template("{pace}", &ctx(&cfg, &ov, 1_000_000));
        assert!(out.starts_with('↑'), "{out}");
    }

    #[test]
    fn colors_wrap_values_when_enabled() {
        let cfg = Config::default(); // colors = true
        let ov = overview(30.0, 0);
        let out = render_template("{week_pct}", &ctx(&cfg, &ov, 0));
        assert!(out.contains("\x1b["), "expected ANSI codes: {out:?}");
    }

    /// An unknown placeholder survives verbatim, so a leftover brace in the
    /// output means a typo in the preset.
    #[test]
    fn every_preset_uses_only_known_placeholders() {
        let cfg = plain_config();
        let ov = overview(30.0, 1_000_000);

        for (key, template) in crate::config::PRESETS {
            let out = render_template(template, &ctx(&cfg, &ov, 1_000_000));
            assert!(!out.contains('{'), "preset {key}: unknown placeholder in {out:?}");
            assert!(!out.contains('}'), "preset {key}: unclosed brace in {out:?}");
        }
    }

    #[test]
    fn preset_keys_are_unique_and_first_is_the_default() {
        let mut keys: Vec<_> = crate::config::PRESETS.iter().map(|(key, _)| *key).collect();
        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), count, "preset keys must be distinct");
        assert_eq!(crate::config::PRESETS[0].1, crate::config::DEFAULT_TEMPLATE);
    }

    /// Every translation key referenced by the tables must exist. `rust-i18n`
    /// echoes the key back when it is missing, which would surface as raw
    /// `preset.default` text in the UI.
    #[test]
    fn preset_and_placeholder_keys_are_translated() {
        for language in [Language::En, Language::Ru] {
            let _guard = i18n::test_guard(language);

            for (key, _) in crate::config::PRESETS {
                assert_ne!(crate::tr(key), *key, "missing translation for {key} in {language:?}");
            }
            for (name, key) in PLACEHOLDERS {
                assert_ne!(crate::tr(key), *key, "missing description for {name} in {language:?}");
            }
        }
    }

    #[test]
    fn basename_handles_both_separators() {
        assert_eq!(basename("D:\\git\\roxblnfk\\claude-status"), "claude-status");
        assert_eq!(basename("/home/user/proj/"), "proj");
    }
}
