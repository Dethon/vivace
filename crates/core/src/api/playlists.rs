//! Changing playlists and the list of playlists in the library.

use http::Method;
use librespot_protocol::playlist4_external::{
    Add, CreateListReply, Delta, Item, ListAttributes, ListAttributesPartialState, ListChanges,
    ListUpdateRequest, Op, Rem, UpdateListAttributes, op::Kind as OpKind,
};
use protobuf::Message;

use super::{Api, transport::decode};
use crate::{
    Error, Result,
    model::{Id, Kind},
};

const PROTOBUF: &str = "application/x-protobuf";

impl Api {
    /// Creates an empty playlist at the top of the user's library.
    pub async fn create_playlist(&self, name: &str) -> Result<Id> {
        let mut attributes = ListAttributes::new();
        attributes.set_name(name.into());
        let request = ListUpdateRequest {
            attributes: Some(attributes).into(),
            ..ListUpdateRequest::default()
        };
        let body = request.write_to_bytes().map_err(decode)?;
        let response = self
            .0
            .spclient(
                Method::POST,
                "/playlist/v2/playlist",
                Some(PROTOBUF),
                body.into(),
            )
            .await?;
        let reply = CreateListReply::parse_from_bytes(&response).map_err(decode)?;
        let playlist = Kind::Playlist
            .id(reply.uri())
            .ok_or_else(|| Error::Decode(format!("unexpected playlist uri {}", reply.uri())))?;

        // Creation needs the attributes but does not apply them; the name is
        // set like any later rename.
        self.change(&playlist_changes(playlist), renamed(name))
            .await?;

        // A new playlist exists on its own until it is added to the library.
        let mut add = Add::new();
        add.items.push(item(Kind::Playlist.uri(playlist)));
        add.set_add_first(true);
        self.change(&self.rootlist(), added(add)).await?;
        Ok(playlist)
    }

    /// Appends tracks to a playlist the user can edit.
    pub async fn add_to_playlist(&self, playlist: Id, tracks: &[Id]) -> Result<()> {
        let mut add = Add::new();
        add.items = tracks
            .iter()
            .map(|&track| item(Kind::Track.uri(track)))
            .collect();
        add.set_add_last(true);
        self.change(&playlist_changes(playlist), added(add)).await
    }

    /// Removes every occurrence of the given tracks from a playlist.
    pub async fn remove_from_playlist(&self, playlist: Id, tracks: &[Id]) -> Result<()> {
        let uris = tracks.iter().map(|&track| Kind::Track.uri(track));
        self.change(&playlist_changes(playlist), removed(uris))
            .await
    }

    /// Removes a playlist from the user's library. For a playlist they own
    /// this is what Spotify calls deleting it.
    pub async fn remove_playlist(&self, playlist: Id) -> Result<()> {
        let uris = std::iter::once(Kind::Playlist.uri(playlist));
        self.change(&self.rootlist(), removed(uris)).await
    }

    fn rootlist(&self) -> String {
        format!(
            "/playlist/v2/user/{}/rootlist/changes",
            self.0.session().username()
        )
    }

    async fn change(&self, endpoint: &str, op: Op) -> Result<()> {
        let changes = ListChanges {
            deltas: vec![Delta {
                ops: vec![op],
                ..Delta::default()
            }],
            ..ListChanges::default()
        };
        let body = changes.write_to_bytes().map_err(decode)?;
        self.0
            .spclient(Method::POST, endpoint, Some(PROTOBUF), body.into())
            .await?;
        Ok(())
    }
}

fn playlist_changes(playlist: Id) -> String {
    format!("/playlist/v2/playlist/{playlist}/changes")
}

fn item(uri: String) -> Item {
    let mut item = Item::new();
    item.set_uri(uri);
    item
}

fn added(add: Add) -> Op {
    let mut op = Op::new();
    op.set_kind(OpKind::ADD);
    op.add = Some(add).into();
    op
}

fn renamed(name: &str) -> Op {
    let mut attributes = ListAttributes::new();
    attributes.set_name(name.into());
    let mut state = ListAttributesPartialState::new();
    state.values = Some(attributes).into();
    let mut update = UpdateListAttributes::new();
    update.new_attributes = Some(state).into();
    let mut op = Op::new();
    op.set_kind(OpKind::UPDATE_LIST_ATTRIBUTES);
    op.update_list_attributes = Some(update).into();
    op
}

/// An operation removing the entries with these URIs, wherever they are.
fn removed(uris: impl Iterator<Item = String>) -> Op {
    let mut rem = Rem::new();
    rem.items = uris.map(item).collect();
    rem.set_items_as_key(true);
    let mut op = Op::new();
    op.set_kind(OpKind::REM);
    op.rem = Some(rem).into();
    op
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_addresses_entries_by_uri() {
        let op = removed(["spotify:track:a".to_owned(), "spotify:track:b".to_owned()].into_iter());
        assert_eq!(op.kind(), OpKind::REM);
        assert!(op.rem.items_as_key());
        let uris: Vec<&str> = op.rem.items.iter().map(|item| item.uri()).collect();
        assert_eq!(uris, ["spotify:track:a", "spotify:track:b"]);
    }

    #[test]
    fn additions_keep_their_placement() {
        let mut add = Add::new();
        add.items.push(item("spotify:track:a".into()));
        add.set_add_last(true);
        let op = added(add);
        assert_eq!(op.kind(), OpKind::ADD);
        assert!(op.add.add_last());
        assert_eq!(op.add.items[0].uri(), "spotify:track:a");
    }
}
