//! Keeps Spotify's record of how far the user got in the episodes played on
//! this computer, so every device can pick up where they left off, and
//! starts episodes from there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_connect::Spirc;
use librespot_core::SpotifyUri;
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use tokio::sync::{mpsc, oneshot};

use crate::{
    Api,
    model::{Id, Kind, Progress},
};

/// How often the position of a playing episode is recorded.
const SAVE_INTERVAL: Duration = Duration::from_secs(30);
/// Positions closer to the start are neither recorded nor resumed.
pub(crate) const MIN_POSITION_MS: u32 = 2_000;
/// Smaller moves than this since the last record are not worth another.
const MIN_CHANGE_MS: u32 = 1_000;
/// How long a start waits to learn where the episode was left.
const LOOKUP_TIMEOUT: Duration = Duration::from_millis(1_500);
/// An episode found to have a resume point after it started is moved there
/// only while it is still this close to its beginning.
const RESUME_WINDOW_MS: u32 = 4_000;

/// Asks the recorder to store the current position at once.
#[derive(Clone)]
pub(crate) struct Recorder {
    flushes: mpsc::UnboundedSender<oneshot::Sender<()>>,
}

impl Recorder {
    pub fn start(api: Api, spirc: Arc<Spirc>, events: PlayerEventChannel) -> Self {
        let (flushes, flush_requests) = mpsc::unbounded_channel();
        tokio::spawn(run(api, spirc, events, flush_requests));
        Self { flushes }
    }

    /// Stores the position of the episode playing now, if any, and waits
    /// until Spotify has it.
    pub async fn flush(&self) {
        let (done, stored) = oneshot::channel();
        if self.flushes.send(done).is_ok() {
            // An error means the recorder had already ended.
            _ = stored.await;
        }
    }
}

/// The episode playing on this computer.
struct Listening {
    episode: Id,
    position_ms: u32,
    /// When `position_ms` was reported.
    at: Instant,
    playing: bool,
    /// The position last recorded.
    saved_ms: Option<u32>,
    /// Whether its resume point was looked up already.
    looked_up: bool,
}

impl Listening {
    fn new(episode: Id, position_ms: u32) -> Self {
        Self {
            episode,
            position_ms,
            at: Instant::now(),
            playing: false,
            saved_ms: None,
            looked_up: false,
        }
    }

    fn position_ms(&self) -> u32 {
        if self.playing {
            let elapsed = self.at.elapsed().as_millis().min(u32::MAX.into()) as u32;
            self.position_ms.saturating_add(elapsed)
        } else {
            self.position_ms
        }
    }

    fn moved(&mut self, position_ms: u32, playing: bool) {
        self.position_ms = position_ms;
        self.at = Instant::now();
        self.playing = playing;
    }

    /// The position to record, unless it is too early or barely changed.
    fn unsaved(&mut self) -> Option<u32> {
        let position_ms = self.position_ms();
        let changed = self
            .saved_ms
            .is_none_or(|saved| saved.abs_diff(position_ms) >= MIN_CHANGE_MS);
        (position_ms >= MIN_POSITION_MS && changed).then(|| {
            self.saved_ms = Some(position_ms);
            position_ms
        })
    }
}

/// Where an episode was left, found after it started.
struct Found {
    episode: Id,
    position_ms: u32,
}

async fn run(
    api: Api,
    spirc: Arc<Spirc>,
    mut events: PlayerEventChannel,
    mut flushes: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
) {
    let (found_tx, mut found) = mpsc::unbounded_channel();
    let mut ticks = tokio::time::interval(SAVE_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut current: Option<Listening> = None;
    loop {
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    // The player is gone; keep what was heard.
                    if let Some(listening) = &mut current {
                        save(&api, listening).await;
                    }
                    return;
                };
                if let Some(lookup) = handle(&api, &mut current, event) {
                    let (api, found) = (api.clone(), found_tx.clone());
                    tokio::spawn(async move {
                        if let Some(position_ms) = resume_point(&api, lookup).await {
                            _ = found.send(Found { episode: lookup, position_ms });
                        }
                    });
                }
            }
            _ = ticks.tick() => {
                if let Some(listening) = current.as_mut().filter(|listening| listening.playing) {
                    save_later(&api, listening);
                }
            }
            Some(Found { episode, position_ms }) = found.recv() => {
                // Only if the episode is still on and nobody moved it meanwhile.
                let untouched = current.as_ref().is_some_and(|listening| {
                    listening.episode == episode && listening.position_ms() < RESUME_WINDOW_MS
                });
                if untouched && let Err(error) = spirc.set_position_ms(position_ms) {
                    log::warn!("couldn't resume the episode: {error}");
                }
            }
            Some(done) = flushes.recv() => {
                if let Some(listening) = &mut current {
                    save(&api, listening).await;
                }
                _ = done.send(());
            }
        }
    }
}

/// Follows the player, recording positions as they become worth keeping.
/// Returns an episode whose resume point should be looked up because it
/// started from the beginning.
fn handle(api: &Api, current: &mut Option<Listening>, event: PlayerEvent) -> Option<Id> {
    let playing = matches!(event, PlayerEvent::Playing { .. });
    match event {
        PlayerEvent::Loading {
            track_id,
            position_ms,
            ..
        }
        | PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } if current.as_ref().map(|listening| listening.episode) != episode(&track_id) => {
            // Another item: the previous episode ends where it was.
            if let Some(previous) = current.as_mut() {
                save_later(api, previous);
            }
            *current = episode(&track_id).map(|id| Listening::new(id, position_ms));
            // A preloaded episode starts with `Playing` and no `Loading`.
            if let Some(listening) = current.as_mut() {
                return started(listening, playing, position_ms);
            }
            None
        }
        PlayerEvent::Playing { position_ms, .. } => {
            let listening = current.as_mut()?;
            started(listening, true, position_ms)
        }
        PlayerEvent::Paused { position_ms, .. } => {
            let listening = current.as_mut()?;
            listening.moved(position_ms, false);
            save_later(api, listening);
            None
        }
        PlayerEvent::Seeked { position_ms, .. } => {
            let listening = current.as_mut()?;
            let playing = listening.playing;
            listening.moved(position_ms, playing);
            save_later(api, listening);
            None
        }
        PlayerEvent::PositionChanged { position_ms, .. }
        | PlayerEvent::PositionCorrection { position_ms, .. } => {
            let listening = current.as_mut()?;
            let playing = listening.playing;
            listening.moved(position_ms, playing);
            None
        }
        PlayerEvent::EndOfTrack { track_id, .. } => {
            let listening =
                current.take_if(|listening| Some(listening.episode) == episode(&track_id))?;
            let api = api.clone();
            tokio::spawn(async move {
                record(&api, listening.episode, Progress::Completed).await;
            });
            None
        }
        PlayerEvent::Stopped { .. } => {
            if let Some(mut listening) = current.take() {
                save_later(api, &mut listening);
            }
            None
        }
        _ => None,
    }
}

/// Notes a start or resume of the current episode. The first time it plays
/// from the beginning, its resume point is worth looking up: starts this
/// computer begins itself carry the position already, but an episode the
/// context or queue moved on to does not.
fn started(listening: &mut Listening, playing: bool, position_ms: u32) -> Option<Id> {
    listening.moved(position_ms, playing);
    if !playing || listening.looked_up {
        return None;
    }
    listening.looked_up = true;
    (position_ms < MIN_POSITION_MS).then_some(listening.episode)
}

/// Records the position in the background.
fn save_later(api: &Api, listening: &mut Listening) {
    if let Some(position_ms) = listening.unsaved() {
        let (api, episode) = (api.clone(), listening.episode);
        tokio::spawn(async move {
            record(&api, episode, Progress::InProgress { position_ms }).await;
        });
    }
}

async fn save(api: &Api, listening: &mut Listening) {
    if let Some(position_ms) = listening.unsaved() {
        record(api, listening.episode, Progress::InProgress { position_ms }).await;
    }
}

async fn record(api: &Api, episode: Id, progress: Progress) {
    if let Err(error) = api.set_episode_progress(episode, progress).await {
        log::warn!("couldn't record how far the episode got: {error}");
    }
}

/// Where the user left an episode, if partway through it.
pub(crate) async fn resume_point(api: &Api, episode: Id) -> Option<u32> {
    match tokio::time::timeout(LOOKUP_TIMEOUT, api.episode_progress(episode)).await {
        Ok(Ok(Progress::InProgress { position_ms })) if position_ms >= MIN_POSITION_MS => {
            Some(position_ms)
        }
        Ok(Ok(_)) => None,
        Ok(Err(error)) => {
            log::warn!("couldn't look up where the episode was left: {error}");
            None
        }
        Err(_) => {
            log::warn!("looking up where the episode was left took too long");
            None
        }
    }
}

fn episode(uri: &SpotifyUri) -> Option<Id> {
    Kind::Episode.id(&uri.to_uri())
}
