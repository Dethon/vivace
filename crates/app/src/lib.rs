//! Vivace's user interface.

mod app;
mod backend;
mod content;
mod format;
mod images;
mod logging;
mod media;
mod models;
mod settings;

slint::include_modules!();

pub use app::App;
pub use images::app_icon;
pub use logging::init as init_logging;
pub use settings::{Settings, WindowState};
pub use vivace_core::Config;
