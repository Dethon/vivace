//! What a page shows, as plain data built off the UI thread.

use vivace_core::{
    Api, Result,
    model::{Album, AlbumKind, Artist, Id, Image, ItemRef, Kind, Playlist, ShelfItem, Show, Track},
};

use crate::format;

// Tracks listed on a search page and as an artist's most popular.
const SEARCH_TRACKS: usize = 6;
const POPULAR_TRACKS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Page {
    Home,
    Liked,
    YourEpisodes,
    Playlist(Id),
    Album(Id),
    Artist(Id),
    Search(String),
    Show(Id),
    /// The settings screen, which has no content to fetch.
    Settings,
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
    /// Shows a collection's emblem instead of artwork.
    pub emblem: Option<Emblem>,
    /// What the header's save or follow button acts on, if it has one.
    pub save: Option<Saveable>,
}

/// The user's own collections, drawn as an emblem rather than artwork.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emblem {
    LikedSongs,
    YourEpisodes,
}

/// Something that can be kept in the user's library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saveable {
    Track(Id),
    /// A podcast episode, kept in Your Episodes.
    Episode(Id),
    Album(Id),
    Artist(Id),
    Show(Id),
}

impl Saveable {
    pub fn id(self) -> Id {
        let (Saveable::Track(id)
        | Saveable::Episode(id)
        | Saveable::Album(id)
        | Saveable::Artist(id)
        | Saveable::Show(id)) = self;
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
    /// Which date, if any, each row shows.
    pub dates: Dates,
    /// All tracks are from the album the page is about.
    pub album_view: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dates {
    None,
    /// When the track was added to the playlist or library.
    Added,
    /// When the episode was published.
    Released,
}

impl Dates {
    /// The column heading, empty for no date column.
    pub fn label(self) -> &'static str {
        match self {
            Dates::None => "",
            Dates::Added => "Date added",
            Dates::Released => "Released",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackEntry {
    pub track: Track,
    /// The date shown for the row as Unix seconds, or 0 when unknown.
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
    pub target: Page,
}

impl Page {
    /// Fetches everything the page shows. `collection_uri` is the user's
    /// collection, the context Liked Songs play in.
    pub async fn load(&self, api: &Api, collection_uri: &str) -> Result<PageData> {
        match self {
            Page::Home => home(api).await,
            Page::Liked => liked(api, collection_uri).await,
            Page::YourEpisodes => your_episodes(api, collection_uri).await,
            Page::Playlist(id) => playlist(api, *id).await,
            Page::Album(id) => album(api, *id).await,
            Page::Artist(id) => artist(api, *id).await,
            Page::Search(query) => search(api, query).await,
            Page::Show(id) => show(api, *id).await,
            Page::Settings => Ok(PageData {
                header: None,
                context: None,
                sections: Vec::new(),
            }),
        }
    }
}

async fn home(api: &Api) -> Result<PageData> {
    let (feed, saved, artists, shows) = tokio::join!(
        api.home(),
        api.saved(),
        api.followed_artists(),
        api.saved_shows()
    );
    // The feed is a private interface; the library still makes a useful page without it.
    let feed = feed.unwrap_or_else(|error| {
        log::warn!("couldn't load the home feed: {error}");
        Vec::new()
    });
    let (saved, artists, shows) = (saved?, artists?, shows?);
    let show_ids: Vec<Id> = shows.iter().map(|saved| saved.id).collect();
    let album_ids: Vec<Id> = saved.albums.iter().map(|saved| saved.id).collect();
    let artist_ids: Vec<Id> = artists.iter().map(|saved| saved.id).collect();
    let (albums, artists, shows) = tokio::try_join!(
        api.albums(&album_ids),
        api.artists(&artist_ids),
        api.shows(&show_ids)
    )?;

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
    let shows = shows.iter().flatten().map(show_card);
    push_cards(&mut sections, "Your podcasts", false, shows);
    if sections.is_empty() {
        sections.push(Section::Message("Nothing to show yet".into()));
    }

    Ok(PageData {
        header: None,
        context: None,
        sections,
    })
}

async fn liked(api: &Api, collection_uri: &str) -> Result<PageData> {
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
            emblem: Some(Emblem::LikedSongs),
            save: None,
        }),
        context: Some(collection_uri.into()),
        sections: vec![track_list(
            tracks,
            Dates::Added,
            false,
            "Songs you like will appear here",
        )],
    })
}

async fn your_episodes(api: &Api, collection_uri: &str) -> Result<PageData> {
    let saved = api.saved_episodes().await?;
    let items: Vec<ItemRef> = saved
        .iter()
        .map(|saved| ItemRef::episode(saved.id))
        .collect();
    // Episodes taken down since they were saved come back empty and are left out.
    let episodes: Vec<TrackEntry> = api
        .items(&items)
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
            title: "Your Episodes".into(),
            subtitle: format::count(episodes.len(), "episode"),
            image: None,
            round: false,
            emblem: Some(Emblem::YourEpisodes),
            save: None,
        }),
        context: Some(format!("{collection_uri}:your-episodes")),
        sections: vec![track_list(
            episodes,
            Dates::Added,
            false,
            "Episodes you save will appear here",
        )],
    })
}

async fn playlist(api: &Api, id: Id) -> Result<PageData> {
    let details = api.playlist(id).await?;
    let items: Vec<ItemRef> = details.entries.iter().map(|entry| entry.item).collect();
    let tracks: Vec<TrackEntry> = api
        .items(&items)
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
            emblem: None,
            save: None,
        }),
        context: Some(Kind::Playlist.uri(id)),
        sections: vec![track_list(
            tracks,
            Dates::Added,
            false,
            "This playlist is empty",
        )],
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
            emblem: None,
            save: Some(Saveable::Album(id)),
        }),
        context: Some(Kind::Album.uri(id)),
        sections: vec![track_list(
            tracks,
            Dates::None,
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
            dates: Dates::None,
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
            emblem: None,
            save: Some(Saveable::Artist(id)),
        }),
        context: Some(Kind::Artist.uri(id)),
        sections,
    })
}

async fn show(api: &Api, id: Id) -> Result<PageData> {
    let details = api.show(id).await?;
    let episodes: Vec<TrackEntry> = details
        .episodes
        .into_iter()
        .map(|episode| TrackEntry {
            track: episode.track,
            added_at: episode.published_at,
        })
        .collect();

    Ok(PageData {
        header: Some(Header {
            kind: "Podcast",
            title: details.show.name.into(),
            subtitle: details.show.publisher.into(),
            image: details.show.image,
            round: false,
            emblem: None,
            save: Some(Saveable::Show(id)),
        }),
        context: Some(Kind::Show.uri(id)),
        sections: vec![track_list(
            episodes,
            Dates::Released,
            true,
            "This podcast has no episodes",
        )],
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
            dates: Dates::None,
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
    push_cards(
        &mut sections,
        "Podcasts",
        false,
        results.shows.iter().map(show_card),
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

fn track_list(tracks: Vec<TrackEntry>, dates: Dates, album_view: bool, empty: &str) -> Section {
    if tracks.is_empty() {
        Section::Message(empty.into())
    } else {
        Section::Tracks(TrackSection {
            title: None,
            tracks,
            dates,
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

fn show_card(show: &Show) -> Card {
    Card {
        title: show.name.to_string(),
        subtitle: show.publisher.to_string(),
        image: show.image.clone(),
        round: false,
        target: Page::Show(show.id),
    }
}

fn artist_card(artist: &Artist) -> Card {
    Card {
        title: artist.name.to_string(),
        subtitle: "Artist".into(),
        image: artist.image.clone(),
        round: true,
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
