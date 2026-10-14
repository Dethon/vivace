use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use librespot_protocol::connect::{Cluster, ClusterUpdate};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    Api, Connect, Result, auth,
    model::{Id, ItemRef, Kind},
    progress::{self, Recorder, StartPositions},
};

const CLUSTER_UPDATES: &str = "hm://connect-state/v1/cluster";
const DEFAULT_VOLUME: u16 = u16::MAX / 2;
const VOLUME_SETTLE: Duration = Duration::from_millis(300);
/// How long closing waits for the last episode position to be recorded.
const FINAL_SAVE_TIMEOUT: Duration = Duration::from_secs(2);
// Context librespot gives a list of tracks loaded without one.
const TRACK_LIST_URI: &str = "spotify:web-api";
// Provider Spotify assigns to tracks the user queued by hand.
pub(crate) const QUEUE_PROVIDER: &str = "queue";

type ClusterUpdates = futures_util::stream::BoxStream<
    'static,
    std::result::Result<ClusterUpdate, librespot_core::Error>,
>;

/// Creates the audio sink on the player thread.
pub type SinkFactory = Arc<dyn Fn() -> Box<dyn Sink> + Send + Sync>;

/// The stream quality to ask for; all are Ogg Vorbis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// 96 kbps.
    Low,
    /// 160 kbps.
    Normal,
    /// 320 kbps, the most librespot can stream.
    #[default]
    VeryHigh,
}

#[derive(Clone)]
pub struct Config {
    /// Name shown in Spotify's device picker.
    pub device_name: String,
    pub quality: Quality,
    /// Even out loudness between tracks.
    pub normalise: bool,
    /// Keep the same Connect device id across runs. Tools that run next to
    /// the app turn this off so they show up as a device of their own.
    pub persistent_device: bool,
    /// Replaces the system's default audio output, e.g. to discard audio in tools.
    pub sink: Option<SinkFactory>,
}

impl Config {
    /// The name this computer has in Spotify's device lists unless the user
    /// picks one: the app's with the computer's, so that each computer
    /// running it can be told apart.
    pub fn default_device_name() -> String {
        match auth::machine_name() {
            machine if machine.is_empty() => "Vivace".into(),
            machine => format!("Vivace ({machine})"),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: Self::default_device_name(),
            quality: Quality::default(),
            normalise: false,
            persistent_device: true,
            sink: None,
        }
    }
}

/// Where playback of a context starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    Beginning,
    Index(u32),
    Item(ItemRef),
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueItem {
    pub item: ItemRef,
    /// Added by the user rather than coming from the playing context.
    pub queued: bool,
}

/// What this computer is playing and will play next.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Queue {
    /// Empty for a list of tracks played without a context.
    pub context_uri: Box<str>,
    pub current: Option<ItemRef>,
    /// Upcoming tracks and episodes; local files are omitted.
    pub next: Vec<QueueItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The local player moved to another track or episode.
    Item(ItemRef),
    Playback(Playback),
    Position(u32),
    Stopped,
    Unavailable,
    /// Volume in `0.0..=1.0`.
    Volume(f32),
    Shuffle(bool),
    Repeat(Repeat),
    Queue(Queue),
    /// The account's devices, or playback on another one, changed.
    Connect(Connect),
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
    recorder: Recorder,
    /// Counts loads, so one that waited for a resume point can tell it was
    /// overtaken by a newer one.
    loads: Arc<AtomicU64>,
}

impl Engine {
    /// Signs in, opening the browser when no valid credentials are stored,
    /// and registers this computer as a Connect device.
    pub async fn connect(config: Config) -> Result<(Self, Events)> {
        let sink = match config.sink.clone() {
            Some(sink) => sink,
            None => {
                let backend = audio_backend::find(None).ok_or(crate::Error::NoAudioBackend)?;
                Arc::new(move || backend(None, AudioFormat::default()))
            }
        };

        let cache = auth::cache()?;
        let stored = {
            let cache = cache.clone();
            // Keyring backends block, some on their own event loop.
            tokio::task::spawn_blocking(move || auth::stored_credentials(&cache)).await
        };
        if let Ok(Some(credentials)) = stored {
            match Self::start(&config, sink.clone(), cache.clone(), credentials).await {
                Err(crate::Error::Spotify(error)) if rejects_credentials(&error) => {
                    log::warn!("stored credentials were rejected, signing in again: {error}");
                }
                result => return result,
            }
        }
        let credentials = auth::browser_login().await?;
        Self::start(&config, sink, cache, credentials).await
    }

    async fn start(
        config: &Config,
        sink: SinkFactory,
        cache: Cache,
        credentials: Credentials,
    ) -> Result<(Self, Events)> {
        let initial_volume = cache.volume().unwrap_or(DEFAULT_VOLUME);
        let mut session_config = SessionConfig::default();
        if config.persistent_device
            && let Some(device_id) = auth::device_id(&session_config.device_id)
        {
            session_config.device_id = device_id;
        }
        let session = Session::new(session_config, Some(cache));

        let mixer = mixer::find(None).ok_or(crate::Error::NoAudioBackend)?(MixerConfig::default())?;
        let player_config = PlayerConfig {
            bitrate: match config.quality {
                Quality::Low => Bitrate::Bitrate96,
                Quality::Normal => Bitrate::Bitrate160,
                Quality::VeryHigh => Bitrate::Bitrate320,
            },
            normalisation: config.normalise,
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

        let (own_clusters_tx, own_clusters) = mpsc::unbounded_channel();
        let start_positions = StartPositions::default();
        let connect_config = ConnectConfig {
            name: config.device_name.clone(),
            device_type: DeviceType::Computer,
            initial_volume,
            emit_set_queue_events: true,
            cluster_listener: Some(own_clusters_tx),
            start_positions: Some(start_positions.clone()),
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
        // The session has just stored reusable credentials in its cache.
        tokio::task::spawn_blocking(auth::protect_credentials);

        let (closed_tx, closed) = oneshot::channel();
        tokio::spawn(async move {
            task.await;
            // The receiver is gone when the engine was dropped first.
            _ = closed_tx.send(());
        });

        let recorder = Recorder::start(
            Api::new(session.clone()),
            spirc.clone(),
            player.get_player_event_channel(),
            start_positions,
        );
        let events = Events {
            player: player.get_player_event_channel(),
            cluster,
            own_clusters,
            closed: Some(closed),
            queue: None,
            current: None,
            pending: None,
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
            recorder,
            loads: Arc::default(),
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

    /// Plays a playlist, album, artist or collection on this computer. An
    /// episode starts where the user left it.
    pub fn play_context(
        &self,
        uri: &str,
        start: Start,
        shuffle: bool,
        repeat: Repeat,
    ) -> Result<()> {
        let uri = uri.to_string();
        let episode = match start {
            Start::Item(item) => episode(item),
            Start::Beginning | Start::Index(_) => None,
        };
        self.load(episode, move |seek_to| {
            let options = LoadRequestOptions {
                start_playing: true,
                seek_to,
                context_options: Some(LoadContextOptions::Options(Options {
                    shuffle,
                    repeat: repeat == Repeat::Context,
                    repeat_track: repeat == Repeat::Track,
                })),
                playing_track: match start {
                    Start::Beginning => None,
                    Start::Index(index) => Some(PlayingTrack::Index(index)),
                    Start::Item(item) => Some(PlayingTrack::Uri(item.uri())),
                },
            };
            LoadRequest::from_context_uri(uri, options)
        })
    }

    /// Plays an ad-hoc list of tracks, such as search results, from `index`.
    /// An episode starts where the user left it.
    pub fn play_tracks(&self, items: &[ItemRef], index: u32, repeat: Repeat) -> Result<()> {
        let episode = items.get(index as usize).copied().and_then(episode);
        let uris: Vec<String> = items.iter().map(|item| item.uri()).collect();
        self.load(episode, move |seek_to| {
            let options = LoadRequestOptions {
                start_playing: true,
                seek_to,
                context_options: Some(LoadContextOptions::Options(Options {
                    shuffle: false,
                    repeat: repeat == Repeat::Context,
                    repeat_track: repeat == Repeat::Track,
                })),
                playing_track: Some(PlayingTrack::Index(index)),
            };
            LoadRequest::from_tracks(uris, options)
        })
    }

    /// Loads what `request` describes, starting at the given position. A
    /// starting episode is first looked up so it begins where it was left,
    /// unless another load comes in meanwhile.
    fn load(
        &self,
        episode: Option<Id>,
        request: impl FnOnce(u32) -> LoadRequest + Send + 'static,
    ) -> Result<()> {
        // Loads are ignored unless this device is the active one.
        self.spirc.activate()?;
        let load = self.loads.fetch_add(1, Ordering::Relaxed) + 1;
        let Some(episode) = episode else {
            return Ok(self.spirc.load(request(0))?);
        };
        let (api, spirc, loads) = (self.api(), self.spirc.clone(), self.loads.clone());
        tokio::spawn(async move {
            let seek_to = progress::resume_point(&api, episode).await.unwrap_or(0);
            if loads.load(Ordering::Relaxed) == load
                && let Err(error) = spirc.load(request(seek_to))
            {
                log::warn!("couldn't start the episode: {error}");
            }
        });
        Ok(())
    }

    /// Records where the playing episode is, for closing the app.
    pub async fn save_progress(&self) {
        if tokio::time::timeout(FINAL_SAVE_TIMEOUT, self.recorder.flush())
            .await
            .is_err()
        {
            log::warn!("couldn't record the episode position in time");
        }
    }

    /// Queues a track to play after the current one and anything already queued.
    pub fn add_to_queue(&self, item: ItemRef) -> Result<()> {
        let uri = match item.kind {
            Kind::Episode => SpotifyUri::Episode { id: item.id },
            _ => SpotifyUri::Track { id: item.id },
        };
        Ok(self.spirc.add_to_queue(uri)?)
    }

    /// Removes the tracks the user queued, keeping those of the context.
    pub fn clear_queue(&self) -> Result<()> {
        Ok(self.spirc.clear_queue()?)
    }

    /// Moves playback from whichever device has it to this computer.
    pub fn transfer_here(&self) -> Result<()> {
        Ok(self.spirc.transfer(None)?)
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
    // Cluster changes caused by other devices, and those answering our own.
    cluster: ClusterUpdates,
    own_clusters: mpsc::UnboundedReceiver<Cluster>,
    closed: Option<oneshot::Receiver<()>>,
    queue: Option<Queue>,
    /// The item last announced with [`Event::Item`].
    current: Option<ItemRef>,
    /// An event held back while the item it belongs to is announced first.
    pending: Option<Event>,
    device_id: Box<str>,
    // Keeps the event channel open for as long as events are being read.
    _player: Arc<Player>,
}

impl Events {
    /// Waits for the next event; `None` after [`Event::Disconnected`].
    pub async fn recv(&mut self) -> Option<Event> {
        if let Some(event) = self.pending.take() {
            return Some(event);
        }
        loop {
            let closed = self.closed.as_mut()?;
            tokio::select! {
                _ = closed => {
                    self.closed = None;
                    return Some(Event::Disconnected);
                }
                event = self.player.recv() => match self.announce(event?) {
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
                        return Some(self.connect(&update.cluster));
                    }
                }
                Some(cluster) = self.own_clusters.recv() => return Some(self.connect(&cluster)),
            }
        }
    }
}

impl Events {
    /// Converts a player event, first announcing the item it is about when
    /// that changed. A track that was preloaded, as at every gapless
    /// change, starts with `Playing` and no `Loading`.
    fn announce(&mut self, event: PlayerEvent) -> Option<Event> {
        let item = match &event {
            PlayerEvent::Loading { track_id, .. }
            | PlayerEvent::Playing { track_id, .. }
            | PlayerEvent::Paused { track_id, .. } => ItemRef::from_uri(&track_id.to_uri()),
            _ => None,
        };
        let converted = player_event(event);
        match item {
            Some(item) if self.current != Some(item) => {
                self.current = Some(item);
                // `Loading` converts to the announcement itself.
                self.pending = converted.filter(|event| *event != Event::Item(item));
                Some(Event::Item(item))
            }
            _ => converted,
        }
    }

    fn connect(&self, cluster: &Cluster) -> Event {
        Event::Connect(Connect::from_cluster(cluster, &self.device_id))
    }
}

fn player_event(event: PlayerEvent) -> Option<Event> {
    Some(match event {
        // `Loading` carries the id that was requested, which is the one the
        // queue and playlists use; `TrackChanged` reports a relinked id.
        PlayerEvent::Loading { track_id, .. } => {
            Event::Item(ItemRef::from_uri(&track_id.to_uri())?)
        }
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
            // librespot names a plain list of tracks after the Web API.
            context_uri: if context_uri == TRACK_LIST_URI {
                Box::default()
            } else {
                context_uri.into()
            },
            current: current_track.and_then(|track| ItemRef::from_uri(&track.uri)),
            next: next_tracks
                .iter()
                .filter_map(|track| {
                    Some(QueueItem {
                        item: ItemRef::from_uri(&track.uri)?,
                        queued: track.provider == QUEUE_PROVIDER,
                    })
                })
                .collect(),
        }),
        _ => return None,
    })
}

fn rejects_credentials(error: &librespot_core::Error) -> bool {
    matches!(
        error.kind,
        ErrorKind::Unauthenticated | ErrorKind::PermissionDenied
    )
}

fn episode(item: ItemRef) -> Option<Id> {
    (item.kind == Kind::Episode).then_some(item.id)
}
