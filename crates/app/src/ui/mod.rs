//! The statistics window.

pub mod compact;
pub mod history;
mod model_override;
mod models;
mod overview;
pub mod rings;
mod settings;

use claude_status_core::{Config, timefmt, tr, tr_args};
use eframe::egui;

use crate::state::{AppState, Period, Range};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    History,
    Models,
    Settings,
}

impl Tab {
    pub fn label(self) -> String {
        match self {
            Tab::Overview => tr("ui.tab.overview"),
            Tab::History => tr("ui.tab.history"),
            Tab::Models => tr("ui.tab.models"),
            Tab::Settings => tr("ui.tab.settings"),
        }
    }

    pub const ALL: [Tab; 4] = [Tab::Overview, Tab::History, Tab::Models, Tab::Settings];
}

/// Widget state that survives between frames.
pub struct UiState {
    pub tab: Option<Tab>,
    /// The plaque, and the window geometry it will hand back.
    pub compact: compact::Compact,
    pub settings: settings::SettingsState,
    /// Which breakdown the models tab is showing.
    pub breakdown: models::Breakdown,
}

impl UiState {
    pub fn new(config: &Config) -> Self {
        Self {
            tab: None,
            compact: compact::Compact::new(&config.compact),
            settings: settings::SettingsState::default(),
            breakdown: models::Breakdown::default(),
        }
    }

    fn tab(&mut self) -> Tab {
        *self.tab.get_or_insert(Tab::Overview)
    }
}

/// Draws the whole window. Returns `true` when the state should be re-read.
pub fn draw(ui: &mut egui::Ui, state: &mut AppState, ui_state: &mut UiState) -> bool {
    if ui_state.compact.active {
        let action = compact::draw(ui, state, &mut ui_state.compact);
        if action.leave {
            ui_state.compact.leave(ui.ctx());
        }
        return action.refresh;
    }

    let mut refresh_requested = false;
    let mut active = ui_state.tab();

    egui::Panel::top("tabs").show(ui, |ui| {
        ui.horizontal(|ui| {
            for tab in Tab::ALL {
                if ui.selectable_label(active == tab, tab.label()).clicked() {
                    active = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The window is decorated by the system, so the row of
                // minimise/maximise/close is out of reach; this is the nearest
                // thing to it that belongs to us.
                if ui.button("◎").on_hover_text(tr("compact.enter")).clicked() {
                    ui_state.compact.enter(ui.ctx());
                }
                let hint = if state.config.probe.enabled {
                    tr("ui.refresh_hint")
                } else {
                    tr("ui.refresh_hint_plain")
                };
                if ui.button("⟳").on_hover_text(hint).clicked() {
                    state.request_probe();
                    refresh_requested = true;
                }
                if state.probing() {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(egui::RichText::new(tr("ui.probing")).weak().small());
                } else if state.refreshed_at > 0 {
                    let text =
                        tr_args("ui.refreshed_at", &[("time", &timefmt::clock(state.refreshed_at))]);
                    ui.label(egui::RichText::new(text).weak().small());
                }
            });
        });
    });
    ui_state.tab = Some(active);

    if let Some(error) = &state.error {
        let text = tr_args("ui.error", &[("error", error)]);
        egui::Panel::bottom("error").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(229, 57, 53), text);
        });
    }

    // A tab may ask to switch to another one — for example the "register the
    // hook" button on an empty overview.
    let mut goto = None;
    let mut open_compact = false;
    egui::CentralPanel::default().show(ui, |ui| match active {
        Tab::Overview => goto = overview::draw(ui, state),
        Tab::History => history::draw(ui, state),
        Tab::Models => models::draw(ui, state, &mut ui_state.breakdown),
        Tab::Settings => open_compact = settings::draw(ui, state, &mut ui_state.settings),
    });

    if open_compact {
        ui_state.compact.enter(ui.ctx());
    }
    ui_state.tab = Some(goto.unwrap_or(active));
    refresh_requested
}

/// Picks the window a tab looks at: the lengths, then a way through them.
///
/// Returns `true` when the choice changed and the data has to be re-read. The
/// arrows are what make a length useful for anything but the present — a month
/// that only ever ends today cannot answer what last month cost.
pub fn period_picker(ui: &mut egui::Ui, period: &mut Period, now: i64) -> bool {
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.label(tr("history.period"));
        for range in Range::ALL {
            if ui.selectable_label(period.range == range, range.label()).clicked()
                && period.range != range
            {
                // The step is measured in windows, so keeping it across a
                // change of length would land somewhere arbitrary — six days
                // back becomes six months back.
                *period = Period::new(range);
                changed = true;
            }
        }

        if !period.steppable() {
            return;
        }

        ui.add_space(8.0);
        if ui.small_button("◀").on_hover_text(tr("history.period_back")).clicked() {
            period.back += 1;
            changed = true;
        }
        let forward = ui
            .add_enabled(!period.at_present(), egui::Button::new("▶").small())
            .on_hover_text(tr("history.period_forward"));
        if forward.clicked() {
            period.back -= 1;
            changed = true;
        }

        ui.label(egui::RichText::new(period.label(now)).weak());
        if !period.at_present() && ui.small_button(tr("history.period_now")).clicked() {
            period.back = 0;
            changed = true;
        }
    });

    changed
}

/// Colour by window fill — the same language the tray icon speaks.
pub fn level_color(pct: f64) -> egui::Color32 {
    if pct >= 90.0 {
        egui::Color32::from_rgb(229, 57, 53)
    } else if pct >= 75.0 {
        egui::Color32::from_rgb(251, 140, 0)
    } else if pct >= 50.0 {
        egui::Color32::from_rgb(253, 216, 53)
    } else {
        egui::Color32::from_rgb(67, 176, 71)
    }
}

/// The same for the model-scoped cap. The plaque gives it a palette of its own
/// so it is not read as part of the budget the other three rings share, and the
/// bar on the overview has to speak the same colours.
pub fn scoped_color(pct: f64) -> egui::Color32 {
    rings::color_at(rings::Palette::Scoped, (pct / 100.0) as f32)
}

/// Compact notation for large token counts: `1.2M`, `340k`.
pub fn human_tokens(tokens: i64) -> String {
    let abs = tokens.unsigned_abs();
    match abs {
        0..=9_999 => tokens.to_string(),
        10_000..=999_999 => format!("{:.0}k", tokens as f64 / 1_000.0),
        1_000_000..=999_999_999 => format!("{:.1}M", tokens as f64 / 1_000_000.0),
        _ => format!("{:.2}B", tokens as f64 / 1_000_000_000.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_status_core::{Language, i18n};

    /// `bar_label` writes the reading onto the fill in dark ink, so every colour
    /// a bar may take has to stay light enough to read it against.
    #[test]
    fn the_scoped_bar_stays_light_and_apart_from_the_shared_budget() {
        for pct in [0.0, 25.0, 50.0, 60.0, 75.0, 100.0] {
            let c = scoped_color(pct);
            let luma = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
            assert!(luma > 130.0, "{pct}% is too dark to write on: {c:?}, luma {luma:.0}");
            assert_ne!(c, level_color(pct), "the scoped cap must not read as the week");
        }
    }

    #[test]
    fn human_tokens_switches_units() {
        assert_eq!(human_tokens(0), "0");
        assert_eq!(human_tokens(9_999), "9999");
        assert_eq!(human_tokens(10_000), "10k");
        assert_eq!(human_tokens(1_500_000), "1.5M");
        assert_eq!(human_tokens(2_400_000_000), "2.40B");
    }

    #[test]
    fn tab_labels_are_translated_and_unique() {
        for language in [Language::En, Language::Ru] {
            i18n::apply(language);

            let labels: Vec<_> = Tab::ALL.iter().map(|t| t.label()).collect();
            for (tab, label) in Tab::ALL.iter().zip(&labels) {
                assert!(!label.starts_with("ui.tab."), "{tab:?} is untranslated in {language:?}");
            }

            let mut sorted = labels.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(sorted.len(), labels.len(), "duplicate labels in {language:?}");
        }
    }
}
