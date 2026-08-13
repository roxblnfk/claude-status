//! The "Models" tab: where the tokens went, by model and by project.
//!
//! `rate_limits` carry a single window percentage with no split at all, and the
//! aggregates Claude Code keeps in `stats-cache.json` stopped being recomputed.
//! The figures here are counted from the session logs instead — see
//! [`claude_status_core::scan`] — which is also what makes the per-project
//! breakdown possible: Claude Code aggregates nothing of the sort.

use claude_status_core::{Totals, timefmt, tr, tr_args};
use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::state::AppState;
use crate::ui::human_tokens;

/// Row height. Fixed so the table can lay itself out without measuring every
/// row, which is what lets the columns stretch.
const ROW_HEIGHT: f32 = 22.0;

/// Width of a numeric column. Enough for `994.9M` and the widest header.
const NUMERIC_COLUMN: f32 = 78.0;
/// Width of the share bar column. Above egui's 96 px floor for a progress bar:
/// below it the widget still asks for 96 and the table clips the overflow,
/// which is what square-ends off the right side of the bar.
const SHARE_COLUMN: f32 = 120.0;
/// Lower bound for the name before the table starts scrolling sideways.
const MIN_NAME_COLUMN: f32 = 140.0;

/// Which breakdown the table shows. Kept between frames by the caller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Breakdown {
    #[default]
    Models,
    Projects,
}

pub fn draw(ui: &mut egui::Ui, state: &mut AppState, breakdown: &mut Breakdown) {
    let now = timefmt::now();
    let mut period = state.models_period;
    if crate::ui::period_picker(ui, &mut period, now) {
        state.models_period = period;
        state.refresh();
    }
    ui.add_space(6.0);

    summary(ui, state);
    ui.add_space(12.0);

    ui.horizontal(|ui| {
        ui.strong(tr("models.table.title"));
        ui.add_space(8.0);
        for (option, key) in [
            (Breakdown::Models, "models.table.by_model"),
            (Breakdown::Projects, "models.table.by_project"),
        ] {
            if ui.selectable_label(*breakdown == option, tr(key)).clicked() {
                *breakdown = option;
            }
        }

        // The filter belongs to the models table alone; the list of projects is
        // where one is picked, so showing it there would offer to narrow the
        // very list it was picked from.
        if *breakdown == Breakdown::Models
            && let Some(project) = state.project.clone()
        {
            ui.add_space(8.0);
            ui.label(tr_args("models.table.in_project", &[("project", &project)]));
            if ui.small_button(tr("models.table.all_projects")).clicked() {
                state.select_project(None);
            }
        }
    });
    ui.add_space(4.0);

    let rows = match *breakdown {
        Breakdown::Models => &state.models,
        Breakdown::Projects => &state.projects,
    };
    if rows.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label(tr("models.empty"));
        });
        return;
    }

    // The table scrolls on its own, so it must not sit inside another scroll
    // area — nested scrolling would fight over the wheel.
    let picked = totals_table(ui, rows, *breakdown == Breakdown::Projects);
    if let Some(project) = picked {
        state.select_project(Some(project));
        *breakdown = Breakdown::Models;
    }
}

/// What the counted history covers, and the button that recounts it.
fn summary(ui: &mut egui::Ui, state: &mut AppState) {
    let scanning = state.scanning();
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("totals").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            // Claude Code's own counters reach back past the oldest log still
            // on disk, so they are worth showing even though nothing else here
            // comes from them any more.
            if let Some(stats) = &state.stats {
                ui.label(tr("models.totals.sessions"));
                ui.label(stats.total_sessions.to_string());
                ui.end_row();

                if let Some(date) = &stats.first_session_date {
                    ui.label(tr("models.totals.first_session"));
                    ui.label(date.split('T').next().unwrap_or(date));
                    ui.end_row();
                }
            }

            ui.label(tr("models.totals.counted_since"))
                .on_hover_text(tr("models.totals.counted_since_hint"));
            ui.label(state.counted_since.clone().unwrap_or_else(|| tr("models.totals.never")));
            ui.end_row();

            // The share is about the window on screen rather than all time:
            // "a third of this month went to agents" is actionable in a way
            // that a lifetime average is not.
            ui.label(tr("models.totals.agents")).on_hover_text(tr("models.totals.agents_hint"));
            ui.label(tr_args(
                "models.totals.agents_value",
                &[
                    ("pct", &format!("{:.0}", state.usage_totals.agent_share())),
                    ("tokens", &crate::ui::human_tokens(state.usage_totals.agent_tokens)),
                    ("total", &crate::ui::human_tokens(state.usage_totals.total())),
                ],
            ));
            ui.end_row();

            ui.label(tr("models.totals.last_scan"));
            ui.horizontal(|ui| {
                let when = if state.last_scan_ts > 0 {
                    timefmt::datetime(state.last_scan_ts)
                } else {
                    tr("models.totals.never")
                };
                ui.label(when);

                let label =
                    if scanning { tr("models.scan.running") } else { tr("models.scan.button") };
                if ui.add_enabled(!scanning, egui::Button::new(label)).clicked() {
                    state.start_scan();
                }
            });
            ui.end_row();
        });
    });

    if let Some(outcome) = &state.scan_message {
        ui.add_space(4.0);
        match outcome {
            Ok(text) => ui.label(egui::RichText::new(text).weak()),
            Err(text) => ui.colored_label(egui::Color32::from_rgb(229, 87, 100), text),
        };
    }
}

/// Draws the table, returning the row clicked when the names lead somewhere.
fn totals_table(ui: &mut egui::Ui, rows: &[Totals], names_open_a_project: bool) -> Option<String> {
    let mut picked = None;
    let max_total = rows.first().map_or(1, |row| row.total().max(1));

    // The name column is sized by hand rather than with `Column::remainder`:
    // remainder measures the full width and knows nothing about the vertical
    // scrollbar, so the last column ends up clipped underneath it.
    let name_width = (ui.available_width() - reserved_width(ui)).max(MIN_NAME_COLUMN);

    TableBuilder::new(ui)
        .striped(true)
        // The name gets everything the other columns leave over, so the table
        // spans the full window width and long identifiers stay legible.
        .column(Column::exact(name_width).clip(true))
        .column(Column::exact(SHARE_COLUMN))
        // Fixed rather than `auto`: the cells align their contents to the right
        // through a stretching layout, so auto-sizing would measure the column
        // itself instead of the number and never shrink back.
        .columns(Column::exact(NUMERIC_COLUMN), 4)
        .auto_shrink([false, false])
        .header(ROW_HEIGHT, |mut header| {
            header.col(|ui| {
                centered(ui, |ui| {
                    ui.strong(tr("models.table.name"));
                });
            });
            header.col(|ui| {
                centered(ui, |ui| {
                    ui.strong(tr("models.table.share"));
                });
            });
            for key in [
                "models.table.total",
                "models.table.agents",
                "models.table.output",
                "models.table.cache",
            ] {
                header.col(|ui| {
                    right_aligned(ui, |ui| {
                        ui.strong(tr(key));
                    });
                });
            }
        })
        .body(|body| {
            body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                let entry = &rows[row.index()];

                row.col(|ui| {
                    centered(ui, |ui| {
                        let messages = tr_args(
                            "models.table.messages",
                            &[("count", &entry.messages.to_string())],
                        );
                        // A project name is a way into that project's models;
                        // a model name leads nowhere and must not look as if
                        // it did.
                        if names_open_a_project {
                            let hint = format!("{}\n{messages}", tr("models.table.open_project"));
                            if ui.link(&entry.name).on_hover_text(hint).clicked() {
                                picked = Some(entry.name.clone());
                            }
                        } else {
                            ui.label(&entry.name).on_hover_text(messages);
                        }
                    });
                });
                // The bar gives scale: the busiest entry usually has orders of
                // magnitude more tokens than the rest.
                row.col(|ui| {
                    centered(ui, |ui| {
                        // Stated rather than left to the widget: unasked, it
                        // demands 96 px whatever the cell can spare.
                        let width = ui.available_width();
                        ui.add(
                            egui::ProgressBar::new(entry.total() as f32 / max_total as f32)
                                .desired_width(width)
                                .desired_height(8.0),
                        );
                    });
                });
                // Input is left out on purpose: beside cache traffic it is a
                // rounding error, and the column is better spent on where the
                // spending actually went.
                row.col(|ui| {
                    right_aligned(ui, |ui| {
                        ui.label(human_tokens(entry.total()));
                    });
                });
                row.col(|ui| {
                    right_aligned(ui, |ui| {
                        let share = entry.agent_share();
                        let text = if share > 0.0 { format!("{share:.0}%") } else { "—".into() };
                        ui.label(text).on_hover_text(tr_args(
                            "models.table.agents_hint",
                            &[("tokens", &human_tokens(entry.agent_tokens))],
                        ));
                    });
                });
                for value in [entry.output, entry.cache()] {
                    row.col(|ui| {
                        right_aligned(ui, |ui| {
                            ui.label(human_tokens(value));
                        });
                    });
                }
            });
        });

    picked
}

/// Everything the name column does not get: the other columns, the gaps between
/// all six, and the vertical scrollbar.
fn reserved_width(ui: &egui::Ui) -> f32 {
    const COLUMNS: usize = 6;
    let spacing = ui.spacing().item_spacing.x * (COLUMNS - 1) as f32;
    SHARE_COLUMN + 4.0 * NUMERIC_COLUMN + spacing + ui.spacing().scroll.bar_width
}

/// Puts cell contents on the row's centre line.
///
/// A table cell lays its contents out from the top, which a 22 px row makes
/// plain: text sits high and a thin bar clings to the ceiling.
fn centered(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), contents);
}

/// Numbers read as a column only when they line up on the right.
fn right_aligned(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // In a right-to-left layout this space lands on the right, keeping the
        // last column off the window edge.
        ui.add_space(6.0);
        contents(ui);
    });
}
