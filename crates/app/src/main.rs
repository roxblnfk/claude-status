//! `claude-status` — monitoring Claude Code subscription limits.
//!
//! With no arguments it launches the GUI with a tray icon. The subcommands are
//! for maintenance: register the hook, check the state, preview the line.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod hook;
mod icon;
mod screen;
mod state;
mod taskbar;
mod tray;
mod ui;

use std::time::{Duration, Instant};

use anyhow::Result;
use claude_status_core::{Config, autostart, timefmt, tr, tr_args, update};
use eframe::egui;

use crate::state::AppState;
use crate::tray::{Tray, TrayAction};
use crate::ui::UiState;

fn main() -> Result<()> {
    // Resolve the local time zone before the GUI starts: `time` can only do it
    // while the process is single-threaded.
    timefmt::init_local_offset();
    // The language has to be in place before any command produces output.
    let config = Config::load_and_apply_language();

    match cli::parse(std::env::args().skip(1))? {
        cli::Command::Gui { hidden } => run_gui(hidden, &config),
        // Handed the configuration already read: this runs on every assistant
        // message, so a second read of the same file is worth avoiding. It
        // reports nothing upwards either — a hook that exits non-zero would
        // break the session's status line.
        cli::Command::Hook => {
            hook::run(&config);
            Ok(())
        }
        command => cli::run(command),
    }
}

/// The floor the normal window may be resized to. The plaque sets its own and
/// has to put this one back.
pub const MIN_WINDOW_SIZE: [f32; 2] = [460.0, 320.0];

/// What the window opens at before it has been sized by hand.
const DEFAULT_WINDOW_SIZE: [f32; 2] = [720.0, 560.0];

fn run_gui(hidden: bool, config: &Config) -> Result<()> {
    // The image an update displaced cannot be deleted while it is running; by
    // now it is not.
    update::clean_leftovers();
    tray::init_platform()?;

    let size = config.window.size().map_or(DEFAULT_WINDOW_SIZE, |(w, h)| [w, h]);
    let viewport = egui::ViewportBuilder::default()
        .with_title(tr("ui.window_title"))
        .with_inner_size(size)
        .with_min_inner_size(MIN_WINDOW_SIZE)
        // For the compact plaque, which is a frameless square of rings with
        // nothing behind them. Windows only wires up the alpha channel when
        // the window is created this way, so it cannot wait for the switch.
        .with_transparent(true)
        // Started by the session: the icon appears, the window does not.
        .with_visible(!hidden);
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("claude-status", options, Box::new(move |cc| Ok(Box::new(App::new(cc, hidden)))))
        .map_err(|e| anyhow::anyhow!(tr_args("error.run_gui", &[("error", &e.to_string())])))
}

struct App {
    state: AppState,
    ui_state: UiState,
    /// Created on the first frame: `tray-icon` needs a thread with a message
    /// loop, and that only exists inside `eframe`.
    tray: Option<Tray>,
    last_refresh: Instant,
    /// Closing the window hides it in the tray; a real exit takes a command.
    quitting: bool,
    /// Spent on the first frame: `with_visible(false)` alone is not enough,
    /// eframe shows the window once it has something to draw.
    hide_on_start: bool,
    /// Whether the taskbar lists the window. The plaque takes its place there,
    /// and showing the window again puts it back whatever we asked for.
    on_taskbar: bool,
    /// Where the window stood last frame — the position in pixels, the size in
    /// points. Read as the frames go by because the viewport is gone by the
    /// time there is anything to write it to.
    geometry: Option<([f32; 2], egui::Vec2)>,
    /// The position the configuration remembers, until the first frame has
    /// moved the window there. The viewport builder cannot do it: it takes a
    /// position in points, and which pixel that lands on is settled by the
    /// display the window happens to open on.
    place_at: Option<[f32; 2]>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, hidden: bool) -> Self {
        setup_fonts(&cc.egui_ctx);
        let state = AppState::load();
        let place_at = state.config.window.position().map(|(x, y)| [x, y]);
        Self {
            ui_state: UiState::new(&state.config),
            state,
            tray: None,
            last_refresh: Instant::now(),
            quitting: false,
            hide_on_start: hidden,
            on_taskbar: true,
            geometry: None,
            place_at,
        }
    }

    fn ensure_tray(&mut self, ctx: &egui::Context) {
        if self.tray.is_some() {
            return;
        }
        // Waking the paint loop: without it a click on the icon would go
        // unhandled while the window is hidden.
        let wake_ctx = ctx.clone();
        match Tray::new(&self.state, self.ui_state.compact.active, move || {
            wake_ctx.request_repaint()
        }) {
            Ok(tray) => self.tray = Some(tray),
            Err(e) => {
                let message = tray::unavailable_message(&e);
                self.state.error = Some(match self.state.error.take() {
                    Some(existing) => format!("{existing}; {message}"),
                    None => message,
                });
                // Retrying every frame is pointless — the application stays
                // useful as a plain window.
                self.tray = None;
            }
        }
    }

    fn refresh(&mut self) {
        self.state.refresh();
        self.last_refresh = Instant::now();
        if let Some(tray) = &mut self.tray
            && let Err(e) = tray.update(&self.state, self.ui_state.compact.active)
        {
            self.state.error = Some(tr_args("error.icon_update", &[("error", &format!("{e:#}"))]));
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        // A window left on a monitor that has since gone cannot be dragged
        // back — there is nothing on screen to take hold of. Asking the tray
        // for it is the way out.
        let per_point = ctx.pixels_per_point();
        if let Some((at, size)) = self.geometry
            && !screen::is_on_a_monitor(at, [size.x * per_point, size.y * per_point])
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(screen::to_points(
                screen::FALLBACK_POSITION,
                per_point,
            )));
        }
        // Showing a window puts it back on the taskbar; if the plaque is what is
        // coming back, the next tick has to take it off again.
        self.on_taskbar = true;
    }

    /// Notes where the window stands, unless it is minimised — a minimised
    /// window reports a position off the screen that it must not come back to.
    fn track_geometry(&mut self, ctx: &egui::Context) {
        let per_point = ctx.pixels_per_point();
        if let Some(geometry) = ctx.input(|i| {
            let viewport = i.viewport();
            (viewport.minimized != Some(true))
                .then(|| {
                    let position = screen::to_pixels(viewport.outer_rect?.min, per_point);
                    Some((position, viewport.inner_rect?.size()))
                })
                .flatten()
        }) {
            self.geometry = Some(geometry);
        }
    }

    /// Moves the window to where the last session left it.
    ///
    /// Spent on the first frame, once there is a scale factor to read the
    /// stored pixels against. A monitor that has gone since takes its
    /// coordinates with it, and the window is left wherever it opened.
    fn place_on_start(&mut self, ctx: &egui::Context) {
        // The size is read first: without it there is nothing to check the
        // position against, and the request has to wait for a later frame.
        let Some((_, size)) = self.geometry else { return };
        let Some(at) = self.place_at.take() else { return };
        let per_point = ctx.pixels_per_point();
        if screen::is_on_a_monitor(at, [size.x * per_point, size.y * per_point]) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(screen::to_points(
                at, per_point,
            )));
        }
    }

    /// Stores where the window and the plaque were left, for the next launch.
    ///
    /// Only one of the two is on screen, so the other's place comes from what
    /// the plaque kept when it swapped them over.
    fn save_geometry(&mut self) {
        let Some((position, size)) = self.geometry else { return };
        let (plaque_at, plaque_side) = self.ui_state.compact.plaque();
        let config = &mut self.state.config;

        if self.ui_state.compact.active {
            (config.compact.x, config.compact.y) = (Some(position[0]), Some(position[1]));
            config.compact.size = size.x.min(size.y);
            if let Some((at, size)) = self.ui_state.compact.window() {
                config.window.set((at[0], at[1]), (size.x, size.y));
            }
        } else {
            config.window.set((position[0], position[1]), (size.x, size.y));
            config.compact.size = plaque_side;
            if let Some(at) = plaque_at {
                (config.compact.x, config.compact.y) = (Some(at[0]), Some(at[1]));
            }
        }

        // Nothing is left on screen to report a failure to, and a window that
        // opens in the wrong place is not worth holding up the exit for.
        let _ = config.save();
    }

    /// Keeps the taskbar in step with which shape the window is in.
    fn sync_taskbar(&mut self, frame: &eframe::Frame) {
        let listed = !self.ui_state.compact.active;
        if self.on_taskbar == listed {
            return;
        }
        self.on_taskbar = listed;
        if let Err(e) = taskbar::set_listed(frame, listed) {
            self.state.error = Some(tr_args("error.taskbar", &[("error", &format!("{e:#}"))]));
        }
    }
}

impl eframe::App for App {
    /// Work unrelated to painting.
    ///
    /// `eframe` calls `logic` even while the window is hidden, as long as
    /// somebody requested a repaint — `ui` is not called at all in that case.
    /// So the tray and the data refresh live here: an application minimised to
    /// the tray must keep working.
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        tray::pump_platform_events();
        self.ensure_tray(ctx);
        self.sync_taskbar(frame);
        self.track_geometry(ctx);
        self.place_on_start(ctx);

        if self.hide_on_start {
            self.hide_on_start = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        let actions = self.tray.as_ref().map(Tray::poll).unwrap_or_default();
        for action in actions {
            match action {
                TrayAction::Show => self.show_window(ctx),
                TrayAction::FullWindow => {
                    if self.ui_state.compact.active {
                        self.ui_state.compact.leave(ctx);
                    }
                    self.show_window(ctx);
                }
                TrayAction::Refresh => {
                    self.state.request_probe();
                    self.refresh();
                }
                TrayAction::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        // A process cannot swap out its own image, so stepping into a freshly
        // installed version means handing over to a new one. It goes straight
        // to the tray: the window this was pressed in is about to vanish.
        if std::mem::take(&mut self.state.restart_requested) {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).arg(autostart::TRAY_FLAG).spawn();
            }
            self.quitting = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let period = Duration::from_secs(self.state.config.tray.refresh_secs.max(1));
        if self.last_refresh.elapsed() >= period || self.state.probe_outcome_pending() {
            self.refresh();
        }

        // The close button hides the window in the tray: the program keeps
        // collecting statistics.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // The tick is needed while hidden too, otherwise the icon stops updating.
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if ui::draw(ui, &mut self.state, &mut self.ui_state) {
            self.refresh();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_geometry();
    }

    /// The plaque has no background of its own: everything outside the rings is
    /// whatever it is floating over.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.ui_state.compact.active {
            egui::Color32::TRANSPARENT.to_normalized_gamma_f32()
        } else {
            visuals.window_fill().to_normalized_gamma_f32()
        }
    }
}


/// Loads a system font covering Cyrillic and the typography the status line uses.
///
/// The font bundled with egui covers Cyrillic but not the arrows and block
/// characters of the bars — without a substitute they render as tofu.
fn setup_fonts(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\segoeui.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];

    let Some(data) = CANDIDATES.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "system".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(data)),
    );
    // The system font goes second: egui's own stays primary and the missing
    // glyphs are pulled from the system one.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("system".to_owned());
    }
    ctx.set_fonts(fonts);
}
#[cfg(test)]
mod translations {
    use claude_status_core::tr;
    use std::path::{Path, PathBuf};

    /// Every translation key written out in the sources must exist.
    ///
    /// `rust-i18n` echoes an unknown key back rather than failing, so a typo —
    /// or a key that ends up nested under the wrong parent — renders as
    /// `settings.tray.refresh` in the middle of the window and nothing
    /// complains. This asks for each one and reports the misses together.
    #[test]
    fn every_key_used_in_the_sources_exists() {
        // Locale-independent: a key that exists translates in either language,
        // and one that does not echoes back in both.
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/");

        let mut missing = Vec::new();
        let mut checked = 0;
        for file in rust_files(crates) {
            let text = std::fs::read_to_string(&file).expect("a source file");
            for key in keys(&text) {
                checked += 1;
                if tr(&key) == key {
                    missing.push(format!("  {}: {key}", file.display()));
                }
            }
        }

        assert!(checked > 100, "the scan found almost nothing — has it stopped working?");
        assert!(missing.is_empty(), "translation keys with nothing behind them:\n{}", missing.join("\n"));
    }

    fn rust_files(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else { return found };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(rust_files(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
        found
    }

    /// Literal keys handed to `tr` / `tr_args`, across a line break or not.
    ///
    /// A call with a computed key — `tr(&format!(…))`, `tr(chosen)` — has no
    /// literal to check and is passed over.
    fn keys(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        for pattern in ["tr(", "tr_args("] {
            let mut offset = 0;
            while let Some(pos) = text[offset..].find(pattern) {
                let at = offset + pos;
                offset = at + pattern.len();

                // `tr(` also sits inside `tr_args(` and inside any identifier
                // ending in those letters.
                let standalone = text[..at]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_');
                if !standalone {
                    continue;
                }
                if let Some(body) = text[offset..].trim_start().strip_prefix('"')
                    && let Some(end) = body.find('"')
                    && looks_like_a_key(&body[..end])
                {
                    found.push(body[..end].to_string());
                }
            }
        }
        found
    }

    /// Keeps the scan off anything that is plainly not a key — including the
    /// `"tr("` written out in this very file.
    fn looks_like_a_key(text: &str) -> bool {
        text.contains('.')
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    }
}
