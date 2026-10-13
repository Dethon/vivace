//! Controlling playback on other Connect devices.

use bytes::Bytes;
use http::Method;
use librespot_protocol::connect::SetVolumeCommand;
use protobuf::Message;
use serde::Serialize;

use super::{Api, transport::decode};
use crate::Result;

const JSON: &str = "application/json";
const PROTOBUF: &str = "application/x-protobuf";

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

impl Api {
    /// Sends a transport command to another device.
    pub async fn remote(&self, device: &str, command: RemoteCommand) -> Result<()> {
        let endpoint = format!(
            "/connect-state/v1/player/command/from/{}/to/{device}",
            self.device_id()
        );
        let body = serde_json::to_vec(&command.body()).map_err(decode)?;
        self.0
            .spclient(Method::POST, &endpoint, Some(JSON), body.into())
            .await?;
        Ok(())
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
        self.0
            .spclient(Method::POST, &endpoint, None, Bytes::new())
            .await?;
        Ok(())
    }

    fn device_id(&self) -> &str {
        self.0.session().device_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(command: RemoteCommand) -> String {
        serde_json::to_string(&command.body()).unwrap()
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
