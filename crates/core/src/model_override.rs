//! Choosing which models Claude Code uses.
//!
//! `opus`, `sonnet` and `haiku` are aliases, and Claude Code decides on its own
//! which release each of them means. That is not always the release a person
//! wants: someone who got along with Opus 4.8 is moved onto Opus 5 without being
//! asked. The resolution can be redirected, and every lever for it sits in
//! `~/.claude/settings.json` — either the top-level `model` key or an
//! environment variable under `env`.
//!
//! **Nothing here is checked against the account.** Which models a subscription
//! may run is known only to Claude Code, and a name it does not accept surfaces
//! as a session that refuses to start. So a value is offered from a list and then
//! taken as typed: the list is a convenience, not a gate. `Warning` carries what
//! could be worked out without asking Claude Code, which is less than a person
//! might hope.

use std::path::PathBuf;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{paths, settings, tr, tr_args};

/// The top-level settings key holding the model a session starts on.
pub const MODEL_KEY: &str = "model";

/// The object inside the settings holding environment variables.
const ENV_KEY: &str = "env";

/// Variable that overrides the session model. Does the same job as [`MODEL_KEY`],
/// which is what this module writes instead — two ways of saying one thing.
const MODEL_ENV_VAR: &str = "ANTHROPIC_MODEL";

/// The name the Haiku override used to go by. Still honoured by Claude Code, but
/// a settings file carrying it is worth pointing at.
const LEGACY_HAIKU_VAR: &str = "ANTHROPIC_SMALL_FAST_MODEL";

/// Suffix asking Claude Code for the 1M-token context of a model that has one.
///
/// Not part of any model id — Claude Code strips it and passes the rest on — so
/// it has to survive a value going through here untouched.
pub const LONG_CONTEXT_SUFFIX: &str = "[1m]";

/// Opus releases, newest first.
const OPUS: &[&str] =
    &["claude-opus-5", "claude-opus-4-8", "claude-opus-4-7", "claude-opus-4-6", "claude-opus-4-5"];

/// Sonnet releases, newest first.
const SONNET: &[&str] = &["claude-sonnet-5", "claude-sonnet-4-6", "claude-sonnet-4-5"];

/// Haiku releases. The one family with no 1M context.
const HAIKU: &[&str] = &["claude-haiku-4-5"];

/// Models outside the three aliased families.
const OTHER: &[&str] = &["claude-fable-5"];

/// Names Claude Code resolves for itself. `opusplan` is Opus for planning and
/// Sonnet for the work.
pub const ALIASES: &[&str] = &["opus", "sonnet", "haiku", "opusplan"];

/// Which model a slot points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// The model a session starts on, before anything is chosen with `/model`.
    Default,
    /// What the `opus` alias resolves to — the reason this module exists.
    Opus,
    Sonnet,
    Haiku,
    /// What subagents run on, whatever the session itself uses.
    Subagent,
}

impl Slot {
    pub const ALL: [Slot; 5] =
        [Slot::Default, Slot::Opus, Slot::Sonnet, Slot::Haiku, Slot::Subagent];

    /// How many slots there are — the width of a row of editable fields.
    pub const COUNT: usize = Slot::ALL.len();

    /// The name used on the command line and to build translation keys.
    pub fn name(self) -> &'static str {
        match self {
            Slot::Default => "default",
            Slot::Opus => "opus",
            Slot::Sonnet => "sonnet",
            Slot::Haiku => "haiku",
            Slot::Subagent => "subagent",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|slot| slot.name() == name.trim().to_lowercase())
    }

    /// The settings key the value is written to, for showing next to the field.
    ///
    /// Whoever edits `settings.json` by hand deserves to be told which line this
    /// page is going to touch.
    pub fn key(self) -> &'static str {
        match self {
            Slot::Default => MODEL_KEY,
            Slot::Opus => "ANTHROPIC_DEFAULT_OPUS_MODEL",
            Slot::Sonnet => "ANTHROPIC_DEFAULT_SONNET_MODEL",
            Slot::Haiku => "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            Slot::Subagent => "CLAUDE_CODE_SUBAGENT_MODEL",
        }
    }

    /// Whether the value lives under `env` rather than at the top level.
    fn in_env(self) -> bool {
        self != Slot::Default
    }

    pub fn label(self) -> String {
        tr(&format!("settings.models.slot.{}", self.name()))
    }

    pub fn hint(self) -> String {
        tr(&format!("settings.models.hint.{}", self.name()))
    }

    /// Names worth offering for this slot, most recent first.
    ///
    /// An alias slot offers only its own family: redirecting `opus` at a Sonnet
    /// would work, and would make every later `/model opus` a lie.
    pub fn suggestions(self) -> Vec<String> {
        let families: &[&[&str]] = match self {
            Slot::Opus => &[OPUS],
            Slot::Sonnet => &[SONNET],
            Slot::Haiku => &[HAIKU],
            Slot::Default | Slot::Subagent => &[OPUS, SONNET, HAIKU, OTHER],
        };

        let aliases: &[&str] = match self {
            // An alias slot pointed at an alias is either a no-op or a loop.
            Slot::Default | Slot::Subagent => ALIASES,
            _ => &[],
        };

        let mut out = Vec::new();
        for name in aliases.iter().chain(families.iter().flat_map(|f| f.iter())) {
            out.push((*name).to_string());
            if takes_long_context(name) {
                out.push(format!("{name}{LONG_CONTEXT_SUFFIX}"));
            }
        }
        out
    }
}

/// Whether a name can carry [`LONG_CONTEXT_SUFFIX`].
fn takes_long_context(name: &str) -> bool {
    !name.contains("haiku")
}

/// Whether this build recognises the name, suffix and all.
///
/// A `false` is not a verdict — ids appear faster than releases of this program —
/// only grounds for asking the user to look twice.
pub fn is_known(value: &str) -> bool {
    let bare = value.strip_suffix(LONG_CONTEXT_SUFFIX).unwrap_or(value);
    [OPUS, SONNET, HAIKU, OTHER, ALIASES].iter().any(|group| group.contains(&bare))
}

/// What each slot is set to, if anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides([Option<String>; Slot::COUNT]);

impl Overrides {
    pub fn get(&self, slot: Slot) -> Option<&str> {
        self.0[slot as usize].as_deref()
    }

    /// Sets a slot, or clears it when the value is blank.
    ///
    /// A text field that has been emptied means "not set", not "set to the empty
    /// string" — the latter would be written into the file and break sessions.
    pub fn set(&mut self, slot: Slot, value: &str) {
        let value = value.trim();
        self.0[slot as usize] = (!value.is_empty()).then(|| value.to_string());
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }

    /// The slots that are set, in declaration order.
    pub fn entries(&self) -> impl Iterator<Item = (Slot, &str)> {
        Slot::ALL.into_iter().filter_map(|slot| self.get(slot).map(|value| (slot, value)))
    }

    /// The values as editable text, blank where a slot is unset.
    ///
    /// What a screen of text fields needs, and the shape an edit comes back in —
    /// a field being edited holds a half-typed id that must not reach the file.
    pub fn to_fields(&self) -> [String; Slot::COUNT] {
        std::array::from_fn(|i| self.0[i].clone().unwrap_or_default())
    }

    /// Reads edited fields back, blank meaning unset.
    pub fn from_fields(fields: &[String; Slot::COUNT]) -> Self {
        let mut out = Self::default();
        for (slot, value) in Slot::ALL.into_iter().zip(fields) {
            out.set(slot, value);
        }
        out
    }

    /// Reads the overrides out of a settings document.
    pub fn from_settings(settings: &Map<String, Value>) -> Self {
        let env = settings.get(ENV_KEY).and_then(Value::as_object);
        let mut out = Self::default();
        for slot in Slot::ALL {
            let raw = if slot.in_env() {
                env.and_then(|env| env.get(slot.key()))
            } else {
                settings.get(slot.key())
            };
            if let Some(value) = raw.and_then(Value::as_str) {
                out.set(slot, value);
            }
        }
        out
    }

    /// Writes exactly this state into a settings document.
    ///
    /// Slots left unset have their keys removed rather than left alone: the
    /// screen showing them empty has to mean they will be empty, or the button
    /// cannot be trusted. Everything else in the document — and everything else
    /// under `env` — is preserved.
    pub fn apply_to(&self, settings: &mut Map<String, Value>) -> Result<()> {
        match self.get(Slot::Default) {
            Some(value) => settings.insert(MODEL_KEY.into(), Value::String(value.into())),
            None => settings.remove(MODEL_KEY),
        };

        let env_slots: Vec<Slot> = Slot::ALL.into_iter().filter(|slot| slot.in_env()).collect();
        // Nothing to write and no `env` to clean out: touching the document at
        // all would only add an empty object to it.
        if !settings.contains_key(ENV_KEY)
            && env_slots.iter().all(|slot| self.get(*slot).is_none())
        {
            return Ok(());
        }

        let env = settings::object_mut(settings, ENV_KEY)?;
        for slot in env_slots {
            match self.get(slot) {
                Some(value) => env.insert(slot.key().into(), Value::String(value.into())),
                None => env.remove(slot.key()),
            };
        }
        if env.is_empty() {
            settings.remove(ENV_KEY);
        }
        Ok(())
    }
}

/// Reads which overrides the Claude Code settings currently carry.
pub fn read() -> Result<Overrides> {
    Ok(Overrides::from_settings(&settings::read()?))
}

/// Reads the overrides together with what is worth warning about them.
pub fn read_with_warnings() -> Result<(Overrides, Vec<Warning>)> {
    let settings = settings::read()?;
    let overrides = Overrides::from_settings(&settings);
    let warnings = warnings(&overrides, &settings, |name| std::env::var(name).ok());
    Ok((overrides, warnings))
}

/// Writes the overrides to the Claude Code settings, returning the file written.
pub fn apply(overrides: &Overrides) -> Result<PathBuf> {
    let mut settings = settings::read()?;
    overrides.apply_to(&mut settings)?;
    settings::write(&settings)?;
    paths::claude_settings()
}

/// Removes every override this module knows how to set.
pub fn clear() -> Result<PathBuf> {
    apply(&Overrides::default())
}

/// Something about the current state a person should know before trusting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The same variable is set in the environment this program was started
    /// from. Which value a session ends up with then depends on how Claude Code
    /// was launched, which is not something either file can settle.
    Environment { name: String, value: String },
    /// `ANTHROPIC_MODEL` sits under `env` while the top-level `model` key is
    /// what this page writes. Both name the session model; the one that loses is
    /// not worth guessing at.
    DuplicateDefault { value: String },
    /// The settings still carry the name the Haiku override used to go by.
    Legacy { name: String, value: String },
    /// A name this build has not heard of.
    Unknown { slot: Slot, value: String },
}

impl Warning {
    pub fn text(&self) -> String {
        match self {
            Warning::Environment { name, value } => {
                tr_args("settings.models.warn.environment", &[("name", name), ("value", value)])
            }
            Warning::DuplicateDefault { value } => {
                tr_args("settings.models.warn.duplicate", &[("value", value)])
            }
            Warning::Legacy { name, value } => {
                tr_args("settings.models.warn.legacy", &[("name", name), ("value", value)])
            }
            Warning::Unknown { slot, value } => tr_args(
                "settings.models.warn.unknown",
                &[("slot", &slot.label()), ("value", value)],
            ),
        }
    }
}

/// Works out what to warn about, given a way to look up the environment.
///
/// The lookup is a parameter rather than `std::env` directly so that the rule
/// can be tested: process environment is global, and tests share a process.
pub fn warnings(
    overrides: &Overrides,
    settings: &Map<String, Value>,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<Warning> {
    let mut out = Vec::new();

    for (slot, value) in overrides.entries() {
        if !is_known(value) {
            out.push(Warning::Unknown { slot, value: value.to_string() });
        }
    }

    let settings_env = settings.get(ENV_KEY).and_then(Value::as_object);
    let in_settings_env = |name: &str| {
        settings_env.and_then(|e| e.get(name)).and_then(Value::as_str).map(str::to_string)
    };

    if let Some(value) = in_settings_env(MODEL_ENV_VAR) {
        out.push(Warning::DuplicateDefault { value });
    }
    if let Some(value) = in_settings_env(LEGACY_HAIKU_VAR) {
        out.push(Warning::Legacy { name: LEGACY_HAIKU_VAR.to_string(), value });
    }

    // The real environment beats nothing reliably, but it is the one thing that
    // can make this screen describe a state no session is actually in.
    for name in [MODEL_ENV_VAR, LEGACY_HAIKU_VAR]
        .into_iter()
        .chain(Slot::ALL.into_iter().filter(|s| s.in_env()).map(Slot::key))
    {
        if let Some(value) = env(name) {
            out.push(Warning::Environment { name: name.to_string(), value });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{self, Language};
    use serde_json::json;

    fn settings_with(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn reads_the_top_level_key_and_the_environment_ones() {
        let settings = settings_with(json!({
            "model": "opus[1m]",
            "env": {
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-8",
                "CLAUDE_CODE_SUBAGENT_MODEL": "claude-haiku-4-5",
            },
        }));
        let overrides = Overrides::from_settings(&settings);

        assert_eq!(overrides.get(Slot::Default), Some("opus[1m]"));
        assert_eq!(overrides.get(Slot::Opus), Some("claude-opus-4-8"));
        assert_eq!(overrides.get(Slot::Subagent), Some("claude-haiku-4-5"));
        assert_eq!(overrides.get(Slot::Sonnet), None);
    }

    #[test]
    fn survives_a_roundtrip_through_the_document() {
        let mut overrides = Overrides::default();
        overrides.set(Slot::Opus, "claude-opus-4-8");
        overrides.set(Slot::Default, "opus");

        let mut settings = Map::new();
        overrides.apply_to(&mut settings).unwrap();
        assert_eq!(Overrides::from_settings(&settings), overrides);
    }

    /// The settings file is the user's. Everything we did not put there — other
    /// keys, other variables, the hook itself — has to come out unharmed.
    #[test]
    fn writing_leaves_the_rest_of_the_file_alone() {
        let mut settings = settings_with(json!({
            "statusLine": { "type": "command", "command": "\"/opt/cs/claude-status\" hook" },
            "theme": "dark",
            "env": { "EDITOR": "vim", "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-7" },
        }));

        let mut overrides = Overrides::from_settings(&settings);
        overrides.set(Slot::Opus, "claude-opus-4-8");
        overrides.apply_to(&mut settings).unwrap();

        assert_eq!(settings["theme"], json!("dark"));
        assert!(settings.contains_key("statusLine"));
        assert_eq!(settings["env"]["EDITOR"], json!("vim"));
        assert_eq!(settings["env"]["ANTHROPIC_DEFAULT_OPUS_MODEL"], json!("claude-opus-4-8"));
    }

    /// An emptied field means the override is gone, not set to `""` — writing an
    /// empty string would leave sessions refusing to start.
    #[test]
    fn clearing_removes_the_keys_rather_than_blanking_them() {
        let mut settings = settings_with(json!({
            "model": "opus",
            "env": { "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-8", "EDITOR": "vim" },
        }));

        let mut overrides = Overrides::from_settings(&settings);
        overrides.set(Slot::Default, "   ");
        overrides.set(Slot::Opus, "");
        assert!(overrides.is_empty());
        overrides.apply_to(&mut settings).unwrap();

        assert!(!settings.contains_key("model"));
        assert!(!settings["env"].as_object().unwrap().contains_key("ANTHROPIC_DEFAULT_OPUS_MODEL"));
        assert_eq!(settings["env"]["EDITOR"], json!("vim"), "somebody else's variable");
    }

    /// `"env": {}` says nothing and belongs to nobody; an `env` we emptied
    /// ourselves should not be left behind in someone else's file.
    #[test]
    fn an_env_emptied_of_our_keys_is_removed() {
        let mut settings =
            settings_with(json!({ "env": { "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-8" } }));
        Overrides::default().apply_to(&mut settings).unwrap();
        assert!(!settings.contains_key("env"), "{settings:?}");
    }

    #[test]
    fn nothing_set_creates_nothing() {
        let mut settings = Map::new();
        Overrides::default().apply_to(&mut settings).unwrap();
        assert!(settings.is_empty(), "{settings:?}");
    }

    /// An `env` holding something other than an object is a setting we do not
    /// understand; replacing it wholesale would destroy it silently.
    #[test]
    fn refuses_to_overwrite_an_env_that_is_not_an_object() {
        let mut settings = settings_with(json!({ "env": "inherit" }));
        let mut overrides = Overrides::default();
        overrides.set(Slot::Opus, "claude-opus-4-8");
        assert!(overrides.apply_to(&mut settings).is_err());
        assert_eq!(settings["env"], json!("inherit"), "left as it was");
    }

    #[test]
    fn the_long_context_suffix_is_recognised_but_haiku_has_none() {
        assert!(is_known("claude-opus-4-8"));
        assert!(is_known("claude-opus-4-8[1m]"));
        assert!(is_known("opus"));
        assert!(!is_known("claude-opus-9"));

        assert!(Slot::Opus.suggestions().contains(&"claude-opus-4-8[1m]".to_string()));
        assert!(!Slot::Haiku.suggestions().iter().any(|s| s.contains(LONG_CONTEXT_SUFFIX)));
    }

    /// Pointing `opus` at a Sonnet would work and would make every later
    /// `/model opus` misleading, so the list does not propose it.
    #[test]
    fn an_alias_slot_only_offers_its_own_family() {
        let opus = Slot::Opus.suggestions();
        assert!(opus.iter().all(|s| s.contains("opus")), "{opus:?}");
        assert!(!opus.iter().any(|s| ALIASES.contains(&s.as_str())), "{opus:?}");

        let default = Slot::Default.suggestions();
        assert!(default.contains(&"opus".to_string()));
        assert!(default.iter().any(|s| s.contains("sonnet")));
    }

    /// A field emptied on screen has to reach the file as "remove the key",
    /// which is what makes the screen and the file mean the same thing.
    #[test]
    fn fields_round_trip_and_blanks_mean_unset() {
        let mut overrides = Overrides::default();
        overrides.set(Slot::Opus, "claude-opus-4-8");

        let mut fields = overrides.to_fields();
        assert_eq!(fields[Slot::Opus as usize], "claude-opus-4-8");
        assert_eq!(fields[Slot::Sonnet as usize], "");
        assert_eq!(Overrides::from_fields(&fields), overrides);

        fields[Slot::Opus as usize] = "  ".into();
        assert!(Overrides::from_fields(&fields).is_empty());
    }

    #[test]
    fn slots_round_trip_through_their_names() {
        for slot in Slot::ALL {
            assert_eq!(Slot::parse(slot.name()), Some(slot));
        }
        assert_eq!(Slot::parse("OPUS"), Some(Slot::Opus));
        assert_eq!(Slot::parse("frobnicate"), None);
    }

    /// Labels and hints are built from the slot name, so no scan of the sources
    /// can catch a missing one — this asks for each of them instead.
    #[test]
    fn every_slot_has_a_label_and_a_hint_in_both_languages() {
        for language in [Language::En, Language::Ru] {
            let _guard = i18n::test_guard(language);
            for slot in Slot::ALL {
                assert!(!slot.label().starts_with("settings.models."), "{slot:?} {language:?}");
                assert!(!slot.hint().starts_with("settings.models."), "{slot:?} {language:?}");
            }
        }
    }

    #[test]
    fn an_unfamiliar_name_is_reported_but_not_refused() {
        let mut overrides = Overrides::default();
        overrides.set(Slot::Opus, "claude-opus-9-9");
        let found = warnings(&overrides, &Map::new(), no_env);

        assert_eq!(
            found,
            vec![Warning::Unknown { slot: Slot::Opus, value: "claude-opus-9-9".into() }]
        );
        assert_eq!(overrides.get(Slot::Opus), Some("claude-opus-9-9"), "still set");
    }

    #[test]
    fn a_variable_in_the_real_environment_is_reported() {
        let found = warnings(&Overrides::default(), &Map::new(), |name| {
            (name == "ANTHROPIC_DEFAULT_OPUS_MODEL").then(|| "claude-opus-4-6".to_string())
        });
        assert_eq!(
            found,
            vec![Warning::Environment {
                name: "ANTHROPIC_DEFAULT_OPUS_MODEL".into(),
                value: "claude-opus-4-6".into(),
            }]
        );
    }

    /// Two keys naming the session model, one of which this page does not write.
    #[test]
    fn anthropic_model_beside_the_model_key_is_reported() {
        let settings = settings_with(json!({
            "model": "opus",
            "env": { "ANTHROPIC_MODEL": "claude-sonnet-5", "ANTHROPIC_SMALL_FAST_MODEL": "x" },
        }));
        let found = warnings(&Overrides::from_settings(&settings), &settings, no_env);

        assert!(found.contains(&Warning::DuplicateDefault { value: "claude-sonnet-5".into() }));
        assert!(found.iter().any(|w| matches!(w, Warning::Legacy { .. })));
    }

    #[test]
    fn a_plain_state_warns_about_nothing() {
        let settings = settings_with(json!({ "env": { "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-8" } }));
        let found = warnings(&Overrides::from_settings(&settings), &settings, no_env);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn warning_texts_are_translated() {
        let _guard = i18n::test_guard(Language::En);
        let all = [
            Warning::Environment { name: "A".into(), value: "b".into() },
            Warning::DuplicateDefault { value: "b".into() },
            Warning::Legacy { name: "A".into(), value: "b".into() },
            Warning::Unknown { slot: Slot::Opus, value: "b".into() },
        ];
        for warning in all {
            let text = warning.text();
            // The English text of one of them opens with "settings.json", so the
            // check has to be for the key itself, not merely its first word.
            assert!(!text.starts_with("settings.models."), "{warning:?} -> {text}");
            assert!(!text.contains("%{"), "unsubstituted placeholder: {text}");
        }
    }
}
