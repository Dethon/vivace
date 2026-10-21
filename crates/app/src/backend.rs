//! The engine's thread: owns the session and answers the UI's requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vivace_core::{
    Api, Config, Connect, Engine, Event, LeftOff, Playback, Queue, RemoteCommand, RemotePlay,
    Repeat, Resume, Start,
    model::{Chapter, Id, Image, ItemRef, Kind, Lyrics, Progress, Saved, Track},
};

use crate::{
    content::{EpisodeOrder, Page, PageData, Saveable},
    images::{self, ImageKey},
    library,
    media::MediaCommand,
    updater::Found,
};

const RECONNECT_DELAY: Duration = Duration::from_secs(5);
// Playback cut by a dropped connection plays on by itself if the next
// session comes this soon after it was last seen playing; after longer, such
// as waking from sleep, it waits.
const RESUME_PLAYING_WITHIN: Duration = Duration::from_secs(60);
// How soon after being resumed a speaker that drops out is resumed again:
// Soloist stops a second after a resume that follows a short pause.
const RESUME_DROPOUT: Duration = Duration::from_secs(3);
// Episodes whose progress is looked up at once while finding where the user
// left off in a podcast; the newest few usually tell.
const PROGRESS_LOOKUPS: usize = 10;
const WORKER_THREADS: usize = 2;
const BLOCKING_THREADS: usize = 4;
const BLOCKING_THREAD_IDLE: Duration = Duration::from_secs(2);
// Upcoming tracks resolved for the queue panel.
const QUEUE_LENGTH: usize = 50;

pub enum Request {
    /// Reconnects with a different quality, normalisation or device name.
    Configure(Config),
    /// Forgets the saved login and starts signing in again.
    SignOut,
    Library,
    Page {
        token: u64,
        page: Page,
    },
    Image(ImageKey),
    /// Artwork for a playlist without a picture of its own.
    PlaylistCover(Id),
    PlayContext {
        uri: String,
        start: Start,
        shuffle: bool,
        repeat: Repeat,
    },
    PlayTracks {
        tracks: Vec<ItemRef>,
        index: u32,
        repeat: Repeat,
    },
    /// Plays a podcast from where the user left off among the `episodes`
    /// its page lists.
    PlayShow {
        uri: String,
        episodes: Vec<ItemRef>,
        order: EpisodeOrder,
    },
    Play,
    Pause,
    Next,
    Previous,
    Seek(u32),
    Shuffle(bool),
    Repeat(Repeat),
    Volume(f32),
    AddToQueue(ItemRef),
    Lyrics(Id),
    /// Chapters of an episode.
    Chapters(Id),
    /// Adds a track to a playlist; without one, to a new playlist called `name`.
    AddToPlaylist {
        playlist: Option<Id>,
        name: String,
        track: ItemRef,
    },
    RemoveFromPlaylist {
        playlist: Id,
        track: ItemRef,
    },
    RenamePlaylist {
        playlist: Id,
        name: String,
    },
    /// Takes a playlist out of the library, which deletes one the user owns.
    RemovePlaylist {
        playlist: Id,
        name: String,
    },
    /// Moves playback to a device; `local` when it is this computer.
    Transfer {
        device: Box<str>,
        local: bool,
    },
    SetSaved {
        item: Saveable,
        saved: bool,
    },
    /// How far the user got in an episode.
    EpisodeProgress(Id),
    /// Marks an episode played or unplayed.
    SetProgress {
        episode: Id,
        progress: Progress,
    },
    /// Records the playing episode's position and ends the session, for
    /// closing the app.
    Quit,
}

pub enum Update {
    Connecting,
    Connected {
        username: String,
    },
    /// The saved login was forgotten; signing in starts over.
    SignedOut,
    /// Signing in failed; another attempt follows shortly.
    Failed(String),
    Library {
        contents: library::Contents,
        liked: Vec<Id>,
        /// Episodes in Your Episodes.
        episodes: Vec<Id>,
    },
    /// Something was saved to or removed from the library, which is worth
    /// listing again.
    LibraryChanged,
    Page {
        token: u64,
        result: Result<PageData, String>,
    },
    Image {
        key: ImageKey,
        pixels: Option<SharedPixelBuffer<Rgba8Pixel>>,
    },
    PlaylistCover {
        playlist: Id,
        image: Option<Image>,
    },
    NowPlaying(Track),
    Playback(Playback),
    Position(u32),
    Stopped,
    Volume(f32),
    Shuffle(bool),
    Repeat(Repeat),
    Queue(ResolvedQueue),
    /// The account's devices, or playback on another one, changed.
    Connect(Connect),
    /// Lyrics for a track; `None` when it has none.
    Lyrics {
        track: Id,
        lyrics: Option<Lyrics>,
    },
    /// An episode's chapters; empty when it has none.
    Chapters {
        episode: Id,
        chapters: Vec<Chapter>,
    },
    /// A playlist was edited; `notice` says how.
    PlaylistChanged {
        playlist: Id,
        notice: String,
    },
    /// A playlist left the library.
    PlaylistRemoved {
        playlist: Id,
        notice: String,
    },
    /// A media key or the system's player controls were used.
    Media(MediaCommand),
    EpisodeProgress {
        episode: Id,
        progress: Progress,
    },
    /// The episode a podcast's Play button starts at, to be played in the
    /// podcast's context.
    ShowStart {
        uri: String,
        episode: ItemRef,
    },
    /// Saving or removing an item failed and must be undone on screen.
    SaveFailed {
        item: Saveable,
        saved: bool,
    },

    Notice(String),
    /// What a check for a new version of Vivace found; `asked` when the
    /// user started it.
    Release {
        found: Found,
        asked: bool,
    },
}

pub struct ResolvedQueue {
    /// Increases with every queue change, so late answers can be dropped.
    pub sequence: u64,
    pub context_uri: Box<str>,
    /// The current track followed by the upcoming ones.
    pub tracks: Vec<Track>,
    /// How many tracks after the current one were queued by hand.
    pub queued: usize,
}

/// Starts the backend thread and returns the channel for talking to it,
/// with one that tells when the thread has finished.
pub fn spawn(
    config: Config,
    updates: UnboundedSender<Update>,
) -> (UnboundedSender<Request>, std::sync::mpsc::Receiver<()>) {
    let (requests, receiver) = unbounded_channel();
    let (finished_tx, finished) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("backend".into())
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(WORKER_THREADS)
                // Blocking work is image decoding and name lookups: a few
                // threads keep up, and idle ones are let go quickly.
                .max_blocking_threads(BLOCKING_THREADS)
                .thread_keep_alive(BLOCKING_THREAD_IDLE)
                .enable_all()
                .build()
                .expect("tokio runtime starts")
                .block_on(run(config, receiver, updates));
            _ = finished_tx.send(());
        })
        .expect("backend thread starts");
    (requests, finished)
}

/// How a session ended.
enum Ended {
    /// The UI is gone.
    Closed,
    /// The connection dropped; try again after a pause.
    Lost(Lost),
    /// The user changed the configuration or signed out; reconnect at once.
    Restart {
        /// The new configuration; none after signing out.
        config: Option<Config>,
        /// What was playing, if it should carry over.
        left_off: Option<LeftOff>,
    },
}

/// A session whose connection dropped, for the next one to carry on from.
struct Lost {
    session_id: String,
    /// When this computer was last seen playing, if it still was then.
    playing_at: Option<SystemTime>,
    /// Where its playback stood, to start it again should Spotify no
    /// longer have it.
    left_off: Option<LeftOff>,
}

impl Lost {
    /// What the next session takes back, decided as it connects.
    fn resume(&self) -> Resume {
        Resume {
            session_id: self.session_id.clone(),
            // A clock set back counts as too long.
            play: self.playing_at.is_some_and(|at| {
                at.elapsed()
                    .is_ok_and(|elapsed| elapsed < RESUME_PLAYING_WITHIN)
            }),
            left_off: self.left_off.clone(),
        }
    }
}

/// Keeps a session alive until the UI goes away, reconnecting as needed.
async fn run(
    mut config: Config,
    mut requests: UnboundedReceiver<Request>,
    updates: UnboundedSender<Update>,
) {
    // The session whose connection dropped.
    let mut lost: Option<Lost> = None;
    // Playback to carry into the session a settings change starts.
    let mut left_off: Option<LeftOff> = None;
    // Shared by every session: the UI drops a queue numbered below the
    // last it showed, so a new session must not count from zero again.
    let queue_sequence = Arc::<AtomicU64>::default();
    loop {
        if updates.send(Update::Connecting).is_err() {
            return;
        }
        config.resume = lost.as_ref().map(Lost::resume);
        match Engine::connect(config.clone()).await {
            Ok((engine, mut events)) => {
                // Only the first session after a drop takes playback back.
                lost = None;
                if let Some(left_off) = left_off.take()
                    && let Err(error) = engine.pick_up(left_off)
                {
                    _ = updates.send(Update::Notice(error.to_string()));
                }
                let session = Session {
                    api: engine.api(),
                    liked_uri: engine.liked_songs_uri(),
                    engine,
                    updates: updates.clone(),
                    queue_sequence: queue_sequence.clone(),
                    remote: RemoteControl::default(),
                    commands: Arc::default(),
                    plays: Arc::default(),
                    remote_track: None,
                };
                match session.serve(&mut requests, &mut events).await {
                    Ended::Closed => return,
                    Ended::Lost(dropped) => lost = Some(dropped),
                    Ended::Restart {
                        config: changed,
                        left_off: carried,
                    } => {
                        left_off = carried;
                        if let Some(changed) = changed {
                            // Tools keep the sink they started with.
                            config = Config {
                                sink: config.sink.clone(),
                                ..changed
                            };
                        }
                        continue;
                    }
                }
            }
            Err(error) => {
                if updates.send(Update::Failed(error.to_string())).is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

struct Session {
    engine: Engine,
    api: Api,
    liked_uri: String,
    updates: UnboundedSender<Update>,
    /// Increases with every queue change, across sessions.
    queue_sequence: Arc<AtomicU64>,
    remote: RemoteControl,
    /// Counts the commands sent to other devices, so that a slow recovery
    /// gives way to whatever the user pressed since.
    commands: Arc<AtomicU64>,
    /// Counts the requests to play something, so that a podcast's starting
    /// episode, still being looked up, gives way to what was played since.
    plays: Arc<AtomicU64>,
    /// Track last announced for that device, to resolve each one once.
    remote_track: Option<ItemRef>,
}

impl Session {
    /// Serves requests until the session ends, and says why it did.
    async fn serve(
        mut self,
        requests: &mut UnboundedReceiver<Request>,
        events: &mut vivace_core::Events,
    ) -> Ended {
        let username = self.engine.username();
        if self.updates.send(Update::Connected { username }).is_err() {
            return Ended::Closed;
        }
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(Request::Quit) => {
                        self.engine.save_progress().await;
                        return Ended::Closed;
                    }
                    Some(Request::Configure(config)) => {
                        self.engine.save_progress().await;
                        return Ended::Restart {
                            config: Some(config),
                            left_off: self.engine.left_off(),
                        };
                    }
                    Some(Request::SignOut) => {
                        self.engine.save_progress().await;
                        // Keyring backends block.
                        _ = tokio::task::spawn_blocking(vivace_core::sign_out).await;
                        self.send(Update::SignedOut);
                        return Ended::Restart {
                            config: None,
                            left_off: None,
                        };
                    }
                    Some(request) => self.handle(request),
                    None => return Ended::Closed,
                },
                event = events.recv() => match event {
                    Some(Event::Disconnected) | None => {
                        return Ended::Lost(Lost {
                            session_id: self.engine.session_id(),
                            playing_at: self.engine.playing_at(),
                            left_off: self.engine.left_off(),
                        });
                    }
                    Some(event) => self.forward(event),
                },
            }
        }
    }

    /// Sends a transport command to whichever device is playing.
    fn transport(
        &mut self,
        remote: RemoteCommand,
        local: impl FnOnce(&Engine) -> vivace_core::Result<()>,
    ) -> vivace_core::Result<()> {
        let Some(device) = self.remote.transport_target(remote, Instant::now()) else {
            return local(&self.engine);
        };
        self.command_remote(device, remote);
        Ok(())
    }

    fn command_remote(&self, device: Box<str>, remote: RemoteCommand) {
        let commands = self.commands.clone();
        let this = commands.fetch_add(1, Ordering::Relaxed) + 1;
        self.spawn(move |api, _| async move {
            let latest = || commands.load(Ordering::Relaxed) == this;
            let mut result = api.remote(&device, remote).await;
            // A speaker that dropped out refuses commands until it takes
            // playback again.
            if result.is_err() && remote == RemoteCommand::Resume && latest() {
                result = api.transfer(&device).await;
                if result.is_ok() && latest() {
                    result = api.remote(&device, remote).await;
                }
            }
            failure_notice(result)
        });
    }

    fn handle(&mut self, request: Request) {
        if matches!(
            request,
            Request::PlayContext { .. } | Request::PlayTracks { .. } | Request::PlayShow { .. }
        ) {
            self.plays.fetch_add(1, Ordering::Relaxed);
        }
        let result = match request {
            // These end the session and are taken by `serve` before this.
            Request::Configure(_) | Request::SignOut | Request::Quit => Ok(()),
            Request::SetProgress { episode, progress } => {
                let updates = self.updates.clone();
                self.spawn(move |api, _| async move {
                    let error = api.set_episode_progress(episode, progress).await.err()?;
                    _ = updates.send(Update::Notice(format!(
                        "Couldn't mark the episode: {error}"
                    )));
                    // Shows the episode as Spotify still has it.
                    let progress = api.episode_progress(episode).await.ok()?;
                    Some(Update::EpisodeProgress { episode, progress })
                });
                Ok(())
            }
            Request::EpisodeProgress(episode) => {
                self.spawn(move |api, _| async move {
                    match api.episode_progress(episode).await {
                        Ok(progress) => Some(Update::EpisodeProgress { episode, progress }),
                        Err(error) => {
                            log::warn!("couldn't look up an episode's progress: {error}");
                            None
                        }
                    }
                });
                Ok(())
            }
            Request::Library => {
                self.spawn(|api, _| async move {
                    match load_library(&api).await {
                        Ok(update) => update,
                        Err(error) => {
                            Update::Notice(format!("Couldn't load your library: {error}"))
                        }
                    }
                });
                Ok(())
            }
            Request::Page { token, page } => {
                self.spawn(move |api, liked_uri| async move {
                    let result = page.load(&api, &liked_uri).await;
                    Update::Page {
                        token,
                        result: result.map_err(|error| error.to_string()),
                    }
                });
                Ok(())
            }
            Request::Image(key) => {
                self.spawn(move |api, _| async move {
                    let pixels = images::load(&api, &key).await;
                    Update::Image { key, pixels }
                });
                Ok(())
            }
            Request::PlaylistCover(playlist) => {
                self.spawn(move |api, _| async move {
                    let image = api.playlist_cover(playlist).await.ok().flatten();
                    Update::PlaylistCover { playlist, image }
                });
                Ok(())
            }
            Request::Lyrics(track) => {
                self.spawn(move |api, _| async move {
                    let lyrics = api.lyrics(track).await.unwrap_or_else(|error| {
                        log::warn!("couldn't load lyrics: {error}");
                        None
                    });
                    Update::Lyrics { track, lyrics }
                });
                Ok(())
            }
            Request::Chapters(episode) => {
                self.spawn(move |api, _| async move {
                    let chapters = api.chapters(episode).await.unwrap_or_else(|error| {
                        log::warn!("couldn't load chapters: {error}");
                        Vec::new()
                    });
                    Update::Chapters { episode, chapters }
                });
                Ok(())
            }
            Request::AddToPlaylist {
                playlist,
                name,
                track,
            } => {
                self.spawn(move |api, _| async move {
                    let result = async {
                        let playlist = match playlist {
                            Some(playlist) => playlist,
                            None => api.create_playlist(&name).await?,
                        };
                        api.add_to_playlist(playlist, &[track]).await?;
                        Ok::<_, vivace_core::Error>(playlist)
                    };
                    match result.await {
                        Ok(playlist) => Update::PlaylistChanged {
                            playlist,
                            notice: format!("Added to {name}"),
                        },
                        Err(error) => Update::Notice(format!("Couldn't add to {name}: {error}")),
                    }
                });
                Ok(())
            }
            Request::RenamePlaylist { playlist, name } => {
                self.spawn(move |api, _| async move {
                    match api.rename_playlist(playlist, &name).await {
                        Ok(()) => Update::PlaylistChanged {
                            playlist,
                            notice: format!("Renamed to {name}"),
                        },
                        Err(error) => {
                            Update::Notice(format!("Couldn't rename the playlist: {error}"))
                        }
                    }
                });
                Ok(())
            }
            Request::RemovePlaylist { playlist, name } => {
                self.spawn(move |api, _| async move {
                    match api.remove_playlist(playlist).await {
                        Ok(()) => Update::PlaylistRemoved {
                            playlist,
                            notice: format!("Removed {name}"),
                        },
                        Err(error) => Update::Notice(format!("Couldn't remove {name}: {error}")),
                    }
                });
                Ok(())
            }
            Request::RemoveFromPlaylist { playlist, track } => {
                self.spawn(move |api, _| async move {
                    match api.remove_from_playlist(playlist, &[track]).await {
                        Ok(()) => Update::PlaylistChanged {
                            playlist,
                            notice: "Removed from this playlist".into(),
                        },
                        Err(error) => Update::Notice(format!("Couldn't remove the track: {error}")),
                    }
                });
                Ok(())
            }
            Request::SetSaved { item, saved } => {
                self.spawn(move |api, _| async move {
                    let result = match item {
                        Saveable::Track(id) => api.set_saved(Kind::Track, id, saved).await,
                        Saveable::Episode(id) => api.set_episode_saved(id, saved).await,
                        Saveable::Album(id) => api.set_saved(Kind::Album, id, saved).await,
                        Saveable::Artist(id) => api.set_followed(id, saved).await,
                        Saveable::Show(id) => api.set_show_saved(id, saved).await,
                    };
                    match result {
                        // Albums, artists and podcasts are listed in the sidebar.
                        Ok(()) if !matches!(item, Saveable::Track(_) | Saveable::Episode(_)) => {
                            Some(Update::LibraryChanged)
                        }
                        Ok(()) => None,
                        Err(failure) => {
                            log::warn!("couldn't update the library: {failure}");
                            Some(Update::SaveFailed { item, saved })
                        }
                    }
                });
                Ok(())
            }
            Request::PlayContext {
                uri,
                start,
                shuffle,
                repeat,
            } => match self.remote.playing.clone() {
                // Playback stays on whichever device has it, as in Spotify's apps.
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        let play = RemotePlay::Context { uri: &uri, start };
                        let result = api.remote_play(&device, play, shuffle, repeat).await;
                        failure_notice(result)
                    });
                    Ok(())
                }
                None => self.engine.play_context(&uri, start, shuffle, repeat),
            },
            Request::PlayTracks {
                tracks,
                index,
                repeat,
            } => match self.remote.playing.clone() {
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        let play = RemotePlay::Tracks {
                            tracks: &tracks,
                            index,
                        };
                        let result = api.remote_play(&device, play, false, repeat).await;
                        failure_notice(result)
                    });
                    Ok(())
                }
                None => self.engine.play_tracks(&tracks, index, repeat),
            },
            Request::PlayShow {
                uri,
                episodes,
                order,
            } => {
                let plays = self.plays.clone();
                let this = plays.load(Ordering::Relaxed);
                let updates = self.updates.clone();
                self.spawn(move |api, _| async move {
                    let episode = show_start(&api, episodes, order, &updates).await?;
                    (plays.load(Ordering::Relaxed) == this)
                        .then_some(Update::ShowStart { uri, episode })
                });
                Ok(())
            }
            Request::Play => self.transport(RemoteCommand::Resume, Engine::play),
            Request::Pause => self.transport(RemoteCommand::Pause, Engine::pause),
            Request::Next => self.transport(RemoteCommand::Next, Engine::next),
            Request::Previous => self.transport(RemoteCommand::Previous, Engine::previous),
            Request::Seek(position_ms) => self
                .transport(RemoteCommand::Seek(position_ms), |engine| {
                    engine.seek(position_ms)
                }),
            Request::Shuffle(shuffle) => self
                .transport(RemoteCommand::Shuffle(shuffle), |engine| {
                    engine.set_shuffle(shuffle)
                }),
            Request::Repeat(repeat) => match self.remote.playing.clone() {
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        let context = RemoteCommand::RepeatContext(repeat.repeats_context());
                        let track = RemoteCommand::RepeatTrack(repeat.repeats_track());
                        let result = match api.remote(&device, context).await {
                            Ok(()) => api.remote(&device, track).await,
                            failed => failed,
                        };
                        failure_notice(result)
                    });
                    Ok(())
                }
                None => self.engine.set_repeat(repeat),
            },
            Request::Volume(volume) => {
                match self.remote.playing.clone() {
                    Some(device) => self.spawn(move |api, _| async move {
                        let result = api.remote_volume(&device, volume).await;
                        failure_notice(result)
                    }),
                    None => self.engine.set_volume(volume),
                }
                Ok(())
            }
            Request::AddToQueue(track) => match self.remote.playing.clone() {
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        match api.remote_queue(&device, track).await {
                            Ok(()) => Update::Notice("Added to queue".into()),
                            Err(error) => {
                                Update::Notice(format!("Couldn't queue the track: {error}"))
                            }
                        }
                    });
                    Ok(())
                }
                None => {
                    self.send(Update::Notice("Added to queue".into()));
                    self.engine.add_to_queue(track)
                }
            },
            Request::Transfer { device, local } => {
                if local {
                    self.engine.transfer_here()
                } else {
                    self.spawn(move |api, _| async move {
                        let error = api.transfer(&device).await.err()?;
                        Some(Update::Notice(format!("Couldn't switch device: {error}")))
                    });
                    Ok(())
                }
            }
        };
        if let Err(error) = result {
            self.send(Update::Notice(error.to_string()));
        }
    }

    fn forward(&mut self, event: Event) {
        match event {
            Event::Item(item) => self.announce(item),
            Event::Queue(queue) => {
                let sequence = self.next_queue_sequence();
                self.spawn(move |api, _| async move { resolve_queue(&api, queue, sequence).await });
            }
            Event::Playback(playback) => self.send(Update::Playback(playback)),
            Event::Position(position_ms) => self.send(Update::Position(position_ms)),
            Event::Stopped => self.send(Update::Stopped),
            Event::Unavailable => self.send(Update::Notice("This track is unavailable".into())),
            Event::Volume(volume) => self.send(Update::Volume(volume)),
            Event::Shuffle(shuffle) => self.send(Update::Shuffle(shuffle)),
            Event::Repeat(repeat) => self.send(Update::Repeat(repeat)),
            Event::Connect(connect) => self.connect_changed(connect),
            Event::Disconnected => {}
        }
    }

    fn connect_changed(&mut self, connect: Connect) {
        if let Some(device) = self.remote.follow(&connect, Instant::now()) {
            log::info!("{device} dropped out after being resumed; resuming it again");
            self.command_remote(device, RemoteCommand::Resume);
        }
        match &connect.remote {
            Some(remote) => {
                // The remote device's track and queue stand in for the local ones.
                if remote.queue.current != self.remote_track {
                    self.remote_track = remote.queue.current;
                    if let Some(track) = remote.queue.current {
                        self.announce(track);
                    }
                }
                let (queue, sequence) = (remote.queue.clone(), self.next_queue_sequence());
                self.spawn(move |api, _| async move { resolve_queue(&api, queue, sequence).await });
            }
            None => self.remote_track = None,
        }
        self.send(Update::Connect(connect));
    }

    fn next_queue_sequence(&self) -> u64 {
        self.queue_sequence.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Resolves a track or episode and reports it as the one playing.
    fn announce(&self, item: ItemRef) {
        self.spawn(move |api, _| async move {
            let resolved = api
                .items(&[item])
                .await
                .map(|mut tracks| tracks.pop().flatten());
            match resolved {
                Ok(Some(track)) => Some(Update::NowPlaying(track)),
                Ok(None) => None,
                Err(error) => Some(Update::Notice(error.to_string())),
            }
        });
    }

    /// Runs a request in the background and reports what it produces.
    fn spawn<F, U>(&self, work: impl FnOnce(Api, String) -> F + Send + 'static)
    where
        F: Future<Output = U> + Send,
        U: Into<Option<Update>>,
    {
        let (api, liked_uri, updates) = (
            self.api.clone(),
            self.liked_uri.clone(),
            self.updates.clone(),
        );
        tokio::spawn(async move {
            if let Some(update) = work(api, liked_uri).await.into() {
                // The UI is gone when this fails; nothing is left to tell.
                _ = updates.send(update);
            }
        });
    }

    fn send(&self, update: Update) {
        _ = self.updates.send(update);
    }
}

/// Which other device the player's controls command.
#[derive(Default)]
struct RemoteControl {
    /// The device that is playing, when it is not this computer.
    playing: Option<Box<str>>,
    /// The other device that played last, kept while no device is active:
    /// one can drop out for a moment, and the controls still mean it.
    last: Option<Box<str>>,
    /// The other device last resumed from here, and when.
    resumed: Option<(Box<str>, Instant)>,
}

impl RemoteControl {
    /// The device a transport command goes to; `None` for this computer.
    fn transport_target(&mut self, command: RemoteCommand, now: Instant) -> Option<Box<str>> {
        let device = self.playing.clone().or_else(|| self.last.clone())?;
        if command == RemoteCommand::Resume {
            self.resumed = Some((device.clone(), now));
        }
        Some(device)
    }

    /// Follows a change in the account's playback, and returns the device
    /// to resume again when it dropped out just after being resumed.
    fn follow(&mut self, connect: &Connect, now: Instant) -> Option<Box<str>> {
        self.playing = connect.remote.as_ref().map(|remote| remote.device.clone());
        if self.playing.is_some() {
            self.last = self.playing.clone();
        } else if connect.devices.iter().any(|device| device.is_active) {
            // This computer plays.
            self.last = None;
        } else if let Some((device, at)) = self.resumed.take()
            && now.duration_since(at) < RESUME_DROPOUT
            && self.last.as_ref() == Some(&device)
        {
            return Some(device);
        }
        None
    }
}

/// Tells the user why a command failed, if it did.
/// The episode a podcast's Play button starts at, given those its page
/// lists: where the user left off (see [`pick_up`]), or the page's first one
/// when there is nowhere to pick up. Reports each episode's progress on the
/// way, as rows show it.
async fn show_start(
    api: &Api,
    listed: Vec<ItemRef>,
    order: EpisodeOrder,
    updates: &UnboundedSender<Update>,
) -> Option<ItemRef> {
    let mut newest_first = listed.clone();
    if order == EpisodeOrder::OldestFirst {
        newest_first.reverse();
    }
    let mut known = Vec::with_capacity(newest_first.len());
    for chunk in newest_first.chunks(PROGRESS_LOOKUPS) {
        let ids: Vec<Id> = chunk.iter().map(|item| item.id).collect();
        for (&episode, progress) in ids.iter().zip(api.episodes_progress(&ids).await) {
            if let Some(progress) = progress {
                _ = updates.send(Update::EpisodeProgress { episode, progress });
            }
            // One that couldn't be looked up counts as not started.
            known.push(progress.unwrap_or_default());
        }
        let all = known.len() == newest_first.len();
        match pick_up(&known, all) {
            PickUp::At(index) => return Some(newest_first[index]),
            PickUp::Nowhere => break,
            PickUp::NeedOlder => {}
        }
    }
    listed.first().copied()
}

#[derive(Debug, PartialEq, Eq)]
enum PickUp {
    At(usize),
    Nowhere,
    /// The episodes known so far don't tell; older ones might.
    NeedOlder,
}

/// Where the user left off in a podcast, given how far they got in its
/// episodes, newest first; `all` when no older ones follow. That is the
/// newest episode they started, unless they finished it; then the one after
/// it; or, when there is none, the closest one before it not yet finished.
fn pick_up(newest_first: &[Progress], all: bool) -> PickUp {
    let undecided = if all {
        PickUp::Nowhere
    } else {
        PickUp::NeedOlder
    };
    let Some(heard) = newest_first
        .iter()
        .position(|progress| *progress != Progress::NotStarted)
    else {
        return undecided;
    };
    match newest_first[heard] {
        Progress::InProgress { .. } => PickUp::At(heard),
        _ if heard > 0 => PickUp::At(heard - 1),
        _ => newest_first
            .iter()
            .position(|progress| *progress != Progress::Completed)
            .map_or(undecided, PickUp::At),
    }
}

fn failure_notice(result: vivace_core::Result<()>) -> Option<Update> {
    result.err().map(|error| Update::Notice(error.to_string()))
}

async fn resolve_queue(api: &Api, queue: Queue, sequence: u64) -> Option<Update> {
    let current = queue.current?;
    let upcoming = &queue.next[..queue.next.len().min(QUEUE_LENGTH)];
    let items: Vec<ItemRef> = std::iter::once(current)
        .chain(upcoming.iter().map(|item| item.item))
        .collect();
    let mut resolved = api.items(&items).await.ok()?.into_iter();

    let mut tracks = vec![resolved.next().flatten()?];
    let mut queued = 0;
    for (track, item) in resolved.zip(upcoming) {
        if let Some(track) = track {
            queued += usize::from(item.queued);
            tracks.push(track);
        }
    }
    Some(Update::Queue(ResolvedQueue {
        sequence,
        context_uri: queue.context_uri,
        tracks,
        queued,
    }))
}

/// Everything Your Library lists, with what the sidebar shows of each.
async fn load_library(api: &Api) -> vivace_core::Result<Update> {
    let (rootlist, saved, artists, shows, episodes) = tokio::try_join!(
        api.rootlist(),
        api.saved(),
        api.followed_artists(),
        api.saved_shows(),
        api.saved_episodes()
    )?;
    let ids = |saved: &[Saved]| saved.iter().map(|saved| saved.id).collect::<Vec<Id>>();
    let (album_ids, artist_ids, show_ids) = (ids(&saved.albums), ids(&artists), ids(&shows));
    let (album_details, artist_details, show_details) = tokio::try_join!(
        api.albums(&album_ids),
        api.artists(&artist_ids),
        api.shows(&show_ids)
    )?;
    // Items Spotify no longer has come back empty and are left out.
    fn dated<T>(details: Vec<Option<T>>, saved: &[Saved]) -> Vec<(T, i64)> {
        details
            .into_iter()
            .zip(saved)
            .filter_map(|(item, saved)| Some((item?, saved.added_at)))
            .collect()
    }
    Ok(Update::Library {
        contents: library::Contents {
            rootlist,
            albums: dated(album_details, &saved.albums),
            artists: dated(artist_details, &artists),
            shows: dated(show_details, &shows),
        },
        liked: ids(&saved.tracks),
        episodes: available_episodes(api, &episodes).await,
    })
}

/// The saved episodes that still exist. Spotify keeps episodes taken down
/// since in the set; they can be neither shown nor played, so they are not
/// counted. Without metadata every episode counts.
async fn available_episodes(api: &Api, saved: &[Saved]) -> Vec<Id> {
    let items: Vec<ItemRef> = saved
        .iter()
        .map(|saved| ItemRef::episode(saved.id))
        .collect();
    match api.items(&items).await {
        Ok(found) => found
            .into_iter()
            .zip(saved)
            .filter_map(|(episode, saved)| episode.map(|_| saved.id))
            .collect(),
        Err(error) => {
            log::warn!("couldn't look up saved episodes: {error}");
            saved.iter().map(|saved| saved.id).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use vivace_core::{Device, DeviceKind, Remote};

    use super::*;

    const SPEAKER: &str = "speaker";

    fn playing_on(device: &str) -> Connect {
        Connect {
            devices: Vec::new(),
            remote: Some(Remote {
                device: device.into(),
                queue: Queue::default(),
                playing: true,
                position_ms: 0,
                duration_ms: 0,
                shuffle: false,
                repeat: Repeat::Off,
                volume: 1.0,
            }),
        }
    }

    fn playing_here() -> Connect {
        Connect {
            devices: vec![Device {
                id: "here".into(),
                name: "This computer".into(),
                kind: DeviceKind::Computer,
                is_active: true,
                is_local: true,
            }],
            remote: None,
        }
    }

    /// A speaker played, was resumed at `start`, and the account then said
    /// nothing was active.
    fn resumed_then_dropped(after: Duration) -> Option<Box<str>> {
        let start = Instant::now();
        let mut remote = RemoteControl::default();
        remote.follow(&playing_on(SPEAKER), start);
        remote.transport_target(RemoteCommand::Resume, start);
        remote.follow(&playing_on(SPEAKER), start);
        remote.follow(&Connect::default(), start + after)
    }

    const STARTED: Progress = Progress::InProgress { position_ms: 1000 };
    const NEW: Progress = Progress::NotStarted;
    const DONE: Progress = Progress::Completed;

    #[test]
    fn a_podcast_picks_up_after_the_newest_episode_heard() {
        assert_eq!(pick_up(&[NEW, NEW, DONE, NEW], true), PickUp::At(1));
        assert_eq!(pick_up(&[NEW, NEW, DONE, STARTED], false), PickUp::At(1));
    }

    #[test]
    fn a_podcast_resumes_the_newest_episode_started() {
        assert_eq!(pick_up(&[NEW, STARTED, DONE], true), PickUp::At(1));
        assert_eq!(pick_up(&[STARTED, DONE], true), PickUp::At(0));
    }

    #[test]
    fn a_podcast_heard_up_to_date_goes_back_to_the_closest_unheard_episode() {
        assert_eq!(pick_up(&[DONE, DONE, NEW, NEW], true), PickUp::At(2));
        assert_eq!(pick_up(&[DONE, DONE, STARTED], true), PickUp::At(2));
        assert_eq!(pick_up(&[DONE, DONE], false), PickUp::NeedOlder);
        assert_eq!(pick_up(&[DONE, DONE], true), PickUp::Nowhere);
    }

    #[test]
    fn a_podcast_never_heard_has_nowhere_to_pick_up() {
        assert_eq!(pick_up(&[NEW, NEW], false), PickUp::NeedOlder);
        assert_eq!(pick_up(&[NEW, NEW], true), PickUp::Nowhere);
        assert_eq!(pick_up(&[], true), PickUp::Nowhere);
    }

    #[test]
    fn controls_keep_commanding_a_speaker_that_dropped_out() {
        let now = Instant::now();
        let mut remote = RemoteControl::default();
        assert_eq!(remote.transport_target(RemoteCommand::Pause, now), None);
        remote.follow(&playing_on(SPEAKER), now);
        remote.follow(&Connect::default(), now);
        assert_eq!(
            remote
                .transport_target(RemoteCommand::Pause, now)
                .as_deref(),
            Some(SPEAKER)
        );
        remote.follow(&playing_here(), now);
        assert_eq!(remote.transport_target(RemoteCommand::Pause, now), None);
    }

    #[test]
    fn a_speaker_that_drops_out_just_after_resuming_is_resumed_again() {
        assert_eq!(
            resumed_then_dropped(Duration::from_secs(1)).as_deref(),
            Some(SPEAKER)
        );
    }

    #[test]
    fn a_speaker_that_drops_out_later_is_left_alone() {
        assert_eq!(resumed_then_dropped(RESUME_DROPOUT), None);
    }

    #[test]
    fn a_speaker_is_resumed_again_only_once() {
        let start = Instant::now();
        let mut remote = RemoteControl::default();
        remote.follow(&playing_on(SPEAKER), start);
        remote.transport_target(RemoteCommand::Resume, start);
        assert!(remote.follow(&Connect::default(), start).is_some());
        assert_eq!(remote.follow(&Connect::default(), start), None);
    }

    #[test]
    fn only_the_speaker_that_was_resumed_is_resumed_again() {
        let start = Instant::now();
        let mut remote = RemoteControl::default();
        remote.follow(&playing_on(SPEAKER), start);
        remote.transport_target(RemoteCommand::Resume, start);
        remote.follow(&playing_on("other"), start);
        assert_eq!(remote.follow(&Connect::default(), start), None);
    }
}
