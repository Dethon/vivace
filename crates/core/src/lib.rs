//! UI-independent Spotify engine: authentication, playback and catalogue access.

mod api;
mod auth;
mod connect;
mod engine;
pub mod model;

use std::time::Duration;

pub use api::{Api, RemoteCommand, RemotePlay, SavedItems};
pub use auth::data_dir;
pub use connect::{Connect, Device, DeviceKind, Remote};
pub use engine::{
    Config, Engine, Event, Events, Playback, Queue, QueueItem, Repeat, SinkFactory, Start,
};

// Tokio reads this when a runtime is built without an explicit size.
const WORKER_THREADS_VARIABLE: &str = "TOKIO_WORKER_THREADS";
const PLAYER_WORKER_THREADS: &str = "2";

/// Keeps librespot's player from starting one idle thread per CPU core.
///
/// Its playback runtime is built with Tokio's defaults, which size the pool
/// to the machine; two workers are plenty for one audio stream.
///
/// # Safety
///
/// Changes the process environment, so it must be called while no other
/// thread exists: first thing in `main`.
pub unsafe fn limit_player_threads() {
    if std::env::var_os(WORKER_THREADS_VARIABLE).is_none() {
        // SAFETY: the caller guarantees this is the only thread.
        unsafe { std::env::set_var(WORKER_THREADS_VARIABLE, PLAYER_WORKER_THREADS) };
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no usable config directory on this platform")]
    NoConfigDir,
    #[error("login failed: {0}")]
    OAuth(#[from] librespot_oauth::OAuthError),
    #[error("no audio output available")]
    NoAudioBackend,
    #[error("Spotify is rate limiting requests")]
    RateLimited { retry_after: Option<Duration> },
    #[error("not found")]
    NotFound,
    /// A private query Spotify no longer recognises; the feature needs an update.
    #[error("Spotify changed this feature's interface")]
    QueryChanged,
    #[error("Spotify answered with HTTP {0}")]
    Status(u16),
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response from Spotify: {0}")]
    Decode(String),
    #[error(transparent)]
    Spotify(#[from] librespot_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
