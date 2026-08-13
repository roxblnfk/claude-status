//! The "Models" settings page: which models Claude Code is pinned to.
//!
//! Unlike everything on the other pages, this writes into somebody else's file —
//! `~/.claude/settings.json` — so it behaves like the hook section rather than
//! like a configuration screen: what is on disk is read on every refresh and
//! shown as it is, and the edits go out on a button rather than on a keystroke.
//! The window has no authority to say what a session will actually run, only
//! what has been asked for; the notes at the bottom exist because the difference
//! is easy to miss.

use claude_status_core::{
    model_override::{self, Overrides, Slot, Warning},
    paths, tr, tr_args,
};
use eframe::egui;

use crate::state::AppState;

/// Values being edited, one per slot, blank where nothing is set.
pub type Fields = [String; Slot::COUNT];

/// What survives between frames.
#[derive(Default)]
pub struct ModelState {
    /// Loaded from the file on the first draw and after every write. Kept apart
    /// from it so that a half-typed id is never what gets saved — and so that a
    /// background refresh cannot yank a field out from under the cursor.
    fields: Option<Fields>,
    confirming_clear: bool,
}

impl ModelState {
    /// Disarms the confirmation, keeping the edits.
    ///
    /// Called on the way off the page: a question asked there should not still be
    /// waiting on the way back, while a half-finished edit should be.
    pub fn forget_confirmation(&mut self) {
        self.confirming_clear = false;
    }
}

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut AppState,
    ui_state: &mut ModelState,
    message: &mut Option<Result<String, String>>,
) {
    let ModelState { fields, confirming_clear } = ui_state;
    let fields = fields.get_or_insert_with(|| state.model_overrides.to_fields());
    let edited = Overrides::from_fields(fields);
    let on_disk = &state.model_overrides;
    // Set when the file has been written, which makes the draft and everything
    // else read from it stale. Acted on after the frame: the widgets below hold
    // borrows of both.
    let mut written = false;

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.models.title"));
        ui.add_space(4.0);
        ui.label(tr("settings.models.explanation"));
        ui.add_space(8.0);

        slots(ui, on_disk, fields);

        ui.add_space(6.0);
        ui.label(egui::RichText::new(tr("settings.models.suffix_note")).weak());

        if on_disk.is_empty() && edited.is_empty() {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tr("settings.models.nothing_set")).weak());
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let pending = edited != *on_disk;
            if ui.add_enabled(pending, egui::Button::new(tr("settings.models.apply"))).clicked() {
                *message = Some(write(&edited));
                written = true;
            }
            if pending {
                ui.label(
                    egui::RichText::new(tr("settings.models.pending"))
                        .color(crate::ui::level_color(80.0)),
                );
            }
        });

        ui.add_space(4.0);
        written |= clear_button(ui, on_disk, confirming_clear, message);

        if !state.model_warnings.is_empty() {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            for warning in &state.model_warnings {
                let level = match warning {
                    // A name we do not recognise is a maybe; a value that can be
                    // decided elsewhere makes this whole screen unreliable.
                    Warning::Unknown { .. } => 80.0,
                    _ => 95.0,
                };
                ui.colored_label(crate::ui::level_color(level), warning.text());
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        for note in [
            "settings.models.restart_note",
            "settings.models.availability_note",
            "settings.models.precedence_note",
        ] {
            ui.label(egui::RichText::new(tr(note)).weak());
            ui.add_space(2.0);
        }
        if let Ok(path) = paths::claude_settings() {
            ui.label(egui::RichText::new(path.display().to_string()).weak());
        }
    });

    if written {
        // Drop the draft so the next frame reads back what actually landed in
        // the file, which is not always what was asked for.
        ui_state.fields = None;
        state.refresh();
    }
}

/// Width reserved for the slot names.
///
/// Fixed rather than fitted: a grid column sized to its contents collapses to
/// the narrowest one and then wraps "Модель сессии" a letter at a time.
const LABEL_WIDTH: f32 = 150.0;

/// One row per slot: what it is, what it is set to, and where that is written.
fn slots(ui: &mut egui::Ui, on_disk: &Overrides, fields: &mut Fields) {
    for slot in Slot::ALL {
        ui.horizontal(|ui| {
            ui.scope(|ui| {
                ui.set_min_width(LABEL_WIDTH);
                ui.set_max_width(LABEL_WIDTH);
                ui.strong(slot.label());
            });
            field(ui, slot, &mut fields[slot as usize]);
        });

        let mut notes =
            vec![slot.hint(), tr_args("settings.models.writes_key", &[("key", slot.key())])];
        // The file can change under us — another window of ours, a hand-edited
        // settings.json — and an edit in progress must not hide that.
        let saved = on_disk.get(slot);
        if saved.unwrap_or_default() != fields[slot as usize].trim() {
            let value = saved.map(str::to_string).unwrap_or_else(|| tr("settings.models.unset"));
            notes.push(tr_args("settings.models.on_disk", &[("value", &value)]));
        }
        ui.label(egui::RichText::new(notes.join(" · ")).weak());
        ui.add_space(8.0);
    }
}

/// A model name: picked from a list, or typed when the list does not have it.
///
/// Both, rather than either: the list covers what is current at the time of this
/// release, and the field covers everything that comes after it — a dropdown
/// alone would go stale and start refusing perfectly good models.
fn field(ui: &mut egui::Ui, slot: Slot, value: &mut String) {
    let unset = tr("settings.models.unset");

    ui.add(
        egui::TextEdit::singleline(value)
            .desired_width(230.0)
            .font(egui::TextStyle::Monospace)
            .hint_text(&unset),
    );

    // The list carries no value of its own — it only fills the field. Showing the
    // current model here as well would put the same string on screen twice and
    // leave which of the two is authoritative to guesswork.
    egui::ComboBox::from_id_salt(("model-slot", slot.name()))
        .selected_text(tr("settings.models.pick"))
        .width(130.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(value.trim().is_empty(), &unset).clicked() {
                value.clear();
            }
            for suggestion in slot.suggestions() {
                let chosen = value.trim() == suggestion;
                let label = egui::RichText::new(&suggestion).monospace();
                if ui.selectable_label(chosen, label).clicked() {
                    *value = suggestion;
                }
            }
        });
}

/// Taking every override back out, behind a confirmation.
///
/// One click undoes as much as five fields do, and what it undoes is the reason
/// somebody came here — a Claude Code that picks the models itself. Returns
/// whether the file was written.
fn clear_button(
    ui: &mut egui::Ui,
    on_disk: &Overrides,
    confirming: &mut bool,
    message: &mut Option<Result<String, String>>,
) -> bool {
    if !*confirming {
        if ui
            .add_enabled(!on_disk.is_empty(), egui::Button::new(tr("settings.models.clear")))
            .on_hover_text(tr("settings.models.clear_hint"))
            .clicked()
        {
            *confirming = true;
        }
        return false;
    }

    let mut written = false;
    ui.horizontal(|ui| {
        ui.colored_label(crate::ui::level_color(95.0), tr("settings.models.clear_confirm"));
        if ui.button(tr("settings.storage.reset_yes")).clicked() {
            *confirming = false;
            *message = Some(match model_override::clear() {
                Ok(_) => Ok(tr("settings.models.cleared")),
                Err(e) => Err(format!("{e:#}")),
            });
            written = true;
        }
        if ui.button(tr("settings.storage.reset_no")).clicked() {
            *confirming = false;
        }
    });
    written
}

/// Puts the edited state in the Claude Code settings.
fn write(overrides: &Overrides) -> Result<String, String> {
    match model_override::apply(overrides) {
        Ok(path) => {
            Ok(tr_args("settings.models.applied", &[("path", &path.display().to_string())]))
        }
        Err(e) => Err(format!("{e:#}")),
    }
}
