//! Catalogue and library access over the playback session.
//!
//! Everything here uses Spotify's client endpoints with the session's own
//! token; the public Web API rejects that token with HTTP 429.

mod catalog;
mod chapters;
mod graph;
mod home;
mod library;
mod lyrics;
mod playlists;
mod remote;
mod resumption;
mod search;
mod transport;

use std::sync::Arc;

use librespot_core::Session;

pub use library::SavedItems;
pub use remote::{RemoteCommand, RemotePlay};
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

    /// Resolves a context the way Spirc does before loading it, and counts
    /// the items of its first page, for checking what a load would get.
    #[doc(hidden)]
    pub async fn resolve_context(&self, uri: &str) -> crate::Result<usize> {
        let context = self.0.session().spclient().get_context(uri).await?;
        Ok(context.pages.first().map_or(0, |page| page.tracks.len()))
    }
}
