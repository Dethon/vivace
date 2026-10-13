//! Vivace's user interface.

mod app;
mod backend;
mod content;
mod format;
mod images;
mod media;
mod models;

slint::include_modules!();

pub use app::App;
pub use vivace_core::Config;
