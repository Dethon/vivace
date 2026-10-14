//! What Your Library lists: the user's collections, playlists and folders,
//! saved albums, followed artists and podcasts, filtered and sorted as the
//! sidebar asks.

use std::cmp::Reverse;

use vivace_core::model::{Album, Artist, Placement, Playlist, Rootlist, Show};

/// One kind of item the sidebar can be narrowed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

impl Filter {
    /// The chips in the order they are shown, which their indices follow.
    pub const CHIPS: [Filter; 4] = [
        Filter::Playlists,
        Filter::Albums,
        Filter::Artists,
        Filter::Podcasts,
    ];

    /// The filter of a chip index; anything else lists everything.
    pub fn from_chip(index: i32) -> Self {
        usize::try_from(index)
            .ok()
            .and_then(|index| Self::CHIPS.get(index).copied())
            .unwrap_or_default()
    }

    pub fn chip(self) -> i32 {
        Self::CHIPS
            .iter()
            .position(|chip| *chip == self)
            .map_or(-1, |index| index as i32)
    }

    fn shows(self, kind: Filter) -> bool {
        self == Filter::All || self == kind
    }
}

/// How the sidebar orders what it lists, after the collections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// Playlists and folders as the user arranged them, then albums,
    /// artists and podcasts, each newest first.
    #[default]
    Custom,
    RecentlyAdded,
    Alphabetical,
    Creator,
}

impl Sort {
    /// Every order, as the sort menu lists them.
    pub const ALL: [Sort; 4] = [
        Sort::Custom,
        Sort::RecentlyAdded,
        Sort::Alphabetical,
        Sort::Creator,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Custom => "Custom order",
            Sort::RecentlyAdded => "Recently added",
            Sort::Alphabetical => "Alphabetical",
            Sort::Creator => "Creator",
        }
    }
}

/// An entry of the sidebar, by index into [`Contents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    LikedSongs,
    YourEpisodes,
    Folder(usize),
    Playlist(usize),
    Album(usize),
    Artist(usize),
    Show(usize),
}

/// Everything in the user's library, as Spotify last listed it.
#[derive(Debug, Clone, Default)]
pub struct Contents {
    pub rootlist: Rootlist,
    /// Saved albums with when they were saved, in Unix seconds.
    pub albums: Vec<(Album, i64)>,
    pub artists: Vec<(Artist, i64)>,
    pub shows: Vec<(Show, i64)>,
}

impl Contents {
    pub fn playlists(&self) -> &[Playlist] {
        &self.rootlist.playlists
    }

    /// The rows the sidebar shows: inside `folder` only that folder's
    /// playlists and folders, otherwise everything `filter` lets through.
    pub fn rows(&self, filter: Filter, sort: Sort, folder: Option<usize>) -> Vec<Row> {
        let mut rows = Vec::new();
        if folder.is_none() {
            if filter.shows(Filter::Playlists) {
                rows.push(Row::LikedSongs);
            }
            if filter.shows(Filter::Podcasts) {
                rows.push(Row::YourEpisodes);
            }
        }
        let collections = rows.len();

        // Entries with when they were added, in custom order.
        let mut entries: Vec<(Row, i64)> = Vec::new();
        if folder.is_some() || filter.shows(Filter::Playlists) {
            entries.extend(
                self.rootlist
                    .layout
                    .iter()
                    .filter(|placed| placed.folder == folder)
                    .map(|placed| {
                        let row = match placed.item {
                            Placement::Playlist(index) => Row::Playlist(index),
                            Placement::Folder(index) => Row::Folder(index),
                        };
                        (row, placed.added_at)
                    }),
            );
        }
        if folder.is_none() {
            let newest_first = |items: Vec<(Row, i64)>| {
                let mut items = items;
                items.sort_by_key(|(_, added)| Reverse(*added));
                items
            };
            if filter.shows(Filter::Albums) {
                entries.extend(newest_first(
                    self.albums
                        .iter()
                        .enumerate()
                        .map(|(index, (_, added))| (Row::Album(index), *added))
                        .collect(),
                ));
            }
            if filter.shows(Filter::Artists) {
                entries.extend(newest_first(
                    self.artists
                        .iter()
                        .enumerate()
                        .map(|(index, (_, added))| (Row::Artist(index), *added))
                        .collect(),
                ));
            }
            if filter.shows(Filter::Podcasts) {
                entries.extend(newest_first(
                    self.shows
                        .iter()
                        .enumerate()
                        .map(|(index, (_, added))| (Row::Show(index), *added))
                        .collect(),
                ));
            }
        }

        // Sorts are stable, so ties keep the custom order.
        match sort {
            Sort::Custom => {}
            Sort::RecentlyAdded => entries.sort_by_key(|(_, added)| Reverse(*added)),
            Sort::Alphabetical => entries.sort_by_cached_key(|(row, _)| self.name(*row)),
            Sort::Creator => {
                entries.sort_by_cached_key(|(row, _)| (self.creator(*row), self.name(*row)));
            }
        }
        rows.extend(entries.into_iter().map(|(row, _)| row));
        debug_assert!(rows.len() >= collections);
        rows
    }

    /// The folders holding `folder`, outermost first, with it last.
    pub fn folder_path(&self, folder: usize) -> Vec<usize> {
        let mut path = vec![folder];
        while let Some(parent) = self
            .rootlist
            .layout
            .iter()
            .find(|placed| placed.item == Placement::Folder(path[0]))
            .and_then(|placed| placed.folder)
        {
            path.insert(0, parent);
        }
        path
    }

    /// The folder holding `folder`, if any.
    pub fn parent(&self, folder: usize) -> Option<usize> {
        let path = self.folder_path(folder);
        path.len().checked_sub(2).map(|index| path[index])
    }

    /// How many playlists and folders sit directly in `folder`.
    pub fn folder_size(&self, folder: usize) -> usize {
        self.rootlist
            .layout
            .iter()
            .filter(|placed| placed.folder == Some(folder))
            .count()
    }

    /// The name a row sorts by, ignoring case.
    fn name(&self, row: Row) -> String {
        let name: &str = match row {
            Row::LikedSongs => "Liked Songs",
            Row::YourEpisodes => "Your Episodes",
            Row::Folder(index) => &self.rootlist.folders[index].name,
            Row::Playlist(index) => &self.rootlist.playlists[index].name,
            Row::Album(index) => &self.albums[index].0.name,
            Row::Artist(index) => &self.artists[index].0.name,
            Row::Show(index) => &self.shows[index].0.name,
        };
        name.to_lowercase()
    }

    /// Who made a row's item, ignoring case; folders have no creator.
    fn creator(&self, row: Row) -> String {
        let creator: &str = match row {
            Row::LikedSongs | Row::YourEpisodes | Row::Folder(_) => "",
            Row::Playlist(index) => &self.rootlist.playlists[index].owner,
            Row::Album(index) => self.albums[index]
                .0
                .artists
                .first()
                .map_or("", |artist| &artist.name),
            Row::Artist(index) => &self.artists[index].0.name,
            Row::Show(index) => &self.shows[index].0.publisher,
        };
        creator.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use vivace_core::model::{AlbumKind, Folder, Id, Placed};

    use super::*;

    fn id(n: u8) -> Id {
        Id::from_raw(&[n; 16]).unwrap()
    }

    fn playlist(n: u8, name: &str, owner: &str) -> Playlist {
        Playlist {
            id: id(n),
            name: name.into(),
            owner: owner.into(),
            description: "".into(),
            length: 0,
            image: None,
            collaborative: false,
        }
    }

    fn placed(item: Placement, folder: Option<usize>, added_at: i64) -> Placed {
        Placed {
            item,
            folder,
            added_at,
        }
    }

    /// "Zebra" at the top, then folder "Mix" holding "Apple", then "Mango";
    /// one album and one show.
    fn contents() -> Contents {
        Contents {
            rootlist: Rootlist {
                playlists: vec![
                    playlist(1, "Zebra", "carol"),
                    playlist(2, "Apple", "bob"),
                    playlist(3, "Mango", "alice"),
                ],
                folders: vec![Folder {
                    id: "aa".into(),
                    name: "Mix".into(),
                }],
                layout: vec![
                    placed(Placement::Playlist(0), None, 30),
                    placed(Placement::Folder(0), None, 10),
                    placed(Placement::Playlist(1), Some(0), 50),
                    placed(Placement::Playlist(2), None, 20),
                ],
            },
            albums: vec![(
                Album {
                    id: id(4),
                    name: "Discovery".into(),
                    artists: Box::default(),
                    cover: None,
                    year: 2001,
                    kind: AlbumKind::Album,
                },
                40,
            )],
            artists: Vec::new(),
            shows: vec![(
                Show {
                    id: id(5),
                    name: "Caso 63".into(),
                    publisher: "Spotify Studios".into(),
                    image: None,
                },
                5,
            )],
        }
    }

    #[test]
    fn everything_lists_collections_then_the_top_level() {
        let rows = contents().rows(Filter::All, Sort::Custom, None);
        assert_eq!(
            rows,
            [
                Row::LikedSongs,
                Row::YourEpisodes,
                Row::Playlist(0),
                Row::Folder(0),
                Row::Playlist(2),
                Row::Album(0),
                Row::Show(0),
            ]
        );
    }

    #[test]
    fn a_folder_lists_only_what_it_holds() {
        let contents = contents();
        assert_eq!(
            contents.rows(Filter::All, Sort::Custom, Some(0)),
            [Row::Playlist(1)]
        );
        assert_eq!(contents.folder_size(0), 1);
        assert_eq!(contents.parent(0), None);
    }

    #[test]
    fn filters_keep_their_collection() {
        let contents = contents();
        assert_eq!(
            contents.rows(Filter::Podcasts, Sort::Custom, None),
            [Row::YourEpisodes, Row::Show(0)]
        );
        assert_eq!(
            contents.rows(Filter::Albums, Sort::Custom, None),
            [Row::Album(0)]
        );
    }

    #[test]
    fn sorting_leaves_the_collections_first() {
        let contents = contents();
        assert_eq!(
            contents.rows(Filter::Playlists, Sort::Alphabetical, None),
            [
                Row::LikedSongs,
                Row::Playlist(2),
                Row::Folder(0),
                Row::Playlist(0)
            ]
        );
        assert_eq!(
            contents.rows(Filter::All, Sort::RecentlyAdded, None),
            [
                Row::LikedSongs,
                Row::YourEpisodes,
                Row::Album(0),
                Row::Playlist(0),
                Row::Playlist(2),
                Row::Folder(0),
                Row::Show(0),
            ]
        );
        // Folders have no creator and come first; then alice, bob, carol.
        assert_eq!(
            contents.rows(Filter::Playlists, Sort::Creator, None),
            [
                Row::LikedSongs,
                Row::Folder(0),
                Row::Playlist(2),
                Row::Playlist(0)
            ]
        );
    }

    #[test]
    fn chips_map_to_filters() {
        assert_eq!(Filter::from_chip(1), Filter::Albums);
        assert_eq!(Filter::from_chip(-1), Filter::All);
        assert_eq!(Filter::Podcasts.chip(), 3);
        assert_eq!(Filter::All.chip(), -1);
    }
}
