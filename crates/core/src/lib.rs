//! UI-independent Spotify engine: authentication, playback and catalogue access.

mod api;
mod auth;
mod connect;
mod engine;
pub mod model;

use std::time::Duration;

pub use api::{Api, RemoteCommand, SavedItems};
pub use auth::data_dir;
pub use connect::{Connect, Device, DeviceKind, Remote};
pub use engine::{
    Config, Engine, Event, Events, Playback, Queue, QueueItem, Repeat, SinkFactory, Start,
};

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
