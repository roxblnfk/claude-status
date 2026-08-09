//! The statistics window.

mod history;
mod models;
mod overview;
mod settings;

use claude_status_core::{timefmt, tr, tr_args};
use eframe::egui;

use crate::state::AppState;

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
#[derive(Default)]
pub struct UiState {
    pub tab: Option<Tab>,
    pub settings: settings::SettingsState,
}

impl UiState {
    fn tab(&mut self) -> Tab {
        *self.tab.get_or_insert(Tab::Overview)
    }
}

/// Draws the whole window. Returns `true` when the state should be re-read.
pub fn draw(ui: &mut egui::Ui, state: &mut AppState, ui_state: &mut UiState) -> bool {
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
                if ui.button("⟳").on_hover_text(tr("ui.refresh_hint")).clicked() {
                    refresh_requested = true;
                }
                if state.refreshed_at > 0 {
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
    egui::CentralPanel::default().show(ui, |ui| match active {
        Tab::Overview => goto = overview::draw(ui, state),
        Tab::History => history::draw(ui, state),
        Tab::Models => models::draw(ui, state),
        Tab::Settings => settings::draw(ui, state, &mut ui_state.settings),
    });

    ui_state.tab = Some(goto.unwrap_or(active));
    refresh_requested
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
