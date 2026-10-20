//! Controlling playback on other Connect devices.

use std::time::{Duration, Instant};

use bytes::Bytes;
use http::Method;
use librespot_protocol::connect::SetVolumeCommand;
use protobuf::Message;
use serde::Serialize;

use super::{Api, transport::decode};
use crate::{Error, Repeat, Result, Start, engine::QUEUE_PROVIDER, model::ItemRef};

const JSON: &str = "application/json";
const PROTOBUF: &str = "application/x-protobuf";
// How long a sleeping speaker is given to wake for a transfer, and how often
// it is asked again meanwhile.
const WAKE_TIMEOUT: Duration = Duration::from_secs(20);
const WAKE_RETRY: Duration = Duration::from_secs(1);

/// A transport command for the device that is currently playing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemoteCommand {
    Resume,
    Pause,
    Next,
    Previous,
    /// Position in milliseconds.
    Seek(u32),
    Shuffle(bool),
    RepeatContext(bool),
    RepeatTrack(bool),
}

#[derive(Serialize)]
struct Envelope {
    command: Command,
}

#[derive(Serialize)]
struct Command {
    endpoint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Value {
    Flag(bool),
    Position(u32),
}

impl RemoteCommand {
    fn body(self) -> Envelope {
        let (endpoint, value) = match self {
            RemoteCommand::Resume => ("resume", None),
            RemoteCommand::Pause => ("pause", None),
            RemoteCommand::Next => ("skip_next", None),
            RemoteCommand::Previous => ("skip_prev", None),
            RemoteCommand::Seek(position_ms) => ("seek_to", Some(Value::Position(position_ms))),
            RemoteCommand::Shuffle(on) => ("set_shuffling_context", Some(Value::Flag(on))),
            RemoteCommand::RepeatContext(on) => ("set_repeating_context", Some(Value::Flag(on))),
            RemoteCommand::RepeatTrack(on) => ("set_repeating_track", Some(Value::Flag(on))),
        };
        Envelope {
            command: Command { endpoint, value },
        }
    }
}

/// What to start on another device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RemotePlay<'a> {
    /// A playlist, album, artist or collection, by URI.
    Context { uri: &'a str, start: Start },
    /// An ad-hoc list of tracks and episodes, starting at `index`.
    Tracks { tracks: &'a [ItemRef], index: u32 },
}

// The shape Spotify's own clients send for `play`; field names are the
// protobuf ones.
#[derive(Serialize)]
struct PlayEnvelope<'a> {
    command: PlayCommand<'a>,
}

#[derive(Serialize)]
struct PlayCommand<'a> {
    endpoint: &'static str,
    context: PlayContext<'a>,
    play_origin: PlayOrigin,
    options: PlayOptions,
}

#[derive(Serialize)]
struct PlayContext<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pages: Vec<Page>,
}

#[derive(Serialize)]
struct Page {
    tracks: Vec<TrackRef>,
}

#[derive(Serialize)]
struct TrackRef {
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<&'static str>,
}

#[derive(Serialize)]
struct PlayOrigin {
    feature_identifier: &'static str,
}

#[derive(Serialize)]
struct PlayOptions {
    license: &'static str,
    skip_to: SkipTo,
    player_options_override: Modes,
}

#[derive(Serialize)]
struct SkipTo {
    #[serde(skip_serializing_if = "Option::is_none")]
    track_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    track_index: Option<u32>,
}

#[derive(Serialize)]
struct Modes {
    shuffling_context: bool,
    repeating_context: bool,
    repeating_track: bool,
}

#[derive(Serialize)]
struct QueueEnvelope {
    command: QueueCommand,
}

#[derive(Serialize)]
struct QueueCommand {
    endpoint: &'static str,
    track: TrackRef,
}

fn play_body(play: RemotePlay<'_>, shuffle: bool, repeat: Repeat) -> PlayEnvelope<'_> {
    let (context, skip_to) = match play {
        RemotePlay::Context { uri, start } => (
            PlayContext {
                uri: Some(uri),
                url: Some(format!("context://{uri}")),
                pages: Vec::new(),
            },
            match start {
                Start::Beginning => SkipTo {
                    track_uri: None,
                    track_index: None,
                },
                Start::Index(index) => SkipTo {
                    track_uri: None,
                    track_index: Some(index),
                },
                Start::Item(item) => SkipTo {
                    track_uri: Some(item.uri()),
                    track_index: None,
                },
            },
        ),
        RemotePlay::Tracks { tracks, index } => (
            PlayContext {
                uri: None,
                url: None,
                pages: vec![Page {
                    tracks: tracks
                        .iter()
                        .map(|item| TrackRef {
                            uri: item.uri(),
                            provider: None,
                        })
                        .collect(),
                }],
            },
            SkipTo {
                track_uri: None,
                track_index: Some(index),
            },
        ),
    };
    PlayEnvelope {
        command: PlayCommand {
            endpoint: "play",
            context,
            play_origin: PlayOrigin {
                feature_identifier: "vivace",
            },
            options: PlayOptions {
                license: "premium",
                skip_to,
                player_options_override: Modes {
                    shuffling_context: shuffle,
                    repeating_context: repeat.repeats_context(),
                    repeating_track: repeat.repeats_track(),
                },
            },
        },
    }
}

impl Api {
    /// Starts playback of a context or track list on another device.
    pub async fn remote_play(
        &self,
        device: &str,
        play: RemotePlay<'_>,
        shuffle: bool,
        repeat: Repeat,
    ) -> Result<()> {
        let body = serde_json::to_vec(&play_body(play, shuffle, repeat)).map_err(decode)?;
        self.command(device, body).await
    }

    /// Queues a track on another device.
    pub async fn remote_queue(&self, device: &str, item: ItemRef) -> Result<()> {
        let envelope = QueueEnvelope {
            command: QueueCommand {
                endpoint: "add_to_queue",
                track: TrackRef {
                    uri: item.uri(),
                    provider: Some(QUEUE_PROVIDER),
                },
            },
        };
        self.command(device, serde_json::to_vec(&envelope).map_err(decode)?)
            .await
    }

    async fn command(&self, device: &str, body: Vec<u8>) -> Result<()> {
        let endpoint = format!(
            "/connect-state/v1/player/command/from/{}/to/{device}",
            self.device_id()
        );
        self.0
            .spclient(Method::POST, &endpoint, Some(JSON), body.into())
            .await?;
        Ok(())
    }

    /// Sends a transport command to another device.
    pub async fn remote(&self, device: &str, command: RemoteCommand) -> Result<()> {
        log::debug!("sending {command:?} to {device}");
        let body = serde_json::to_vec(&command.body()).map_err(decode)?;
        self.command(device, body).await
    }

    /// Sets another device's volume, in `0.0..=1.0`.
    pub async fn remote_volume(&self, device: &str, volume: f32) -> Result<()> {
        let endpoint = format!(
            "/connect-state/v1/connect/volume/from/{}/to/{device}",
            self.device_id()
        );
        let command = SetVolumeCommand {
            volume: (volume.clamp(0.0, 1.0) * f32::from(u16::MAX)) as i32,
            ..SetVolumeCommand::default()
        };
        let body = command.write_to_bytes().map_err(decode)?;
        self.0
            .spclient(Method::PUT, &endpoint, Some(PROTOBUF), body.into())
            .await?;
        Ok(())
    }

    /// Moves whatever is playing to `device`.
    pub async fn transfer(&self, device: &str) -> Result<()> {
        let endpoint = format!(
            "/connect-state/v1/connect/transfer/from/{}/to/{device}",
            self.device_id()
        );
        log::debug!("moving playback to {device}");
        let woken_by = Instant::now() + WAKE_TIMEOUT;
        loop {
            match self
                .0
                .spclient(Method::POST, &endpoint, None, Bytes::new())
                .await
            {
                // A sleeping speaker, such as an Echo Spotify lists as
                // offline, refuses the transfer with 410 but starts waking
                // up, and takes it once awake.
                Err(Error::Status(410)) if Instant::now() < woken_by => {
                    tokio::time::sleep(WAKE_RETRY).await;
                }
                // Spotify's answer when the device never takes it.
                Err(Error::Status(410)) => return Err(Error::NoResponse),
                result => return result.map(drop),
            }
        }
    }

    /// Asks this device to play a track list under librespot's placeholder
    /// context, as another client resuming such a list does, for tests.
    #[doc(hidden)]
    pub async fn play_placeholder_list_here(&self, tracks: &[ItemRef]) -> Result<()> {
        let play = RemotePlay::Tracks { tracks, index: 0 };
        let mut body = serde_json::to_value(play_body(play, false, Repeat::Off)).map_err(decode)?;
        body["command"]["context"]["uri"] = "spotify:web-api".into();
        body["command"]["context"]["url"] = "context://spotify:web-api".into();
        let body = serde_json::to_vec(&body).map_err(decode)?;
        self.command(&self.device_id().to_owned(), body).await
    }

    fn device_id(&self) -> &str {
        self.0.session().device_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Id;

    fn json(command: RemoteCommand) -> String {
        serde_json::to_string(&command.body()).unwrap()
    }

    #[test]
    fn play_names_the_context_and_where_to_start() {
        let track = Id::from_base62("4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let play = RemotePlay::Context {
            uri: "spotify:album:4m2880jivSbbyEGAKfITCa",
            start: Start::Item(ItemRef::track(track)),
        };
        let body = serde_json::to_value(play_body(play, true, Repeat::Context)).unwrap();
        let command = &body["command"];
        assert_eq!(command["endpoint"], "play");
        assert_eq!(
            command["context"]["uri"],
            "spotify:album:4m2880jivSbbyEGAKfITCa"
        );
        assert_eq!(
            command["context"]["url"],
            "context://spotify:album:4m2880jivSbbyEGAKfITCa"
        );
        assert_eq!(
            command["options"]["skip_to"]["track_uri"],
            "spotify:track:4uLU6hMCjMI75M1A2tKUQC"
        );
        let modes = &command["options"]["player_options_override"];
        assert_eq!(modes["shuffling_context"], true);
        assert_eq!(modes["repeating_context"], true);
        assert_eq!(modes["repeating_track"], false);
    }

    #[test]
    fn a_track_list_travels_as_a_page() {
        let track = Id::from_base62("4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let tracks = [ItemRef::track(track), ItemRef::episode(track)];
        let play = RemotePlay::Tracks {
            tracks: &tracks,
            index: 1,
        };
        let body = serde_json::to_value(play_body(play, false, Repeat::Off)).unwrap();
        let command = &body["command"];
        assert!(command["context"].get("uri").is_none());
        assert_eq!(
            command["context"]["pages"][0]["tracks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            command["context"]["pages"][0]["tracks"][1]["uri"],
            "spotify:episode:4uLU6hMCjMI75M1A2tKUQC"
        );
        assert_eq!(command["options"]["skip_to"]["track_index"], 1);
    }

    #[test]
    fn commands_serialise_to_spotify_endpoints() {
        assert_eq!(
            json(RemoteCommand::Pause),
            r#"{"command":{"endpoint":"pause"}}"#
        );
        assert_eq!(
            json(RemoteCommand::Next),
            r#"{"command":{"endpoint":"skip_next"}}"#
        );
        assert_eq!(
            json(RemoteCommand::Seek(42_000)),
            r#"{"command":{"endpoint":"seek_to","value":42000}}"#
        );
        assert_eq!(
            json(RemoteCommand::Shuffle(true)),
            r#"{"command":{"endpoint":"set_shuffling_context","value":true}}"#
        );
    }
}
