//! The engine's thread: owns the session and answers the UI's requests.

use std::time::Duration;

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vivace_core::{
    Api, Config, Connect, Engine, Event, Playback, Queue, RemoteCommand, RemotePlay, Repeat, Start,
    model::{Id, Image, ItemRef, Kind, Lyrics, Playlist, Progress, Saved, Track},
};

use crate::{
    content::{Page, PageData, Saveable},
    images::{self, ImageKey},
    media::MediaCommand,
};

const RECONNECT_DELAY: Duration = Duration::from_secs(5);
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
        playlists: Vec<Playlist>,
        liked: Vec<Id>,
        /// Episodes in Your Episodes.
        episodes: Vec<Id>,
        /// Saved albums, followed artists and followed podcasts.
        saved: Vec<Id>,
    },
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
    /// Saving or removing an item failed and must be undone on screen.
    SaveFailed {
        item: Saveable,
        saved: bool,
    },

    Notice(String),
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
    Lost,
    /// The user changed the configuration or signed out; reconnect at once.
    Restart(Option<Config>),
}

/// Keeps a session alive until the UI goes away, reconnecting as needed.
async fn run(
    mut config: Config,
    mut requests: UnboundedReceiver<Request>,
    updates: UnboundedSender<Update>,
) {
    loop {
        if updates.send(Update::Connecting).is_err() {
            return;
        }
        match Engine::connect(config.clone()).await {
            Ok((engine, mut events)) => {
                let session = Session {
                    api: engine.api(),
                    liked_uri: engine.liked_songs_uri(),
                    engine,
                    updates: updates.clone(),
                    queue_sequence: 0,
                    remote: None,
                    remote_track: None,
                };
                match session.serve(&mut requests, &mut events).await {
                    Ended::Closed => return,
                    Ended::Lost => {}
                    Ended::Restart(changed) => {
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
    queue_sequence: u64,
    /// The device that is playing, when it is not this computer.
    remote: Option<Box<str>>,
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
                        return Ended::Restart(Some(config));
                    }
                    Some(Request::SignOut) => {
                        self.engine.save_progress().await;
                        // Keyring backends block.
                        _ = tokio::task::spawn_blocking(vivace_core::sign_out).await;
                        self.send(Update::SignedOut);
                        return Ended::Restart(None);
                    }
                    Some(request) => self.handle(request),
                    None => return Ended::Closed,
                },
                event = events.recv() => match event {
                    Some(Event::Disconnected) | None => return Ended::Lost,
                    Some(event) => self.forward(event),
                },
            }
        }
    }

    /// Sends a transport command to whichever device is playing.
    fn transport(
        &self,
        remote: RemoteCommand,
        local: impl FnOnce(&Engine) -> vivace_core::Result<()>,
    ) -> vivace_core::Result<()> {
        let Some(device) = self.remote.clone() else {
            return local(&self.engine);
        };
        self.spawn(move |api, _| async move {
            let result = api.remote(&device, remote).await;
            result.err().map(|error| Update::Notice(error.to_string()))
        });
        Ok(())
    }

    fn handle(&self, request: Request) {
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
                    let library = tokio::try_join!(
                        api.playlists(),
                        api.saved(),
                        api.followed_artists(),
                        api.saved_shows(),
                        api.saved_episodes()
                    );
                    match library {
                        Ok((playlists, saved, artists, shows, episodes)) => Update::Library {
                            playlists,
                            liked: saved.tracks.iter().map(|saved| saved.id).collect(),
                            episodes: available_episodes(&api, &episodes).await,
                            saved: saved
                                .albums
                                .iter()
                                .chain(&artists)
                                .chain(&shows)
                                .map(|saved| saved.id)
                                .collect(),
                        },
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
                    let failure = result.err()?;
                    log::warn!("couldn't update the library: {failure}");
                    Some(Update::SaveFailed { item, saved })
                });
                Ok(())
            }
            Request::PlayContext {
                uri,
                start,
                shuffle,
                repeat,
            } => match self.remote.clone() {
                // Playback stays on whichever device has it, as in Spotify's apps.
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        let play = RemotePlay::Context { uri: &uri, start };
                        let result = api.remote_play(&device, play, shuffle, repeat).await;
                        result.err().map(|error| Update::Notice(error.to_string()))
                    });
                    Ok(())
                }
                None => self.engine.play_context(&uri, start, shuffle, repeat),
            },
            Request::PlayTracks {
                tracks,
                index,
                repeat,
            } => match self.remote.clone() {
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        let play = RemotePlay::Tracks {
                            tracks: &tracks,
                            index,
                        };
                        let result = api.remote_play(&device, play, false, repeat).await;
                        result.err().map(|error| Update::Notice(error.to_string()))
                    });
                    Ok(())
                }
                None => self.engine.play_tracks(&tracks, index, repeat),
            },
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
            Request::Repeat(repeat) => match self.remote.clone() {
                Some(device) => {
                    self.spawn(move |api, _| async move {
                        let context = RemoteCommand::RepeatContext(repeat == Repeat::Context);
                        let track = RemoteCommand::RepeatTrack(repeat == Repeat::Track);
                        let result = match api.remote(&device, context).await {
                            Ok(()) => api.remote(&device, track).await,
                            failed => failed,
                        };
                        result.err().map(|error| Update::Notice(error.to_string()))
                    });
                    Ok(())
                }
                None => self.engine.set_repeat(repeat),
            },
            Request::Volume(volume) => {
                match self.remote.clone() {
                    Some(device) => self.spawn(move |api, _| async move {
                        let result = api.remote_volume(&device, volume).await;
                        result.err().map(|error| Update::Notice(error.to_string()))
                    }),
                    None => self.engine.set_volume(volume),
                }
                Ok(())
            }
            Request::AddToQueue(track) => match self.remote.clone() {
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
                        let result = api.transfer(&device).await;
                        result
                            .err()
                            .map(|error| Update::Notice(format!("Couldn't switch device: {error}")))
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
                self.queue_sequence += 1;
                let sequence = self.queue_sequence;
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
        self.remote = connect.remote.as_ref().map(|remote| remote.device.clone());
        match &connect.remote {
            Some(remote) => {
                // The remote device's track and queue stand in for the local ones.
                if remote.queue.current != self.remote_track {
                    self.remote_track = remote.queue.current;
                    if let Some(track) = remote.queue.current {
                        self.announce(track);
                    }
                }
                self.queue_sequence += 1;
                let (queue, sequence) = (remote.queue.clone(), self.queue_sequence);
                self.spawn(move |api, _| async move { resolve_queue(&api, queue, sequence).await });
            }
            None => self.remote_track = None,
        }
        self.send(Update::Connect(connect));
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
