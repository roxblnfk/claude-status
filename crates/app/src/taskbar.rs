//! Taking the window off the taskbar while the plaque is up.
//!
//! egui carries `taskbar` on the viewport builder alone, and a builder is read
//! when a window is created — the one window here is created once, at startup.
//! `ITaskbarList` changes it on a window already on screen, which is what winit
//! does behind its own `set_skip_taskbar`.

use anyhow::Result;
use raw_window_handle::HasWindowHandle;

/// Adds the window to the taskbar or takes it off.
#[cfg(windows)]
pub fn set_listed(window: &impl HasWindowHandle, listed: bool) -> Result<()> {
    use anyhow::Context as _;
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};

    let handle = window.window_handle().context(claude_status_core::tr("error.window_handle"))?;
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        anyhow::bail!(claude_status_core::tr("error.window_handle"));
    };
    let hwnd = HWND(win32.hwnd.get() as *mut std::ffi::c_void);

    unsafe {
        // The event loop thread is already an apartment — winit opens one for
        // drag and drop — and asking again on it is answered with `S_FALSE`.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let taskbar: ITaskbarList = CoCreateInstance(&TaskbarList, None, CLSCTX_ALL)?;
        taskbar.HrInit()?;
        if listed { taskbar.AddTab(hwnd) } else { taskbar.DeleteTab(hwnd) }?;
    }
    Ok(())
}

/// Elsewhere the plaque keeps its place in the window list: neither X11 nor
/// macOS offers this without going around winit entirely.
#[cfg(not(windows))]
pub fn set_listed(_window: &impl HasWindowHandle, _listed: bool) -> Result<()> {
    Ok(())
}
