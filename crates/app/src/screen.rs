//! Where a window sits, in a unit that survives the journey.
//!
//! egui speaks in ui points, and a point is a different pixel on every display
//! scaled differently — a position in points says nothing off the monitor it
//! was read on. Anything that has to outlive a move between monitors, or a
//! restart, is kept in pixels; sizes stay in points, where the same number is
//! the same apparent size on either display.

use eframe::egui;

pub fn to_pixels(points: egui::Pos2, pixels_per_point: f32) -> [f32; 2] {
    [points.x * pixels_per_point, points.y * pixels_per_point]
}

pub fn to_points(pixels: [f32; 2], pixels_per_point: f32) -> egui::Pos2 {
    egui::pos2(pixels[0] / pixels_per_point, pixels[1] / pixels_per_point)
}

/// Whether a window of this geometry would show on some monitor.
///
/// Nothing between here and the operating system clamps a window to the
/// desktop, so a position saved on a display that has since been unplugged
/// opens the window where no click can reach it.
#[cfg(windows)]
pub fn is_on_a_monitor(position: [f32; 2], size: [f32; 2]) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};

    let rect = RECT {
        left: position[0] as i32,
        top: position[1] as i32,
        right: (position[0] + size[0].max(1.0)) as i32,
        bottom: (position[1] + size[1].max(1.0)) as i32,
    };
    !unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.0.is_null()
}

/// Elsewhere the window managers are the ones that place windows, and they do
/// not hand out coordinates off the desktop to begin with.
#[cfg(not(windows))]
pub fn is_on_a_monitor(_position: [f32; 2], _size: [f32; 2]) -> bool {
    true
}

/// Where a window goes when the place it remembers has gone. The primary
/// monitor starts at the origin, so a little way in from there is on screen.
pub const FALLBACK_POSITION: [f32; 2] = [64.0, 64.0];
