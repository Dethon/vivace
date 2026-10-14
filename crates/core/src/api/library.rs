use bytes::Bytes;
use http::Method;
use librespot_protocol::playlist4_external::{ListItems, SelectedListContent};
use prost::Message as _;
use protobuf::Message as _;

use super::{Api, transport::decode};
use crate::{
    Result,
    model::{Folder, Id, Image, Kind, Placed, Placement, Playlist, Rootlist, Saved},
};

const COLLECTION: &str = "application/vnd.collection-v2.spotify.proto";
const ROOTLIST_PAGE: usize = 1000;
// A folder in the rootlist is the span between these markers; the start
// carries its id and URL-encoded name.
const FOLDER_START: &str = "spotify:start-group:";
const FOLDER_END: &str = "spotify:end-group:";
const COLLECTION_PAGE: i32 = 2000;
// Collection sets: liked tracks and saved albums share one, followed artists have their own.
const SET_ITEMS: &str = "collection";
const SET_ARTISTS: &str = "artist";
const SET_SHOWS: &str = "show";
const SET_EPISODES: &str = "listenlater";

// Messages of Spotify's collection service, which librespot does not ship.
#[derive(Clone, PartialEq, prost::Message)]
struct PageRequest {
    #[prost(string, tag = "1")]
    username: String,
    #[prost(string, tag = "2")]
    set: String,
    #[prost(string, tag = "3")]
    pagination_token: String,
    #[prost(int32, tag = "4")]
    limit: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct PageResponse {
    #[prost(message, repeated, tag = "1")]
    items: Vec<CollectionItem>,
    #[prost(string, tag = "2")]
    next_page_token: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct CollectionItem {
    #[prost(string, tag = "1")]
    uri: String,
    #[prost(int32, tag = "2")]
    added_at: i32,
    #[prost(bool, tag = "3")]
    is_removed: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct WriteRequest {
    #[prost(string, tag = "1")]
    username: String,
    #[prost(string, tag = "2")]
    set: String,
    #[prost(message, repeated, tag = "3")]
    items: Vec<CollectionItem>,
}

/// The user's saved tracks and albums, each newest first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavedItems {
    pub tracks: Vec<Saved>,
    pub albums: Vec<Saved>,
}

impl Api {
    /// The user's playlists in their own order, with folders flattened.
    pub async fn playlists(&self) -> Result<Vec<Playlist>> {
        Ok(self.rootlist().await?.playlists)
    }

    /// The user's playlists and folders in their own order.
    pub async fn rootlist(&self) -> Result<Rootlist> {
        let user = self.0.session().username();
        let mut rootlist = Rootlist::default();
        // Folders the next entry is inside, innermost last.
        let mut open: Vec<usize> = Vec::new();
        let mut from = 0;
        loop {
            let endpoint = format!(
                "/playlist/v2/user/{user}/rootlist?decorate=attributes,length,owner&from={from}&length={ROOTLIST_PAGE}"
            );
            let response = self
                .0
                .spclient(Method::GET, &endpoint, None, Bytes::new())
                .await?;
            let list = SelectedListContent::parse_from_bytes(&response).map_err(decode)?;
            let contents = &list.contents;

            read_rootlist(contents, &mut rootlist, &mut open);

            from += contents.items.len();
            if contents.items.is_empty() || !contents.truncated() {
                return Ok(rootlist);
            }
        }
    }

    pub async fn saved(&self) -> Result<SavedItems> {
        let mut saved = SavedItems::default();
        for item in self.collection(SET_ITEMS).await? {
            let entry = |id| Saved {
                id,
                added_at: item.added_at.into(),
            };
            if let Some(id) = Kind::Track.id(&item.uri) {
                saved.tracks.push(entry(id));
            } else if let Some(id) = Kind::Album.id(&item.uri) {
                saved.albums.push(entry(id));
            }
        }
        saved
            .tracks
            .sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
        saved
            .albums
            .sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
        Ok(saved)
    }

    /// Artists the user follows, newest first.
    pub async fn followed_artists(&self) -> Result<Vec<Saved>> {
        let mut artists: Vec<Saved> = self
            .collection(SET_ARTISTS)
            .await?
            .iter()
            .filter_map(|item| {
                Some(Saved {
                    id: Kind::Artist.id(&item.uri)?,
                    added_at: item.added_at.into(),
                })
            })
            .collect();
        artists.sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
        Ok(artists)
    }

    /// Podcasts the user follows, newest first.
    pub async fn saved_shows(&self) -> Result<Vec<Saved>> {
        let mut shows: Vec<Saved> = self
            .collection(SET_SHOWS)
            .await?
            .iter()
            .filter_map(|item| {
                Some(Saved {
                    id: Kind::Show.id(&item.uri)?,
                    added_at: item.added_at.into(),
                })
            })
            .collect();
        shows.sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
        Ok(shows)
    }

    /// Episodes in "Your Episodes", newest first.
    pub async fn saved_episodes(&self) -> Result<Vec<Saved>> {
        let mut episodes: Vec<Saved> = self
            .collection(SET_EPISODES)
            .await?
            .iter()
            .filter_map(|item| {
                Some(Saved {
                    id: Kind::Episode.id(&item.uri)?,
                    added_at: item.added_at.into(),
                })
            })
            .collect();
        episodes.sort_by_key(|saved| std::cmp::Reverse(saved.added_at));
        Ok(episodes)
    }

    /// Adds an episode to "Your Episodes", or removes it.
    pub async fn set_episode_saved(&self, episode: Id, saved: bool) -> Result<()> {
        self.write(SET_EPISODES, Kind::Episode.uri(episode), saved)
            .await
    }

    /// Follows or unfollows a podcast.
    pub async fn set_show_saved(&self, show: Id, saved: bool) -> Result<()> {
        self.write(SET_SHOWS, Kind::Show.uri(show), saved).await
    }

    /// Adds a track or album to the library, or removes it.
    pub async fn set_saved(&self, kind: Kind, id: Id, saved: bool) -> Result<()> {
        self.write(SET_ITEMS, kind.uri(id), saved).await
    }

    /// Follows or unfollows an artist.
    pub async fn set_followed(&self, artist: Id, followed: bool) -> Result<()> {
        self.write(SET_ARTISTS, Kind::Artist.uri(artist), followed)
            .await
    }

    async fn write(&self, set: &str, uri: String, present: bool) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let request = WriteRequest {
            username: self.0.session().username(),
            set: set.into(),
            items: vec![CollectionItem {
                uri,
                added_at: now.try_into().unwrap_or(i32::MAX),
                is_removed: !present,
            }],
        };
        self.0
            .spclient(
                Method::POST,
                "/collection/v2/write",
                Some(COLLECTION),
                request.encode_to_vec().into(),
            )
            .await?;
        Ok(())
    }

    async fn collection(&self, set: &str) -> Result<Vec<CollectionItem>> {
        let mut request = PageRequest {
            username: self.0.session().username(),
            set: set.into(),
            pagination_token: String::new(),
            limit: COLLECTION_PAGE,
        };
        let mut items = Vec::new();
        loop {
            let response = self
                .0
                .spclient(
                    Method::POST,
                    "/collection/v2/paging",
                    Some(COLLECTION),
                    request.encode_to_vec().into(),
                )
                .await?;
            let page = PageResponse::decode(response).map_err(decode)?;
            items.extend(page.items.into_iter().filter(|item| !item.is_removed));
            if page.next_page_token.is_empty() {
                return Ok(items);
            }
            request.pagination_token = page.next_page_token;
        }
    }
}
/// Adds one page of the rootlist to `rootlist`. `open` holds the folders
/// the page starts inside, innermost last, and those it ends inside.
fn read_rootlist(contents: &ListItems, rootlist: &mut Rootlist, open: &mut Vec<usize>) {
    for (position, item) in contents.items.iter().enumerate() {
        let added_at = item.attributes.timestamp() / 1000;
        let folder = open.last().copied();
        let uri = item.uri();
        if let Some((id, name)) = uri
            .strip_prefix(FOLDER_START)
            .and_then(|folder| folder.split_once(':'))
        {
            open.push(rootlist.folders.len());
            rootlist.layout.push(Placed {
                item: Placement::Folder(rootlist.folders.len()),
                folder,
                added_at,
            });
            rootlist.folders.push(Folder {
                id: id.into(),
                name: url_decoded(name).into(),
            });
        } else if uri.starts_with(FOLDER_END) {
            open.pop();
        } else if let Some(id) = Kind::Playlist.id(uri)
            // Every entry has its metadata at the same position, markers too.
            && let Some(meta) = contents.meta_items.get(position)
        {
            rootlist.layout.push(Placed {
                item: Placement::Playlist(rootlist.playlists.len()),
                folder,
                added_at,
            });
            rootlist.playlists.push(Playlist {
                id,
                name: meta.attributes.name().into(),
                owner: meta.owner_username().into(),
                description: meta.attributes.description().into(),
                length: meta.length().try_into().unwrap_or_default(),
                image: meta
                    .attributes
                    .picture_size
                    .first()
                    .map(|size| Image::from_url(size.url())),
                collaborative: meta.attributes.collaborative(),
            });
        }
    }
}

/// Decodes a folder name as the rootlist stores it, `+` for spaces.
fn url_decoded(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        match byte {
            b'+' => bytes.push(b' '),
            b'%' => match rest.get(..2).and_then(|hex| std::str::from_utf8(hex).ok()) {
                Some(hex) if let Ok(value) = u8::from_str_radix(hex, 16) => {
                    bytes.push(value);
                    rest = &rest[2..];
                }
                _ => bytes.push(byte),
            },
            _ => bytes.push(byte),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use librespot_protocol::playlist4_external::{Item, MetaItem};

    use super::*;

    fn entry(uri: &str, name: &str) -> (Item, MetaItem) {
        let mut item = Item::new();
        item.set_uri(uri.into());
        let mut meta = MetaItem::new();
        meta.attributes
            .mut_or_insert_default()
            .set_name(name.into());
        (item, meta)
    }

    #[test]
    fn folders_hold_what_lies_between_their_markers() {
        let entries = [
            entry("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M", "Top"),
            entry("spotify:start-group:aa:Road+trip", ""),
            entry("spotify:playlist:37i9dQZF1DX0XUsuxWHRQd", "Driving"),
            entry("spotify:start-group:bb:Night", ""),
            entry("spotify:playlist:37i9dQZF1DWZd79rJ6a7lp", "Late"),
            entry("spotify:end-group:bb", ""),
            entry("spotify:end-group:aa", ""),
            entry("spotify:playlist:37i9dQZF1DX4sWSpwq3LiO", "Calm"),
        ];
        let mut contents = ListItems::new();
        for (item, meta) in entries {
            contents.items.push(item);
            contents.meta_items.push(meta);
        }
        let mut rootlist = Rootlist::default();
        let mut open = Vec::new();
        read_rootlist(&contents, &mut rootlist, &mut open);

        let names: Vec<&str> = rootlist.playlists.iter().map(|list| &*list.name).collect();
        assert_eq!(names, ["Top", "Driving", "Late", "Calm"]);
        assert_eq!(&*rootlist.folders[0].name, "Road trip");
        let folders: Vec<(Placement, Option<usize>)> = rootlist
            .layout
            .iter()
            .map(|placed| (placed.item, placed.folder))
            .collect();
        assert_eq!(
            folders,
            [
                (Placement::Playlist(0), None),
                (Placement::Folder(0), None),
                (Placement::Playlist(1), Some(0)),
                (Placement::Folder(1), Some(0)),
                (Placement::Playlist(2), Some(1)),
                (Placement::Playlist(3), None),
            ]
        );
        assert!(open.is_empty());
    }

    #[test]
    fn folder_names_are_url_decoded() {
        assert_eq!(url_decoded("Road+trip%21"), "Road trip!");
        assert_eq!(url_decoded("Caf%C3%A9+%26+co"), "Café & co");
        assert_eq!(url_decoded("100%"), "100%");
    }
}
