//! Concentric gauge rings — the picture the compact plaque is made of.
//!
//! The tray icon draws the same two gauges with tiny-skia into a bitmap; here
//! there are four of them, they follow the window's size and they have to show
//! a ration spent past its allowance, which a clamped arc cannot.

use eframe::egui::{
    self, Color32, Pos2, Rect, Stroke, Vec2,
    epaint::{PathShape, PathStroke},
};

/// Where one ring sits.
#[derive(Clone, Copy)]
struct Band {
    center: Pos2,
    radius: f32,
    width: f32,
}

/// One gauge.
pub struct Gauge {
    /// How full, `1.0` being the limit. `None` — nothing has been reported and
    /// the ring shows its bare track.
    pub fill: Option<f32>,
    /// Where the clock stands in the same window, 0..1.
    pub elapsed: Option<f32>,
    pub palette: Palette,
}

/// Which colours a ring runs through.
///
/// The scoped cap gets its own family so that a glance separates it from the
/// three rings that share one budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Limit,
    Scoped,
}

/// Clearance from the plaque's edge to the outer ring, as a share of the side.
const MARGIN: f32 = 0.035;
/// Ring thickness, same units.
const STROKE: f32 = 0.068;
/// Clearance between two rings, same units.
const GAP: f32 = 0.030;

/// Radius of the ring nearest the edge.
fn outer_radius(side: f32, width: f32) -> f32 {
    side * (0.5 - MARGIN) - width / 2.0
}

/// The clear circle `rings` bands leave at the centre.
pub fn hole_radius(rect: Rect, rings: usize) -> f32 {
    let side = rect.width().min(rect.height());
    let width = side * STROKE;
    let innermost = outer_radius(side, width) - rings.saturating_sub(1) as f32 * side * (STROKE + GAP);
    (innermost - width / 2.0).max(0.0)
}

/// The unspent part of a ring: grey 150 at alpha 80. Premultiplied because
/// that is the only constructor `Color32` offers as a `const fn`.
const TRACK: Color32 = Color32::from_rgba_premultiplied(47, 47, 47, 80);
/// Backing disc, so the gauges read over a bright window as well as a dark one.
/// It reaches no further than the outer ring: the corners of the plaque are
/// what keeps it from looking like a window.
const BACKDROP: Color32 = Color32::from_rgba_premultiplied(8, 8, 10, 210);

/// Colour stops of a palette, keyed by laps into the ring: `1.0` is the limit
/// and the stop past it is where an overspent ring ends up.
const LIMIT_STOPS: &[(f32, Color32)] = &[
    (0.0, Color32::from_rgb(67, 176, 71)),
    (0.5, Color32::from_rgb(253, 216, 53)),
    (0.75, Color32::from_rgb(251, 140, 0)),
    (1.0, Color32::from_rgb(229, 57, 53)),
    (2.0, Color32::from_rgb(123, 31, 162)),
];

/// Blue through violet to magenta. Every stop up to the limit is kept light:
/// the overview writes the reading onto the bar in dark ink.
const SCOPED_STOPS: &[(f32, Color32)] = &[
    (0.0, Color32::from_rgb(79, 195, 247)),
    (0.6, Color32::from_rgb(129, 140, 248)),
    (1.0, Color32::from_rgb(216, 120, 222)),
    (2.0, Color32::from_rgb(123, 31, 162)),
];

/// Draws the gauges into the largest circle `rect` holds, outermost first.
///
/// `opacity` scales everything painted: the plaque sits over somebody else's
/// window and has to get out of the way when the focus is elsewhere.
pub fn draw(painter: &egui::Painter, rect: Rect, gauges: &[Gauge], opacity: f32) {
    let side = rect.width().min(rect.height());
    if side <= 0.0 {
        return;
    }
    let center = rect.center();
    let width = side * STROKE;
    let step = side * (STROKE + GAP);
    let outer = outer_radius(side, width);

    painter.circle_filled(center, outer + width / 2.0, BACKDROP.gamma_multiply(opacity));

    for (i, gauge) in gauges.iter().enumerate() {
        let radius = outer - i as f32 * step;
        // Any further in and the ring would close over the centre rather than
        // stay a ring.
        if radius <= width {
            break;
        }
        ring(painter, Band { center, radius, width }, gauge, side, opacity);
    }
}

/// The arcs one ring is built from, as fractions of a turn from twelve o'clock.
///
/// They never overlap. Laying the progress over a full-circle track would do
/// for an opaque plaque, but a half-transparent one would show every overlap as
/// a brighter band.
#[derive(Debug, Default, PartialEq)]
struct Arcs {
    /// What is left unspent.
    track: Option<(f32, f32)>,
    /// The first lap, up to the limit.
    first: Option<(f32, f32)>,
    /// What went past the limit, drawn over the start of the first lap.
    over: Option<(f32, f32)>,
}

/// As far round as the picture can go: past two laps the overflow would begin
/// covering itself.
///
/// The cap stops a hair short of closing the second lap, because a closed ring
/// reads as two laps to the point — and a reading that got clamped never is
/// that. The sliver of the first lap left showing before twelve o'clock is what
/// says the gauge ran off the end of its scale; without it 216 % looked exactly
/// like 200 %.
const MAX_LAPS: f32 = 1.97;

fn arcs(fill: Option<f32>) -> Arcs {
    let Some(fill) = fill.filter(|f| f.is_finite()).map(|f| f.clamp(0.0, MAX_LAPS)) else {
        return Arcs { track: Some((0.0, 1.0)), ..Arcs::default() };
    };

    // Past the limit the first lap survives only where the overflow has not
    // reached yet, which is what makes the second lap legible as a second lap.
    let from = (fill - 1.0).max(0.0);
    let to = fill.min(1.0);
    Arcs {
        track: (fill < 1.0).then_some((fill, 1.0)),
        first: (to > from).then_some((from, to)),
        over: (fill > 1.0).then_some((0.0, fill - 1.0)),
    }
}

fn ring(painter: &egui::Painter, band: Band, gauge: &Gauge, side: f32, opacity: f32) {
    let arcs = arcs(gauge.fill);
    if let Some((from, to)) = arcs.track {
        solid_arc(painter, band, from, to, TRACK.gamma_multiply(opacity));
    }
    for (lap, span) in [(0.0, arcs.first), (1.0, arcs.over)] {
        if let Some((from, to)) = span {
            gradient_arc(painter, band, from, to, gauge.palette, lap, opacity);
        }
    }

    let Some(at) = gauge.elapsed.filter(|e| e.is_finite()).map(|e| e.clamp(0.0, 1.0)) else {
        return;
    };
    // The reading alone answers nothing — 60 % spent is comfortable at the end
    // of a window and alarming at its start. The tick is where an even pace
    // would have got to by now.
    let over_fill = arcs.first.is_some_and(|(from, to)| at >= from && at <= to)
        || arcs.over.is_some_and(|(from, to)| at >= from && at <= to);
    // A tick of one colour would vanish on whichever side it landed: the fill is
    // bright and the track is not.
    let color = if over_fill { Color32::from_gray(24) } else { Color32::from_gray(225) };
    painter.line_segment(
        [
            point_at(band.center, band.radius - band.width / 2.0, at),
            point_at(band.center, band.radius + band.width / 2.0, at),
        ],
        Stroke::new((side * 0.014).max(1.5), color.gamma_multiply(opacity)),
    );
}

fn solid_arc(painter: &egui::Painter, band: Band, from: f32, to: f32, color: Color32) {
    let points = arc_points(band.center, band.radius, from, to);
    painter.add(PathShape::line(points, PathStroke::new(band.width, color)));
}

/// The same arc, but coloured by how far along the ring each point sits.
///
/// A gradient is what makes the second lap tell itself apart from the first:
/// both cover the same angles, so only the colour can say which is which.
/// The tessellator hands the callback a position, and the bounding box it comes
/// with is the arc's own — the centre has to be carried in instead.
fn gradient_arc(
    painter: &egui::Painter,
    band: Band,
    from: f32,
    to: f32,
    palette: Palette,
    lap: f32,
    opacity: f32,
) {
    let center = band.center;
    let stroke = PathStroke::new_uv(band.width, move |_bbox, pos| {
        let mut turn = turn_fraction(center, pos);
        // An arc that touches twelve o'clock has the feathered vertices at that
        // end land on the far side of the wrap, where the raw fraction reads as
        // the opposite end of the turn — and so takes the opposite colour.
        if turn < from - 0.02 {
            turn += 1.0;
        } else if turn > to + 0.02 {
            turn -= 1.0;
        }
        color_at(palette, lap + turn).gamma_multiply(opacity)
    });
    painter.add(PathShape::line(arc_points(band.center, band.radius, from, to), stroke));
}

/// Where a point sits on the turn: 0 at twelve o'clock, growing clockwise.
fn turn_fraction(center: Pos2, pos: Pos2) -> f32 {
    let d = pos - center;
    let turn = d.x.atan2(-d.y) / std::f32::consts::TAU;
    if turn < 0.0 { turn + 1.0 } else { turn }
}

fn point_at(center: Pos2, radius: f32, turn: f32) -> Pos2 {
    center + Vec2::angled(turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2) * radius
}

/// egui has no arc primitive, so one is built from a polyline; at two degrees a
/// step the facets are indistinguishable from a curve at any plaque size.
fn arc_points(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    const SEGMENTS_PER_TURN: f32 = 180.0;

    let span = to - from;
    let steps = ((span.abs() * SEGMENTS_PER_TURN).ceil() as usize).max(1);
    (0..=steps)
        .map(|i| point_at(center, radius, from + span * (i as f32 / steps as f32)))
        .collect()
}

/// Colour `turn` laps into the ring.
pub fn color_at(palette: Palette, turn: f32) -> Color32 {
    let stops = match palette {
        Palette::Limit => LIMIT_STOPS,
        Palette::Scoped => SCOPED_STOPS,
    };
    let last = stops[stops.len() - 1];
    let turn = turn.clamp(stops[0].0, last.0);

    for pair in stops.windows(2) {
        let ((a_at, a), (b_at, b)) = (pair[0], pair[1]);
        if turn <= b_at {
            return mix(a, b, (turn - a_at) / (b_at - a_at));
        }
    }
    last.1
}

fn mix(a: Color32, b: Color32, k: f32) -> Color32 {
    let k = k.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k).round() as u8;
    Color32::from_rgb(channel(a.r(), b.r()), channel(a.g(), b.g()), channel(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreported_ring_is_all_track() {
        assert_eq!(arcs(None), Arcs { track: Some((0.0, 1.0)), first: None, over: None });
    }

    #[test]
    fn progress_and_track_share_the_turn_without_overlapping() {
        let a = arcs(Some(0.4));
        assert_eq!(a.first, Some((0.0, 0.4)));
        assert_eq!(a.track, Some((0.4, 1.0)));
        assert_eq!(a.over, None);
    }

    /// The whole point of the second lap: it has to eat into the first one, or
    /// an overspent ration would look exactly like a spent one.
    #[test]
    fn an_overspent_ring_shows_both_laps() {
        let a = arcs(Some(1.3));
        assert_eq!(a.track, None);
        let (over_from, over_to) = a.over.expect("the second lap");
        let (first_from, first_to) = a.first.expect("what is left of the first");
        assert_eq!(over_from, 0.0);
        assert!((over_to - 0.3).abs() < 1e-6, "{over_to}");
        assert_eq!(first_to, 1.0);
        assert!((first_from - over_to).abs() < 1e-6, "the first lap starts where the overflow ends");
    }

    #[test]
    fn beyond_two_laps_the_overflow_stops_growing() {
        assert_eq!(arcs(Some(5.0)), arcs(Some(2.0)));
        assert_eq!(arcs(Some(f32::INFINITY)), arcs(None), "a non-finite reading is no reading");
    }

    /// A ring closed on twelve o'clock would say "two laps exactly", and a
    /// reading clamped to the cap never is one: the daily ration stood at
    /// 216 % and the picture was indistinguishable from 200 %.
    #[test]
    fn the_second_lap_stops_short_of_closing_over_the_first() {
        let a = arcs(Some(5.0));
        let (over_from, over_to) = a.over.expect("the overflow is drawn");
        assert_eq!(over_from, 0.0);
        assert!(over_to < 1.0, "the second lap closed the ring: {over_to}");

        let (from, to) = a.first.expect("a piece of the first lap stays visible");
        assert!((to - 1.0).abs() < 1e-6, "it is the end of the first lap that shows");
        assert!(to - from > 0.02, "too thin to see: {}", to - from);
    }

    #[test]
    fn an_empty_ring_draws_no_progress() {
        let a = arcs(Some(0.0));
        assert_eq!(a.first, None);
        assert_eq!(a.track, Some((0.0, 1.0)));
    }

    #[test]
    fn the_turn_starts_at_twelve_and_runs_clockwise() {
        let center = Pos2::new(50.0, 50.0);
        let at = |turn| turn_fraction(center, point_at(center, 10.0, turn));
        for turn in [0.0, 0.25, 0.5, 0.75] {
            assert!((at(turn) - turn).abs() < 1e-4, "{turn} came back as {}", at(turn));
        }
        assert!(point_at(center, 10.0, 0.25).x > center.x, "a quarter turn is three o'clock");
    }

    #[test]
    fn a_full_turn_is_traced_finely_enough_to_look_round() {
        let points = arc_points(Pos2::ZERO, 10.0, 0.0, 1.0);
        assert_eq!(points.len(), 181);
        // A degenerate span still has to produce a path rather than a panic.
        assert_eq!(arc_points(Pos2::ZERO, 10.0, 0.5, 0.5).len(), 2);
    }

    /// The refresh button in the middle of the plaque is sized from this, so a
    /// hole that collapsed would take the button with it.
    #[test]
    fn the_rings_leave_a_hole_in_the_middle() {
        let plaque = |side: f32| Rect::from_min_size(Pos2::ZERO, Vec2::splat(side));
        let hole = hole_radius(plaque(180.0), 4);
        assert!(hole > 14.0, "too small to press: {hole}");
        assert!(hole < 30.0, "the rings have been crowded out: {hole}");
        assert!(hole_radius(plaque(180.0), 5) < hole, "another ring eats into it");
        assert!(hole_radius(plaque(120.0), 4) > 0.0, "even at the smallest plaque");
        assert_eq!(hole_radius(plaque(180.0), 40), 0.0, "and never goes negative");
    }

    #[test]
    fn the_gradient_escalates_and_then_leaves_the_scale() {
        let green = color_at(Palette::Limit, 0.0);
        let red = color_at(Palette::Limit, 1.0);
        assert_eq!(green, Color32::from_rgb(67, 176, 71));
        assert_eq!(red, Color32::from_rgb(229, 57, 53));
        assert!(color_at(Palette::Limit, 0.6).r() > green.r(), "green gives way to yellow");
        assert_ne!(color_at(Palette::Limit, 1.5), red, "the second lap must not read as the first");
    }

    #[test]
    fn the_scoped_palette_shares_no_colour_with_the_limit_one() {
        for turn in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_ne!(color_at(Palette::Scoped, turn), color_at(Palette::Limit, turn));
        }
    }

    #[test]
    fn colours_outside_the_scale_clamp_to_its_ends() {
        assert_eq!(color_at(Palette::Limit, -3.0), color_at(Palette::Limit, 0.0));
        assert_eq!(color_at(Palette::Limit, 9.0), color_at(Palette::Limit, 2.0));
    }
}
