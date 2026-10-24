//! Repaints the whole window when Windows asks for it.
//!
//! Slint only shows what it has repainted. A repaint Windows asks for, after
//! it dropped the window's image (restored from minimized, display sleep,
//! some window switches), finds nothing changed in the interface, so nothing
//! was shown and the desktop showed through until a hover or a resize. When
//! Windows has part of the window to paint, the window's background changes
//! by a shade nobody can see, which Slint repaints everywhere.

use crate::MainWindow;

#[cfg(windows)]
pub fn install(ui: &MainWindow) {
    use slint::ComponentHandle;
    use slint::winit_030::winit::event::WindowEvent;
    use slint::winit_030::{EventResult, WinitWindowAccessor};

    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |window, event| {
        if matches!(event, WindowEvent::RedrawRequested)
            && asked_by_windows(window)
            && let Some(ui) = weak.upgrade()
        {
            ui.set_repaint(!ui.get_repaint());
        }
        EventResult::Propagate
    });
}

#[cfg(not(windows))]
pub fn install(_ui: &MainWindow) {}

/// Windows has part of the window to paint, rather than Slint having asked
/// to draw what changed.
#[cfg(windows)]
fn asked_by_windows(window: &slint::Window) -> bool {
    use slint::winit_030::WinitWindowAccessor;
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Graphics::Gdi::GetUpdateRect;

    window
        .with_winit_window(
            |window| match window.window_handle().map(|handle| handle.as_raw()) {
                // SAFETY: the handle is the live window's; no rectangle is asked for.
                Ok(RawWindowHandle::Win32(handle)) => unsafe {
                    GetUpdateRect(handle.hwnd.get() as _, std::ptr::null_mut(), 0) != 0
                },
                _ => false,
            },
        )
        .unwrap_or(false)
}
