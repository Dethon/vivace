//! Shapes shared by the responses of Spotify's GraphQL queries.

use serde::{Deserialize, de::IgnoredAny};

use crate::model::{Album, AlbumKind, AlbumRef, Artist, ArtistRef, Image, Kind, Playlist, Track};

const COVER_WIDTH: u32 = 300;

#[derive(Deserialize)]
pub(super) struct Items<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
}

impl<T> Default for Items<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

/// Result entries wrap their payload as `{data}`, tracks as `{item: {data}}`.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Wrapped<T> {
    Item { item: Payload<T> },
    Data(Payload<T>),
}

#[derive(Deserialize)]
pub(super) struct Payload<T> {
    pub data: T,
}

impl<T> Wrapped<Lenient<T>> {
    pub fn into_inner(self) -> Option<T> {
        let (Wrapped::Item { item: payload } | Wrapped::Data(payload)) = self;
        match payload.data {
            Lenient::Known(value) => Some(value),
            Lenient::Other(_) => None,
        }
    }
}

/// Tolerates entries of an unexpected shape, such as pre-releases or
/// removed content, instead of failing the whole response.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Lenient<T> {
    Known(T),
    Other(IgnoredAny),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TrackData {
    pub uri: String,
    pub name: String,
    pub album_of_track: AlbumOfTrack,
    pub artists: Items<ArtistData>,
    pub duration: Duration,
    #[serde(default)]
    pub content_rating: Option<ContentRating>,
    #[serde(default)]
    pub playability: Option<Playability>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AlbumOfTrack {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub cover_art: Option<Sources>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Duration {
    pub total_milliseconds: u32,
}

#[derive(Deserialize)]
pub(super) struct ContentRating {
    pub label: String,
}

#[derive(Deserialize)]
pub(super) struct Playability {
    pub playable: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AlbumData {
    pub uri: String,
    pub name: String,
    pub artists: Items<ArtistData>,
    #[serde(default)]
    pub cover_art: Option<Sources>,
    #[serde(default)]
    pub date: Option<Date>,
    // Search calls it `type`, the home feed `albumType`.
    #[serde(default, rename = "type", alias = "albumType")]
    pub kind: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Date {
    pub year: u16,
}

#[derive(Deserialize)]
pub(super) struct ArtistData {
    pub uri: String,
    pub profile: Profile,
    #[serde(default)]
    pub visuals: Option<Visuals>,
}

#[derive(Deserialize)]
pub(super) struct Profile {
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Visuals {
    #[serde(default)]
    pub avatar_image: Option<Sources>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlaylistData {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub images: Items<Sources>,
    #[serde(default)]
    owner_v2: Option<Payload<Owner>>,
}

#[derive(Deserialize)]
pub(super) struct Owner {
    pub name: String,
}

#[derive(Deserialize)]
pub(super) struct Sources {
    pub sources: Vec<Source>,
}

#[derive(Deserialize)]
pub(super) struct Source {
    pub url: String,
    #[serde(default)]
    pub width: Option<u32>,
}

impl Sources {
    /// Picks the smallest image at least [`COVER_WIDTH`] wide, else the largest.
    pub fn image(&self) -> Option<Image> {
        let width = |source: &&Source| source.width.unwrap_or_default();
        self.sources
            .iter()
            .filter(|source| width(source) >= COVER_WIDTH)
            .min_by_key(width)
            .or_else(|| self.sources.iter().max_by_key(width))
            .map(|source| Image::from_url(&source.url))
    }
}

pub(super) fn artist_refs(artists: Items<ArtistData>) -> Box<[ArtistRef]> {
    artists
        .items
        .into_iter()
        .filter_map(|artist| {
            Some(ArtistRef {
                id: Kind::Artist.id(&artist.uri)?,
                name: artist.profile.name.into(),
            })
        })
        .collect()
}

pub(super) fn track(data: TrackData) -> Option<Track> {
    Some(Track {
        id: Kind::Track.id(&data.uri)?,
        name: data.name.into(),
        artists: artist_refs(data.artists),
        album: AlbumRef {
            id: Kind::Album.id(&data.album_of_track.uri)?,
            name: data.album_of_track.name.into(),
        },
        duration_ms: data.duration.total_milliseconds,
        explicit: data
            .content_rating
            .is_some_and(|rating| rating.label == "EXPLICIT"),
        playable: data
            .playability
            .is_none_or(|playability| playability.playable),
        cover: data
            .album_of_track
            .cover_art
            .as_ref()
            .and_then(Sources::image),
    })
}

pub(super) fn album(data: AlbumData) -> Option<Album> {
    Some(Album {
        id: Kind::Album.id(&data.uri)?,
        name: data.name.into(),
        artists: artist_refs(data.artists),
        kind: match data.kind.as_deref() {
            Some("SINGLE") => AlbumKind::Single,
            Some("COMPILATION") => AlbumKind::Compilation,
            Some("EP") => AlbumKind::Ep,
            _ => AlbumKind::Album,
        },
        year: data.date.map_or(0, |date| date.year),
        cover: data.cover_art.as_ref().and_then(Sources::image),
    })
}

pub(super) fn artist(data: ArtistData) -> Option<Artist> {
    Some(Artist {
        id: Kind::Artist.id(&data.uri)?,
        name: data.profile.name.into(),
        image: data
            .visuals
            .and_then(|visuals| visuals.avatar_image)
            .as_ref()
            .and_then(Sources::image),
    })
}

pub(super) fn playlist(data: PlaylistData) -> Option<Playlist> {
    Some(Playlist {
        id: Kind::Playlist.id(&data.uri)?,
        name: data.name.into(),
        owner: data
            .owner_v2
            .map(|owner| owner.data.name)
            .unwrap_or_default()
            .into(),
        description: data.description.into(),
        length: 0,
        image: data.images.items.first().and_then(Sources::image),
    })
}
