//! Changing playlists and the list of playlists in the library.

use http::Method;
use librespot_protocol::playlist4_external::{
    Add, CreateListReply, Delta, Item, ListAttributes, ListAttributesPartialState, ListChanges,
    ListUpdateRequest, Op, Rem, SelectedListContent, UpdateListAttributes, op::Kind as OpKind,
};
use protobuf::Message;

use super::{Api, transport::decode};
use crate::{
    Error, Result,
    model::{Id, ItemRef, Kind},
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
        self.change(&self.rootlist_changes(), added(add)).await?;
        Ok(playlist)
    }

    pub async fn rename_playlist(&self, playlist: Id, name: &str) -> Result<()> {
        self.change(&playlist_changes(playlist), renamed(name))
            .await
    }

    /// Appends tracks or episodes to a playlist the user can edit.
    pub async fn add_to_playlist(&self, playlist: Id, items: &[ItemRef]) -> Result<()> {
        let mut add = Add::new();
        add.items = items.iter().map(|entry| item(entry.uri())).collect();
        add.set_add_last(true);
        self.change(&playlist_changes(playlist), added(add)).await
    }

    /// Removes every occurrence of the given items from a playlist.
    pub async fn remove_from_playlist(&self, playlist: Id, items: &[ItemRef]) -> Result<()> {
        let uris = items.iter().map(|entry| entry.uri());
        self.change(&playlist_changes(playlist), removed(uris))
            .await
    }

    /// Removes a playlist from the user's library. For a playlist they own
    /// this is what Spotify calls deleting it.
    pub async fn remove_playlist(&self, playlist: Id) -> Result<()> {
        let uris = std::iter::once(Kind::Playlist.uri(playlist));
        self.change(&self.rootlist_changes(), removed(uris)).await
    }

    /// Puts the `length` rootlist entries from `index` into a folder,
    /// given by its start and end markers. Both markers go in one change:
    /// Spotify refuses a folder left open. For tests that need a folder.
    #[doc(hidden)]
    pub async fn add_folder(&self, start: &str, end: &str, index: i32, length: i32) -> Result<()> {
        let marker = |uri: &str, at: i32| {
            let mut add = Add::new();
            add.items = vec![item(uri.into())];
            add.set_from_index(at);
            added(add)
        };
        // Placing entries by index needs the revision it counts from.
        let endpoint = format!(
            "/playlist/v2/user/{}/rootlist?from=0&length=1",
            self.0.session().username()
        );
        let current = self
            .0
            .spclient(Method::GET, &endpoint, None, bytes::Bytes::new())
            .await?;
        let revision = SelectedListContent::parse_from_bytes(&current)
            .map_err(decode)?
            .revision()
            .to_vec();
        let ops = vec![marker(start, index), marker(end, index + length + 1)];
        self.changes(&self.rootlist_changes(), ops, Some(revision))
            .await
    }

    /// Lets anyone a playlist is shared with edit it, or stops that. For
    /// tests that need a collaborative playlist.
    #[doc(hidden)]
    pub async fn set_collaborative(&self, playlist: Id, collaborative: bool) -> Result<()> {
        let mut attributes = ListAttributes::new();
        attributes.set_collaborative(collaborative);
        let mut state = ListAttributesPartialState::new();
        state.values = Some(attributes).into();
        let mut update = UpdateListAttributes::new();
        update.new_attributes = Some(state).into();
        let mut op = Op::new();
        op.set_kind(OpKind::UPDATE_LIST_ATTRIBUTES);
        op.update_list_attributes = Some(update).into();
        self.change(&playlist_changes(playlist), op).await
    }

    /// Removes raw rootlist entries by URI. For tests that need a folder.
    #[doc(hidden)]
    pub async fn remove_from_rootlist(&self, uris: &[&str]) -> Result<()> {
        let uris = uris.iter().map(|uri| (*uri).to_owned());
        self.change(&self.rootlist_changes(), removed(uris)).await
    }

    fn rootlist_changes(&self) -> String {
        format!(
            "/playlist/v2/user/{}/rootlist/changes",
            self.0.session().username()
        )
    }

    async fn change(&self, endpoint: &str, op: Op) -> Result<()> {
        self.changes(endpoint, vec![op], None).await
    }

    /// Applies operations together, after `revision` when they place
    /// entries by index.
    async fn changes(&self, endpoint: &str, ops: Vec<Op>, revision: Option<Vec<u8>>) -> Result<()> {
        let changes = ListChanges {
            base_revision: revision,
            deltas: vec![Delta {
                ops,
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
