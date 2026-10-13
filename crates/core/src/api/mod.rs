//! Catalogue and library access over the playback session.
//!
//! Everything here uses Spotify's client endpoints with the session's own
//! token; the public Web API rejects that token with HTTP 429.

mod catalog;
mod graph;
mod home;
mod library;
mod lyrics;
mod remote;
mod search;
mod transport;

use std::sync::Arc;

use librespot_core::Session;

pub use library::SavedItems;
pub use remote::RemoteCommand;
use transport::Transport;

/// Cheaply cloneable handle for catalogue and library requests.
#[derive(Clone)]
pub struct Api(Arc<Transport>);

impl Api {
    pub(crate) fn new(session: Session) -> Self {
        Self(Arc::new(Transport::new(session)))
    }

    /// Downloads a resource such as cover art.
    pub async fn fetch(&self, url: &str) -> crate::Result<Vec<u8>> {
        Ok(self.0.get(url).await?.to_vec())
    }
}
