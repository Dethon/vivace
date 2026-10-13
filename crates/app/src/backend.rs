//! The engine's thread: owns the session and answers the UI's requests.

use std::time::Duration;

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vivace_core::{
    Api, Config, Engine, Event, Playback, Queue, Repeat, Start,
    model::{Id, Image, Kind, Playlist, Track},
};

use crate::{
    content::{Page, PageData},
    images::{self, ImageKey},
    media::MediaCommand,
};

const RECONNECT_DELAY: Duration = Duration::from_secs(5);
// Upcoming tracks resolved for the queue panel.
const QUEUE_LENGTH: usize = 50;

pub enum Request {
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
        tracks: Vec<Id>,
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
    AddToQueue(Id),
    SetLiked {
        track: Id,
        liked: bool,
    },
}

pub enum Update {
    Connecting,
    Connected,
    /// Signing in failed; another attempt follows shortly.
    Failed(String),
    Library {
        playlists: Vec<Playlist>,
        liked: Vec<Id>,
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
    /// Liking or unliking a track failed and must be undone on screen.
    /// A media key or the system's player controls were used.
    Media(MediaCommand),
    LikeFailed {
        track: Id,
        liked: bool,
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

/// Starts the backend thread and returns the channel for talking to it.
pub fn spawn(config: Config, updates: UnboundedSender<Update>) -> UnboundedSender<Request> {
    let (requests, receiver) = unbounded_channel();
    std::thread::Builder::new()
        .name("backend".into())
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime starts")
                .block_on(run(config, receiver, updates));
        })
        .expect("backend thread starts");
    requests
}

/// Keeps a session alive until the UI goes away, reconnecting as needed.
async fn run(
    config: Config,
    mut requests: UnboundedReceiver<Request>,
    updates: UnboundedSender<Update>,
) {
    loop {
        if updates.send(Update::Connecting).is_err() {
            return;
        }
        let config = Config {
            device_name: config.device_name.clone(),
            sink: config.sink.clone(),
        };
        match Engine::connect(config).await {
            Ok((engine, mut events)) => {
                let session = Session {
                    api: engine.api(),
                    liked_uri: engine.liked_songs_uri(),
                    engine,
                    updates: updates.clone(),
                    queue_sequence: 0,
                };
                if session.serve(&mut requests, &mut events).await.is_break() {
                    return;
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
}

impl Session {
    /// Serves requests until the session drops (`Continue`) or the UI closes (`Break`).
    async fn serve(
        mut self,
        requests: &mut UnboundedReceiver<Request>,
        events: &mut vivace_core::Events,
    ) -> std::ops::ControlFlow<()> {
        use std::ops::ControlFlow::{Break, Continue};

        if self.updates.send(Update::Connected).is_err() {
            return Break(());
        }
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(request) => self.handle(request),
                    None => return Break(()),
                },
                event = events.recv() => match event {
                    Some(Event::Disconnected) | None => return Continue(()),
                    Some(event) => self.forward(event),
                },
            }
        }
    }

    fn handle(&self, request: Request) {
        let result = match request {
            Request::Library => {
                self.spawn(|api, _| async move {
                    match tokio::try_join!(api.playlists(), api.saved()) {
                        Ok((playlists, saved)) => Update::Library {
                            playlists,
                            liked: saved.tracks.iter().map(|saved| saved.id).collect(),
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
            Request::SetLiked { track, liked } => {
                self.spawn(move |api, _| async move {
                    let failure = api.set_saved(Kind::Track, track, liked).await.err()?;
                    log::warn!("couldn't update Liked Songs: {failure}");
                    Some(Update::LikeFailed { track, liked })
                });
                Ok(())
            }
            Request::PlayContext {
                uri,
                start,
                shuffle,
                repeat,
            } => self.engine.play_context(&uri, start, shuffle, repeat),
            Request::PlayTracks {
                tracks,
                index,
                repeat,
            } => self.engine.play_tracks(&tracks, index, repeat),
            Request::Play => self.engine.play(),
            Request::Pause => self.engine.pause(),
            Request::Next => self.engine.next(),
            Request::Previous => self.engine.previous(),
            Request::Seek(position_ms) => self.engine.seek(position_ms),
            Request::Shuffle(shuffle) => self.engine.set_shuffle(shuffle),
            Request::Repeat(repeat) => self.engine.set_repeat(repeat),
            Request::Volume(volume) => {
                self.engine.set_volume(volume);
                Ok(())
            }
            Request::AddToQueue(track) => self.engine.add_to_queue(track),
        };
        if let Err(error) = result {
            self.send(Update::Notice(error.to_string()));
        }
    }

    fn forward(&mut self, event: Event) {
        match event {
            Event::Track(id) => self.spawn(move |api, _| async move {
                match api
                    .tracks(&[id])
                    .await
                    .map(|mut tracks| tracks.pop().flatten())
                {
                    Ok(Some(track)) => Some(Update::NowPlaying(track)),
                    Ok(None) => None,
                    Err(error) => Some(Update::Notice(error.to_string())),
                }
            }),
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
            // Device switching is not built yet.
            Event::Devices(_) | Event::Disconnected => {}
        }
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
    let ids: Vec<Id> = std::iter::once(current)
        .chain(upcoming.iter().map(|item| item.track))
        .collect();
    let mut resolved = api.tracks(&ids).await.ok()?.into_iter();

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
