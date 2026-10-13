//! What a page shows, as plain data built off the UI thread.

use vivace_core::{
    Api, Result,
    model::{Album, AlbumKind, Artist, Id, Image, Kind, Playlist, ShelfItem, Track},
};

use crate::format;

// Tracks listed on a search page and as an artist's most popular.
const SEARCH_TRACKS: usize = 6;
const POPULAR_TRACKS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Page {
    Home,
    Liked,
    Playlist(Id),
    Album(Id),
    Artist(Id),
    Search(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageData {
    pub header: Option<Header>,
    /// Spotify context the page's tracks play in; without one they play as a plain list.
    pub context: Option<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub kind: &'static str,
    pub title: String,
    pub subtitle: String,
    pub image: Option<Image>,
    pub round: bool,
    /// Shows the Liked Songs emblem instead of artwork.
    pub liked_songs: bool,
    /// What the header's save or follow button acts on, if it has one.
    pub save: Option<Saveable>,
}

/// Something that can be kept in the user's library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saveable {
    Track(Id),
    Album(Id),
    Artist(Id),
}

impl Saveable {
    pub fn id(self) -> Id {
        let (Saveable::Track(id) | Saveable::Album(id) | Saveable::Artist(id)) = self;
        id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Section {
    Tracks(TrackSection),
    Cards(CardSection),
    Message(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackSection {
    pub title: Option<&'static str>,
    pub tracks: Vec<TrackEntry>,
    /// Whether to show when each track was added.
    pub dated: bool,
    /// All tracks are from the album the page is about.
    pub album_view: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackEntry {
    pub track: Track,
    /// Unix seconds, or 0 when unknown.
    pub added_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardSection {
    pub title: String,
    pub cards: Vec<Card>,
    /// Whether to show every card over several rows rather than one row.
    pub wrap: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub image: Option<Image>,
    pub round: bool,
    /// Shows the Liked Songs emblem instead of artwork.
    pub liked_songs: bool,
    pub target: Page,
}

impl Page {
    /// Fetches everything the page shows. `liked_uri` is the user's Liked Songs context.
    pub async fn load(&self, api: &Api, liked_uri: &str) -> Result<PageData> {
        match self {
            Page::Home => home(api).await,
            Page::Liked => liked(api, liked_uri).await,
            Page::Playlist(id) => playlist(api, *id).await,
            Page::Album(id) => album(api, *id).await,
            Page::Artist(id) => artist(api, *id).await,
            Page::Search(query) => search(api, query).await,
        }
    }
}

async fn home(api: &Api) -> Result<PageData> {
    let (feed, saved, artists) = tokio::join!(api.home(), api.saved(), api.followed_artists());
    // The feed is a private interface; the library still makes a useful page without it.
    let feed = feed.unwrap_or_else(|error| {
        log::warn!("couldn't load the home feed: {error}");
        Vec::new()
    });
    let (saved, artists) = (saved?, artists?);
    let album_ids: Vec<Id> = saved.albums.iter().map(|saved| saved.id).collect();
    let artist_ids: Vec<Id> = artists.iter().map(|saved| saved.id).collect();
    let (albums, artists) = tokio::try_join!(api.albums(&album_ids), api.artists(&artist_ids))?;

    let mut sections = Vec::new();
    for shelf in &feed {
        let cards = shelf.items.iter().map(|item| match item {
            ShelfItem::Album(album) => album_card(album),
            ShelfItem::Playlist(playlist) => playlist_card(playlist),
            ShelfItem::Artist(artist) => artist_card(artist),
        });
        push_cards(&mut sections, &shelf.title, false, cards);
    }
    let albums = albums.iter().flatten().map(album_card);
    push_cards(&mut sections, "Your albums", false, albums);
    let artists = artists.iter().flatten().map(artist_card);
    push_cards(&mut sections, "Your artists", false, artists);
    if sections.is_empty() {
        sections.push(Section::Message("Nothing to show yet".into()));
    }

    Ok(PageData {
        header: None,
        context: None,
        sections,
    })
}

async fn liked(api: &Api, liked_uri: &str) -> Result<PageData> {
    let saved = api.saved().await?.tracks;
    let ids: Vec<Id> = saved.iter().map(|saved| saved.id).collect();
    let tracks: Vec<TrackEntry> = api
        .tracks(&ids)
        .await?
        .into_iter()
        .zip(&saved)
        .filter_map(|(track, saved)| {
            Some(TrackEntry {
                track: track?,
                added_at: saved.added_at,
            })
        })
        .collect();

    Ok(PageData {
        header: Some(Header {
            kind: "Playlist",
            title: "Liked Songs".into(),
            subtitle: format::count(tracks.len(), "song"),
            image: None,
            round: false,
            liked_songs: true,
            save: None,
        }),
        context: Some(liked_uri.into()),
        sections: vec![track_list(
            tracks,
            true,
            false,
            "Songs you like will appear here",
        )],
    })
}

async fn playlist(api: &Api, id: Id) -> Result<PageData> {
    let details = api.playlist(id).await?;
    let ids: Vec<Id> = details.entries.iter().map(|entry| entry.track).collect();
    let tracks: Vec<TrackEntry> = api
        .tracks(&ids)
        .await?
        .into_iter()
        .zip(&details.entries)
        .filter_map(|(track, entry)| {
            Some(TrackEntry {
                track: track?,
                added_at: entry.added_at,
            })
        })
        .collect();

    let playlist = details.playlist;
    // Playlists without their own picture borrow the first track's cover.
    let image = playlist
        .image
        .or_else(|| tracks.first().and_then(|entry| entry.track.cover.clone()));
    Ok(PageData {
        header: Some(Header {
            kind: "Playlist",
            title: playlist.name.into(),
            subtitle: format!(
                "{} · {}",
                playlist.owner,
                format::count(tracks.len(), "song")
            ),
            image,
            round: false,
            liked_songs: false,
            save: None,
        }),
        context: Some(Kind::Playlist.uri(id)),
        sections: vec![track_list(tracks, true, false, "This playlist is empty")],
    })
}

async fn album(api: &Api, id: Id) -> Result<PageData> {
    let details = api.album(id).await?;
    let album = details.album;
    let tracks: Vec<TrackEntry> = details.tracks.into_iter().map(undated).collect();

    let subtitle = [
        artist_names(&album),
        if album.year > 0 {
            album.year.to_string()
        } else {
            String::new()
        },
        format::count(tracks.len(), "song"),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    Ok(PageData {
        header: Some(Header {
            kind: album_kind(album.kind),
            title: album.name.into(),
            subtitle,
            image: album.cover,
            round: false,
            liked_songs: false,
            save: Some(Saveable::Album(id)),
        }),
        context: Some(Kind::Album.uri(id)),
        sections: vec![track_list(
            tracks,
            false,
            true,
            "This release has no tracks",
        )],
    })
}

async fn artist(api: &Api, id: Id) -> Result<PageData> {
    let details = api.artist(id).await?;

    let mut sections = Vec::new();
    if !details.top_tracks.is_empty() {
        sections.push(Section::Tracks(TrackSection {
            title: Some("Popular"),
            tracks: details
                .top_tracks
                .into_iter()
                .take(POPULAR_TRACKS)
                .map(undated)
                .collect(),
            dated: false,
            album_view: false,
        }));
    }
    push_cards(
        &mut sections,
        "Albums",
        true,
        details.albums.iter().map(release_card),
    );
    push_cards(
        &mut sections,
        "Singles and EPs",
        true,
        details.singles.iter().map(release_card),
    );
    push_cards(
        &mut sections,
        "Compilations",
        true,
        details.compilations.iter().map(release_card),
    );
    push_cards(
        &mut sections,
        "Appears on",
        true,
        details.appears_on.iter().map(release_card),
    );

    Ok(PageData {
        header: Some(Header {
            kind: "Artist",
            title: details.artist.name.into(),
            subtitle: String::new(),
            image: details.artist.image,
            round: true,
            liked_songs: false,
            save: Some(Saveable::Artist(id)),
        }),
        context: Some(Kind::Artist.uri(id)),
        sections,
    })
}

async fn search(api: &Api, query: &str) -> Result<PageData> {
    let results = api.search(query).await?;

    let mut sections = Vec::new();
    if !results.tracks.is_empty() {
        sections.push(Section::Tracks(TrackSection {
            title: Some("Songs"),
            tracks: results
                .tracks
                .into_iter()
                .take(SEARCH_TRACKS)
                .map(undated)
                .collect(),
            dated: false,
            album_view: false,
        }));
    }
    push_cards(
        &mut sections,
        "Artists",
        false,
        results.artists.iter().map(artist_card),
    );
    push_cards(
        &mut sections,
        "Albums",
        false,
        results.albums.iter().map(album_card),
    );
    push_cards(
        &mut sections,
        "Playlists",
        false,
        results.playlists.iter().map(playlist_card),
    );
    if sections.is_empty() {
        sections.push(Section::Message(format!("No results for “{query}”")));
    }

    Ok(PageData {
        header: None,
        context: None,
        sections,
    })
}

fn track_list(tracks: Vec<TrackEntry>, dated: bool, album_view: bool, empty: &str) -> Section {
    if tracks.is_empty() {
        Section::Message(empty.into())
    } else {
        Section::Tracks(TrackSection {
            title: None,
            tracks,
            dated,
            album_view,
        })
    }
}

fn undated(track: Track) -> TrackEntry {
    TrackEntry { track, added_at: 0 }
}

fn push_cards(
    sections: &mut Vec<Section>,
    title: &str,
    wrap: bool,
    cards: impl Iterator<Item = Card>,
) {
    let cards: Vec<Card> = cards.collect();
    if !cards.is_empty() {
        sections.push(Section::Cards(CardSection {
            title: title.into(),
            cards,
            wrap,
        }));
    }
}

fn album_kind(kind: AlbumKind) -> &'static str {
    match kind {
        AlbumKind::Album => "Album",
        AlbumKind::Single => "Single",
        AlbumKind::Compilation => "Compilation",
        AlbumKind::Ep => "EP",
    }
}

fn artist_names(album: &Album) -> String {
    let names: Vec<&str> = album.artists.iter().map(|artist| &*artist.name).collect();
    names.join(", ")
}

/// Drops the HTML tags Spotify puts in playlist descriptions.
fn plain_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for character in html.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&quot;", "\"")
}

fn playlist_card(playlist: &Playlist) -> Card {
    Card {
        title: playlist.name.to_string(),
        subtitle: if playlist.description.is_empty() {
            format!("By {}", playlist.owner)
        } else {
            plain_text(&playlist.description)
        },
        image: playlist.image.clone(),
        round: false,
        liked_songs: false,
        target: Page::Playlist(playlist.id),
    }
}

/// Card for an album among other kinds of results: names its artists.
fn album_card(album: &Album) -> Card {
    Card {
        title: album.name.to_string(),
        subtitle: artist_names(album),
        image: album.cover.clone(),
        round: false,
        liked_songs: false,
        target: Page::Album(album.id),
    }
}

/// Card for a release on its artist's page: shows year and kind instead.
fn release_card(album: &Album) -> Card {
    let kind = album_kind(album.kind);
    Card {
        subtitle: if album.year > 0 {
            format!("{} · {kind}", album.year)
        } else {
            kind.into()
        },
        ..album_card(album)
    }
}

fn artist_card(artist: &Artist) -> Card {
    Card {
        title: artist.name.to_string(),
        subtitle: "Artist".into(),
        image: artist.image.clone(),
        round: true,
        liked_songs: false,
        target: Page::Artist(artist.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_lose_their_markup() {
        assert_eq!(
            plain_text("<a href=spotify:playlist:1>Nakama Records</a>, Nozi &amp; more"),
            "Nakama Records, Nozi & more"
        );
        assert_eq!(plain_text("plain"), "plain");
    }
}
