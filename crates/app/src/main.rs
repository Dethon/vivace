#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::{CloseRequestResponse, ComponentHandle, LogicalSize};
use vivace::{App, MainWindow, Settings, WindowState};

// Smaller than this and the saved size is ignored as a likely glitch.
const MIN_RESTORED_SIZE: f32 = 320.0;

fn main() -> Result<(), slint::PlatformError> {
    // SAFETY: nothing has started a thread yet.
    unsafe { vivace::limit_player_threads() };
    vivace::tune_allocator();
    vivace::init_logging();

    let ui = MainWindow::new()?;
    ui.set_app_icon(vivace::app_icon());
    let settings = Settings::load();
    if let Some(window) = settings.window {
        if window.width >= MIN_RESTORED_SIZE && window.height >= MIN_RESTORED_SIZE {
            ui.window()
                .set_size(LogicalSize::new(window.width, window.height));
        }
        ui.window().set_maximized(window.maximized);
    }

    let app = App::start(&ui, settings.config())?;

    ui.window().on_close_requested({
        let ui = ui.as_weak();
        move || {
            if let Some(ui) = ui.upgrade() {
                remember_window(ui.window());
            }
            CloseRequestResponse::HideWindow
        }
    });

    ui.show()?;
    app.attach_media_keys();
    slint::run_event_loop()?;
    ui.hide()?;
    app.quit();
    Ok(())
}

fn remember_window(window: &slint::Window) {
    let maximized = window.is_maximized();
    let size = window.size().to_logical(window.scale_factor());
    Settings::update(|settings| {
        // A maximised window keeps the size it had before, for when it is restored.
        let restored = settings.window.filter(|_| maximized);
        settings.window = Some(WindowState {
            width: restored.map_or(size.width, |window| window.width),
            height: restored.map_or(size.height, |window| window.height),
            maximized,
        });
    });
}
