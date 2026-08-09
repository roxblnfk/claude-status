//! Drawing the tray icon: two concentric gauges — the session limit on the
//! outer ring, today's budget on the inner one.

use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Icon side in pixels, with headroom for a HiDPI tray.
pub const SIZE: u32 = 64;

/// Ring thickness, as a share of the icon side.
const STROKE: f32 = 0.155;
/// Clearance between the two rings. Wide enough that at a 16 px tray size,
/// where the whole icon is a quarter of this, the seam still shows when both
/// gauges are full and the same colour.
const GAP: f32 = 0.075;

/// A finished icon: RGBA8, `SIZE`×`SIZE`.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Draws the icon.
///
/// `session_pct` fills the outer ring, `daily_pct` the inner one. `None` leaves
/// a ring showing nothing but its track: no data yet.
pub fn render(session_pct: Option<f64>, daily_pct: Option<f64>) -> Rgba {
    let mut pixmap = Pixmap::new(SIZE, SIZE).expect("the icon size is positive");

    let center = SIZE as f32 / 2.0;
    let stroke = SIZE as f32 * STROKE;
    // Inscribed: the outer edge of the outer ring touches the icon bounds.
    let outer = center - stroke / 2.0;
    let inner = outer - stroke - SIZE as f32 * GAP;

    draw_ring(&mut pixmap, center, outer, stroke, session_pct);
    draw_ring(&mut pixmap, center, inner, stroke, daily_pct);

    Rgba { width: SIZE, height: SIZE, data: pixmap.take() }
}

/// One gauge.
///
/// The track goes down first and is never covered by more than the progress
/// arc, so the ring keeps its outline whether the limit is untouched or spent
/// to the last percent — and it stays a ring, never filling into a disc.
fn draw_ring(pixmap: &mut Pixmap, center: f32, radius: f32, width: f32, pct: Option<f64>) {
    draw_arc(pixmap, center, radius, width, 0.0, 1.0, rgba(128, 128, 128, 90));

    let Some(pct) = pct else { return };
    let fraction = (pct / 100.0).clamp(0.0, 1.0) as f32;
    if fraction.is_nan() || fraction <= 0.0 {
        return;
    }
    draw_arc(pixmap, center, radius, width, 0.0, fraction, level_color(pct));
}

/// Colour by fill level: calm below half, alarming past 80 %.
fn level_color(pct: f64) -> Color {
    if pct >= 90.0 {
        rgba(229, 57, 53, 255) // red
    } else if pct >= 75.0 {
        rgba(251, 140, 0, 255) // orange
    } else if pct >= 50.0 {
        rgba(253, 216, 53, 255) // yellow
    } else {
        rgba(67, 176, 71, 255) // green
    }
}

fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// Draws an arc from `from` to `to` (fractions of a full turn), starting at
/// twelve o'clock and going clockwise.
///
/// tiny-skia has no arc primitive, so the arc is built from a polyline: at
/// 64 px a two-degree step makes the facets indistinguishable from a curve.
fn draw_arc(
    pixmap: &mut Pixmap,
    center: f32,
    radius: f32,
    width: f32,
    from: f32,
    to: f32,
    color: Color,
) {
    const SEGMENTS_PER_TURN: usize = 180;

    let span = to - from;
    let segments = ((span.abs() * SEGMENTS_PER_TURN as f32).ceil() as usize).max(2);

    let mut pb = PathBuilder::new();
    for i in 0..=segments {
        let t = from + span * (i as f32 / segments as f32);
        // −π/2 moves the start to the top; the sign turns it clockwise.
        let angle = t * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let (x, y) = (center + radius * angle.cos(), center + radius * angle.sin());
        if i == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    let Some(path) = pb.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;

    let stroke = Stroke { width, line_cap: LineCap::Round, ..Stroke::default() };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque_pixels(icon: &Rgba) -> usize {
        icon.data.chunks_exact(4).filter(|px| px[3] > 0).count()
    }

    /// Gauge pixels: the track is grey, the progress is coloured. Counting
    /// opaque pixels is meaningless — the progress is drawn over the track, so
    /// the total painted area does not depend on the fill.
    fn coloured_pixels(icon: &Rgba) -> usize {
        icon.data
            .chunks_exact(4)
            .filter(|px| {
                let (max, min) = (px[..3].iter().max(), px[..3].iter().min());
                px[3] > 0 && max.zip(min).is_some_and(|(hi, lo)| hi - lo > 20)
            })
            .count()
    }

    #[test]
    fn produces_a_correctly_sized_rgba_buffer() {
        let icon = render(Some(50.0), Some(20.0));
        assert_eq!(icon.width, SIZE);
        assert_eq!(icon.height, SIZE);
        assert_eq!(icon.data.len(), (SIZE * SIZE * 4) as usize);
    }

    #[test]
    fn fuller_ring_covers_more_pixels() {
        let empty = coloured_pixels(&render(Some(0.0), None));
        let half = coloured_pixels(&render(Some(50.0), None));
        let full = coloured_pixels(&render(Some(100.0), None));
        assert_eq!(empty, 0, "at zero usage the gauge is empty");
        assert!(half < full, "{half} < {full}");
    }

    #[test]
    fn the_inner_ring_reflects_the_daily_budget() {
        // The outer ring is empty in both cases; only the inner one differs.
        let grey = coloured_pixels(&render(Some(0.0), None));
        let coloured = coloured_pixels(&render(Some(0.0), Some(42.0)));
        assert!(grey < coloured, "{grey} < {coloured}");
    }

    /// A row through the middle of the icon crosses, from the left edge:
    /// the outer ring, a gap, the inner ring, then the hole in the centre.
    /// Both limits are full — the icon must still read as two rings.
    #[test]
    fn full_gauges_stay_rings() {
        let icon = render(Some(100.0), Some(100.0));
        let row = SIZE / 2;
        let opaque = |x: u32| icon.data[((row * SIZE + x) * 4 + 3) as usize] > 0;

        let bands = (0..SIZE / 2).fold((0usize, false), |(count, was), x| match opaque(x) {
            true if !was => (count + 1, true),
            other => (count, other),
        });
        assert_eq!(bands.0, 2, "two rings before the centre");
        assert!(!opaque(SIZE / 2 - 1), "the centre stays hollow");
    }

    #[test]
    fn the_outer_ring_is_inscribed() {
        let icon = render(Some(100.0), None);
        let row = SIZE / 2;
        let alpha = |x: u32| icon.data[((row * SIZE + x) * 4 + 3) as usize];
        assert!(alpha(0) > 0, "the ring reaches the icon bounds");
    }

    #[test]
    fn corners_stay_transparent() {
        let icon = render(Some(100.0), Some(100.0));
        let alpha_at = |x: u32, y: u32| icon.data[((y * SIZE + x) * 4 + 3) as usize];
        assert_eq!(alpha_at(0, 0), 0);
        assert_eq!(alpha_at(SIZE - 1, SIZE - 1), 0);
    }

    #[test]
    fn out_of_range_percentages_do_not_panic() {
        for pct in [-50.0, 0.0, 100.0, 250.0, f64::NAN] {
            let icon = render(Some(pct), Some(pct));
            assert_eq!(icon.data.len(), (SIZE * SIZE * 4) as usize, "pct = {pct}");
        }
    }

    #[test]
    fn missing_data_still_renders_the_track() {
        assert!(opaque_pixels(&render(None, None)) > 0, "an empty icon must not be transparent");
    }

    #[test]
    fn colour_escalates_with_usage() {
        assert_eq!(level_color(10.0), rgba(67, 176, 71, 255));
        assert_eq!(level_color(60.0), rgba(253, 216, 53, 255));
        assert_eq!(level_color(80.0), rgba(251, 140, 0, 255));
        assert_eq!(level_color(95.0), rgba(229, 57, 53, 255));
    }
}
