//! The "Settings" tab: the status line template and registering the hook.

use claude_status_core::{
    Db, Language, autostart,
    config::{MIN_PROBE_INTERVAL_SECS, PRESETS},
    i18n,
    install::{self, InstallStatus},
    paths,
    render::{self, PLACEHOLDERS, RenderContext},
    timefmt, tr, tr_args, update,
};
use eframe::egui;

use crate::state::{AppState, UpdateStage};

/// Widget state that survives between frames.
pub struct SettingsState {
    /// The template being edited. Kept apart from the configuration so that
    /// edits apply on a button press rather than on every keystroke.
    template: Option<String>,
    refresh_interval: u64,
    /// Outcome of the last action on the Claude Code settings.
    message: Option<Result<String, String>>,
    /// Whether the destructive reset is awaiting confirmation.
    confirming_reset: bool,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self { template: None, refresh_interval: 60, message: None, confirming_reset: false }
    }
}

pub fn draw(ui: &mut egui::Ui, state: &mut AppState, ui_state: &mut SettingsState) {
    let template = ui_state
        .template
        .get_or_insert_with(|| state.config.statusline.template.clone());

    egui::ScrollArea::vertical().show(ui, |ui| {
        hook_section(ui, state, &mut ui_state.refresh_interval, &mut ui_state.message);
        ui.add_space(12.0);
        statusline_section(ui, state, template);
        ui.add_space(12.0);
        language_section(ui, state);
        ui.add_space(12.0);
        tray_section(ui, state);
        ui.add_space(12.0);
        autostart_section(ui, state, &mut ui_state.message);
        ui.add_space(12.0);
        update_section(ui, state);
        ui.add_space(12.0);
        probe_section(ui, state);
        ui.add_space(12.0);
        storage_section(ui, state, &mut ui_state.confirming_reset, &mut ui_state.message);
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            if ui.button(tr("settings.save")).clicked() {
                state.config.statusline.template = template.clone();
                ui_state.message = Some(match state.config.save() {
                    Ok(()) => Ok(tr("settings.saved")),
                    Err(e) => Err(format!("{e:#}")),
                });
            }
            if let Ok(path) = paths::config_path() {
                ui.label(egui::RichText::new(path.display().to_string()).weak().small());
            }
        });

        if let Some(message) = &ui_state.message {
            ui.add_space(6.0);
            match message {
                Ok(text) => ui.colored_label(crate::ui::level_color(0.0), text),
                Err(text) => ui.colored_label(crate::ui::level_color(100.0), text),
            };
        }
    });
}

/// Registering the hook — without it there is nothing to collect limits with.
fn hook_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    refresh_interval: &mut u64,
    message: &mut Option<Result<String, String>>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.hook.title"));
        ui.add_space(4.0);
        ui.label(tr("settings.hook.explanation"));
        ui.add_space(6.0);

        match &state.install {
            InstallStatus::Ours { command } => {
                ui.colored_label(crate::ui::level_color(0.0), tr("settings.hook.state_ours"));
                ui.label(egui::RichText::new(command).weak().small());
            }
            InstallStatus::Stale { command } => {
                ui.colored_label(crate::ui::level_color(80.0), tr("settings.hook.state_stale"));
                ui.label(egui::RichText::new(command).weak().small());
                ui.label(tr("settings.hook.state_stale_note"));
            }
            InstallStatus::Foreign { command } => {
                ui.colored_label(crate::ui::level_color(80.0), tr("settings.hook.state_foreign"));
                ui.label(egui::RichText::new(command).weak().small());
                ui.label(tr("settings.hook.state_foreign_note"));
            }
            InstallStatus::Absent => {
                ui.colored_label(crate::ui::level_color(95.0), tr("settings.hook.state_absent"));
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(tr("settings.hook.interval"));
            ui.add(
                egui::DragValue::new(refresh_interval)
                    .range(1..=3600)
                    .suffix(tr("unit.seconds_suffix")),
            );
            ui.label(tr("settings.hook.interval_hint"));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let foreign = matches!(state.install, InstallStatus::Foreign { .. });
            let label = if foreign {
                tr("settings.hook.replace")
            } else {
                tr("settings.hook.register")
            };

            if ui.button(label).clicked() {
                *message = Some(match install::install(Some(*refresh_interval), foreign) {
                    Ok(path) => Ok(tr_args(
                        "settings.hook.registered_at",
                        &[("path", &path.display().to_string())],
                    )),
                    Err(e) => Err(format!("{e:#}")),
                });
                state.refresh();
            }

            if ui
                .add_enabled(state.install.is_ours(), egui::Button::new(tr("settings.hook.remove")))
                .clicked()
            {
                *message = Some(match install::uninstall() {
                    Ok(()) => Ok(tr("settings.hook.removed")),
                    Err(e) => Err(format!("{e:#}")),
                });
                state.refresh();
            }
        });

        ui.add_space(4.0);
        ui.label(egui::RichText::new(tr("settings.hook.applied_note")).weak().small());
    });
}

fn statusline_section(ui: &mut egui::Ui, state: &mut AppState, template: &mut String) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.statusline.title"));
        ui.add_space(4.0);

        ui.checkbox(&mut state.config.statusline.enabled, tr("settings.statusline.enabled"));
        ui.checkbox(&mut state.config.statusline.colors, tr("settings.statusline.colors"));
        ui.horizontal(|ui| {
            ui.label(tr("settings.statusline.bar_width"));
            ui.add(egui::DragValue::new(&mut state.config.statusline.bar_width).range(0..=40));
        });

        ui.add_space(6.0);
        presets(ui, template);

        ui.add_space(6.0);
        ui.label(tr("settings.statusline.template"));
        ui.add(
            egui::TextEdit::multiline(template)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );

        ui.add_space(6.0);
        ui.label(tr("settings.statusline.preview"));
        ui.add(
            egui::Label::new(egui::RichText::new(preview(state, template)).monospace())
                .wrap_mode(egui::TextWrapMode::Extend),
        );

        ui.add_space(8.0);
        egui::CollapsingHeader::new(tr("settings.statusline.placeholders")).show(ui, |ui| {
            egui::Grid::new("placeholders").num_columns(2).striped(true).show(ui, |ui| {
                for (name, description_key) in PLACEHOLDERS {
                    // Clicking appends the placeholder to the template — faster
                    // than typing it out.
                    let button = egui::Button::new(egui::RichText::new(*name).monospace())
                        .frame(false);
                    if ui.add(button).clicked() {
                        template.push_str(name);
                    }
                    ui.label(tr(description_key));
                    ui.end_row();
                }
            });
        });
    });
}

/// Picking a ready-made template.
///
/// The label reflects whether the current text still matches a preset —
/// otherwise the list would silently keep showing a stale choice after a manual
/// edit.
fn presets(ui: &mut egui::Ui, template: &mut String) {
    let current = PRESETS
        .iter()
        .find(|(_, preset)| *preset == template.as_str())
        .map_or_else(|| tr("settings.statusline.preset_custom"), |(key, _)| tr(key));

    ui.horizontal(|ui| {
        ui.label(tr("settings.statusline.preset"));
        egui::ComboBox::from_id_salt("statusline-preset")
            .selected_text(current)
            .show_ui(ui, |ui| {
                for (key, preset) in PRESETS {
                    let selected = *preset == template.as_str();
                    if ui.selectable_label(selected, tr(key)).on_hover_text(*preset).clicked() {
                        *template = (*preset).to_string();
                    }
                }
            });
    });
}

/// Renders the line from the edited template, but without ANSI: in the window
/// the escape codes would show up as noise.
fn preview(state: &AppState, template: &str) -> String {
    let mut config = state.config.clone();
    config.statusline.colors = false;

    let now = timefmt::now();
    let ctx = RenderContext {
        input: None,
        overview: &state.overview,
        config: &config,
        now,
    };
    render::render_template(template, &ctx)
}

fn language_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.language.title"));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            for language in Language::ALL {
                let label = match language {
                    Language::Auto => tr("settings.language.auto"),
                    Language::En => "English".to_string(),
                    Language::Ru => "Русский".to_string(),
                };
                // Applied immediately rather than on save: a language you
                // cannot see the effect of is impossible to choose.
                if ui
                    .selectable_value(&mut state.config.ui.language, language, label)
                    .clicked()
                {
                    i18n::apply(state.config.ui.language);
                }
            }
        });
        ui.label(egui::RichText::new(tr("settings.language.restart_note")).weak().small());
    });
}

fn tray_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.tray.title"));
        ui.add_space(4.0);

        ui.label(egui::RichText::new(tr("settings.tray.icon_hint")).weak().small());

        ui.horizontal(|ui| {
            ui.label(tr("settings.tray.refresh"));
            ui.add(
                egui::DragValue::new(&mut state.config.tray.refresh_secs)
                    .range(1..=600)
                    .suffix(tr("unit.seconds_suffix")),
            );
        });
    });
}

/// Starting with the session.
///
/// The tick is not a configuration value: it is read from and written to the
/// operating system directly, so it stays honest if the entry is removed by
/// anything else.
fn autostart_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    message: &mut Option<Result<String, String>>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.autostart.title"));
        ui.add_space(4.0);

        let mut on = state.autostart.is_on();
        if ui.checkbox(&mut on, tr("settings.autostart.enabled")).changed() {
            *message = Some(match autostart::set(on) {
                Ok(()) => Ok(tr(if on { "settings.autostart.on" } else { "settings.autostart.off" })),
                Err(e) => Err(format!("{e:#}")),
            });
            state.refresh();
        }

        ui.label(egui::RichText::new(tr("settings.autostart.hint")).weak().small());

        if let autostart::State::Elsewhere { path } = &state.autostart {
            ui.add_space(4.0);
            ui.colored_label(
                crate::ui::level_color(80.0),
                tr_args("settings.autostart.elsewhere", &[("path", path)]),
            );
            ui.label(egui::RichText::new(tr("settings.autostart.elsewhere_fix")).weak().small());
        }
    });
}

/// Self-update.
///
/// One button that carries the whole flow: it offers to check, then to download
/// what the check found, then to restart into it. Nothing reaches the network
/// before it is pressed.
fn update_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.update.title"));
        ui.add_space(4.0);
        ui.label(tr("settings.update.explanation"));
        ui.add_space(6.0);

        ui.label(
            egui::RichText::new(tr_args(
                "settings.update.current",
                &[("version", &update::Version::current().to_string())],
            ))
            .weak()
            .small(),
        );
        ui.add_space(4.0);

        let stage = state.update_stage();
        let busy = matches!(stage, UpdateStage::Checking | UpdateStage::Downloading);
        let label = match &stage {
            UpdateStage::Checking => tr("settings.update.checking"),
            UpdateStage::Downloading => tr("settings.update.downloading"),
            UpdateStage::Available(found) => {
                tr_args("settings.update.download", &[("version", &found.version.to_string())])
            }
            UpdateStage::Installed(_) => tr("settings.update.restart"),
            _ => tr("settings.update.check"),
        };

        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                match stage {
                    UpdateStage::Available(_) => state.download_update(),
                    // Handled in the paint loop: restarting means really
                    // quitting, and only that loop knows how — the close
                    // button merely hides the window in the tray.
                    UpdateStage::Installed(_) => state.restart_requested = true,
                    _ => state.check_for_update(),
                }
            }
            if busy {
                ui.spinner();
            }
        });

        let note = match state.update_stage() {
            UpdateStage::UpToDate => Some(Ok(tr("update.up_to_date"))),
            UpdateStage::Available(found) => Some(Ok(tr_args(
                "update.available",
                &[("version", &found.version.to_string())],
            ))),
            UpdateStage::Installed(version) => Some(Ok(tr_args(
                "update.installed",
                &[("version", &version.to_string())],
            ))),
            UpdateStage::Failed(text) => Some(Err(text)),
            _ => None,
        };
        if let Some(note) = note {
            ui.add_space(4.0);
            match note {
                Ok(text) => ui.colored_label(crate::ui::level_color(0.0), text),
                Err(text) => ui.colored_label(crate::ui::level_color(100.0), text),
            };
        }
    });
}

/// Asking Claude Code directly — the source that also works where no status
/// line is drawn.
fn probe_section(ui: &mut egui::Ui, state: &mut AppState) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.probe.title"));
        ui.add_space(4.0);
        ui.label(tr("settings.probe.explanation"));
        ui.add_space(6.0);

        ui.checkbox(&mut state.config.probe.enabled, tr("settings.probe.enabled"));

        ui.add_enabled_ui(state.config.probe.enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr("settings.probe.interval"));
                ui.add(
                    egui::DragValue::new(&mut state.config.probe.interval_secs)
                        .range(MIN_PROBE_INTERVAL_SECS..=21_600)
                        .speed(30)
                        .custom_formatter(|v, _| timefmt::duration(v as i64))
                        .suffix(""),
                );
            });
            ui.label(egui::RichText::new(tr("settings.probe.interval_hint")).weak().small());
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let busy = state.probing();
            let label = if busy {
                tr("settings.probe.running")
            } else {
                tr("settings.probe.run_now")
            };
            if ui.add_enabled(!busy, egui::Button::new(label)).clicked() {
                state.start_probe();
            }
            if busy {
                ui.spinner();
            }
        });

        if let Some(message) = &state.probe_message {
            ui.add_space(4.0);
            match message {
                Ok(text) => ui.colored_label(crate::ui::level_color(0.0), text),
                Err(text) => ui.colored_label(crate::ui::level_color(100.0), text),
            };
        }
    });
}

fn storage_section(
    ui: &mut egui::Ui,
    state: &mut AppState,
    confirming_reset: &mut bool,
    message: &mut Option<Result<String, String>>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("settings.storage.title"));
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label(tr("settings.storage.retention"));
            ui.add(
                egui::DragValue::new(&mut state.config.storage.retention_days)
                    .range(0..=3650)
                    .suffix(tr("unit.days_suffix")),
            );
            ui.label(egui::RichText::new(tr("settings.storage.retention_hint")).weak().small());
        });

        if let Ok(path) = paths::db_path() {
            ui.label(egui::RichText::new(path.display().to_string()).weak().small());
        }

        ui.add_space(8.0);
        reset_button(ui, state, confirming_reset, message);
    });
}

/// Wiping the history is irreversible, so it takes a second click to confirm.
fn reset_button(
    ui: &mut egui::Ui,
    state: &mut AppState,
    confirming: &mut bool,
    message: &mut Option<Result<String, String>>,
) {
    if !*confirming {
        if ui
            .button(tr("settings.storage.reset"))
            .on_hover_text(tr("settings.storage.reset_hint"))
            .clicked()
        {
            *confirming = true;
        }
        return;
    }

    ui.horizontal(|ui| {
        ui.colored_label(crate::ui::level_color(95.0), tr("settings.storage.reset_confirm"));
        if ui.button(tr("settings.storage.reset_yes")).clicked() {
            *confirming = false;
            *message = Some(match reset_history() {
                Ok((removed, backup)) => {
                    state.refresh();
                    Ok(tr_args(
                        "settings.storage.reset_done",
                        &[("count", &removed.to_string()), ("backup", &backup)],
                    ))
                }
                Err(e) => Err(format!("{e:#}")),
            });
        }
        if ui.button(tr("settings.storage.reset_no")).clicked() {
            *confirming = false;
        }
    });
}

/// Wipes the history, keeping a copy of the database first.
///
/// Samples cannot be recomputed from anywhere — Claude Code keeps no record of
/// past limit readings — so the backup is what makes a misfired click survivable.
fn reset_history() -> anyhow::Result<(usize, String)> {
    let db = Db::open_default()?;
    let backup = db.backup()?;
    let removed = db.clear_samples()?;
    Ok((removed, backup.display().to_string()))
}
