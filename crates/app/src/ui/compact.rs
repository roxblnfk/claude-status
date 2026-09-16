//! The compact plaque: the four gauges in a frameless square that floats over
//! whatever the user is actually working in.

use claude_status_core::{Config, WindowState, config::MIN_COMPACT_SIZE, pace::Overview, tr, tr_args};
use eframe::egui;

use crate::state::AppState;
use crate::ui::rings::{self, Gauge, Palette};

/// What the plaque needs to remember between frames.
#[derive(Default)]
pub struct Compact {
    pub active: bool,
    /// The normal window's geometry, to come back to.
    restore: Option<(egui::Pos2, egui::Vec2)>,
    /// Where the plaque was last left. Held for the session only: a position
    /// written to disk outlives the monitor it was taken on.
    position: Option<egui::Pos2>,
    /// The window's size last frame — which edge a resize moved.
    last_size: Option<egui::Vec2>,
}

impl Compact {
    /// Turns the window into the plaque.
    pub fn enter(&mut self, ctx: &egui::Context, side: f32) {
        self.restore = ctx.input(|i| {
            let viewport = i.viewport();
            Some((viewport.outer_rect?.min, viewport.inner_rect?.size()))
        });
        self.active = true;
        self.last_size = None;

        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::viewport::WindowLevel::AlwaysOnTop,
        ));
        // Before the resize: the window still carries the main window's floor,
        // which is several times the plaque's side and would swallow it.
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::Vec2::splat(
            MIN_COMPACT_SIZE,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::splat(side)));
        if let Some(position) = self.position {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
        }
    }

    /// Gives the normal window back. Returns the side the plaque ended up with.
    pub fn leave(&mut self, ctx: &egui::Context) -> Option<f32> {
        self.active = false;
        let (position, size) = ctx.input(|i| {
            let viewport = i.viewport();
            (viewport.outer_rect.map(|r| r.min), viewport.inner_rect.map(|r| r.size()))
        });
        self.position = position;

        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::viewport::WindowLevel::Normal,
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(crate::MIN_WINDOW_SIZE.into()));
        if let Some((position, size)) = self.restore.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
        }
        size.map(|size| size.x.min(size.y))
    }

    /// Pulls the window back into a square after a resize.
    ///
    /// Neither winit nor egui can lock an aspect ratio, so the side is taken
    /// from whichever edge the user moved and the other one follows it.
    fn keep_square(&mut self, ctx: &egui::Context) {
        let Some(size) = ctx.input(|i| i.viewport().inner_rect).map(|r| r.size()) else {
            return;
        };
        if (size.x - size.y).abs() <= 1.0 {
            self.last_size = Some(size);
            return;
        }

        let last = self.last_size.unwrap_or(size);
        let moved_horizontally = (size.x - last.x).abs() >= (size.y - last.y).abs();
        let side = if moved_horizontally { size.x } else { size.y }.max(MIN_COMPACT_SIZE);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::Vec2::splat(side)));
        self.last_size = Some(egui::Vec2::splat(side));
    }
}

/// What the plaque was asked for this frame.
pub struct Action {
    /// Give the full window back.
    pub leave: bool,
    /// Re-read the state.
    pub refresh: bool,
}

/// Draws the plaque.
pub fn draw(ui: &mut egui::Ui, state: &mut AppState, compact: &mut Compact) -> Action {
    compact.keep_square(ui.ctx());

    let rect = ui.max_rect();
    let side = rect.width().min(rect.height());
    let square = egui::Rect::from_center_size(rect.center(), egui::Vec2::splat(side));
    let focused = ui.ctx().input(|i| i.viewport().focused).unwrap_or(true);
    // Hovering is attention too, and a plaque one is about to grab hold of
    // should not be the faint one.
    let awake = focused || ui.rect_contains_pointer(rect);
    let opacity = if awake { 1.0 } else { state.config.compact.inactive_opacity() };

    // The plaque is its own title bar: with no decorations there is nothing
    // else to take hold of. Claimed before the buttons so that they — added
    // later, and so on top — keep their own clicks.
    let plaque = ui.interact(rect, ui.id().with("plaque"), egui::Sense::click_and_drag());
    if plaque.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    rings::draw(ui.painter(), square, &gauges(&state.overview), opacity);

    let mut action = Action {
        leave: plaque.double_clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)),
        refresh: refresh_button(ui, state, square, opacity),
    };
    // The corner is the one part of a square plaque the rings never reach.
    if ui.rect_contains_pointer(rect) {
        let button = (side * 0.16).clamp(16.0, 26.0);
        let at = egui::Rect::from_min_size(
            egui::pos2(rect.right() - button, rect.top()),
            egui::Vec2::splat(button),
        );
        action.leave |= ui
            .put(at, egui::Button::new("↗").frame(false))
            .on_hover_text(tr("compact.restore"))
            .clicked();
    }

    // A tooltip is laid out inside its own viewport, so on a plaque 180 points
    // wide anything but a wrapped column of short lines is simply cut off.
    plaque.on_hover_ui(|ui| {
        ui.set_max_width((side - 24.0).max(80.0));
        for line in readout(state) {
            ui.label(line);
        }
        ui.label(egui::RichText::new(tr("compact.drag")).weak().small());
    });
    action
}

/// The centre of the rings, which is otherwise a hole.
///
/// Returns `true` when the state should be re-read. A running probe replaces
/// the button with the spinner: the answer takes seconds to come back, and the
/// plaque has no room to say so in words.
fn refresh_button(
    ui: &mut egui::Ui,
    state: &mut AppState,
    square: egui::Rect,
    opacity: f32,
) -> bool {
    let at = egui::Rect::from_center_size(
        square.center(),
        egui::Vec2::splat(rings::hole_radius(square, RINGS) * 2.0 - 2.0),
    );
    // On a plaque dragged down to its floor the hole is a few points across,
    // and a button that small is a nuisance rather than a control.
    if at.width() < 14.0 {
        return false;
    }

    let ink = egui::Color32::from_gray(200).gamma_multiply(opacity);
    if state.probing() {
        ui.put(at, egui::Spinner::new().size(at.width() * 0.8).color(ink));
        return false;
    }

    let hint = if state.config.probe.enabled {
        tr("ui.refresh_hint")
    } else {
        tr("ui.refresh_hint_plain")
    };
    let button = egui::Button::new(egui::RichText::new("⟳").size(at.width() * 0.62).color(ink))
        .fill(egui::Color32::from_gray(48).gamma_multiply(opacity))
        // Half the side turns the square into the circle the hole calls for.
        .corner_radius(at.width() / 2.0);

    let pressed = ui.put(at, button).on_hover_text(hint).clicked();
    if pressed {
        state.request_probe();
    }
    pressed
}

/// How many rings the plaque carries. The centre button is sized from the hole
/// they leave, so the two have to agree.
const RINGS: usize = 4;

/// The rings, outermost first.
fn gauges(overview: &Overview) -> [Gauge; RINGS] {
    [
        window_gauge(overview.five_hour, Palette::Limit),
        Gauge {
            fill: overview.daily.map(|d| d.used_fraction() as f32),
            elapsed: overview.daily.map(|d| d.day_elapsed as f32),
            palette: Palette::Limit,
        },
        window_gauge(overview.week, Palette::Limit),
        window_gauge(overview.week_opus, Palette::Scoped),
    ]
}

fn window_gauge(window: Option<WindowState>, palette: Palette) -> Gauge {
    Gauge {
        fill: window.and_then(|w| w.live_used_pct()).map(|pct| (pct / 100.0) as f32),
        elapsed: window.map(|w| w.elapsed_fraction() as f32),
        palette,
    }
}

/// One short line per ring, in the order the rings sit.
///
/// Short on purpose: anything wider than the plaque wraps, and a wrapped column
/// of four entries stops being something read at a glance.
fn readout(state: &AppState) -> Vec<String> {
    let overview = &state.overview;
    let scoped = match &state.scoped_model {
        Some(model) => model.clone(),
        None => tr("compact.ring.scoped"),
    };
    let rings = [
        (tr("compact.ring.session"), overview.five_hour.map(|w| w.live_used_pct())),
        (tr("compact.ring.today"), overview.daily.map(|d| Some(d.used_pct()))),
        (tr("compact.ring.week"), overview.week.map(|w| w.live_used_pct())),
        (scoped, overview.week_opus.map(|w| w.live_used_pct())),
    ];

    rings
        .into_iter()
        .filter_map(|(title, pct)| {
            let pct = pct?;
            Some(match pct {
                Some(pct) => tr_args(
                    "compact.ring.value",
                    &[("title", &title), ("pct", &format!("{pct:.0}"))],
                ),
                None => tr_args("compact.ring.expired", &[("title", &title)]),
            })
        })
        .collect()
}

/// Stores the side the plaque was left at, so the next one opens the same size.
pub fn remember_size(config: &mut Config, side: f32) -> anyhow::Result<()> {
    if (config.compact.size() - side).abs() < 1.0 {
        return Ok(());
    }
    config.compact.size = side;
    config.save()
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_status_core::{pace::DailyBudget, statusline::SEVEN_DAY_SECS};

    fn week(used_pct: f64, now: i64) -> WindowState {
        WindowState::new(used_pct, SEVEN_DAY_SECS, SEVEN_DAY_SECS, now)
    }

    #[test]
    fn the_rings_run_from_the_session_outwards_to_the_scoped_cap() {
        let now = SEVEN_DAY_SECS / 2;
        let overview = Overview {
            five_hour: Some(WindowState::new(50.0, now + 3600, 18_000, now)),
            week: Some(week(30.0, now)),
            week_opus: Some(week(80.0, now)),
            daily: Some(DailyBudget {
                spent_pct: 6.0,
                allowance_pct: 12.0,
                day_elapsed: 0.25,
                estimated: false,
            }),
            ..Overview::default()
        };

        let gauges = gauges(&overview);
        assert_eq!(gauges[0].fill, Some(0.5), "the session is the outer ring");
        assert_eq!(gauges[1].fill, Some(0.5), "then today's ration");
        assert_eq!(gauges[1].elapsed, Some(0.25));
        assert_eq!(gauges[2].fill, Some(0.3), "then the week");
        assert_eq!(gauges[3].fill, Some(0.8), "the scoped cap is innermost");
        assert_eq!(gauges[3].palette, Palette::Scoped, "and the only one in its own colours");
    }

    /// The same silence the overview keeps: the recorded percentage belongs to
    /// a window that no longer exists, so the ring shows its track alone.
    #[test]
    fn a_window_that_has_reset_fills_nothing() {
        let expired = week(90.0, SEVEN_DAY_SECS + 1);
        let gauge = window_gauge(Some(expired), Palette::Limit);
        assert_eq!(gauge.fill, None);
        assert!(gauge.elapsed.is_some(), "the clock has still run its course");
    }

    #[test]
    fn an_overspent_ration_reads_past_a_full_ring() {
        let daily = DailyBudget {
            spent_pct: 18.0,
            allowance_pct: 12.0,
            day_elapsed: 0.5,
            estimated: false,
        };
        let overview = Overview { daily: Some(daily), ..Overview::default() };
        assert_eq!(gauges(&overview)[1].fill, Some(1.5));
    }

    #[test]
    fn nothing_reported_leaves_every_ring_empty() {
        for gauge in gauges(&Overview::default()) {
            assert_eq!(gauge.fill, None);
            assert_eq!(gauge.elapsed, None);
        }
    }
}
