use bytes::Bytes;
use http::Method;
use librespot_protocol::{
    extended_metadata::{
        BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
    },
    extension_kind::ExtensionKind,
    playlist4_external::SelectedListContent,
};
use prost::Message as _;
use protobuf::{EnumOrUnknown, Message};

use super::{Api, transport::decode};
use crate::{
    Error, Result,
    model::{Chapter, Id, Kind},
};

const PROTOBUF: &str = "application/x-protobuf";
const START: &str = "chapter.start_position_in_milliseconds";

// `spotify.contentagnostic.v2.IdentityTrait`, which librespot-protocol does
// not compile; only the field read here.
#[derive(Clone, PartialEq, prost::Message)]
struct IdentityTrait {
    #[prost(string, tag = "2")]
    title: String,
}

impl Api {
    /// The chapters an episode is divided into, in order; empty when it has
    /// none.
    ///
    /// Spotify's desktop client reads the list the same way. Pathfinder's
    /// `queryNpvEpisodeChapters` and the episode's `DISPLAY_SEGMENTS`
    /// metadata carry the same chapters, but answered empty for about half
    /// of the requests when tried on 2026-10-14.
    pub async fn chapters(&self, episode: Id) -> Result<Vec<Chapter>> {
        let endpoint = format!(
            "/playlist/v2/list/podcast-chapters/{}",
            Kind::Episode.uri(episode)
        );
        let list = match self
            .0
            .spclient(Method::GET, &endpoint, None, Bytes::new())
            .await
        {
            Ok(response) => SelectedListContent::parse_from_bytes(&response).map_err(decode)?,
            Err(Error::NotFound) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut chapters = starts(&list);
        if chapters.is_empty() {
            return Ok(Vec::new());
        }

        // Titles are a nicety: the marks are still worth showing without them.
        match self.chapter_titles(&chapters).await {
            Ok(titles) => {
                for ((_, chapter), title) in chapters.iter_mut().zip(titles) {
                    chapter.title = title.into();
                }
            }
            Err(error) => log::warn!("couldn't load chapter titles: {error}"),
        }
        Ok(chapters.into_iter().map(|(_, chapter)| chapter).collect())
    }

    async fn chapter_titles(&self, chapters: &[(String, Chapter)]) -> Result<Vec<String>> {
        let request = BatchedEntityRequest {
            entity_request: chapters
                .iter()
                .map(|(uri, _)| EntityRequest {
                    entity_uri: uri.clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(ExtensionKind::IDENTITY_TRAIT),
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

        let mut titles = vec![String::new(); chapters.len()];
        for data in response
            .extended_metadata
            .iter()
            .flat_map(|array| &array.extension_data)
        {
            let Some(position) = chapters.iter().position(|(uri, _)| *uri == data.entity_uri)
            else {
                continue;
            };
            if let Some(identity) = data
                .extension_data
                .as_ref()
                .and_then(|any| IdentityTrait::decode(any.value.as_slice()).ok())
            {
                titles[position] = identity.title.trim().to_owned();
            }
        }
        Ok(titles)
    }
}

/// Each chapter's URI and start, in order, still without titles.
fn starts(list: &SelectedListContent) -> Vec<(String, Chapter)> {
    let mut chapters: Vec<(String, Chapter)> = list
        .contents
        .items
        .iter()
        .filter_map(|item| {
            let start_ms = item
                .attributes
                .format_attributes
                .iter()
                .find(|attribute| attribute.key() == START)?
                .value()
                .parse()
                .ok()?;
            let chapter = Chapter {
                start_ms,
                title: Box::default(),
            };
            Some((item.uri().to_owned(), chapter))
        })
        .collect();
    chapters.sort_by_key(|(_, chapter)| chapter.start_ms);
    chapters
}

#[cfg(test)]
mod tests {
    use librespot_protocol::playlist4_external::{
        FormatListAttribute, Item, ItemAttributes, ListItems,
    };

    use super::*;

    fn item(uri: &str, start: &str) -> Item {
        let attribute = |key: &str, value: &str| FormatListAttribute {
            key: Some(key.into()),
            value: Some(value.into()),
            ..Default::default()
        };
        Item {
            uri: Some(uri.into()),
            attributes: Some(ItemAttributes {
                format_attributes: vec![
                    attribute(START, start),
                    attribute("chapter.end_position_in_milliseconds", "999999"),
                ],
                ..Default::default()
            })
            .into(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_chapter_starts_in_order() {
        let list = SelectedListContent {
            contents: Some(ListItems {
                items: vec![
                    item("spotify:podcast-chapter:b", "74000"),
                    item("spotify:podcast-chapter:a", "0"),
                    // Without a start it cannot be placed.
                    Item {
                        uri: Some("spotify:podcast-chapter:c".into()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .into(),
            ..Default::default()
        };

        let chapters = starts(&list);
        let starts: Vec<_> = chapters
            .iter()
            .map(|(uri, chapter)| (uri.as_str(), chapter.start_ms))
            .collect();
        assert_eq!(
            starts,
            [
                ("spotify:podcast-chapter:a", 0),
                ("spotify:podcast-chapter:b", 74000)
            ]
        );
    }

    #[test]
    fn decodes_the_identity_title() {
        // Field 1 is the kind of entity, 2 its title, 3 a description.
        let mut bytes = vec![0x0a, 7];
        bytes.extend_from_slice(b"Chapter");
        bytes.extend_from_slice(&[0x12, 12]);
        bytes.extend_from_slice(b"Introduction");
        bytes.extend_from_slice(&[0x1a, 3]);
        bytes.extend_from_slice(b"Lex");

        let identity = IdentityTrait::decode(bytes.as_slice()).unwrap();
        assert_eq!(identity.title, "Introduction");
    }
}
