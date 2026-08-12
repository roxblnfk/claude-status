//! The "History" tab: limit usage over time plus the daily aggregates Claude
//! Code keeps in `stats-cache.json`.

use std::collections::BTreeMap;

use claude_status_core::{
    Sample,
    stats_cache::StatsCache,
    statusline::{FIVE_HOUR_SECS, SEVEN_DAY_SECS},
    timefmt, tr, tr_args,
};
use eframe::egui;
use egui_plot::{
    Bar, BarChart, Corner, HoverPosition, Legend, Line, Plot, PlotPoint, PlotPoints,
};

use crate::state::{AppState, Range};
use crate::ui::human_tokens;

/// Height of the limits plot.
const LIMITS_HEIGHT: f32 = 190.0;
/// Height of the sessions plot.
const DAILY_HEIGHT: f32 = 150.0;
/// Height of the token plot: a stack of five models needs the room.
const TOKENS_HEIGHT: f32 = DAILY_HEIGHT * 2.0;
/// How many days of daily aggregates to show.
const DAILY_DAYS: i64 = 30;
/// How many models get their own colour before the rest are lumped together.
const TOP_MODELS: usize = 5;

/// A series ready for both plotting and hit-testing.
type Points = Vec<[f64; 2]>;

/// Signature `BarChart::element_formatter` expects.
type BarLabel = Box<dyn Fn(&Bar, &BarChart) -> String>;

pub fn draw(ui: &mut egui::Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.label(tr("history.period"));
        for range in Range::ALL {
            if ui.selectable_label(state.range == range, range.label()).clicked()
                && state.range != range
            {
                state.range = range;
                state.refresh();
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text =
                tr_args("history.sample_count", &[("count", &state.history.len().to_string())]);
            ui.label(egui::RichText::new(text).weak());
        });
    });
    ui.add_space(6.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.strong(tr("history.limits.title"));
        limits_plot(ui, &state.history, state.scoped_model.as_deref());

        if let Some(stats) = &state.stats {
            ui.add_space(14.0);
            ui.strong(tr_args("history.sessions.title", &[("days", &DAILY_DAYS.to_string())]));
            sessions_plot(ui, stats);

            ui.add_space(14.0);
            ui.strong(tr_args("history.tokens.title", &[("days", &DAILY_DAYS.to_string())]));
            tokens_plot(ui, stats);
        }
    });
}

/// Limit percentages over the selected span.
fn limits_plot(ui: &mut egui::Ui, history: &[Sample], scoped_model: Option<&str>) {
    if history.is_empty() {
        empty_note(ui, LIMITS_HEIGHT);
        return;
    }

    // Points are measured in hours from the start of the span: that keeps the
    // axis labels readable and the values precise, which raw unix seconds would
    // not.
    let origin = history.first().map_or(0, |s| s.ts);
    let span_hours = history.last().map_or(1.0, |s| (s.ts - origin) as f64 / 3600.0).max(1.0);

    let five = series(history, origin, FIVE_HOUR_SECS, |s| s.five_pct, |s| s.five_resets_at);
    let week = series(history, origin, SEVEN_DAY_SECS, |s| s.week_pct, |s| s.week_resets_at);
    let opus = series(history, origin, SEVEN_DAY_SECS, |s| s.opus_pct, |s| s.opus_resets_at);

    // Hit-testing goes against the moments actually sampled, not against the
    // drawn points: the latter now carry the reset drops, which belong to one
    // series alone and would pull the tooltip off the shared instant.
    let moments: Vec<f64> = history.iter().map(|s| hours_since(origin, s.ts)).collect();

    // The per-model cap was Opus alone when the plot was written; it has not
    // been for a while. The probe knows whose cap it is, so the legend says so
    // — the same way the card on the overview does.
    let scoped_name = match scoped_model {
        Some(model) => tr_args("history.series.week_scoped", &[("model", model)]),
        None => tr("history.series.week_opus"),
    };
    let series_names =
        [tr("history.series.five_hour"), tr("history.series.week"), scoped_name];
    let all = [five, week, opus];

    // Lines go through the plot's own `label_formatter`, which renders a real
    // egui tooltip — no hover delay, kept on screen, on a proper background.
    let labels = (series_names.clone(), all.clone());
    let id = plot_id("limits");
    let plot = grounded(ui, id, LIMITS_HEIGHT)
        // Percentages live in 0..100 — anything else is not a view worth
        // panning to, so the axis is pinned instead of auto-fitted.
        .default_y_bounds(0.0, 100.0)
        .default_x_bounds(0.0, span_hours)
        // Past a couple of days the time of day says nothing about which point
        // is which; the date does.
        .x_axis_formatter(move |mark, _| {
            let ts = origin + (mark.value * 3600.0) as i64;
            if span_hours > 48.0 { timefmt::date(ts) } else { timefmt::clock(ts) }
        })
        .label_formatter(move |hover| {
            let (HoverPosition::NearDataPoint { position, .. }
            | HoverPosition::Elsewhere { position }) = hover;
            limits_tooltip(&labels.0, &labels.1, &moments, origin, *position)
        });

    let response = plot.show(ui, |plot_ui| {
        // Re-applied every frame: the `default_*` bounds only seed the very
        // first one, and a plot keeps its view in egui's memory ever after.
        // Without this the period buttons changed the data underneath a frozen
        // week-wide viewport — a day drawn into a seventh of the width, a month
        // running off the right edge.
        plot_ui.set_plot_bounds_y(0.0..=100.0);
        let margin = span_hours * 0.02;
        plot_ui.set_plot_bounds_x(-margin..=span_hours + margin);
        for (name, points) in series_names.iter().zip(all) {
            plot_ui.line(Line::new(name.clone(), PlotPoints::from(points)).width(2.0));
        }
    });
    remember_legend_corner(ui, id, response.response.hovered());
}

/// Value of every series at the moment nearest the cursor.
fn limits_tooltip(
    names: &[String; 3],
    series: &[Points; 3],
    moments: &[f64],
    origin: i64,
    cursor: PlotPoint,
) -> Option<String> {
    let nearest = nearest_moment(moments, cursor.x)?;

    let ts = origin + (nearest * 3600.0) as i64;
    let mut lines = vec![timefmt::datetime(ts)];

    for (name, points) in names.iter().zip(series) {
        // Each series is sampled at the same instants, so pin them all to the
        // hovered moment rather than to each one's own nearest point.
        if let Some(point) = points.iter().find(|p| p[0] == nearest) {
            lines.push(format!("{name}: {:.1}%", point[1]));
        }
    }
    (lines.len() > 1).then(|| lines.join("\n"))
}

fn nearest_moment(moments: &[f64], x: f64) -> Option<f64> {
    moments.iter().copied().min_by(|a, b| (a - x).abs().total_cmp(&(b - x).abs()))
}

/// Sessions started per day.
fn sessions_plot(ui: &mut egui::Ui, stats: &StatsCache) {
    let days: Vec<(i64, f64)> = recent_days(
        stats.daily_activity.iter().filter_map(|d| {
            timefmt::parse_day_key(&d.date).map(|day| (day, d.session_count as f64))
        }),
    );

    if days.is_empty() {
        empty_note(ui, DAILY_HEIGHT);
        return;
    }

    let bars: Vec<Bar> = days.iter().map(|(day, count)| Bar::new(*day as f64, *count)).collect();
    let ceiling = y_ceiling(days.iter().map(|(_, value)| *value));
    let label = tr("history.sessions.series");

    let id = plot_id("sessions");
    let response = daily_plot(ui, id, &days, DAILY_HEIGHT).show(ui, |plot_ui| {
        plot_ui.set_plot_bounds_y(0.0..=ceiling);
        plot_ui.bar_chart(
            BarChart::new(label.clone(), bars)
                .color(egui::Color32::from_rgb(67, 140, 200))
                .width(0.8)
                .element_formatter(bar_tooltip_suppressor()),
        );
        plot_ui.pointer_coordinate()
    });

    let tooltip = response.inner.and_then(|point| {
        let day = point.x.round() as i64;
        let (_, count) = days.iter().find(|(d, _)| *d == day)?;
        Some(format!("{}\n{label}: {count:.0}", timefmt::format_day_number(day)))
    });
    remember_legend_corner(ui, id, response.response.hovered());
    show_tooltip(&response.response, tooltip);
}

/// Tokens per day, stacked by model.
fn tokens_plot(ui: &mut egui::Ui, stats: &StatsCache) {
    let per_day = tokens_by_day_and_model(stats);
    if per_day.is_empty() {
        empty_note(ui, DAILY_HEIGHT);
        return;
    }

    let models = top_models(stats);
    let totals: Vec<(i64, f64)> = per_day
        .iter()
        .map(|(day, day_models)| {
            (*day, models.iter().map(|m| tokens_of(day_models, m)).sum::<i64>() as f64)
        })
        .collect();

    // Stacking needs each layer to know the ones below it, so the charts are
    // built in order and every new one stacks on everything already made.
    let mut charts: Vec<BarChart> = Vec::new();
    for (index, model) in models.iter().enumerate() {
        let bars: Vec<Bar> = per_day
            .iter()
            .map(|(day, day_models)| Bar::new(*day as f64, tokens_of(day_models, model) as f64))
            .collect();

        let previous: Vec<&BarChart> = charts.iter().collect();
        charts.push(
            BarChart::new(short_model_name(model), bars)
                .color(model_color(index))
                .width(0.8)
                .stack_on(&previous)
                .element_formatter(bar_tooltip_suppressor()),
        );
    }

    let ceiling = y_ceiling(totals.iter().map(|(_, value)| *value));

    let id = plot_id("tokens");
    let response = daily_plot(ui, id, &totals, TOKENS_HEIGHT).show(ui, |plot_ui| {
        plot_ui.set_plot_bounds_y(0.0..=ceiling);
        for chart in charts {
            plot_ui.bar_chart(chart);
        }
        plot_ui.pointer_coordinate()
    });

    let tooltip = response.inner.and_then(|point| tokens_tooltip(&per_day, &models, point));
    remember_legend_corner(ui, id, response.response.hovered());
    show_tooltip(&response.response, tooltip);
}

/// Which stacked layer the cursor sits in, and how much it holds.
fn tokens_tooltip(
    per_day: &[(i64, BTreeMap<&str, i64>)],
    models: &[String],
    cursor: PlotPoint,
) -> Option<String> {
    let day = cursor.x.round() as i64;
    let (_, day_models) = per_day.iter().find(|(d, _)| *d == day)?;

    // Layers are drawn bottom-up in model order, so walking the same order
    // accumulates the exact boundaries the bars were painted at.
    let mut base = 0.0;
    for model in models {
        let value = tokens_of(day_models, model);
        let top = base + value as f64;
        if value > 0 && cursor.y >= base && cursor.y <= top {
            // The date first, as the sessions plot does it: only every few days
            // gets an axis label, so a bar on its own says nothing about when.
            return Some(format!(
                "{}\n{}\n{}",
                timefmt::format_day_number(day),
                short_model_name(model),
                human_tokens(value)
            ));
        }
        base = top;
    }
    None
}

fn tokens_of(day_models: &BTreeMap<&str, i64>, model: &str) -> i64 {
    day_models.get(model).copied().unwrap_or(0)
}

/// Shared setup for the two daily plots: pinned axes and date labels.
fn daily_plot<'a>(ui: &egui::Ui, id: egui::Id, days: &[(i64, f64)], height: f32) -> Plot<'a> {
    let first = days.first().map_or(0, |(day, _)| *day);
    let last = days.last().map_or(first + 1, |(day, _)| *day);

    grounded(ui, id, height)
        // Half a day of padding on each side so the outermost bars are not
        // sliced in half by the plot edge.
        .default_x_bounds(first as f64 - 0.5, last as f64 + 0.5)
        .default_y_bounds(0.0, y_ceiling(days.iter().map(|(_, value)| *value)))
        .x_axis_formatter(|mark, _| timefmt::format_day_number(mark.value.round() as i64))
}

fn plot_id(name: &str) -> egui::Id {
    egui::Id::new(("claude-status-plot", name))
}

/// Pins a plot in place and hides the Y axis labels.
///
/// Without pinning, a stray scroll or drag sends the view off to arbitrary
/// coordinates with no way back. The vertical margin is zero because every one
/// of these plots starts at zero, and a fractional margin would pad the axis
/// into negative values that mean nothing here.
///
/// `show_x`/`show_y` stay on: the whole hover pipeline hangs off them, and
/// switching them off also disables highlighting the element under the cursor.
fn grounded<'a>(ui: &egui::Ui, id: egui::Id, height: f32) -> Plot<'a> {
    Plot::new(id)
        // Stated rather than derived, so `PlotMemory` can be read back under
        // the same name — that is where the legend hover lands.
        .id(id)
        .height(height)
        .legend(Legend::default().position(legend_corner(ui, id)))
        .allow_drag(false)
        .allow_zoom(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .set_margin_fraction(egui::Vec2::new(0.02, 0.0))
        .show_axes([true, false])
}

/// Which corner the legend sits in this frame.
///
/// It is parked over the data by definition, and on a busy plot it hides the
/// very thing being examined. Hovering it sends it to the other side.
fn legend_corner(ui: &egui::Ui, id: egui::Id) -> Corner {
    if displaced(ui, id) { Corner::LeftTop } else { Corner::RightTop }
}

/// Decides where the legend goes next, once the plot has been drawn.
///
/// It comes back only after the pointer has left the plot, not the moment it
/// is no longer underneath: stepping out from under the cursor ends the hover,
/// which would bring it straight back under the cursor and set it flickering
/// between the two corners every frame.
fn remember_legend_corner(ui: &egui::Ui, id: egui::Id, plot_hovered: bool) {
    let hovered = egui_plot::PlotMemory::load(ui.ctx(), id)
        .is_some_and(|memory| memory.hovered_legend_item.is_some());
    let moved = hovered || (displaced(ui, id) && plot_hovered);
    ui.data_mut(|data| data.insert_temp(displaced_id(id), moved));
}

fn displaced(ui: &egui::Ui, id: egui::Id) -> bool {
    ui.data(|data| data.get_temp(displaced_id(id))).unwrap_or(false)
}

fn displaced_id(id: egui::Id) -> egui::Id {
    id.with("legend-displaced")
}

/// Shows a hover label for a bar chart.
///
/// Bars cannot use the plot's `label_formatter` — `BarChart::on_hover` ignores
/// it and paints its own text as a shape *inside* the plot, which gets clipped
/// at peaks and against the right edge. That text is suppressed by handing the
/// chart an empty [`element_formatter`](bar_tooltip_suppressor), and the label
/// is drawn here instead: an always-open tooltip appears without the usual
/// hover delay and egui keeps it on screen and on a readable background.
fn show_tooltip(response: &egui::Response, text: Option<String>) {
    let Some(text) = text else { return };
    if !response.hovered() {
        return;
    }

    egui::Tooltip::always_open(
        response.ctx.clone(),
        response.layer_id,
        response.id,
        egui::PopupAnchor::Pointer,
    )
    // A model identifier broken across two lines is far harder to read than a
    // wide tooltip; these are a line or two of text either way.
    .show(|ui| ui.add(egui::Label::new(text).wrap_mode(egui::TextWrapMode::Extend)));
}

/// Silences the label a bar chart would otherwise paint inside the plot, while
/// keeping the highlight that the same hover pass draws.
fn bar_tooltip_suppressor() -> BarLabel {
    Box::new(|_, _| String::new())
}

fn empty_note(ui: &mut egui::Ui, height: f32) {
    ui.vertical_centered(|ui| {
        ui.add_space(height / 3.0);
        ui.label(tr("history.empty"));
        ui.add_space(height / 3.0);
    });
}

fn hours_since(origin: i64, ts: i64) -> f64 {
    (ts - origin) as f64 / 3600.0
}

/// Builds a series of `(hours since origin, percent)` points, skipping gaps.
///
/// The percentage is carried forward as a running maximum within each window.
/// Idle Claude Code sessions keep re-reporting an old reading, so the raw rows
/// zig-zag between the true value and a stale one; usage never actually drops
/// inside a window, and the maximum is what really happened.
///
/// The window boundaries are drawn rather than left to be inferred. A window
/// holds its level right up to its `resets_at` and the next one begins at
/// nothing, so the reset gets two points — the old peak and a zero — at the
/// exact moment it happened. Without them the line sloped from the old peak
/// down to the first reading of the new window, which can be hours later, and
/// the drop looked like a gradual decline. The first window in view is anchored
/// the same way, at its start, when that start falls inside the span.
fn series(
    samples: &[Sample],
    origin: i64,
    duration_secs: i64,
    pick: impl Fn(&Sample) -> Option<f64>,
    window: impl Fn(&Sample) -> Option<i64>,
) -> Points {
    let mut points = Points::new();
    let mut peak = f64::NEG_INFINITY;
    // `None` until the first reading: the opening window has no predecessor to
    // close off, which is a different case from the boundary moving.
    let mut seen: Option<Option<i64>> = None;
    let mut last_ts = origin;

    for s in samples {
        let Some(pct) = pick(s) else { continue };
        let boundary = window(s);

        match seen {
            Some(previous) if previous != boundary => {
                // Guarded against a reset stamped outside the gap it should sit
                // in — a point out of order would draw the line backwards.
                if let Some(reset) = previous
                    && (last_ts..=s.ts).contains(&reset)
                {
                    points.push([hours_since(origin, reset), peak]);
                    points.push([hours_since(origin, reset), 0.0]);
                }
                peak = f64::NEG_INFINITY;
            }
            None => {
                if let Some(start) = boundary.map(|r| r - duration_secs)
                    && start >= origin
                {
                    points.push([hours_since(origin, start), 0.0]);
                }
            }
            Some(_) => {}
        }

        seen = Some(boundary);
        peak = peak.max(pct);
        points.push([hours_since(origin, s.ts), peak]);
        last_ts = s.ts;
    }
    points
}

/// Upper Y bound with a little headroom, never zero-height.
fn y_ceiling(values: impl Iterator<Item = f64>) -> f64 {
    let peak = values.fold(0.0_f64, |acc, value| acc.max(value));
    if peak > 0.0 { peak * 1.1 } else { 1.0 }
}

/// Keeps the last [`DAILY_DAYS`] entries, in chronological order.
fn recent_days(entries: impl Iterator<Item = (i64, f64)>) -> Vec<(i64, f64)> {
    let mut days: Vec<(i64, f64)> = entries.collect();
    days.sort_by_key(|(day, _)| *day);
    if days.len() > DAILY_DAYS as usize {
        days.drain(..days.len() - DAILY_DAYS as usize);
    }
    days
}

/// Tokens per day per model over the recent window.
fn tokens_by_day_and_model(stats: &StatsCache) -> Vec<(i64, BTreeMap<&str, i64>)> {
    let mut days: Vec<(i64, BTreeMap<&str, i64>)> = stats
        .daily_model_tokens
        .iter()
        .filter_map(|d| {
            let day = timefmt::parse_day_key(&d.date)?;
            let models = d.tokens_by_model.iter().map(|(m, t)| (m.as_str(), *t)).collect();
            Some((day, models))
        })
        .collect();

    days.sort_by_key(|(day, _)| *day);
    if days.len() > DAILY_DAYS as usize {
        days.drain(..days.len() - DAILY_DAYS as usize);
    }
    days
}

/// The models worth their own colour, busiest first.
fn top_models(stats: &StatsCache) -> Vec<String> {
    stats
        .models_by_usage()
        .into_iter()
        .take(TOP_MODELS)
        .map(|(name, _)| name.to_string())
        .collect()
}

/// Drops the vendor prefix: `claude-opus-4-8` reads better as `opus-4-8` in a
/// legend that has to fit next to a plot.
fn short_model_name(model: &str) -> String {
    model.strip_prefix("claude-").unwrap_or(model).to_string()
}

fn model_color(index: usize) -> egui::Color32 {
    const PALETTE: [egui::Color32; TOP_MODELS] = [
        egui::Color32::from_rgb(67, 140, 200),
        egui::Color32::from_rgb(67, 176, 71),
        egui::Color32::from_rgb(251, 140, 0),
        egui::Color32::from_rgb(171, 108, 214),
        egui::Color32::from_rgb(229, 87, 100),
    ];
    PALETTE[index % PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, five: Option<f64>) -> Sample {
        window_sample(ts, five, Some(999))
    }

    fn window_sample(ts: i64, five: Option<f64>, resets_at: Option<i64>) -> Sample {
        Sample {
            id: ts,
            ts,
            last_seen_ts: ts,
            five_pct: five,
            five_resets_at: resets_at,
            ..Sample::default()
        }
    }

    fn five_series(samples: &[Sample]) -> Points {
        series(
            samples,
            samples.first().map_or(0, |s| s.ts),
            FIVE_HOUR_SECS,
            |s| s.five_pct,
            |s| s.five_resets_at,
        )
    }

    #[test]
    fn series_converts_seconds_to_hours_from_origin() {
        let points = five_series(&[sample(1000, Some(10.0)), sample(1000 + 7200, Some(30.0))]);

        assert_eq!(points.len(), 2);
        assert_eq!(points[0][0], 0.0);
        assert_eq!(points[1][0], 2.0, "7200 seconds is two hours");
        assert_eq!(points[1][1], 30.0);
    }

    #[test]
    fn series_skips_samples_without_the_metric() {
        let samples = [sample(0, None), sample(3600, Some(5.0)), sample(7200, None)];
        assert_eq!(five_series(&samples).len(), 1);
    }

    /// The zig-zag an idle session produces must not show up as usage dropping.
    #[test]
    fn series_carries_the_maximum_forward() {
        let samples = [
            sample(0, Some(3.0)),
            sample(3600, Some(25.0)),
            sample(7200, Some(3.0)), // the idle session reports again
            sample(10800, Some(28.0)),
        ];
        let ys: Vec<f64> = five_series(&samples).iter().map(|p| p[1]).collect();
        assert_eq!(ys, vec![3.0, 25.0, 25.0, 28.0]);
    }

    #[test]
    fn series_restarts_the_maximum_on_a_new_window() {
        let samples = [
            window_sample(0, Some(90.0), Some(100)),
            window_sample(3600, Some(95.0), Some(100)),
            window_sample(7200, Some(2.0), Some(200)), // window reset
            window_sample(10800, Some(5.0), Some(200)),
        ];
        let ys: Vec<f64> = five_series(&samples).iter().map(|p| p[1]).collect();
        assert_eq!(ys, vec![90.0, 95.0, 2.0, 5.0], "a reset is not masked by the old peak");
    }

    /// A window that ends must be drawn ending: the old level runs to the reset
    /// and the new one starts from nought, both at the moment it happened.
    #[test]
    fn series_drops_to_zero_at_the_reset() {
        let reset = 7200;
        let samples = [
            window_sample(0, Some(90.0), Some(reset)),
            window_sample(3600, Some(95.0), Some(reset)),
            // The first reading of the new window lands an hour after the drop.
            window_sample(10800, Some(2.0), Some(reset + FIVE_HOUR_SECS)),
        ];

        let points: Vec<[f64; 2]> = five_series(&samples);
        assert_eq!(
            points,
            vec![[0.0, 90.0], [1.0, 95.0], [2.0, 95.0], [2.0, 0.0], [3.0, 2.0]],
            "the fall is vertical and sits at the reset, not at the next reading"
        );
    }

    /// A reset stamped outside the gap it should sit in would draw the line
    /// backwards; such a boundary is passed over instead.
    #[test]
    fn series_ignores_a_reset_it_cannot_place() {
        let samples = [
            window_sample(3600, Some(90.0), Some(100)),
            window_sample(7200, Some(2.0), Some(200)),
        ];
        let ys: Vec<f64> = five_series(&samples).iter().map(|p| p[1]).collect();
        assert_eq!(ys, vec![90.0, 2.0]);
    }

    /// When a metric appears later than the plot begins, the window it belongs
    /// to opened on screen — so it is drawn from nought.
    #[test]
    fn series_anchors_a_window_that_opens_inside_the_span() {
        let start = 6 * 3600;
        let samples = [
            window_sample(0, None, None), // the span opens without this metric
            window_sample(start + 1800, Some(4.0), Some(start + FIVE_HOUR_SECS)),
        ];

        let points = five_series(&samples);
        assert_eq!(points, vec![[6.0, 0.0], [6.5, 4.0]]);
    }

    #[test]
    fn nearest_moment_picks_the_closest_x() {
        let moments = [0.0, 1.0, 5.0];
        assert_eq!(nearest_moment(&moments, 0.9), Some(1.0));
        assert_eq!(nearest_moment(&moments, 4.0), Some(5.0));
        assert_eq!(nearest_moment(&[], 1.0), None);
    }

    #[test]
    fn limits_tooltip_reports_every_series_at_one_moment() {
        let names = ["5h".to_string(), "week".to_string(), "opus".to_string()];
        let series = [
            // The first series also carries a reset drop, which belongs to it
            // alone and must not become the moment the tooltip reports.
            vec![[0.0, 10.0], [0.5, 10.0], [0.5, 0.0], [1.0, 25.0]],
            vec![[0.0, 50.0], [1.0, 60.0]],
            Points::new(), // this window never arrived
        ];

        let text =
            limits_tooltip(&names, &series, &[0.0, 1.0], 0, PlotPoint::new(0.9, 0.0)).unwrap();
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(lines.len(), 3, "a timestamp and the two present series: {text:?}");
        assert_eq!(lines[1], "5h: 25.0%");
        assert_eq!(lines[2], "week: 60.0%");
    }

    #[test]
    fn limits_tooltip_is_empty_without_data() {
        let names = ["a".to_string(), "b".to_string(), "c".to_string()];
        let empty = [Points::new(), Points::new(), Points::new()];
        assert!(limits_tooltip(&names, &empty, &[], 0, PlotPoint::new(0.0, 0.0)).is_none());
    }

    fn stacked_day() -> Vec<(i64, BTreeMap<&'static str, i64>)> {
        vec![(7, BTreeMap::from([("claude-opus-5", 100), ("claude-fable-5", 50)]))]
    }

    #[test]
    fn tokens_tooltip_names_the_layer_under_the_cursor() {
        let models = vec!["claude-opus-5".to_string(), "claude-fable-5".to_string()];
        let per_day = stacked_day();

        // The first layer spans 0..100, the second 100..150.
        let lower = tokens_tooltip(&per_day, &models, PlotPoint::new(7.0, 40.0)).unwrap();
        assert_eq!(lower, format!("{}\nopus-5\n100", timefmt::format_day_number(7)));

        let upper = tokens_tooltip(&per_day, &models, PlotPoint::new(7.0, 120.0)).unwrap();
        assert_eq!(upper, format!("{}\nfable-5\n50", timefmt::format_day_number(7)));
    }

    #[test]
    fn tokens_tooltip_ignores_empty_space_and_unknown_days() {
        let models = vec!["claude-opus-5".to_string(), "claude-fable-5".to_string()];
        let per_day = stacked_day();

        assert!(tokens_tooltip(&per_day, &models, PlotPoint::new(7.0, 400.0)).is_none());
        assert!(tokens_tooltip(&per_day, &models, PlotPoint::new(99.0, 40.0)).is_none());
    }

    #[test]
    fn tokens_tooltip_skips_models_absent_that_day() {
        // A zero-height layer must not swallow the hover of the one above it.
        let models = vec!["claude-missing".to_string(), "claude-opus-5".to_string()];
        let per_day = vec![(7, BTreeMap::from([("claude-opus-5", 100)]))];

        let text = tokens_tooltip(&per_day, &models, PlotPoint::new(7.0, 0.0)).unwrap();
        assert_eq!(text, format!("{}\nopus-5\n100", timefmt::format_day_number(7)));
    }

    #[test]
    fn recent_days_keeps_the_tail_in_order() {
        let entries = (0..40).map(|d| (39 - d, d as f64));
        let days = recent_days(entries);

        assert_eq!(days.len(), DAILY_DAYS as usize);
        assert_eq!(days.first().unwrap().0, 10, "the oldest days are dropped");
        assert_eq!(days.last().unwrap().0, 39);
        assert!(days.windows(2).all(|w| w[0].0 < w[1].0), "sorted by day");
    }

    #[test]
    fn recent_days_passes_short_input_through() {
        let days = recent_days([(5, 1.0), (3, 2.0)].into_iter());
        assert_eq!(days, vec![(3, 2.0), (5, 1.0)]);
    }

    #[test]
    fn y_ceiling_leaves_headroom_and_never_collapses() {
        assert!((y_ceiling([10.0, 5.0].into_iter()) - 11.0).abs() < 1e-9);
        assert_eq!(y_ceiling(std::iter::empty()), 1.0);
        assert_eq!(y_ceiling([0.0, 0.0].into_iter()), 1.0, "an all-zero day still needs a scale");
    }

    #[test]
    fn model_names_lose_the_vendor_prefix() {
        assert_eq!(short_model_name("claude-opus-4-8"), "opus-4-8");
        assert_eq!(short_model_name("gemma4"), "gemma4");
    }

    #[test]
    fn palette_covers_every_ranked_model() {
        // Indices beyond the palette must wrap rather than panic.
        for index in 0..TOP_MODELS * 3 {
            let _ = model_color(index);
        }
    }
}
