#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::rc::Rc;

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
        let app = Rc::downgrade(&app);
        move || {
            if let Some(ui) = ui.upgrade() {
                remember_window(ui.window());
            }
            // With the tray icon the app keeps playing; without, it ends.
            if !app.upgrade().is_some_and(|app| app.keeps_running_in_tray()) {
                _ = slint::quit_event_loop();
            }
            CloseRequestResponse::HideWindow
        }
    });

    ui.show()?;
    // Opened by the login entry: out of the way until wanted.
    if std::env::args().any(|argument| argument == vivace::BACKGROUND_FLAG) {
        minimise_when_created(&ui);
        if app.keeps_running_in_tray() {
            app.start_in_tray();
        }
    }
    vivace::refresh_login_entry();
    app.attach_media_keys();
    // Runs until quit, as the window may be closed to the tray meanwhile.
    slint::run_event_loop_until_quit()?;
    ui.hide()?;
    app.quit();
    Ok(())
}

/// Minimises the window as soon as the system has created it: asked any
/// earlier, minimising is ignored.
fn minimise_when_created(ui: &MainWindow) {
    use raw_window_handle::HasWindowHandle;

    const CHECK_EVERY: std::time::Duration = std::time::Duration::from_millis(10);
    let window = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, CHECK_EVERY, move || {
        let Some(ui) = window.upgrade() else { return };
        if ui.window().window_handle().window_handle().is_ok() {
            ui.window().set_minimized(true);
            WAITING.with(|timer| timer.take());
        }
    });
    // Kept until the window exists.
    WAITING.with(|waiting| *waiting.borrow_mut() = Some(timer));
}

thread_local! {
    static WAITING: std::cell::RefCell<Option<slint::Timer>> = const { std::cell::RefCell::new(None) };
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
