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
            // The status line never says which model the cap belongs to; the
            // probe does, so the title carries the name once it is known.
            let title = match &state.scoped_model {
                Some(model) => tr_args("overview.card.week_scoped", &[("model", model)]),
                None => tr("overview.card.week_opus"),
            };
            window_card(ui, &title, &w, false);
            ui.add_space(8.0);
        }

        // The advice is about the current week; once it has rolled over there
        // is nothing to divide between the remaining days.
        if let Some(w) = state.overview.week.filter(|w| !w.is_expired()) {
            budget_card(ui, state, &w);
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

        usage_bar(ui, used_pct);

        // The even-pace line only makes sense for the weekly window: over five
        // hours a burst of work is normal rather than overspending.
        if short_window {
            return;
        }

        ui.add_space(4.0);
        let deviation = w.deviation_pct();
        // Percentage points are a poor unit for a person: the same gap said in
        // time ("a day ahead") is something one can act on.
        let gap = timefmt::duration(w.deviation_secs().abs());
        let (verdict, color) = if deviation > 10.0 {
            (tr_args("overview.verdict.ahead", &[("time", &gap)]), level_color(90.0))
        } else if deviation < -10.0 {
            (tr_args("overview.verdict.behind", &[("time", &gap)]), level_color(0.0))
        } else {
            (tr("overview.verdict.on_track"), level_color(0.0))
        };
        ui.horizontal(|ui| {
            ui.label(tr_args("overview.even_pace", &[("pct", &format!("{:.1}", w.expected_pct()))]));
            ui.colored_label(color, verdict);
        })
        .response
        .on_hover_text(tr_args(
            "overview.even_pace_hint",
            &[
                ("expected", &format!("{:.1}", w.expected_pct())),
                ("used", &format!("{used_pct:.1}")),
            ],
        ));
    });
}

/// The usage bar.
///
/// The label is placed by hand rather than handed to the widget, which would
/// always pin it to the left edge. Every fill colour here is a light one, so
/// text that lands on the fill is dark and text on the bare track follows the
/// theme. The awkward case is a short fill: egui never draws it narrower than
/// the bar is tall, so even a reading of nought leaves a coloured cap under a
/// left-pinned label. Below the width the label needs, it moves past the edge
/// of the fill instead of straddling it.
fn usage_bar(ui: &mut egui::Ui, used_pct: f64) {
    let fraction = (used_pct / 100.0).clamp(0.0, 1.0) as f32;
    let padding = ui.spacing().item_spacing.x;
    let galley = ui.painter().layout_no_wrap(
        format!("{used_pct:.1}%"),
        egui::TextStyle::Button.resolve(ui.style()),
        egui::Color32::PLACEHOLDER,
    );

    let rect = ui.add(egui::ProgressBar::new(fraction).fill(level_color(used_pct))).rect;
    let filled = (rect.width() * fraction).max(rect.height());

    let (x, color) = if filled >= padding + galley.size().x + padding {
        (rect.left() + padding, egui::Color32::from_gray(24))
    } else {
        (rect.left() + filled + padding, ui.visuals().text_color())
    };
    let pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
    ui.painter().with_clip_rect(rect).galley(pos, galley, color);
}

/// The "how much may I spend" card — the reason the pace is computed at all.
fn budget_card(ui: &mut egui::Ui, state: &AppState, w: &WindowState) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.strong(tr("overview.budget.title"));
        ui.add_space(4.0);

        egui::Grid::new("budget").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label(tr("overview.budget.remaining"));
            ui.label(format!("{:.1}%", w.remaining_pct()));
            ui.end_row();

            ui.label(tr("overview.budget.per_day")).on_hover_text(tr("overview.budget.per_day_hint"));
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

            ui.label(tr("overview.budget.today")).on_hover_text(tr("overview.budget.today_hint"));
            match state.overview.daily {
                Some(d) => {
                    let text = tr_args(
                        "overview.budget.today_value",
                        &[
                            ("spent", &format!("{:.1}", d.spent_pct)),
                            ("allowance", &format!("{:.1}", d.allowance_pct)),
                            ("left", &format!("{:.1}", d.remaining_pct())),
                        ],
                    );
                    let label = ui.colored_label(level_color(d.used_pct()), text);
                    if d.estimated {
                        label.on_hover_text(tr("overview.budget.today_estimated"));
                    }
                }
                None => {
                    ui.label("—");
                }
            };
            ui.end_row();

            ui.label(tr("overview.budget.pace")).on_hover_text(tr_args(
                "overview.budget.pace_hint",
                &[(
                    "span",
                    &state
                        .overview
                        .week_burn
                        .map(|b| timefmt::duration(b.span_secs))
                        .unwrap_or_else(|| "—".to_owned()),
                )],
            ));
            match state.overview.week_burn {
                Some(burn) => {
                    let burn = burn.pct_per_day;
                    let projected = w.projected_used_at_reset(burn);
                    // Above 100 the projection stops being a figure worth
                    // printing: the limit simply ends earlier, and the next row
                    // says when.
                    let text = if projected > 100.0 {
                        tr_args("overview.budget.pace_over", &[("burn", &format!("{burn:.1}"))])
                    } else {
                        tr_args(
                            "overview.budget.pace_value",
                            &[
                                ("burn", &format!("{burn:.1}")),
                                ("projected", &format!("{projected:.0}")),
                            ],
                        )
                    };
                    ui.colored_label(level_color(projected), text)
                }
                None => ui.label(tr("overview.budget.pace_unknown")),
            };
            ui.end_row();

            if let Some(burn) = state.overview.week_burn
                && let Some(at) = w.exhausted_at(burn.pct_per_day)
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

        // The numbers above answer "how much"; this line answers "so what do I
        // do" — the question the card exists for.
        if let Some((advice, color)) = advice(state, w) {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(2.0);
            match color {
                Some(color) => ui.colored_label(color, advice),
                None => ui.label(advice),
            };
        }
    });
}

/// The plain-language conclusion drawn from the numbers of the budget card.
///
/// Everything but the warning speaks in terms of today's ration. Mixing it with
/// the average-per-day figure produces a sentence that does not add up: the two
/// are the same formula applied at different moments, over different spans.
fn advice(state: &AppState, w: &WindowState) -> Option<(String, Option<egui::Color32>)> {
    // The one case where the rate for the rest of the week is the actionable
    // number: it is the ceiling that has to be respected from here on.
    if let Some(burn) = state.overview.week_burn
        && let Some(at) = w.exhausted_at(burn.pct_per_day)
        && let Some(per_day) = w.allowance_per_day_pct()
    {
        let text = tr_args(
            "overview.budget.advice.slow_down",
            &[("time", &timefmt::datetime(at)), ("pct", &format!("{per_day:.1}"))],
        );
        return Some((text, Some(level_color(100.0))));
    }

    let daily = state.overview.daily?;
    let args = [
        ("allowance", format!("{:.1}", daily.allowance_pct)),
        ("left", format!("{:.1}", daily.remaining_pct())),
    ];
    let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();

    if daily.used_fraction() >= 1.0 {
        let text = tr_args("overview.budget.advice.today_done", &args);
        return Some((text, Some(level_color(80.0))));
    }

    if w.deviation_pct() > 10.0 {
        return Some((tr_args("overview.budget.advice.ahead", &args), Some(level_color(80.0))));
    }

    Some((tr_args("overview.budget.advice.on_track", &args), None))
}

fn freshness(ui: &mut egui::Ui, state: &AppState, now: i64) {
    let Some(staleness) = state.overview.staleness_secs(now) else { return };
    let age = timefmt::duration(staleness);

    // Samples only arrive while Claude Code is running, so "old data" is normal
    // rather than a fault. Mention it, but without alarm.
    let key = if staleness > 3600 { "overview.freshness_stale" } else { "overview.freshness" };
    ui.label(egui::RichText::new(tr_args(key, &[("age", &age)])).weak());

    // A reading can arrive seconds ago and still describe a window that closed
    // hours back: Claude Code refreshes the limits only when it gets a reply,
    // and an idle session keeps resending the last ones it saw. Without this
    // note the screen contradicts itself — "updated a minute ago" next to "no
    // data since the reset".
    let repeating = [state.overview.five_hour, state.overview.week]
        .iter()
        .flatten()
        .any(|w| w.is_expired());
    if repeating {
        ui.label(
            egui::RichText::new(tr("overview.freshness_repeating"))
                .weak()
                .color(level_color(80.0)),
        );
    }
}
