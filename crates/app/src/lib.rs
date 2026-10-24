//! Vivace's user interface.

mod app;
mod backend;
mod content;
mod format;
mod images;
pub mod instance;
mod library;
mod logging;
mod media;
mod models;
pub mod notify;
mod repaint;
mod settings;
pub mod system;
mod thumbbar;
mod tray;
mod updater;

slint::include_modules!();

/// Keeps glibc from growing a separate heap for every thread.
///
/// Its default allows eight heaps per core, and freed memory in one is not
/// reused by another: with a handful of mostly idle threads that left a
/// quarter of the heap unused. Other allocators need no tuning.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const HEAPS: libc::c_int = 2;
        // SAFETY: `mallopt` only changes allocator parameters.
        unsafe { libc::mallopt(libc::M_ARENA_MAX, HEAPS) };
    }
}

pub use app::App;
pub use images::app_icon;
pub use logging::init as init_logging;
pub use settings::{Settings, WindowState};
pub use system::{BACKGROUND_FLAG, refresh_login_entry};
pub use vivace_core::{Config, limit_player_threads};
