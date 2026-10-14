//! Developer tool exercising the engine against a real account.
//!
//!   cli library | home | shows | episodes | show <id> | lyrics <track id> | save-test track|album|artist|show <id> | playlist-test | delete-playlist <id> | playlist <id> | album <id> | artist <id> | search <query>
//!   cli watch [seconds]     prints Connect events without taking over playback
//!   cli listen <context uri> [seconds]     plays and prints what happens
//!   cli track-end <context uri> <ms>     starts the second track at <ms> and waits for the next
//!   cli play <context uri> [seconds]     plays, queues a track, skips and shuffles
//!   cli remote [seconds]     pauses, resumes and takes over whatever another device plays
//!   cli progress | reset-progress <episode id>     shows or clears where an episode was left
//!   cli episode <episode id> [seconds]     plays an episode from where it was left, then records where it got
//!   cli episode-end <episode id>     plays an episode's last seconds, which marks it played
//!
//! Put `silent` first to discard audio instead of playing it: `cli silent play <uri>`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_playback::{
    SAMPLES_PER_SECOND,
    audio_backend::{Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

use vivace_core::{
    Config, Engine, Event, Events, Playback, RemoteCommand, RemotePlay, Repeat, Start,
    model::{Id, ItemRef, Kind, Progress, Track},
};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> Fallible<()> {
    // SAFETY: nothing has started a thread yet.
    unsafe { vivace_core::limit_player_threads() };
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Fallible<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
    // A device of its own, so it can run next to the app and other copies.
    let mut config = Config {
        persistent_device: false,
        ..Config::default()
    };
    if args.first() == Some(&"silent") {
        args.remove(0);
        config.sink = Some(Arc::new(|| Box::new(Silence::default())));
    }

    let started = Instant::now();
    let (engine, mut events) = Engine::connect(config).await?;
    let api = engine.api();
    println!(
        "signed in as {} in {:?}",
        engine.username(),
        started.elapsed()
    );

    let started = Instant::now();
    match args.as_slice() {
        ["library"] => {
            let (playlists, saved, artists) =
                tokio::try_join!(api.playlists(), api.saved(), api.followed_artists())?;
            println!("{} playlists:", playlists.len());
            for playlist in &playlists {
                println!(
                    "  {} | {} | {} tracks | by {}{} | image {:?}",
                    playlist.id,
                    playlist.name,
                    playlist.length,
                    playlist.owner,
                    if playlist.collaborative {
                        " | collaborative"
                    } else {
                        ""
                    },
                    playlist.image
                );
            }
            println!(
                "{} liked tracks, {} saved albums, {} followed artists",
                saved.tracks.len(),
                saved.albums.len(),
                artists.len()
            );
            let liked: Vec<Id> = saved.tracks.iter().take(5).map(|saved| saved.id).collect();
            print_tracks(
                &api.tracks(&liked)
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>(),
            );
        }
        ["home"] => {
            for shelf in api.home().await? {
                println!("{} ({} items)", shelf.title, shelf.items.len());
                for item in shelf.items.iter().take(3) {
                    println!("  {item:?}");
                }
            }
        }
        ["lyrics", id] => match api.lyrics(Id::from_base62(id)?).await? {
            Some(lyrics) => {
                println!(
                    "synced={} provider={} lines={}",
                    lyrics.synced,
                    lyrics.provider,
                    lyrics.lines.len()
                );
                for line in lyrics.lines.iter().take(6) {
                    println!("  {:>6} {}", line.start_ms, line.text);
                }
            }
            None => println!("no lyrics"),
        },
        [
            "save-test",
            kind @ ("track" | "album" | "artist" | "show"),
            id,
        ] => {
            // Saves and removes an item that is not in the library, leaving it as it was.
            let id = Id::from_base62(id)?;
            let kind = *kind;
            let present = || async {
                Ok::<_, vivace_core::Error>(match kind {
                    "track" => api.saved().await?.tracks.iter().any(|saved| saved.id == id),
                    "album" => api.saved().await?.albums.iter().any(|saved| saved.id == id),
                    "show" => api.saved_shows().await?.iter().any(|saved| saved.id == id),
                    _ => api
                        .followed_artists()
                        .await?
                        .iter()
                        .any(|saved| saved.id == id),
                })
            };
            let api = &api;
            let set = |saved| async move {
                match kind {
                    "track" => api.set_saved(Kind::Track, id, saved).await,
                    "album" => api.set_saved(Kind::Album, id, saved).await,
                    "show" => api.set_show_saved(id, saved).await,
                    _ => api.set_followed(id, saved).await,
                }
            };
            if present().await? {
                return Err("that item is already in the library; pick another".into());
            }
            set(true).await?;
            println!("after saving: in library = {}", present().await?);
            set(false).await?;
            println!("after removing: in library = {}", present().await?);
        }
        ["show", id] => {
            let details = api.show(Id::from_base62(id)?).await?;
            println!("{:?}", details.show);
            for episode in details.episodes.iter().take(6) {
                println!(
                    "  published {} cover {:?}",
                    episode.published_at, episode.track.cover
                );
                print_tracks(std::slice::from_ref(&episode.track));
            }
            println!("{} episodes", details.episodes.len());
        }
        ["episode-end", id] => {
            let episode = ItemRef::episode(Id::from_base62(id)?);
            let track = api
                .items(&[episode])
                .await?
                .pop()
                .flatten()
                .ok_or("no such episode")?;
            engine.play_tracks(&[episode], 0, Repeat::Off)?;
            let deadline = tokio::time::sleep(Duration::from_secs(15));
            tokio::pin!(deadline);
            let mut sought = false;
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    event = events.recv() => match event {
                        Some(Event::Playback(Playback { playing: true, .. })) if !sought => {
                            sought = true;
                            println!("-> seek to 4 s before the end");
                            engine.seek(track.duration_ms.saturating_sub(4_000))?;
                        }
                        Some(event @ (Event::Item(_) | Event::Playback(_) | Event::Stopped)) => println!("{event:?}"),
                        Some(_) => {}
                        None => break,
                    },
                }
            }
            println!("recorded {:?}", api.episode_progress(episode.id).await?);
        }
        ["episodes"] => {
            for saved in api.saved_episodes().await? {
                println!("{} {:?}", Kind::Episode.uri(saved.id), saved.added_at);
            }
        }
        ["progress", id] => {
            println!("{:?}", api.episode_progress(Id::from_base62(id)?).await?);
        }
        ["reset-progress", id] => {
            api.set_episode_progress(Id::from_base62(id)?, Progress::NotStarted)
                .await?;
            println!(
                "now {:?}",
                api.episode_progress(Id::from_base62(id)?).await?
            );
        }
        ["episode", id, rest @ ..] => {
            let seconds = rest.first().map_or(Ok(20), |seconds| seconds.parse())?;
            let episode = ItemRef::episode(Id::from_base62(id)?);
            engine.play_tracks(&[episode], 0, Repeat::Off)?;
            watch(&engine, &mut events, seconds, false).await?;
            // As the app does when it closes mid-episode.
            engine.save_progress().await;
            println!("recorded {:?}", api.episode_progress(episode.id).await?);
        }
        ["shows"] => {
            let saved = api.saved_shows().await?;
            let ids: Vec<Id> = saved.iter().map(|saved| saved.id).collect();
            for show in api.shows(&ids).await?.into_iter().flatten() {
                println!(
                    "  {} | {} | {} | {:?}",
                    show.id, show.name, show.publisher, show.image
                );
            }
        }
        ["playlist-test"] => {
            // Creates a playlist, edits it and deletes it again.
            let tracks = [
                ItemRef::track(Id::from_base62("4uLU6hMCjMI75M1A2tKUQC")?),
                ItemRef::track(Id::from_base62("69kOkLUCkxIZYexIgSG8rq")?),
            ];
            let playlist = api.create_playlist("Vivace test").await?;
            println!(
                "created spotify:playlist:{playlist} (delete it by hand if a later step fails)"
            );
            let in_library = |playlists: Vec<vivace_core::model::Playlist>| {
                playlists.iter().any(|listed| listed.id == playlist)
            };
            println!("in library: {}", in_library(api.playlists().await?));

            api.add_to_playlist(playlist, &tracks).await?;
            let details = api.playlist(playlist).await?;
            println!(
                "after adding two: {:?} has {} tracks",
                details.playlist.name,
                details.entries.len()
            );

            api.remove_from_playlist(playlist, &tracks[..1]).await?;
            let entries = api.playlist(playlist).await?.entries;
            println!(
                "after removing one: {} left, the other one kept: {}",
                entries.len(),
                entries.first().is_some_and(|entry| entry.item == tracks[1])
            );

            api.remove_playlist(playlist).await?;
            println!(
                "after deleting, in library: {}",
                in_library(api.playlists().await?)
            );
        }
        ["delete-playlist", id] => {
            api.remove_playlist(Id::from_base62(id)?).await?;
            println!("removed from the library");
        }
        ["playlist", id] => {
            let details = api.playlist(Id::from_base62(id)?).await?;
            println!("{:?}", details.playlist);
            println!(
                "{} entries after {:?}",
                details.entries.len(),
                started.elapsed()
            );
            let items: Vec<ItemRef> = details.entries.iter().map(|entry| entry.item).collect();
            let tracks: Vec<Track> = api.items(&items).await?.into_iter().flatten().collect();
            println!(
                "{} tracks resolved after {:?}",
                tracks.len(),
                started.elapsed()
            );
            print_tracks(&tracks[..tracks.len().min(8)]);
        }
        ["album", id] => {
            let details = api.album(Id::from_base62(id)?).await?;
            println!("{:?} | label {}", details.album, details.label);
            print_tracks(&details.tracks);
        }
        ["artist", id] => {
            let details = api.artist(Id::from_base62(id)?).await?;
            println!("{:?}", details.artist);
            print_tracks(&details.top_tracks);
            for (title, albums) in [
                ("albums", &details.albums),
                ("singles", &details.singles),
                ("compilations", &details.compilations),
                ("appears on", &details.appears_on),
            ] {
                println!("{} {title}:", albums.len());
                for album in albums.iter().take(4) {
                    println!(
                        "  {} | {} | {:?} {}",
                        album.id, album.name, album.kind, album.year
                    );
                }
            }
        }
        ["search", query] => {
            let results = api.search(query, 10).await?;
            print_tracks(&results.tracks);
            for artist in &results.artists {
                println!(
                    "  artist {} | {} | {:?}",
                    artist.id, artist.name, artist.image
                );
            }
            for album in &results.albums {
                println!(
                    "  album {} | {} | {:?} {}",
                    album.id, album.name, album.kind, album.year
                );
            }
            for playlist in &results.playlists {
                println!(
                    "  playlist {} | {} | by {}",
                    playlist.id, playlist.name, playlist.owner
                );
            }
        }
        ["watch", rest @ ..] => {
            let seconds = rest.first().map_or(Ok(5), |seconds| seconds.parse())?;
            watch(&engine, &mut events, seconds, false).await?;
        }
        ["remote", rest @ ..] => {
            let seconds = rest.first().map_or(Ok(20), |seconds| seconds.parse())?;
            remote(&engine, &mut events, seconds).await?;
        }
        ["track-end", uri, position] => {
            // Starts near the end of the second track to see the next one follow on.
            engine.play_context(uri, Start::Index(1), false, Repeat::Off)?;
            let position: u32 = position.parse()?;
            let deadline = tokio::time::sleep(Duration::from_secs(30));
            tokio::pin!(deadline);
            let mut sought = false;
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    event = events.recv() => match event {
                        // Seeking while the track is still loading is ignored, so wait until it plays.
                        Some(Event::Position(played)) if played > 3000 && !sought => {
                            sought = true;
                            println!("-> seek to {position} ms");
                            engine.seek(position)?;
                        }
                        Some(event @ (Event::Item(_) | Event::Playback(_) | Event::Stopped | Event::Position(_))) => println!("{event:?}"),
                        Some(_) => {}
                        None => break,
                    },
                }
            }
        }
        ["listen", uri, rest @ ..] => {
            let seconds = rest.first().map_or(Ok(20), |seconds| seconds.parse())?;
            engine.play_context(uri, Start::Beginning, false, Repeat::Off)?;
            watch(&engine, &mut events, seconds, false).await?;
        }
        ["play", uri, rest @ ..] => {
            let seconds = rest.first().map_or(Ok(20), |seconds| seconds.parse())?;
            engine.play_context(uri, Start::Beginning, false, Repeat::Off)?;
            watch(&engine, &mut events, seconds, true).await?;
        }
        _ => return Err("see the usage at the top of examples/cli.rs".into()),
    }
    println!("done in {:?}", started.elapsed());

    if let Some(usage) = memory_stats::memory_stats() {
        println!("rss: {:.1} MB", usage.physical_mem as f64 / 1_048_576.0);
    }
    Ok(())
}

/// Discards audio at playback speed, so tracks last as long as they would
/// through a sound card.
#[derive(Default)]
struct Silence {
    started: Option<Instant>,
    samples: u64,
}

impl Sink for Silence {
    fn stop(&mut self) -> SinkResult<()> {
        *self = Self::default();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        let started = *self.started.get_or_insert_with(Instant::now);
        self.samples += packet.samples().map_or(0, |samples| samples.len() as u64);
        let due =
            started + Duration::from_secs_f64(self.samples as f64 / f64::from(SAMPLES_PER_SECOND));
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        Ok(())
    }
}

/// Every few seconds acts on whatever another device is playing: pauses and
/// resumes it, starts an album and a track list on it, queues a track, and
/// finally moves its playback to this computer.
async fn remote(engine: &Engine, events: &mut Events, seconds: u64) -> Fallible<()> {
    let api = engine.api();
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut steps = tokio::time::interval(Duration::from_secs(4));
    steps.tick().await;
    let mut step = 0;
    let mut device: Option<Box<str>> = None;
    loop {
        tokio::select! {
                    _ = &mut deadline => return Ok(()),
                    _ = steps.tick() => {
                        // The last step runs after the takeover, when nothing is remote any more.
                        if device.is_none() && step != 6 {
                            continue;
                        }
                        let device = device.as_deref().unwrap_or_default();
                        step += 1;
                        match step {
                            1 => {
                                println!("-> pause the remote device");
                                api.remote(device, RemoteCommand::Pause).await?;
                            }
                            2 => {
                                println!("-> resume the remote device");
                                api.remote(device, RemoteCommand::Resume).await?;
                            }
                            3 => {
                                println!("-> play an album there, from its third track");
                                let play = RemotePlay::Context {
                                    uri: "spotify:album:4m2880jivSbbyEGAKfITCa",
                                    start: Start::Item(ItemRef::track(Id::from_base62("0oks4FnzhNp5QPTZtoet7c")?)),
                                };
                                api.remote_play(device, play, false, Repeat::Off).await?;
                            }
                            4 => {
                                println!("-> queue a track there");
                                let queued = ItemRef::track(Id::from_base62("4uLU6hMCjMI75M1A2tKUQC")?);
                        api.remote_queue(device, queued).await?;
                            }
                            5 => {
                                println!("-> play a list of two tracks there, from the first");
                                let tracks = [
                            ItemRef::track(Id::from_base62("69kOkLUCkxIZYexIgSG8rq")?),
                            ItemRef::track(Id::from_base62("4uLU6hMCjMI75M1A2tKUQC")?),
                        ];
        let play = RemotePlay::Tracks { tracks: &tracks, index: 0 };
                                api.remote_play(device, play, false, Repeat::Off).await?;
                            }
                            6 => {
                                println!("-> take over playback");
                                engine.transfer_here()?;
                            }
                            7 => {
                                println!("-> skip, to see that the queue came along");
                                engine.next()?;
                            }
                            _ => {}
                        }
                    }
                    event = events.recv() => match event {
                        Some(Event::Connect(connect)) => match connect.remote {
                            Some(remote) => {
                                println!(
                                    "remote: playing={} position={} track={:?} context={} next={} queued={}",
                                    remote.playing,
                                    remote.position_ms,
                                    remote.queue.current,
                                    remote.queue.context_uri,
                                    remote.queue.next.len(),
                                    remote.queue.next.iter().filter(|item| item.queued).count(),
                                );
                                device = Some(remote.device);
                            }
                            None => {
                                println!("connect: nothing plays elsewhere");
                                device = None;
                            }
                        },
                        Some(Event::Position(_) | Event::Queue(_)) => {}
                        Some(event) => println!("{event:?}"),
                        None => return Ok(()),
                    },
                }
    }
}

/// Prints events for `seconds`, optionally skipping to the next track once.
async fn watch(engine: &Engine, events: &mut Events, seconds: u64, skip: bool) -> Fallible<()> {
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut skipped = !skip;
    loop {
        tokio::select! {
            _ = &mut deadline => return Ok(()),
            event = events.recv() => match event {
                Some(Event::Position(position_ms)) if position_ms > 4000 && !skipped => {
                    skipped = true;
                    println!("-> queue a track, skip to it, then shuffle");
                    engine.add_to_queue(ItemRef::track(Id::from_base62("4uLU6hMCjMI75M1A2tKUQC")?))?;
                    engine.next()?;
                    engine.set_shuffle(true)?;
                }
                Some(Event::Connect(connect)) => {
                    for device in &connect.devices {
                        println!(
                            "device: {} ({:?}){}{}",
                            device.name,
                            device.kind,
                            if device.is_active { " [active]" } else { "" },
                            if device.is_local { " [this computer]" } else { "" },
                        );
                    }
                    if let Some(remote) = &connect.remote {
                        println!("remote: {remote:?}");
                    }
                }
                Some(Event::Queue(queue)) => println!(
                    "queue: context {} | current {:?} | {} next ({} queued by hand), first {:?}",
                    queue.context_uri,
                    queue.current,
                    queue.next.len(),
                    queue.next.iter().filter(|item| item.queued).count(),
                    queue.next.iter().take(3).map(|item| item.item.id).collect::<Vec<_>>()
                ),
                Some(event) => println!("{event:?}"),
                None => return Ok(()),
            },
        }
    }
}

fn print_tracks(tracks: &[Track]) {
    for track in tracks {
        let artists: Vec<&str> = track.artists.iter().map(|artist| &*artist.name).collect();
        println!(
            "  {} | {} | {} | {} | {}s{}{}",
            track.id,
            track.name,
            artists.join(", "),
            track.album.name,
            track.duration_ms / 1000,
            if track.explicit { " | explicit" } else { "" },
            if track.playable { "" } else { " | unplayable" },
        );
    }
}
