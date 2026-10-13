#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::ComponentHandle;
use vivace::{App, Config, MainWindow};

fn main() -> Result<(), slint::PlatformError> {
    // Warnings go to stderr; `RUST_LOG` selects more, e.g. `RUST_LOG=vivace=debug`.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let ui = MainWindow::new()?;
    let app = App::start(&ui, Config::default())?;
    ui.show()?;
    app.attach_media_keys();
    slint::run_event_loop()?;
    ui.hide()
}
