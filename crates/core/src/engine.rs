use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use librespot_connect::{
    ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options, PlayingTrack,
    Spirc,
};
use librespot_core::{
    Session, SessionConfig, SpotifyUri, authentication::Credentials, cache::Cache,
    config::DeviceType, dealer::protocol::Message, error::ErrorKind,
};
use librespot_playback::{
    audio_backend::{self, Sink},
    config::{AudioFormat, Bitrate, PlayerConfig},
    mixer::{self, Mixer, MixerConfig},
    player::{Player, PlayerEvent, PlayerEventChannel},
};
use librespot_protocol::connect::ClusterUpdate;
use tokio::sync::{oneshot, watch};

use crate::{
    Api, Result, auth,
    model::{Id, Kind},
};

const CLUSTER_UPDATES: &str = "hm://connect-state/v1/cluster";
const DEFAULT_VOLUME: u16 = u16::MAX / 2;
const VOLUME_SETTLE: Duration = Duration::from_millis(300);
// Provider Spotify assigns to tracks the user queued by hand.
const QUEUE_PROVIDER: &str = "queue";

type ClusterUpdates = futures_util::stream::BoxStream<
    'static,
    std::result::Result<ClusterUpdate, librespot_core::Error>,
>;

/// Creates the audio sink on the player thread.
pub type SinkFactory = Arc<dyn Fn() -> Box<dyn Sink> + Send + Sync>;

pub struct Config {
    /// Name shown in Spotify's device picker.
    pub device_name: String,
    /// Replaces the system's default audio output, e.g. to discard audio in tools.
    pub sink: Option<SinkFactory>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: "Vivace".into(),
            sink: None,
        }
    }
}

/// Where playback of a context starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    Beginning,
    Index(u32),
    Track(Id),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub playing: bool,
    pub position_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: Box<str>,
    pub name: Box<str>,
    pub is_active: bool,
    /// Whether this is the device the engine itself plays on.
    pub is_local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueItem {
    pub track: Id,
    /// Added by the user rather than coming from the playing context.
    pub queued: bool,
}

/// What this computer is playing and will play next.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Queue {
    pub context_uri: Box<str>,
    pub current: Option<Id>,
    /// Upcoming tracks; episodes and local files are omitted.
    pub next: Vec<QueueItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The local player moved to another track.
    Track(Id),
    Playback(Playback),
    Position(u32),
    Stopped,
    Unavailable,
    /// Volume in `0.0..=1.0`.
    Volume(f32),
    Shuffle(bool),
    Repeat(Repeat),
    Queue(Queue),
    /// The account's Connect devices changed.
    Devices(Vec<Device>),
    /// The session ended; a new [`Engine`] is needed to continue.
    Disconnected,
}

/// A signed-in session that plays on this computer as a Spotify Connect device.
pub struct Engine {
    session: Session,
    spirc: Arc<Spirc>,
    mixer: Arc<dyn Mixer>,
    // Latest requested volume, published to Connect once it settles.
    volume: watch::Sender<u16>,
}

impl Engine {
    /// Signs in, opening the browser when no valid credentials are stored,
    /// and registers this computer as a Connect device.
    pub async fn connect(config: Config) -> Result<(Self, Events)> {
        let sink = match config.sink {
            Some(sink) => sink,
            None => {
                let backend = audio_backend::find(None).ok_or(crate::Error::NoAudioBackend)?;
                Arc::new(move || backend(None, AudioFormat::default()))
            }
        };

        let cache = auth::cache()?;
        if let Some(credentials) = cache.credentials() {
            match Self::start(
                &config.device_name,
                sink.clone(),
                cache.clone(),
                credentials,
            )
            .await
            {
                Err(crate::Error::Spotify(error)) if rejects_credentials(&error) => {
                    log::warn!("stored credentials were rejected, signing in again: {error}");
                }
                result => return result,
            }
        }
        let credentials = auth::browser_login().await?;
        Self::start(&config.device_name, sink, cache, credentials).await
    }

    async fn start(
        device_name: &str,
        sink: SinkFactory,
        cache: Cache,
        credentials: Credentials,
    ) -> Result<(Self, Events)> {
        let initial_volume = cache.volume().unwrap_or(DEFAULT_VOLUME);
        let session = Session::new(SessionConfig::default(), Some(cache));

        let mixer = mixer::find(None).ok_or(crate::Error::NoAudioBackend)?(MixerConfig::default())?;
        let player_config = PlayerConfig {
            // The highest quality librespot can stream: 320 kbps Ogg Vorbis.
            bitrate: Bitrate::Bitrate320,
            position_update_interval: Some(Duration::from_secs(1)),
            ..PlayerConfig::default()
        };
        let player = Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || sink(),
        );

        let cluster = session
            .dealer()
            .listen_for(CLUSTER_UPDATES, Message::from_raw::<ClusterUpdate>)?;

        let connect_config = ConnectConfig {
            name: device_name.into(),
            device_type: DeviceType::Computer,
            initial_volume,
            emit_set_queue_events: true,
            ..ConnectConfig::default()
        };
        // Spirc connects the session itself and stores reusable credentials.
        let (spirc, task) = Spirc::new(
            connect_config,
            session.clone(),
            credentials,
            player.clone(),
            mixer.clone(),
        )
        .await?;
        let spirc = Arc::new(spirc);

        let (closed_tx, closed) = oneshot::channel();
        tokio::spawn(async move {
            task.await;
            // The receiver is gone when the engine was dropped first.
            _ = closed_tx.send(());
        });

        let events = Events {
            player: player.get_player_event_channel(),
            cluster,
            closed: Some(closed),
            queue: None,
            device_id: session.device_id().into(),
            _player: player,
        };
        let (volume, settled) = watch::channel(initial_volume);
        tokio::spawn(publish_volume(spirc.clone(), settled));

        let engine = Self {
            session,
            spirc,
            mixer,
            volume,
        };
        Ok((engine, events))
    }

    pub fn api(&self) -> Api {
        Api::new(self.session.clone())
    }

    pub fn username(&self) -> String {
        self.session.username()
    }

    /// URI of the user's Liked Songs, usable as a playback context.
    pub fn liked_songs_uri(&self) -> String {
        format!("spotify:user:{}:collection", self.session.username())
    }

    /// Plays a playlist, album, artist or collection on this computer.
    pub fn play_context(
        &self,
        uri: &str,
        start: Start,
        shuffle: bool,
        repeat: Repeat,
    ) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: 0,
            context_options: Some(LoadContextOptions::Options(Options {
                shuffle,
                repeat: repeat == Repeat::Context,
                repeat_track: repeat == Repeat::Track,
            })),
            playing_track: match start {
                Start::Beginning => None,
                Start::Index(index) => Some(PlayingTrack::Index(index)),
                Start::Track(id) => Some(PlayingTrack::Uri(Kind::Track.uri(id))),
            },
        };
        // Loads are ignored unless this device is the active one.
        self.spirc.activate()?;
        self.spirc
            .load(LoadRequest::from_context_uri(uri.into(), options))?;
        Ok(())
    }

    /// Plays an ad-hoc list of tracks, such as search results, from `index`.
    pub fn play_tracks(&self, tracks: &[Id], index: u32, repeat: Repeat) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: 0,
            context_options: Some(LoadContextOptions::Options(Options {
                shuffle: false,
                repeat: repeat == Repeat::Context,
                repeat_track: repeat == Repeat::Track,
            })),
            playing_track: Some(PlayingTrack::Index(index)),
        };
        let uris = tracks.iter().map(|&id| Kind::Track.uri(id)).collect();
        self.spirc.activate()?;
        self.spirc.load(LoadRequest::from_tracks(uris, options))?;
        Ok(())
    }

    /// Queues a track to play after the current one and anything already queued.
    pub fn add_to_queue(&self, track: Id) -> Result<()> {
        Ok(self.spirc.add_to_queue(SpotifyUri::Track { id: track })?)
    }

    /// Removes the tracks the user queued, keeping those of the context.
    pub fn clear_queue(&self) -> Result<()> {
        Ok(self.spirc.clear_queue()?)
    }

    pub fn play(&self) -> Result<()> {
        Ok(self.spirc.play()?)
    }

    pub fn pause(&self) -> Result<()> {
        Ok(self.spirc.pause()?)
    }

    pub fn next(&self) -> Result<()> {
        Ok(self.spirc.next()?)
    }

    pub fn previous(&self) -> Result<()> {
        Ok(self.spirc.prev()?)
    }

    pub fn seek(&self, position_ms: u32) -> Result<()> {
        Ok(self.spirc.set_position_ms(position_ms)?)
    }

    pub fn set_shuffle(&self, shuffle: bool) -> Result<()> {
        Ok(self.spirc.shuffle(shuffle)?)
    }

    pub fn set_repeat(&self, repeat: Repeat) -> Result<()> {
        self.spirc.repeat(repeat == Repeat::Context)?;
        self.spirc.repeat_track(repeat == Repeat::Track)?;
        Ok(())
    }

    /// Sets the volume, in `0.0..=1.0`, and remembers it for the next start.
    ///
    /// The change is audible at once; other devices learn of it when the
    /// value stops moving, so dragging a slider costs one update.
    pub fn set_volume(&self, volume: f32) {
        let volume = (volume.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16;
        self.mixer.set_volume(volume);
        self.volume.send_replace(volume);
    }
}

/// Forwards the volume to Connect once it has been stable for [`VOLUME_SETTLE`].
async fn publish_volume(spirc: Arc<Spirc>, mut volume: watch::Receiver<u16>) {
    while volume.changed().await.is_ok() {
        loop {
            match tokio::time::timeout(VOLUME_SETTLE, volume.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return,
                Err(_) => break,
            }
        }
        let settled = *volume.borrow_and_update();
        if spirc.set_volume(settled).is_err() {
            return;
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Unregisters the Connect device; fails only if the task already ended.
        _ = self.spirc.shutdown();
    }
}

pub struct Events {
    player: PlayerEventChannel,
    cluster: ClusterUpdates,
    closed: Option<oneshot::Receiver<()>>,
    queue: Option<Queue>,
    device_id: Box<str>,
    // Keeps the event channel open for as long as events are being read.
    _player: Arc<Player>,
}

impl Events {
    /// Waits for the next event; `None` after [`Event::Disconnected`].
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            let closed = self.closed.as_mut()?;
            tokio::select! {
                _ = closed => {
                    self.closed = None;
                    return Some(Event::Disconnected);
                }
                event = self.player.recv() => match player_event(event?) {
                    // The queue is re-sent with every state update; most are unchanged.
                    Some(Event::Queue(queue)) if self.queue.as_ref() == Some(&queue) => {}
                    Some(Event::Queue(queue)) => {
                        self.queue = Some(queue.clone());
                        return Some(Event::Queue(queue));
                    }
                    Some(event) => return Some(event),
                    None => {}
                },
                Some(update) = self.cluster.next() => {
                    if let Ok(update) = update {
                        return Some(Event::Devices(devices(&update, &self.device_id)));
                    }
                }
            }
        }
    }
}

fn player_event(event: PlayerEvent) -> Option<Event> {
    Some(match event {
        // `Loading` carries the id that was requested, which is the one the
        // queue and playlists use; `TrackChanged` reports a relinked id.
        PlayerEvent::Loading { track_id, .. } => Event::Track(Id::try_from(&track_id).ok()?),
        PlayerEvent::Playing { position_ms, .. } => Event::Playback(Playback {
            playing: true,
            position_ms,
        }),
        PlayerEvent::Paused { position_ms, .. } => Event::Playback(Playback {
            playing: false,
            position_ms,
        }),
        PlayerEvent::PositionChanged { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. }
        | PlayerEvent::Seeked { position_ms, .. } => Event::Position(position_ms),
        PlayerEvent::Stopped { .. } => Event::Stopped,
        PlayerEvent::Unavailable { .. } => Event::Unavailable,
        PlayerEvent::VolumeChanged { volume } => {
            Event::Volume(f32::from(volume) / f32::from(u16::MAX))
        }
        PlayerEvent::ShuffleChanged { shuffle } => Event::Shuffle(shuffle),
        PlayerEvent::RepeatChanged { context, track } => Event::Repeat(match (track, context) {
            (true, _) => Repeat::Track,
            (false, true) => Repeat::Context,
            (false, false) => Repeat::Off,
        }),
        PlayerEvent::SetQueue {
            context_uri,
            current_track,
            next_tracks,
            ..
        } => Event::Queue(Queue {
            context_uri: context_uri.into(),
            current: current_track.and_then(|track| Kind::Track.id(&track.uri)),
            next: next_tracks
                .iter()
                .filter_map(|track| {
                    Some(QueueItem {
                        track: Kind::Track.id(&track.uri)?,
                        queued: track.provider == QUEUE_PROVIDER,
                    })
                })
                .collect(),
        }),
        _ => return None,
    })
}

fn devices(update: &ClusterUpdate, local_device: &str) -> Vec<Device> {
    let cluster = &update.cluster;
    let mut devices: Vec<Device> = cluster
        .device
        .iter()
        .map(|(id, info)| Device {
            id: id.as_str().into(),
            name: info.name.as_str().into(),
            is_active: *id == cluster.active_device_id,
            is_local: id == local_device,
        })
        .collect();
    devices.sort_by(|a, b| {
        b.is_local
            .cmp(&a.is_local)
            .then_with(|| a.name.cmp(&b.name))
    });
    devices
}

fn rejects_credentials(error: &librespot_core::Error) -> bool {
    matches!(
        error.kind,
        ErrorKind::Unauthenticated | ErrorKind::PermissionDenied
    )
}
