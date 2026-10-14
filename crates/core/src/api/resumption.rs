//! Where the user left off in each episode, shared by all their devices.
//!
//! Spotify's apps keep this in a service of its own, which records every
//! change as a revision; the latest revision of an episode is its state.

use http::Method;
use prost::Message as _;

use super::{Api, transport::decode};
use crate::{
    Result,
    model::{Id, Kind, Progress},
};

const PROTOBUF: &str = "application/x-protobuf";
const LIST: &str =
    "/herodotus/spotify.resumption.v1.ResumePointRevisionService/ListResumePointRevisions";
const CREATE: &str =
    "/herodotus/spotify.resumption.v1.ResumePointRevisionService/CreateResumePointRevision";
// Revisions come newest first; a few cover any that share a timestamp.
const REVISIONS_READ: i32 = 5;

// Messages of the resumption service, which librespot does not ship.
#[derive(Clone, Copy, PartialEq, prost::Message)]
struct Duration {
    #[prost(int64, tag = "1")]
    seconds: i64,
    #[prost(int32, tag = "2")]
    nanos: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, prost::Message)]
struct Timestamp {
    #[prost(int64, tag = "1")]
    seconds: i64,
    #[prost(int32, tag = "2")]
    nanos: i32,
}

/// `google.protobuf.Empty`: the service marks states by a field's presence.
#[derive(Clone, Copy, PartialEq, prost::Message)]
struct Flag {}

#[derive(Clone, PartialEq, prost::Message)]
struct ResumePoint {
    /// The episode's URI.
    #[prost(string, tag = "1")]
    id: String,
    #[prost(message, optional, tag = "2")]
    position: Option<Duration>,
    #[prost(message, optional, tag = "4")]
    finished: Option<Flag>,
    #[prost(message, optional, tag = "8")]
    marked_as_finished: Option<Flag>,
    #[prost(message, optional, tag = "9")]
    unmarked_as_finished: Option<Flag>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Revision {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(message, optional, tag = "2")]
    snapshot: Option<ResumePoint>,
    #[prost(message, optional, tag = "3")]
    created: Option<Timestamp>,
    #[prost(message, optional, tag = "4")]
    updated: Option<Timestamp>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ListRequest {
    #[prost(string, tag = "1")]
    username: String,
    #[prost(string, tag = "2")]
    uri: String,
    #[prost(int32, tag = "3")]
    page_size: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ListResponse {
    #[prost(message, repeated, tag = "1")]
    revisions: Vec<Revision>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct CreateRequest {
    #[prost(string, tag = "1")]
    username: String,
    #[prost(string, tag = "2")]
    uri: String,
    #[prost(message, optional, tag = "4")]
    revision: Option<Revision>,
}

impl Api {
    /// How far the user has listened to an episode, on any device.
    pub async fn episode_progress(&self, episode: Id) -> Result<Progress> {
        let request = ListRequest {
            username: self.0.session().username(),
            uri: Kind::Episode.uri(episode),
            page_size: REVISIONS_READ,
        };
        let response = self
            .0
            .spclient(
                Method::POST,
                LIST,
                Some(PROTOBUF),
                request.encode_to_vec().into(),
            )
            .await?;
        let revisions = ListResponse::decode(response).map_err(decode)?.revisions;
        Ok(latest(&revisions))
    }

    /// Records how far the user has listened to an episode.
    pub async fn set_episode_progress(&self, episode: Id, progress: Progress) -> Result<()> {
        let request = create_request(
            self.0.session().username(),
            Kind::Episode.uri(episode),
            progress,
            now(),
        );
        self.0
            .spclient(
                Method::POST,
                CREATE,
                Some(PROTOBUF),
                request.encode_to_vec().into(),
            )
            .await?;
        Ok(())
    }
}

/// The state the newest revision describes. Spotify's apps write a
/// position and a finished mark with the same time when an episode ends,
/// so on a tie the mark wins.
fn latest(revisions: &[Revision]) -> Progress {
    revisions
        .iter()
        .filter_map(|revision| Some((revision.updated?, progress(revision.snapshot.as_ref()?))))
        .max_by_key(|(updated, progress)| (*updated, *progress == Progress::Completed))
        .map_or(Progress::NotStarted, |(_, progress)| progress)
}

fn progress(point: &ResumePoint) -> Progress {
    if point.finished.is_some() || point.marked_as_finished.is_some() {
        Progress::Completed
    } else if point.unmarked_as_finished.is_some() {
        Progress::NotStarted
    } else {
        match point.position.map_or(0, milliseconds) {
            0 => Progress::NotStarted,
            position_ms => Progress::InProgress { position_ms },
        }
    }
}

fn create_request(
    username: String,
    uri: String,
    progress: Progress,
    now: Timestamp,
) -> CreateRequest {
    let mut point = ResumePoint {
        id: uri.clone(),
        ..ResumePoint::default()
    };
    match progress {
        Progress::InProgress { position_ms } => point.position = Some(duration(position_ms)),
        Progress::Completed => point.finished = Some(Flag {}),
        // Spotify's own "mark as unplayed".
        Progress::NotStarted => point.unmarked_as_finished = Some(Flag {}),
    }
    CreateRequest {
        username,
        uri,
        revision: Some(Revision {
            // The service assigns the id.
            id: String::new(),
            snapshot: Some(point),
            created: Some(now),
            updated: Some(now),
        }),
    }
}

fn duration(milliseconds: u32) -> Duration {
    Duration {
        seconds: (milliseconds / 1000).into(),
        nanos: (milliseconds % 1000 * 1_000_000) as i32,
    }
}

fn milliseconds(duration: Duration) -> u32 {
    let total = duration.seconds * 1000 + i64::from(duration.nanos / 1_000_000);
    total.clamp(0, u32::MAX.into()) as u32
}

fn now() -> Timestamp {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: elapsed.as_secs().try_into().unwrap_or(i64::MAX),
        nanos: elapsed.subsec_nanos() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revision(updated: i64, point: ResumePoint) -> Revision {
        Revision {
            id: String::new(),
            snapshot: Some(point),
            created: None,
            updated: Some(Timestamp {
                seconds: updated,
                nanos: 0,
            }),
        }
    }

    fn at(position_ms: u32) -> ResumePoint {
        ResumePoint {
            position: Some(duration(position_ms)),
            ..ResumePoint::default()
        }
    }

    fn finished() -> ResumePoint {
        ResumePoint {
            finished: Some(Flag {}),
            ..ResumePoint::default()
        }
    }

    #[test]
    fn the_newest_revision_decides() {
        let revisions = [revision(10, finished()), revision(20, at(61_500))];
        assert_eq!(
            latest(&revisions),
            Progress::InProgress {
                position_ms: 61_500
            }
        );
        assert_eq!(latest(&[]), Progress::NotStarted);
    }

    #[test]
    fn a_finished_mark_wins_a_tie() {
        let revisions = [revision(10, at(5_000)), revision(10, finished())];
        assert_eq!(latest(&revisions), Progress::Completed);
    }

    #[test]
    fn unmarking_starts_over() {
        let point = ResumePoint {
            unmarked_as_finished: Some(Flag {}),
            ..ResumePoint::default()
        };
        assert_eq!(progress(&point), Progress::NotStarted);
        assert_eq!(progress(&at(0)), Progress::NotStarted);
    }

    #[test]
    fn positions_keep_their_milliseconds() {
        assert_eq!(
            duration(12_876_782),
            Duration {
                seconds: 12_876,
                nanos: 782_000_000
            }
        );
        assert_eq!(milliseconds(duration(12_876_782)), 12_876_782);
    }

    #[test]
    fn a_position_is_written_as_spotify_writes_it() {
        let now = Timestamp {
            seconds: 1_791_740_060,
            nanos: 0,
        };
        let uri = "spotify:episode:3nIHfVyxUQJUNQ6aIO4ka7".to_string();
        let request = create_request(
            "user".into(),
            uri.clone(),
            Progress::InProgress { position_ms: 1_500 },
            now,
        );
        // The same bytes decode back with the URI as the point's id.
        let decoded = CreateRequest::decode(&*request.encode_to_vec()).unwrap();
        let revision = decoded.revision.unwrap();
        let point = revision.snapshot.unwrap();
        assert_eq!(decoded.uri, uri);
        assert_eq!(point.id, uri);
        assert_eq!(
            point.position,
            Some(Duration {
                seconds: 1,
                nanos: 500_000_000
            })
        );
        assert_eq!(revision.updated, Some(now));

        let reset = create_request("user".into(), uri, Progress::NotStarted, now);
        let point = reset.revision.unwrap().snapshot.unwrap();
        assert!(point.unmarked_as_finished.is_some() && point.position.is_none());
    }
}
