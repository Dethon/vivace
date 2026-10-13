//! The account's Spotify Connect state: its devices and what plays elsewhere.

use std::time::{SystemTime, UNIX_EPOCH};

use librespot_protocol::{connect::Cluster, devices::DeviceType};

use crate::{Queue, QueueItem, Repeat, engine::QUEUE_PROVIDER, model::Kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Computer,
    Phone,
    Speaker,
    Tv,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: Box<str>,
    pub name: Box<str>,
    pub kind: DeviceKind,
    pub is_active: bool,
    /// Whether this is the device the engine itself plays on.
    pub is_local: bool,
}

/// Playback on a device other than this computer.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    /// Id of the device that is playing.
    pub device: Box<str>,
    /// The current track and what follows it on that device.
    pub queue: Queue,
    pub playing: bool,
    /// Position when this state was received; it advances while `playing`.
    pub position_ms: u32,
    pub duration_ms: u32,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// Volume in `0.0..=1.0`.
    pub volume: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connect {
    /// Playable devices, this computer first.
    pub devices: Vec<Device>,
    /// Set while another device is the one playing.
    pub remote: Option<Remote>,
}

impl Connect {
    pub(crate) fn from_cluster(cluster: &Cluster, local_device: &str) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        Self::at(cluster, local_device, now_ms)
    }

    fn at(cluster: &Cluster, local_device: &str, now_ms: i64) -> Self {
        let mut devices: Vec<Device> = cluster
            .device
            .iter()
            // Controllers such as the web player are listed but cannot play.
            .filter(|(_, info)| info.can_play)
            .map(|(id, info)| Device {
                id: id.as_str().into(),
                name: info.name.as_str().into(),
                kind: match info.device_type.enum_value_or_default() {
                    DeviceType::COMPUTER | DeviceType::CHROMEBOOK => DeviceKind::Computer,
                    DeviceType::SMARTPHONE | DeviceType::TABLET => DeviceKind::Phone,
                    DeviceType::SPEAKER
                    | DeviceType::AVR
                    | DeviceType::AUDIO_DONGLE
                    | DeviceType::CAST_AUDIO
                    | DeviceType::HOME_THING => DeviceKind::Speaker,
                    DeviceType::TV | DeviceType::STB | DeviceType::CAST_VIDEO => DeviceKind::Tv,
                    _ => DeviceKind::Other,
                },
                is_active: *id == cluster.active_device_id,
                is_local: id == local_device,
            })
            .collect();
        devices.sort_by(|a, b| {
            b.is_local
                .cmp(&a.is_local)
                .then_with(|| a.name.cmp(&b.name))
        });

        let active = cluster.active_device_id.as_str();
        let remote = cluster
            .device
            .get(active)
            .filter(|_| active != local_device)
            .map(|info| {
                let state = &cluster.player_state;
                let playing = state.is_playing && !state.is_paused;
                let elapsed = if playing {
                    (now_ms - state.timestamp).max(0)
                } else {
                    0
                };
                let position =
                    (state.position_as_of_timestamp + elapsed).clamp(0, state.duration.max(0));
                Remote {
                    device: active.into(),
                    queue: Queue {
                        context_uri: state.context_uri.as_str().into(),
                        current: Kind::Track.id(&state.track.uri),
                        next: state
                            .next_tracks
                            .iter()
                            .filter_map(|track| {
                                Some(QueueItem {
                                    track: Kind::Track.id(&track.uri)?,
                                    queued: track.provider == QUEUE_PROVIDER,
                                })
                            })
                            .collect(),
                    },
                    playing,
                    position_ms: position.try_into().unwrap_or_default(),
                    duration_ms: state.duration.try_into().unwrap_or_default(),
                    shuffle: state.options.shuffling_context,
                    repeat: match (
                        state.options.repeating_track,
                        state.options.repeating_context,
                    ) {
                        (true, _) => Repeat::Track,
                        (false, true) => Repeat::Context,
                        (false, false) => Repeat::Off,
                    },
                    volume: info.volume as f32 / f32::from(u16::MAX),
                }
            });

        Self { devices, remote }
    }
}

#[cfg(test)]
mod tests {
    use librespot_protocol::{connect::DeviceInfo, player::ProvidedTrack};

    use super::*;

    fn device(name: &str, kind: DeviceType, can_play: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.into(),
            can_play,
            device_type: kind.into(),
            volume: u32::from(u16::MAX) / 2,
            ..DeviceInfo::default()
        }
    }

    fn cluster() -> Cluster {
        let mut cluster = Cluster::new();
        cluster
            .device
            .insert("local".into(), device("Vivace", DeviceType::COMPUTER, true));
        cluster.device.insert(
            "kitchen".into(),
            device("Cocina", DeviceType::SPEAKER, true),
        );
        cluster.device.insert(
            "web".into(),
            device("Web Player", DeviceType::COMPUTER, false),
        );
        cluster
    }

    #[test]
    fn lists_playable_devices_with_this_computer_first() {
        let connect = Connect::at(&cluster(), "local", 0);
        let names: Vec<&str> = connect.devices.iter().map(|device| &*device.name).collect();
        assert_eq!(names, ["Vivace", "Cocina"]);
        assert!(connect.devices[0].is_local);
        assert_eq!(connect.devices[1].kind, DeviceKind::Speaker);
        assert_eq!(connect.remote, None);
    }

    #[test]
    fn playback_on_this_computer_is_not_remote() {
        let mut cluster = cluster();
        cluster.active_device_id = "local".into();
        let connect = Connect::at(&cluster, "local", 0);
        assert!(connect.devices[0].is_active);
        assert_eq!(connect.remote, None);
    }

    #[test]
    fn remote_position_advances_only_while_playing() {
        let mut cluster = cluster();
        cluster.active_device_id = "kitchen".into();
        let state = cluster.player_state.mut_or_insert_default();
        state.track = Some(ProvidedTrack {
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".into(),
            ..ProvidedTrack::default()
        })
        .into();
        state.is_playing = true;
        state.timestamp = 1_000;
        state.position_as_of_timestamp = 30_000;
        state.duration = 200_000;
        state.options.mut_or_insert_default().repeating_context = true;

        let remote = Connect::at(&cluster, "local", 6_000).remote.unwrap();
        assert_eq!(&*remote.device, "kitchen");
        assert!(remote.queue.current.is_some());
        assert_eq!(remote.position_ms, 35_000);
        assert_eq!(remote.repeat, Repeat::Context);
        assert!((remote.volume - 0.5).abs() < 0.01);

        cluster.player_state.mut_or_insert_default().is_paused = true;
        let remote = Connect::at(&cluster, "local", 6_000).remote.unwrap();
        assert!(!remote.playing);
        assert_eq!(remote.position_ms, 30_000);
    }
}
