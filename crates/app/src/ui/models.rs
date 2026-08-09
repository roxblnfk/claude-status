//! The "Models" tab: token breakdown per model from `~/.claude/stats-cache.json`.
//!
//! `rate_limits` carry a single window percentage with no per-model split.
//! These figures are computed by Claude Code itself; we only display them.

use claude_status_core::{paths, stats_cache::StatsCache, tr};
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
/// Lower bound for the model name before the table starts scrolling sideways.
const MIN_NAME_COLUMN: f32 = 140.0;

pub fn draw(ui: &mut egui::Ui, state: &AppState) {
    let Some(stats) = &state.stats else {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label(tr("models.missing_cache"));
            if let Ok(path) = paths::claude_stats_cache() {
                ui.label(egui::RichText::new(path.display().to_string()).weak().small());
            }
        });
        return;
    };

    totals(ui, stats);
    ui.add_space(12.0);
    ui.strong(tr("models.table.title"));
    ui.add_space(4.0);

    // The table scrolls on its own, so it must not sit inside another scroll
    // area — nested scrolling would fight over the wheel.
    models_table(ui, stats);
}

fn totals(ui: &mut egui::Ui, stats: &StatsCache) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("totals").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            ui.label(tr("models.totals.sessions"));
            ui.label(stats.total_sessions.to_string());
            ui.end_row();

            ui.label(tr("models.totals.messages"));
            ui.label(stats.total_messages.to_string());
            ui.end_row();

            if let Some(date) = &stats.first_session_date {
                ui.label(tr("models.totals.first_session"));
                ui.label(date.split('T').next().unwrap_or(date));
                ui.end_row();
            }
            if let Some(date) = &stats.last_computed_date {
                ui.label(tr("models.totals.computed"));
                ui.label(date);
                ui.end_row();
            }
        });
    });
}

fn models_table(ui: &mut egui::Ui, stats: &StatsCache) {
    let ranked = stats.models_by_usage();
    let max_total = ranked.first().map_or(1, |(_, usage)| usage.total().max(1));

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
                    ui.strong(tr("models.table.model"));
                });
            });
            header.col(|ui| {
                centered(ui, |ui| {
                    ui.strong(tr("models.table.share"));
                });
            });
            for key in [
                "models.table.total",
                "models.table.input",
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
            body.rows(ROW_HEIGHT, ranked.len(), |mut row| {
                let (name, usage) = ranked[row.index()];

                row.col(|ui| {
                    centered(ui, |ui| {
                        ui.label(name);
                    });
                });
                // The bar gives scale: the busiest model usually has orders of
                // magnitude more tokens than the rest.
                row.col(|ui| {
                    centered(ui, |ui| {
                        // Stated rather than left to the widget: unasked, it
                        // demands 96 px whatever the cell can spare.
                        let width = ui.available_width();
                        ui.add(
                            egui::ProgressBar::new(usage.total() as f32 / max_total as f32)
                                .desired_width(width)
                                .desired_height(8.0),
                        );
                    });
                });
                for value in [
                    usage.total(),
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens + usage.cache_creation_input_tokens,
                ] {
                    row.col(|ui| {
                        right_aligned(ui, |ui| {
                            ui.label(human_tokens(value));
                        });
                    });
                }
            });
        });
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
