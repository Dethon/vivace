use serde::{Deserialize, Serialize};

use super::{
    Api,
    graph::{
        AlbumData, ArtistData, AudiobookData, Items, Lenient, PlaylistData, ShowData, TrackData,
        Wrapped, album, artist, audiobook, playlist, show, track,
    },
    transport::Query,
};
use crate::{Result, model::SearchResults};

const SEARCH: Query = Query {
    name: "searchDesktop",
    hash: "eef7cc54888d91bdd6802623477873caa3948ae173a0c34fd86827b267e94c03",
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Variables<'a> {
    search_term: &'a str,
    offset: u32,
    limit: u32,
    number_of_top_results: u32,
    include_audiobooks: bool,
    include_artist_has_concerts_field: bool,
    include_pre_releases: bool,
    include_local_concerts_field: bool,
    include_authors: bool,
}

impl Api {
    /// Searches the catalogue, returning at most `limit` results of each kind.
    pub async fn search(&self, query: &str, limit: u32) -> Result<SearchResults> {
        let variables = Variables {
            search_term: query,
            offset: 0,
            limit,
            number_of_top_results: 5,
            include_audiobooks: true,
            include_artist_has_concerts_field: false,
            include_pre_releases: true,
            include_local_concerts_field: false,
            include_authors: false,
        };
        let data: Data = self.0.pathfinder(&SEARCH, &variables).await?;
        Ok(data.search.into())
    }
}

#[derive(Deserialize)]
struct Data {
    #[serde(rename = "searchV2")]
    search: Search,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Search {
    tracks_v2: Items<Wrapped<Lenient<TrackData>>>,
    albums_v2: Items<Wrapped<Lenient<AlbumData>>>,
    artists: Items<Wrapped<Lenient<ArtistData>>>,
    playlists: Items<Wrapped<Lenient<PlaylistData>>>,
    #[serde(default)]
    podcasts: Items<Wrapped<Lenient<ShowData>>>,
    #[serde(default)]
    audiobooks: Items<Wrapped<Lenient<AudiobookData>>>,
}

impl From<Search> for SearchResults {
    fn from(search: Search) -> Self {
        fn collect<D, T>(items: Items<Wrapped<Lenient<D>>>, convert: fn(D) -> Option<T>) -> Vec<T> {
            items
                .items
                .into_iter()
                .filter_map(Wrapped::into_inner)
                .filter_map(convert)
                .collect()
        }

        Self {
            tracks: collect(search.tracks_v2, track),
            artists: collect(search.artists, artist),
            albums: collect(search.albums_v2, album),
            playlists: collect(search.playlists, playlist),
            shows: collect(search.podcasts, show),
            audiobooks: collect(search.audiobooks, audiobook),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AlbumKind;

    const RESPONSE: &str = r#"{"searchV2": {
        "tracksV2": {"items": [
            {"item": {"__typename": "TrackResponseWrapper", "data": {
                "__typename": "Track",
                "uri": "spotify:track:69kOkLUCkxIZYexIgSG8rq",
                "name": "Get Lucky",
                "albumOfTrack": {
                    "uri": "spotify:album:4m2880jivSbbyEGAKfITCa",
                    "name": "Random Access Memories",
                    "coverArt": {"sources": [
                        {"url": "https://i.scdn.co/image/small", "width": 64, "height": 64},
                        {"url": "https://i.scdn.co/image/large", "width": 640, "height": 640},
                        {"url": "https://i.scdn.co/image/medium", "width": 300, "height": 300}
                    ]}
                },
                "artists": {"items": [
                    {"uri": "spotify:artist:4tZwfgrHOc3mvqYlEYSvVi", "profile": {"name": "Daft Punk"}}
                ]},
                "duration": {"totalMilliseconds": 369626},
                "contentRating": {"label": "EXPLICIT"},
                "playability": {"playable": false, "reason": "COUNTRY_RESTRICTED"}
            }}},
            {"item": {"__typename": "TrackResponseWrapper", "data": {"__typename": "NotFound"}}}
        ]},
        "albumsV2": {"items": [
            {"__typename": "AlbumResponseWrapper", "data": {
                "__typename": "Album",
                "uri": "spotify:album:4m2880jivSbbyEGAKfITCa",
                "name": "Random Access Memories",
                "artists": {"items": []},
                "coverArt": null,
                "date": {"year": 2013},
                "type": "SINGLE"
            }},
            {"__typename": "PreReleaseResponseWrapper", "data": {"__typename": "PreRelease", "uri": "spotify:prerelease:1"}}
        ]},
        "artists": {"items": [
            {"data": {"uri": "spotify:artist:4tZwfgrHOc3mvqYlEYSvVi", "profile": {"name": "Daft Punk"},
                      "visuals": {"avatarImage": null}}}
        ]},
        "playlists": {"items": [
            {"data": {"uri": "spotify:playlist:37i9dQZF1DZ06evO2CNgSk", "name": "This Is Daft Punk",
                      "images": {"items": [{"sources": [{"url": "https://i.scdn.co/image/pl", "width": null}]}]},
                      "ownerV2": {"data": {"name": "Spotify"}}}}
        ]},
        "podcasts": {"items": [
            {"data": {"__typename": "Podcast", "uri": "spotify:show:2Fi1hZJoCaFDmuAYxHwDku", "name": "Daft Punk",
                      "publisher": {"name": "Melissa"}, "coverArt": {"sources": [{"url": "https://i.scdn.co/image/s", "width": 300}]}}}
        ]},
        "audiobooks": {"items": [
            {"data": {"__typename": "Audiobook", "uri": "spotify:show:6R1EaCTmDiVmSS9B0TrEd4",
                      "name": "El Principito", "authorsV2": [{"name": "Antoine de Saint-Exupéry"}],
                      "coverArt": {"sources": [{"url": "https://i.scdn.co/image/b", "width": 300}]}}}
        ]},
        "genres": {"items": [{"data": {"name": "ignored"}}]}
    }}"#;

    fn results() -> SearchResults {
        serde_json::from_str::<Data>(RESPONSE)
            .unwrap()
            .search
            .into()
    }

    #[test]
    fn parses_tracks_and_skips_unknown_entries() {
        let results = results();
        assert_eq!(results.tracks.len(), 1);
        let track = &results.tracks[0];
        assert_eq!(&*track.name, "Get Lucky");
        assert_eq!(&*track.artists[0].name, "Daft Punk");
        assert_eq!(&*track.album.name, "Random Access Memories");
        assert_eq!(track.duration_ms, 369_626);
        assert!(track.explicit);
        assert!(!track.playable);
        assert_eq!(
            track.cover.as_ref().unwrap().url(),
            "https://i.scdn.co/image/medium"
        );
    }

    #[test]
    fn parses_albums_artists_and_playlists() {
        let results = results();

        assert_eq!(results.albums.len(), 1);
        assert_eq!(results.albums[0].kind, AlbumKind::Single);
        assert_eq!(results.albums[0].year, 2013);
        assert_eq!(results.albums[0].cover, None);

        assert_eq!(&*results.artists[0].name, "Daft Punk");
        assert_eq!(results.artists[0].image, None);

        assert_eq!(&*results.shows[0].name, "Daft Punk");
        assert_eq!(&*results.shows[0].publisher, "Melissa");
        assert!(!results.shows[0].audiobook);
        assert_eq!(
            &*results.audiobooks[0].publisher,
            "Antoine de Saint-Exupéry"
        );
        assert!(results.audiobooks[0].audiobook);

        assert_eq!(&*results.playlists[0].owner, "Spotify");
        assert_eq!(
            results.playlists[0].image.as_ref().unwrap().url(),
            "https://i.scdn.co/image/pl"
        );
    }
}
