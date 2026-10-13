use std::collections::HashMap;

use bytes::Bytes;
use futures_util::future::try_join_all;
use http::Method;
use librespot_protocol::{
    extended_metadata::{
        BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
    },
    extension_kind::ExtensionKind,
    metadata as proto,
    playlist4_external::{ListAttributes, SelectedListContent},
};
use protobuf::{EnumOrUnknown, Message};

use super::{Api, transport::decode};
use crate::{
    Error, Result,
    model::{
        Album, AlbumDetails, AlbumKind, AlbumRef, Artist, ArtistDetails, ArtistRef, Id, Image,
        Kind, Playlist, PlaylistDetails, PlaylistEntry, Track,
    },
};

const PROTOBUF: &str = "application/x-protobuf";
// Entities per metadata request; larger lists are split and fetched concurrently.
const BATCH: usize = 250;
const PLAYLIST_PAGE: usize = 2000;
// Preferred cover width: sharp for headers and cards, small enough for rows.
const COVER_WIDTH: i32 = 300;
// Releases shown per discography section of an artist page.
const ARTIST_RELEASES: usize = 50;

impl Api {
    /// Resolves tracks in the order given; ids Spotify does not know yield `None`.
    pub async fn tracks(&self, ids: &[Id]) -> Result<Vec<Option<Track>>> {
        let country = self.0.session().country();
        self.metadata(Kind::Track, ExtensionKind::TRACK_V4, ids, |message| {
            track(&country, message)
        })
        .await
    }

    /// Resolves album summaries in the order given.
    pub async fn albums(&self, ids: &[Id]) -> Result<Vec<Option<Album>>> {
        self.metadata(Kind::Album, ExtensionKind::ALBUM_V4, ids, album)
            .await
    }

    /// Resolves artist summaries in the order given.
    pub async fn artists(&self, ids: &[Id]) -> Result<Vec<Option<Artist>>> {
        self.metadata(Kind::Artist, ExtensionKind::ARTIST_V4, ids, artist)
            .await
    }

    pub async fn album(&self, id: Id) -> Result<AlbumDetails> {
        let message = self
            .metadata(
                Kind::Album,
                ExtensionKind::ALBUM_V4,
                &[id],
                |message: &proto::Album| Some(message.clone()),
            )
            .await?
            .pop()
            .flatten()
            .ok_or(Error::NotFound)?;

        let ids: Vec<Id> = message
            .disc
            .iter()
            .flat_map(|disc| &disc.track)
            .filter_map(|track| Id::from_raw(track.gid()).ok())
            .collect();
        let tracks = self.tracks(&ids).await?.into_iter().flatten().collect();

        Ok(AlbumDetails {
            album: album(&message).ok_or(Error::NotFound)?,
            label: message.label().into(),
            tracks,
        })
    }

    pub async fn artist(&self, id: Id) -> Result<ArtistDetails> {
        let message = self
            .metadata(
                Kind::Artist,
                ExtensionKind::ARTIST_V4,
                &[id],
                |message: &proto::Artist| Some(message.clone()),
            )
            .await?
            .pop()
            .flatten()
            .ok_or(Error::NotFound)?;

        let country = self.0.session().country();
        let top_tracks: Vec<Id> = message
            .top_track
            .iter()
            .find(|top| top.country() == country)
            .or(message.top_track.first())
            .map(|top| {
                top.track
                    .iter()
                    .filter_map(|track| Id::from_raw(track.gid()).ok())
                    .collect()
            })
            .unwrap_or_default();

        let (top_tracks, albums, singles, compilations, appears_on) = tokio::try_join!(
            self.tracks(&top_tracks),
            self.releases(&message.album_group),
            self.releases(&message.single_group),
            self.releases(&message.compilation_group),
            self.releases(&message.appears_on_group),
        )?;

        Ok(ArtistDetails {
            artist: artist(&message).ok_or(Error::NotFound)?,
            top_tracks: top_tracks.into_iter().flatten().collect(),
            albums,
            singles,
            compilations,
            appears_on,
        })
    }

    /// Resolves the first release of each group, newest first.
    async fn releases(&self, groups: &[proto::AlbumGroup]) -> Result<Vec<Album>> {
        let ids: Vec<Id> = groups
            .iter()
            .filter_map(|group| Id::from_raw(group.album.first()?.gid()).ok())
            .take(ARTIST_RELEASES)
            .collect();
        let mut albums: Vec<Album> = self.albums(&ids).await?.into_iter().flatten().collect();
        albums.sort_by_key(|album| std::cmp::Reverse(album.year));
        Ok(albums)
    }

    pub async fn playlist(&self, id: Id) -> Result<PlaylistDetails> {
        let mut content = self.playlist_page(id, 0, PLAYLIST_PAGE).await?;
        let length = usize::try_from(content.length()).unwrap_or_default();

        let mut entries = Vec::with_capacity(length);
        let mut position = 0;
        loop {
            let items = &content.contents.items;
            position += items.len();
            entries.extend(items.iter().filter_map(|item| {
                Some(PlaylistEntry {
                    track: Kind::Track.id(item.uri())?,
                    added_at: item.attributes.timestamp() / 1000,
                })
            }));
            if items.is_empty() || !content.contents.truncated() || position >= length {
                break;
            }
            content = self.playlist_page(id, position, PLAYLIST_PAGE).await?;
        }

        Ok(PlaylistDetails {
            playlist: Playlist {
                id,
                name: content.attributes.name().into(),
                owner: content.owner_username().into(),
                description: content.attributes.description().into(),
                length: content.length().try_into().unwrap_or_default(),
                image: playlist_image(&content.attributes),
            },
            entries,
        })
    }

    /// Artwork for a playlist that has no picture of its own: the cover of
    /// its first track, as a stand-in for Spotify's mosaic.
    pub async fn playlist_cover(&self, id: Id) -> Result<Option<Image>> {
        let content = self.playlist_page(id, 0, 1).await?;
        if let Some(image) = playlist_image(&content.attributes) {
            return Ok(Some(image));
        }
        let Some(first) = content
            .contents
            .items
            .iter()
            .find_map(|item| Kind::Track.id(item.uri()))
        else {
            return Ok(None);
        };
        let track = self.tracks(&[first]).await?.pop().flatten();
        Ok(track.and_then(|track| track.cover))
    }

    async fn playlist_page(
        &self,
        id: Id,
        from: usize,
        length: usize,
    ) -> Result<SelectedListContent> {
        let endpoint = format!("/playlist/v2/playlist/{id}?from={from}&length={length}");
        let response = self
            .0
            .spclient(Method::GET, &endpoint, None, Bytes::new())
            .await?;
        SelectedListContent::parse_from_bytes(&response).map_err(decode)
    }

    /// Fetches metadata for `ids` and converts each message, keeping the
    /// input order. Duplicate ids are requested once.
    async fn metadata<M, T>(
        &self,
        kind: Kind,
        extension: ExtensionKind,
        ids: &[Id],
        convert: impl Fn(&M) -> Option<T>,
    ) -> Result<Vec<Option<T>>>
    where
        M: Message,
        T: Clone,
    {
        let chunks = ids
            .chunks(BATCH)
            .map(|chunk| self.metadata_chunk(kind, extension, chunk, &convert));
        Ok(try_join_all(chunks).await?.into_iter().flatten().collect())
    }

    async fn metadata_chunk<M, T>(
        &self,
        kind: Kind,
        extension: ExtensionKind,
        ids: &[Id],
        convert: &impl Fn(&M) -> Option<T>,
    ) -> Result<Vec<Option<T>>>
    where
        M: Message,
        T: Clone,
    {
        let mut positions: HashMap<String, Vec<usize>> = HashMap::with_capacity(ids.len());
        for (position, &id) in ids.iter().enumerate() {
            positions.entry(kind.uri(id)).or_default().push(position);
        }

        let request = BatchedEntityRequest {
            entity_request: positions
                .keys()
                .map(|uri| EntityRequest {
                    entity_uri: uri.clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(extension),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let body = request.write_to_bytes().map_err(decode)?;
        let response = self
            .0
            .spclient(
                Method::POST,
                "/extended-metadata/v0/extended-metadata",
                Some(PROTOBUF),
                body.into(),
            )
            .await?;
        let response = BatchedExtensionResponse::parse_from_bytes(&response).map_err(decode)?;

        let mut items = vec![None; ids.len()];
        for data in response
            .extended_metadata
            .iter()
            .flat_map(|array| &array.extension_data)
        {
            let Some(positions) = positions.get(&data.entity_uri) else {
                continue;
            };
            let item = data
                .extension_data
                .as_ref()
                .and_then(|any| M::parse_from_bytes(&any.value).ok())
                .and_then(|message| convert(&message));
            for &position in positions {
                items[position] = item.clone();
            }
        }
        Ok(items)
    }
}

fn track(country: &str, message: &proto::Track) -> Option<Track> {
    let album = message.album.as_ref()?;
    Some(Track {
        id: Id::from_raw(message.gid()).ok()?,
        name: message.name().into(),
        artists: artist_refs(&message.artist),
        album: AlbumRef {
            id: Id::from_raw(album.gid()).ok()?,
            name: album.name().into(),
        },
        duration_ms: message.duration().try_into().unwrap_or_default(),
        explicit: message.explicit(),
        playable: playable(country, message),
        cover: cover(&album.cover_group.image).or_else(|| cover(&album.cover)),
    })
}

fn album(message: &proto::Album) -> Option<Album> {
    use proto::album::Type;

    Some(Album {
        id: Id::from_raw(message.gid()).ok()?,
        name: message.name().into(),
        artists: artist_refs(&message.artist),
        kind: match message.type_() {
            Type::SINGLE => AlbumKind::Single,
            Type::COMPILATION => AlbumKind::Compilation,
            Type::EP => AlbumKind::Ep,
            _ => AlbumKind::Album,
        },
        year: message.date.year().try_into().unwrap_or_default(),
        cover: cover(&message.cover_group.image).or_else(|| cover(&message.cover)),
    })
}

fn artist(message: &proto::Artist) -> Option<Artist> {
    Some(Artist {
        id: Id::from_raw(message.gid()).ok()?,
        name: message.name().into(),
        image: cover(&message.portrait_group.image).or_else(|| cover(&message.portrait)),
    })
}

fn artist_refs(artists: &[proto::Artist]) -> Box<[ArtistRef]> {
    artists
        .iter()
        .filter_map(|artist| {
            Some(ArtistRef {
                id: Id::from_raw(artist.gid()).ok()?,
                name: artist.name().into(),
            })
        })
        .collect()
}

/// Picks the smallest image at least [`COVER_WIDTH`] wide, else the largest.
fn cover(images: &[proto::Image]) -> Option<Image> {
    images
        .iter()
        .filter(|image| image.width() >= COVER_WIDTH)
        .min_by_key(|image| image.width())
        .or_else(|| images.iter().max_by_key(|image| image.width()))
        .map(|image| Image::from_file_id(image.file_id()))
}

fn playlist_image(attributes: &ListAttributes) -> Option<Image> {
    attributes
        .picture_size
        .iter()
        .find(|size| size.target_name() == "default")
        .or(attributes.picture_size.first())
        .map(|size| Image::from_url(size.url()))
        .or_else(|| {
            attributes
                .has_picture()
                .then(|| Image::from_file_id(attributes.picture()))
        })
}

/// Whether the track, or a relinked alternative, can be streamed in `country`.
fn playable(country: &str, track: &proto::Track) -> bool {
    std::iter::once(track)
        .chain(&track.alternative)
        .any(|candidate| !candidate.file.is_empty() && allowed(country, &candidate.restriction))
}

/// Applies the first premium-catalogue restriction, as Spotify's clients do.
///
/// The session learns its country shortly after connecting; until then
/// nothing is treated as restricted, since the player decides for itself.
fn allowed(country: &str, restrictions: &[proto::Restriction]) -> bool {
    if country.is_empty() {
        return true;
    }
    let contains = |countries: &str| {
        countries
            .as_bytes()
            .chunks(2)
            .any(|code| code == country.as_bytes())
    };
    restrictions
        .iter()
        .filter(|restriction| {
            restriction
                .catalogue_str
                .iter()
                .any(|name| name == "premium")
        })
        .find_map(|restriction| {
            if restriction.has_countries_allowed() {
                Some(contains(restriction.countries_allowed()))
            } else if restriction.has_countries_forbidden() {
                Some(!contains(restriction.countries_forbidden()))
            } else {
                None
            }
        })
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restriction(allowed: Option<&str>, forbidden: Option<&str>) -> proto::Restriction {
        let mut restriction = proto::Restriction::new();
        restriction.catalogue_str.push("premium".into());
        if let Some(countries) = allowed {
            restriction.set_countries_allowed(countries.into());
        }
        if let Some(countries) = forbidden {
            restriction.set_countries_forbidden(countries.into());
        }
        restriction
    }

    fn image(width: i32, id: u8) -> proto::Image {
        let mut image = proto::Image::new();
        image.set_width(width);
        image.set_file_id(vec![id]);
        image
    }

    #[test]
    fn allow_list_admits_only_listed_countries() {
        let restrictions = [restriction(Some("ESFRDE"), None)];
        assert!(allowed("FR", &restrictions));
        assert!(!allowed("US", &restrictions));
    }

    #[test]
    fn deny_list_blocks_only_listed_countries() {
        let restrictions = [restriction(None, Some("ESFR"))];
        assert!(!allowed("ES", &restrictions));
        assert!(allowed("US", &restrictions));
    }

    #[test]
    fn unknown_country_is_not_restricted() {
        assert!(allowed("", &[restriction(Some("FR"), None)]));
        assert!(allowed("", &[restriction(Some(""), None)]));
    }

    #[test]
    fn empty_allow_list_blocks_everywhere() {
        assert!(!allowed("ES", &[restriction(Some(""), None)]));
    }

    #[test]
    fn restrictions_for_other_catalogues_are_ignored() {
        let mut free = restriction(Some(""), None);
        free.catalogue_str = vec!["free".into()];
        assert!(allowed("ES", &[free]));
        assert!(allowed("ES", &[]));
    }

    #[test]
    fn track_is_playable_through_a_relinked_alternative() {
        let mut alternative = proto::Track::new();
        alternative.file.push(proto::AudioFile::new());

        let mut track = proto::Track::new();
        assert!(!playable("ES", &track));
        track.alternative.push(alternative);
        assert!(playable("ES", &track));

        track.alternative[0]
            .restriction
            .push(restriction(None, Some("ES")));
        assert!(!playable("ES", &track));
    }

    #[test]
    fn cover_prefers_the_smallest_sufficient_image() {
        let images = [image(64, 1), image(640, 3), image(300, 2)];
        assert_eq!(cover(&images), Some(Image::from_file_id(&[2])));
        assert_eq!(cover(&images[..1]), Some(Image::from_file_id(&[1])));
        assert_eq!(cover(&[]), None);
    }
}
