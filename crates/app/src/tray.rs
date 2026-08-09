//! The system tray icon: a ring gauge, a tooltip and a context menu.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use claude_status_core::{tr, tr_args};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use crate::icon;
use crate::state::AppState;

/// Queue filled by the tray event handlers.
type Queue = Arc<Mutex<VecDeque<TrayAction>>>;

/// What the user did with the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Show the window and bring it to the front.
    Show,
    /// Re-read the database right now.
    Refresh,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    queue: Queue,
    /// The last rendered state, so the system is not poked for nothing.
    drawn: Option<(i64, i64)>,
    tooltip: String,
}

impl Tray {
    /// Creates the icon and subscribes to its events.
    ///
    /// `wake` is called from the event handler thread when the user touches the
    /// icon: the window may be hidden by then, and without waking the paint
    /// loop the click would go unnoticed.
    pub fn new(state: &AppState, wake: impl Fn() + Send + Sync + 'static) -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::new(tr("tray.menu.show"), true, None);
        let refresh = MenuItem::new(tr("tray.menu.refresh"), true, None);
        let quit = MenuItem::new(tr("tray.menu.quit"), true, None);

        menu.append(&show)?;
        menu.append(&refresh)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit)?;

        let tooltip = state.tooltip();
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(&tooltip)
            .with_icon(to_icon(&icon::render(None, None))?)
            .build()
            .with_context(|| tr("error.create_tray"))?;

        let queue: Queue = Arc::default();
        let wake = Arc::new(wake);

        let (show_id, refresh_id, quit_id) =
            (show.id().clone(), refresh.id().clone(), quit.id().clone());
        let menu_queue = Arc::clone(&queue);
        let menu_wake = Arc::clone(&wake);
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == show_id {
                TrayAction::Show
            } else if event.id == refresh_id {
                TrayAction::Refresh
            } else if event.id == quit_id {
                TrayAction::Quit
            } else {
                return;
            };
            push(&menu_queue, action);
            menu_wake();
        }));

        let icon_queue = Arc::clone(&queue);
        let icon_wake = Arc::clone(&wake);
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if is_activation(&event) {
                push(&icon_queue, TrayAction::Show);
                icon_wake();
            }
        }));

        let mut tray = Self { icon, queue, drawn: None, tooltip };
        tray.update(state)?;
        Ok(tray)
    }

    /// Redraws the icon and the tooltip when the data has changed.
    pub fn update(&mut self, state: &AppState) -> Result<()> {
        let ring = state.ring_window().map(|w| w.used_pct);
        let dot = state.dot_window().map(|w| w.used_pct);

        // Rounded to a percent: the icon does not change below that anyway, and
        // updating a system icon is not cheap.
        let key = (quantize(ring), quantize(dot));
        if self.drawn != Some(key) {
            self.icon.set_icon(Some(to_icon(&icon::render(ring, dot))?))?;
            self.drawn = Some(key);
        }

        let tooltip = state.tooltip();
        if tooltip != self.tooltip {
            self.icon.set_tooltip(Some(&tooltip))?;
            self.tooltip = tooltip;
        }
        Ok(())
    }

    /// Takes the accumulated actions.
    pub fn poll(&self) -> Vec<TrayAction> {
        match self.queue.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(poisoned) => poisoned.into_inner().drain(..).collect(),
        }
    }
}

fn push(queue: &Queue, action: TrayAction) {
    match queue.lock() {
        Ok(mut queue) => queue.push_back(action),
        Err(poisoned) => poisoned.into_inner().push_back(action),
    }
}

/// A left click on the icon is the most expected way to open the window.
fn is_activation(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::DoubleClick { button: tray_icon::MouseButton::Left, .. }
            | TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            }
    )
}

/// `None` is encoded as −1, so "no data" differs from "exactly 0 %".
fn quantize(pct: Option<f64>) -> i64 {
    pct.map_or(-1, |p| p.clamp(0.0, 100.0).round() as i64)
}

fn to_icon(rgba: &icon::Rgba) -> Result<Icon> {
    Icon::from_rgba(rgba.data.clone(), rgba.width, rgba.height)
        .with_context(|| tr("error.build_icon"))
}

/// Pumps GTK events.
///
/// On Linux `tray-icon` sits on top of libayatana-appindicator and expects a
/// live GTK main loop, which winit does not provide. We turn it by hand from
/// the paint loop; on other platforms the call is empty.
#[cfg(target_os = "linux")]
pub fn pump_platform_events() {
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn pump_platform_events() {}

/// Initialises the platform tray subsystem. Call before [`Tray::new`].
#[cfg(target_os = "linux")]
pub fn init_platform() -> Result<()> {
    gtk::init().with_context(|| tr("error.gtk_init"))
}

#[cfg(not(target_os = "linux"))]
pub fn init_platform() -> Result<()> {
    Ok(())
}

/// Formats a tray creation failure for display in the window.
pub fn unavailable_message(error: &anyhow::Error) -> String {
    tr_args("error.tray_unavailable", &[("error", &format!("{error:#}"))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_separates_absence_from_zero() {
        assert_eq!(quantize(None), -1);
        assert_eq!(quantize(Some(0.0)), 0);
        assert_eq!(quantize(Some(0.4)), 0);
        assert_eq!(quantize(Some(0.6)), 1);
        assert_eq!(quantize(Some(150.0)), 100, "exceeding the limit clamps to 100");
    }
}
