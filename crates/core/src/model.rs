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
    Episode,
    Show,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Track => "track",
            Kind::Album => "album",
            Kind::Artist => "artist",
            Kind::Playlist => "playlist",
            Kind::Episode => "episode",
            Kind::Show => "show",
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

/// Something the player can play: a track or a podcast episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ItemRef {
    pub kind: Kind,
    pub id: Id,
}

impl ItemRef {
    pub fn track(id: Id) -> Self {
        Self {
            kind: Kind::Track,
            id,
        }
    }

    pub fn episode(id: Id) -> Self {
        Self {
            kind: Kind::Episode,
            id,
        }
    }

    pub fn uri(self) -> String {
        self.kind.uri(self.id)
    }

    /// Reads a track or episode URI; anything else is not playable.
    pub fn from_uri(uri: &str) -> Option<Self> {
        Kind::Track
            .id(uri)
            .map(Self::track)
            .or_else(|| Kind::Episode.id(uri).map(Self::episode))
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

/// A track, or a podcast episode in a track's shape: its show stands in
/// for both the artist and the album.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// [`Kind::Track`] or [`Kind::Episode`].
    pub kind: Kind,
    pub id: Id,
    pub name: Box<str>,
    pub artists: Box<[ArtistRef]>,
    pub album: AlbumRef,
    pub duration_ms: u32,
    pub explicit: bool,
    pub playable: bool,
    pub cover: Option<Image>,
}

impl Track {
    pub fn item(&self) -> ItemRef {
        ItemRef {
            kind: self.kind,
            id: self.id,
        }
    }

    pub fn is_episode(&self) -> bool {
        self.kind == Kind::Episode
    }
}

/// A podcast episode with what a show's episode list adds to a track.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub track: Track,
    /// Unix seconds, or 0 when unknown.
    pub published_at: i64,
}

/// How far the user has listened to an episode, kept by Spotify across devices.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    NotStarted,
    InProgress {
        position_ms: u32,
    },
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Show {
    pub id: Id,
    pub name: Box<str>,
    pub publisher: Box<str>,
    pub image: Option<Image>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShowDetails {
    pub show: Show,
    /// The most recent episodes, newest first; for a series, the first
    /// episodes, oldest first.
    pub episodes: Vec<Episode>,
    /// A series, meant to be heard in order, like Spotify's playback context
    /// for a show, which always runs oldest first.
    pub sequential: bool,
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
    /// Anyone it is shared with can add and remove tracks.
    pub collaborative: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistEntry {
    pub item: ItemRef,
    /// Unix seconds, or 0 when unknown.
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistDetails {
    pub playlist: Playlist,
    /// Tracks and episodes in playlist order; local files are omitted.
    pub entries: Vec<PlaylistEntry>,
}

/// The user's playlists and the folders they sort them into.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rootlist {
    /// Every playlist in the user's order, as if there were no folders.
    pub playlists: Vec<Playlist>,
    pub folders: Vec<Folder>,
    /// Each playlist and folder in the user's order, with the folder it is in.
    pub layout: Vec<Placed>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: Box<str>,
    pub name: Box<str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub item: Placement,
    /// Index into [`Rootlist::folders`] of the folder holding it, if any.
    pub folder: Option<usize>,
    /// When it was added, in Unix seconds; 0 when unknown.
    pub added_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Index into [`Rootlist::playlists`].
    Playlist(usize),
    /// Index into [`Rootlist::folders`].
    Folder(usize),
}

/// An item of the user's library with the time it was saved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Saved {
    pub id: Id,
    /// Unix seconds.
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    /// Whether lines carry the time they are sung at.
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    pub provider: Box<str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    /// When the line starts; 0 throughout for unsynced lyrics.
    pub start_ms: u32,
    pub text: Box<str>,
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
    pub shows: Vec<Show>,
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
    fn playable_items_are_read_from_track_and_episode_uris() {
        let id = Id::from_base62("4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let episode = ItemRef::from_uri("spotify:episode:4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(episode, Some(ItemRef::episode(id)));
        assert_eq!(
            episode.unwrap().uri(),
            "spotify:episode:4uLU6hMCjMI75M1A2tKUQC"
        );
        assert_eq!(
            ItemRef::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC"),
            Some(ItemRef::track(id))
        );
        assert_eq!(
            ItemRef::from_uri("spotify:album:4uLU6hMCjMI75M1A2tKUQC"),
            None
        );
    }

    #[test]
    fn image_url_is_built_from_the_file_id() {
        let image = Image::from_file_id(&[0xab, 0x67, 0x00, 0x1e]);
        assert_eq!(image.url(), "https://i.scdn.co/image/ab67001e");
    }
}
