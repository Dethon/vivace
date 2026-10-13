//! Compact domain types shared with the UI.
//!
//! Lists of these are held for whole playlists, so they favour small
//! representations: boxed strings, 16-byte ids and a single image URL.

pub use librespot_core::SpotifyId as Id;

const IMAGE_HOST: &str = "https://i.scdn.co/image/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Track,
    Album,
    Artist,
    Playlist,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Track => "track",
            Kind::Album => "album",
            Kind::Artist => "artist",
            Kind::Playlist => "playlist",
        }
    }

    /// The `spotify:<kind>:<id>` URI for `id`.
    pub fn uri(self, id: Id) -> String {
        format!("spotify:{}:{id}", self.as_str())
    }

    /// Extracts the id from a URI of this kind.
    pub fn id(self, uri: &str) -> Option<Id> {
        let id = uri.strip_prefix("spotify:")?.strip_prefix(self.as_str())?;
        Id::from_base62(id.strip_prefix(':')?).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Image(Box<str>);

impl Image {
    pub(crate) fn from_url(url: &str) -> Self {
        Self(url.into())
    }

    pub(crate) fn from_file_id(file_id: &[u8]) -> Self {
        let mut url = String::with_capacity(IMAGE_HOST.len() + file_id.len() * 2);
        url.push_str(IMAGE_HOST);
        for byte in file_id {
            url.push_str(&format!("{byte:02x}"));
        }
        Self(url.into())
    }

    pub fn url(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRef {
    pub id: Id,
    pub name: Box<str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRef {
    pub id: Id,
    pub name: Box<str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: Id,
    pub name: Box<str>,
    pub artists: Box<[ArtistRef]>,
    pub album: AlbumRef,
    pub duration_ms: u32,
    pub explicit: bool,
    pub playable: bool,
    pub cover: Option<Image>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumKind {
    Album,
    Single,
    Compilation,
    Ep,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Album {
    pub id: Id,
    pub name: Box<str>,
    pub artists: Box<[ArtistRef]>,
    pub kind: AlbumKind,
    /// Release year, or 0 when unknown.
    pub year: u16,
    pub cover: Option<Image>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumDetails {
    pub album: Album,
    pub label: Box<str>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Artist {
    pub id: Id,
    pub name: Box<str>,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistDetails {
    pub artist: Artist,
    pub top_tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub singles: Vec<Album>,
    pub compilations: Vec<Album>,
    pub appears_on: Vec<Album>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: Id,
    pub name: Box<str>,
    pub owner: Box<str>,
    pub description: Box<str>,
    pub length: u32,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistEntry {
    pub track: Id,
    /// Unix seconds, or 0 when unknown.
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistDetails {
    pub playlist: Playlist,
    /// Track entries in playlist order; episodes and local files are omitted.
    pub entries: Vec<PlaylistEntry>,
}

/// An item of the user's library with the time it was saved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Saved {
    pub id: Id,
    /// Unix seconds.
    pub added_at: i64,
}

/// A titled row of the home feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Shelf {
    pub title: Box<str>,
    pub items: Vec<ShelfItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ShelfItem {
    Album(Album),
    Playlist(Playlist),
    Artist(Artist),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trips_through_kind() {
        let id = Id::from_base62("4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let uri = Kind::Track.uri(id);
        assert_eq!(uri, "spotify:track:4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(Kind::Track.id(&uri), Some(id));
        assert_eq!(Kind::Album.id(&uri), None);
        assert_eq!(
            Kind::Track.id("spotify:tracks:4uLU6hMCjMI75M1A2tKUQC"),
            None
        );
    }

    #[test]
    fn image_url_is_built_from_the_file_id() {
        let image = Image::from_file_id(&[0xab, 0x67, 0x00, 0x1e]);
        assert_eq!(image.url(), "https://i.scdn.co/image/ab67001e");
    }
}
