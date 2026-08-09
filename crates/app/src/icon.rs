//! Drawing the tray icon: a ring gauge for one limit window and a dot in the
//! centre for the other, so a single icon shows both limits at once.

use tiny_skia::{Color, LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};

/// Icon side in pixels, with headroom for a HiDPI tray.
pub const SIZE: u32 = 64;

/// A finished icon: RGBA8, `SIZE`×`SIZE`.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Draws the icon.
///
/// `ring_pct` fills the ring (usually the five-hour window), `dot_pct` colours
/// the centre dot (usually the weekly one). `None` renders muted: no data yet.
pub fn render(ring_pct: Option<f64>, dot_pct: Option<f64>) -> Rgba {
    let mut pixmap = Pixmap::new(SIZE, SIZE).expect("the icon size is positive");

    let center = SIZE as f32 / 2.0;
    let stroke_width = SIZE as f32 * 0.15;
    let radius = center - stroke_width / 2.0 - 2.0;

    // The ring track, visible against both light and dark trays.
    draw_arc(&mut pixmap, center, radius, stroke_width, 0.0, 1.0, rgba(128, 128, 128, 90));

    if let Some(pct) = ring_pct {
        let fraction = (pct / 100.0).clamp(0.0, 1.0) as f32;
        if fraction > 0.0 {
            draw_arc(&mut pixmap, center, radius, stroke_width, 0.0, fraction, level_color(pct));
        }
    }

    let dot_radius = radius - stroke_width * 1.1;
    let dot_color = dot_pct.map_or(rgba(128, 128, 128, 110), level_color);
    fill_circle(&mut pixmap, center, dot_radius, dot_color);

    Rgba { width: SIZE, height: SIZE, data: pixmap.take() }
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

fn fill_circle(pixmap: &mut Pixmap, center: f32, radius: f32, color: Color) {
    if radius <= 0.0 {
        return;
    }
    let mut pb = PathBuilder::new();
    pb.push_circle(center, center, radius);
    let Some(path) = pb.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, tiny_skia::FillRule::Winding, Transform::identity(), None);
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
    fn the_centre_dot_reflects_the_second_window() {
        // The ring is empty in both cases; only the dot differs.
        let grey = coloured_pixels(&render(Some(0.0), None));
        let coloured = coloured_pixels(&render(Some(0.0), Some(42.0)));
        assert!(grey < coloured, "{grey} < {coloured}");
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
