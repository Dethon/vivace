use serde::{Deserialize, Serialize};

use super::{
    Api,
    graph::{AlbumData, ArtistData, Items, Lenient, PlaylistData, album, artist, playlist},
    transport::Query,
};
use crate::{
    Result,
    model::{Shelf, ShelfItem},
};

const HOME: Query = Query {
    name: "home",
    hash: "76243c78b0e20ecdbe41b794dec8cbe73f75e585b0a7201b8d2e84578412847a",
};
const ITEMS_PER_SHELF: u32 = 10;
// Spotify's own filter for the feed; podcasts and audiobooks are not supported yet.
const MUSIC_FACET: &str = "music-chip";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Variables<'a> {
    home_end_user_integration: &'static str,
    time_zone: &'a str,
    // The one variable Spotify spells in snake case.
    #[serde(rename = "sp_t")]
    sp_t: &'static str,
    facet: &'static str,
    section_items_limit: u32,
}

impl Api {
    /// The personalised home feed: recently played, mixes, recommendations.
    pub async fn home(&self) -> Result<Vec<Shelf>> {
        let time_zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
        let variables = Variables {
            home_end_user_integration: "INTEGRATION_DESKTOP",
            time_zone: &time_zone,
            sp_t: "",
            facet: MUSIC_FACET,
            section_items_limit: ITEMS_PER_SHELF,
        };
        let data: Data = self.0.pathfinder(&HOME, &variables).await?;
        Ok(shelves(data.home))
    }
}

#[derive(Deserialize)]
struct Data {
    home: Home,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Home {
    #[serde(default)]
    greeting: Option<Label>,
    section_container: SectionContainer,
}

#[derive(Deserialize)]
struct SectionContainer {
    sections: Items<Section>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Section {
    #[serde(default)]
    data: Option<SectionInfo>,
    #[serde(default)]
    section_items: Items<SectionItem>,
}

#[derive(Deserialize)]
struct SectionInfo {
    #[serde(default)]
    title: Option<Label>,
}

#[derive(Deserialize)]
struct Label {
    #[serde(rename = "transformedLabel")]
    text: String,
}

#[derive(Deserialize)]
struct SectionItem {
    content: Content,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Content {
    #[serde(rename = "AlbumResponseWrapper")]
    Album { data: Lenient<AlbumData> },
    #[serde(rename = "PlaylistResponseWrapper")]
    Playlist { data: Lenient<PlaylistData> },
    #[serde(rename = "ArtistResponseWrapper")]
    Artist { data: Lenient<ArtistData> },
    // Episodes, shows, audiobooks and whatever Spotify adds next.
    #[serde(other)]
    Unsupported,
}

fn shelves(home: Home) -> Vec<Shelf> {
    let mut greeting = home.greeting.map(|label| label.text);
    home.section_container
        .sections
        .items
        .into_iter()
        .filter_map(|section| {
            let items: Vec<ShelfItem> = section
                .section_items
                .items
                .into_iter()
                .filter_map(shelf_item)
                .collect();
            if items.is_empty() {
                return None;
            }
            // The first shelf is untitled quick picks; it carries the greeting.
            let title = section
                .data
                .and_then(|info| info.title)
                .map(|label| label.text)
                .or_else(|| greeting.take())?;
            Some(Shelf {
                title: title.into(),
                items,
            })
        })
        .collect()
}

fn shelf_item(item: SectionItem) -> Option<ShelfItem> {
    fn known<T>(data: Lenient<T>) -> Option<T> {
        match data {
            Lenient::Known(data) => Some(data),
            Lenient::Other(_) => None,
        }
    }
    match item.content {
        Content::Album { data } => known(data).and_then(album).map(ShelfItem::Album),
        Content::Playlist { data } => known(data).and_then(playlist).map(ShelfItem::Playlist),
        Content::Artist { data } => known(data).and_then(artist).map(ShelfItem::Artist),
        Content::Unsupported => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESPONSE: &str = r#"{"home": {
        "greeting": {"transformedLabel": "Good evening"},
        "sectionContainer": {"sections": {"items": [
            {"data": {"__typename": "HomeShortsSectionData"}, "sectionItems": {"items": [
                {"content": {"__typename": "EpisodeOrChapterResponseWrapper", "data": {"__typename": "Episode"}}},
                {"content": {"__typename": "AlbumResponseWrapper", "data": {
                    "__typename": "Album", "albumType": "SINGLE", "name": "Katana",
                    "uri": "spotify:album:4m2880jivSbbyEGAKfITCa",
                    "artists": {"items": [{"uri": "spotify:artist:4tZwfgrHOc3mvqYlEYSvVi", "profile": {"name": "El Naoya"}}]},
                    "coverArt": {"sources": [{"url": "https://i.scdn.co/image/a", "width": 300, "height": 300}]}}}},
                {"content": {"__typename": "UnknownType"}}
            ]}},
            {"data": {"__typename": "HomeGenericSectionData", "title": {"transformedLabel": "Your shows"}},
             "sectionItems": {"items": [
                {"content": {"__typename": "PodcastOrAudiobookResponseWrapper", "data": {"__typename": "Podcast"}}}
            ]}},
            {"data": {"__typename": "HomeGenericSectionData", "title": {"transformedLabel": "Made For You"}},
             "sectionItems": {"items": [
                {"content": {"__typename": "PlaylistResponseWrapper", "data": {
                    "__typename": "Playlist", "name": "Daily Mix 1", "uri": "spotify:playlist:37i9dQZF1DZ06evO2CNgSk",
                    "images": {"items": [{"sources": [{"url": "https://i.scdn.co/image/p"}]}]},
                    "ownerV2": {"data": {"name": "Spotify"}}}}},
                {"content": {"__typename": "ArtistResponseWrapper", "data": {
                    "__typename": "Artist", "uri": "spotify:artist:4tZwfgrHOc3mvqYlEYSvVi",
                    "profile": {"name": "Daft Punk"}, "visuals": {"avatarImage": null}}}},
                {"content": {"__typename": "PlaylistResponseWrapper", "data": {"__typename": "NotFound"}}}
            ]}}
        ]}}
    }}"#;

    #[test]
    fn keeps_music_shelves_and_titles_the_first_with_the_greeting() {
        let shelves = shelves(serde_json::from_str::<Data>(RESPONSE).unwrap().home);

        assert_eq!(shelves.len(), 2);
        assert_eq!(&*shelves[0].title, "Good evening");
        assert!(
            matches!(&shelves[0].items[..], [ShelfItem::Album(album)] if &*album.name == "Katana")
        );

        assert_eq!(&*shelves[1].title, "Made For You");
        assert!(matches!(
            &shelves[1].items[..],
            [ShelfItem::Playlist(playlist), ShelfItem::Artist(artist)]
                if &*playlist.owner == "Spotify" && &*artist.name == "Daft Punk"
        ));
    }
}
