//! The "Overview" tab: current window state and the daily spending advice.

use claude_status_core::{pace::WindowState, timefmt, tr, tr_args};
use eframe::egui;

use crate::state::AppState;
use crate::ui::{Tab, level_color};

/// Returns the tab the user asked to switch to.
pub fn draw(ui: &mut egui::Ui, state: &AppState) -> Option<Tab> {
    let now = timefmt::now();

    if state.overview.sampled_at.is_none() {
        return no_data(ui, state);
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        if let Some(w) = state.overview.five_hour {
            window_card(ui, &tr("overview.card.five_hour"), &w, true);
            ui.add_space(8.0);
        }
        if let Some(w) = state.overview.week {
            window_card(ui, &tr("overview.card.week"), &w, false);
            ui.add_space(8.0);
        }
        if let Some(w) = state.overview.week_opus {
            window_card(ui, &tr("overview.card.week_opus"), &w, false);
            ui.add_space(8.0);
        }

        // The advice is about the current week; once it has rolled over there
        // is nothing to divide between the remaining days.
        if let Some(w) = state.overview.week.filter(|w| !w.is_expired()) {
            budget_card(ui, state, &w, now);
        }

        ui.add_space(8.0);
        freshness(ui, state, now);
    });

    None
}

fn no_data(ui: &mut egui::Ui, state: &AppState) -> Option<Tab> {
    let mut goto = None;

    ui.vertical_centered(|ui| {
        ui.add_space(40.0);
        ui.heading(tr("overview.empty.title"));
        ui.add_space(8.0);
        ui.label(tr("overview.empty.explanation"));
        ui.add_space(12.0);

        if state.install.is_ours() {
            ui.label(tr("overview.empty.installed"));
            ui.add_space(8.0);
            if ui.button(tr("overview.empty.open_settings")).clicked() {
                goto = Some(Tab::Settings);
            }
        } else {
            ui.label(tr("overview.empty.not_installed"));
            ui.add_space(8.0);
            if ui
                .button(tr("overview.empty.register"))
                .on_hover_text(tr("overview.empty.register_hint"))
                .clicked()
            {
                goto = Some(Tab::Settings);
            }
        }
    });

    goto
}

/// A card for one limit window.
fn window_card(ui: &mut egui::Ui, title: &str, w: &WindowState, short_window: bool) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        ui.horizontal(|ui| {
            ui.strong(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if w.is_expired() {
                    ui.label(egui::RichText::new(tr("overview.expired")).weak());
                } else {
                    ui.label(tr_args(
                        "overview.resets",
                        &[
                            ("time", &timefmt::datetime(w.resets_at)),
                            ("left", &timefmt::duration(w.remaining_secs())),
                        ],
                    ));
                }
            });
        });

        ui.add_space(4.0);

        // Past the reset the recorded percentage describes a window that is
        // gone; the new one has not been reported yet, so there is nothing to
        // fill the bar with.
        let Some(used_pct) = w.live_used_pct() else {
            ui.add(
                egui::ProgressBar::new(0.0)
                    .fill(ui.visuals().widgets.inactive.bg_fill)
                    .text(tr("overview.expired_bar")),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr_args(
                    "overview.last_known",
                    &[
                        ("pct", &format!("{:.1}", w.used_pct)),
                        ("time", &timefmt::datetime(w.resets_at)),
                    ],
                ))
                .weak(),
            );
            return;
        };

        ui.add(
            egui::ProgressBar::new((used_pct / 100.0).clamp(0.0, 1.0) as f32)
                .fill(level_color(used_pct))
                .text(format!("{used_pct:.1}%")),
        );

        // The even-pace line only makes sense for the weekly window: over five
        // hours a burst of work is normal rather than overspending.
        if short_window {
            return;
        }

        ui.add_space(4.0);
        let deviation = w.deviation_pct();
        let (verdict, color) = if deviation > 10.0 {
            (tr("overview.verdict.ahead"), level_color(90.0))
        } else if deviation < -10.0 {
            (tr("overview.verdict.behind"), level_color(0.0))
        } else {
            (tr("overview.verdict.on_track"), level_color(0.0))
        };
        ui.horizontal(|ui| {
            ui.label(tr_args("overview.even_pace", &[("pct", &format!("{:.1}", w.expected_pct()))]));
            ui.colored_label(
                color,
                tr_args(
                    "overview.deviation",
                    &[("value", &format!("{deviation:+.1}")), ("verdict", &verdict)],
                ),
            );
        });
    });
}

/// The "how much may I spend" card — the reason the pace is computed at all.
fn budget_card(ui: &mut egui::Ui, state: &AppState, w: &WindowState, now: i64) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("overview.budget.title"));
        ui.add_space(4.0);

        egui::Grid::new("budget").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label(tr("overview.budget.remaining"));
            ui.label(format!("{:.1}%", w.remaining_pct()));
            ui.end_row();

            ui.label(tr("overview.budget.per_day"));
            match w.allowance_per_day_pct() {
                Some(per_day) => ui.label(tr_args(
                    "overview.budget.per_day_value",
                    &[
                        ("pct", &format!("{per_day:.1}")),
                        ("left", &timefmt::duration(w.remaining_secs())),
                    ],
                )),
                None => ui.label("—"),
            };
            ui.end_row();

            ui.label(tr("overview.budget.today"));
            match w.allowance_until(timefmt::end_of_local_day(now)) {
                Some(left) => ui.label(tr_args(
                    "overview.budget.today_value",
                    &[("pct", &format!("{left:.1}"))],
                )),
                None => ui.label("—"),
            };
            ui.end_row();

            ui.label(tr("overview.budget.pace"));
            match state.overview.week_burn_pct_per_day {
                Some(burn) => {
                    let projected = w.projected_used_at_reset(burn);
                    ui.colored_label(
                        level_color(projected),
                        tr_args(
                            "overview.budget.pace_value",
                            &[
                                ("burn", &format!("{burn:.1}")),
                                ("projected", &format!("{projected:.0}")),
                            ],
                        ),
                    )
                }
                None => ui.label(tr("overview.budget.pace_unknown")),
            };
            ui.end_row();

            if let Some(burn) = state.overview.week_burn_pct_per_day
                && let Some(at) = w.exhausted_at(burn)
            {
                ui.label(tr("overview.budget.exhausted"));
                ui.colored_label(
                    level_color(100.0),
                    tr_args(
                        "overview.budget.exhausted_value",
                        &[
                            ("time", &timefmt::datetime(at)),
                            ("left", &timefmt::duration(w.resets_at - at)),
                        ],
                    ),
                );
                ui.end_row();
            }
        });
    });
}

fn freshness(ui: &mut egui::Ui, state: &AppState, now: i64) {
    let Some(staleness) = state.overview.staleness_secs(now) else { return };
    let age = timefmt::duration(staleness);

    // Samples only arrive while Claude Code is running, so "old data" is normal
    // rather than a fault. Mention it, but without alarm.
    let key = if staleness > 3600 { "overview.freshness_stale" } else { "overview.freshness" };
    ui.label(egui::RichText::new(tr_args(key, &[("age", &age)])).weak());
}
