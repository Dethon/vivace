//! Developer tool exercising the engine against a real account.
//!
//!   cli library | home | shows | episodes | folder-test | folder-setup | folder-cleanup <playlist id> | show <id> | lyrics <track id> | chapters <episode id> | save-test track|album|artist|show <id> | playlist-test | delete-playlist <id> | playlist <id> | album <id> | artist <id> | search <query>
//!   cli watch [seconds]     prints Connect events without taking over playback
//!   cli listen <context uri> [seconds]     plays and prints what happens
//!   cli track-end <context uri> <ms>     starts the second track at <ms> and waits for the next
//!   cli play <context uri> [seconds]     plays, queues a track, skips and shuffles
//!   cli queue-test <context uri> <track uri> <queued track uri>     plays the context, then the track on its own, then queues the other, printing each queue reported
//!   cli remote [seconds]     pauses, resumes and takes over whatever another device plays
//!   cli placeholder-test     has another client's play command start a plain list on this device, as Spotify sends it
//!   cli remote-pause <device id>     pauses another device
//!   cli remote-volume <device id> <0.0-1.0>     sets another device's volume
//!   cli transfer <device id>     moves playback to another device
//!   cli control-test <device id> [seconds]     plays Liked Songs, moves it to a device and pauses and resumes it there twice, `seconds` apart
//!   cli pull-test [seconds]     waits, then takes playback here as choosing this computer does, and prints what follows
//!   cli speaker-test <device id> [idle|play|next|context=<uri>|paused-context=<uri>]     takes the account's playback (unless idle), paused or playing, moves it to a device as the device picker does, prints Spotify's answer and gives it back; it commands a real device, so ask before pointing it at someone's speaker
//!   cli resolve <context uri>...     resolves contexts as librespot does before loading them
//!   cli item <track or episode uri>     prints a track's or episode's metadata
//!   cli progress | reset-progress <episode id>     shows or clears where an episode was left
//!   cli drop-test [episode id|paused]     plays Liked Songs or an episode, drops the connection as the server does, reconnects as the app does and checks that playback carries on (or, paused, that play starts it again where it stopped)
//!   cli idle-test [transfer|drop] [list]     plays Liked Songs (or, with `list`, its first tracks as a plain list), pauses, loses playback the way Spotify takes it from a device left paused, and checks that play (or choosing this computer, then play; or, once a dropped connection is back, play) starts it again where it was
//!   cli settings-test [paused]     plays Liked Songs, changes the volume level as the settings do (a new session) and checks that playback carries on, or stays paused, where it was
//!   cli options-test     plays an album silently to check volume levels, gapless playback and autoplay
//!   cli episode <episode id> [seconds]     plays an episode from where it was left, then records where it got
//!   cli episode-end <episode id> [next episode id | show <show id>]     plays an episode's last seconds, which marks it played, and goes on to the next (in its podcast, the one that follows in the podcast's context)
//!
//! `CLI_NAME` names the cli's device, to tell two copies apart.
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
    Config, Connect, Engine, Event, Events, Playback, Remote, RemoteCommand, RemotePlay, Repeat,
    Resume, Start, VolumeLevel,
    model::{Id, ItemRef, Kind, Progress, Track},
};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

// The folder the folder tests make and remove again.
const FOLDER_START: &str = "spotify:start-group:0f1c44e0a1b2c3d4:Vivace+test+folder";
const FOLDER_END: &str = "spotify:end-group:0f1c44e0a1b2c3d4";

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
        device_name: std::env::var("CLI_NAME").unwrap_or_else(|_| "Vivace cli".into()),
        persistent_device: false,
        ..Config::default()
    };
    if args.first() == Some(&"silent") {
        args.remove(0);
        config.sink = Some(Arc::new(|| Box::new(Silence::default())));
    }
    if args.first() == Some(&"options-test") {
        return options_test(config).await;
    }
    if args.first() == Some(&"settings-test") {
        return settings_test(config, args.get(1) == Some(&"paused")).await;
    }
    if args.first() == Some(&"idle-test") {
        let list = args.contains(&"list");
        let mode = args.get(1).copied().filter(|mode| *mode != "list");
        return idle_test(config, mode, list).await;
    }
    if let ["speaker-test", device, rest @ ..] = args.as_slice() {
        return speaker_test(config, device, rest.first().copied()).await;
    }
    if args.first() == Some(&"drop-test") {
        return match args.get(1) {
            Some(&"paused") => drop_paused_test(config).await,
            episode => drop_test(config, episode.copied()).await,
        };
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
        ["chapters", id] => {
            for chapter in api.chapters(Id::from_base62(id)?).await? {
                println!("  {:>8} {}", chapter.start_ms, chapter.title);
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
        ["resolve", uris @ ..] => {
            for uri in uris {
                match api.resolve_context(uri).await {
                    Ok(count) => println!("{uri}: {count} items"),
                    Err(error) => println!("{uri}: {error}"),
                }
            }
        }
        ["item", id] => {
            let item = ItemRef::from_uri(id).ok_or("not a track or episode uri")?;
            println!("{:?}", api.items(&[item]).await?);
        }
        ["show", id] => {
            let details = api.show(Id::from_base62(id)?).await?;
            println!("{:?}", details.show);
            for episode in details.episodes.iter().take(6) {
                println!(
                    "  number {} published {} cover {:?}",
                    episode.number, episode.published_at, episode.track.cover
                );
                print_tracks(std::slice::from_ref(&episode.track));
            }
            println!(
                "{} episodes{}",
                details.episodes.len(),
                if details.sequential { ", a series" } else { "" }
            );
        }
        ["episode-end", id, rest @ ..] => {
            let episode = ItemRef::episode(Id::from_base62(id)?);
            let track = api
                .items(&[episode])
                .await?
                .pop()
                .flatten()
                .ok_or("no such episode")?;
            match rest {
                // In its podcast's context, as a podcast page plays it.
                ["show", show] => engine.play_context(
                    &Kind::Show.uri(Id::from_base62(show)?),
                    Start::Item(episode),
                    false,
                    Repeat::Off,
                )?,
                // A second episode, if given, plays next.
                _ => {
                    let mut list = vec![episode];
                    for next in rest {
                        list.push(ItemRef::episode(Id::from_base62(next)?));
                    }
                    engine.play_tracks(&list, 0, Repeat::Off)?;
                }
            }
            let deadline = tokio::time::sleep(Duration::from_secs(15));
            tokio::pin!(deadline);
            let mut sought = false;
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    event = events.recv() => match event {
                        // Seeking while the episode is still loading is ignored.
                        Some(Event::Position(position_ms)) if position_ms > 1_000 && !sought => {
                            sought = true;
                            println!("-> seek to 4 s before the end");
                            engine.seek(track.duration_ms.saturating_sub(4_000))?;
                        }
                        Some(event @ (Event::Item(_) | Event::Playback(_) | Event::Stopped)) => println!("{event:?}"),
                        Some(Event::Position(position_ms)) if sought => println!("Position({position_ms})"),
                        Some(_) => {}
                        None => break,
                    },
                }
            }
            println!("recorded {:?}", api.episode_progress(episode.id).await?);
        }
        ["placeholder-test"] => {
            // Another client resuming a plain list names it with librespot's
            // placeholder, which Spotify can't resolve.
            let liked = api.saved().await?.tracks;
            let tracks: Vec<ItemRef> = liked
                .iter()
                .take(10)
                .map(|saved| ItemRef::track(saved.id))
                .collect();
            // Spotify knows the device only once it has registered.
            tokio::time::sleep(Duration::from_secs(3)).await;
            println!("-> asking this device to play a list named spotify:web-api");
            api.play_placeholder_list_here(&tracks).await?;
            let played = wait_for(&mut events, 20, |event| {
                matches!(event, Event::Playback(Playback { playing: true, .. }))
            })
            .await;
            match played {
                Some(_) => {
                    engine.pause()?;
                    println!("PASS: the list plays");
                }
                None => println!("FAIL: nothing played"),
            }
        }
        ["remote-pause", device] => {
            println!("{:?}", api.remote(device, RemoteCommand::Pause).await);
        }
        ["remote-volume", device, volume] => {
            api.remote_volume(device, volume.parse()?).await?;
            println!("set {device} to {volume}");
        }
        ["control-test", device, rest @ ..] => {
            // Hands playback to `device`, then pauses and resumes it there as
            // the player controls do, `wait` seconds apart, printing what the
            // account says between.
            let wait = rest.first().map_or(Ok(5), |wait| wait.parse())?;
            engine.play_context(
                &engine.liked_songs_uri(),
                Start::Index(1),
                false,
                Repeat::Off,
            )?;
            let mut connect = None;
            watch_connect(&mut events, &mut connect, 6).await;
            println!("-> transfer: {:?}", api.transfer(device).await);
            watch_connect(&mut events, &mut connect, wait).await;
            for command in [
                RemoteCommand::Pause,
                RemoteCommand::Resume,
                RemoteCommand::Pause,
                RemoteCommand::Resume,
            ] {
                println!("-> {command:?}: {:?}", api.remote(device, command).await);
                watch_connect(&mut events, &mut connect, wait).await;
            }
            watch_connect(&mut events, &mut connect, 5).await;
        }
        ["pull-test", rest @ ..] => {
            // Takes playback here as choosing this computer does, after
            // `seconds`, and prints what follows.
            let seconds = rest.first().map_or(Ok(3), |seconds| seconds.parse())?;
            let mut connect = None;
            watch_connect(&mut events, &mut connect, seconds).await;
            println!("-> choosing this computer");
            engine.transfer_here()?;
            watch_connect(&mut events, &mut connect, 15).await;
        }
        ["transfer", device] => {
            api.transfer(device).await?;
            println!("moved playback to {device}");
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
        ["folder-test"] => {
            // Adds an empty folder at the top of the library, reads it back
            // and removes it again.
            let (start, end) = (FOLDER_START, FOLDER_END);
            api.add_folder(start, end, 0, 0).await?;
            let rootlist = api.rootlist().await?;
            println!("folders: {:?}", rootlist.folders);
            api.remove_from_rootlist(&[start, end]).await?;
            println!(
                "after removing: {} folders",
                api.rootlist().await?.folders.len()
            );
        }
        ["folder-setup", rest @ ..] => {
            // A collaborative playlist inside a folder, left in place to look
            // at; `folder-cleanup <id>` removes both.
            let playlist = match rest.first() {
                Some(id) => Id::from_base62(id)?,
                None => api.create_playlist("Vivace test").await?,
            };
            println!("playlist spotify:playlist:{playlist}");
            // A new playlist goes first: wrap the first entry.
            println!(
                "folder: {:?}",
                api.add_folder(FOLDER_START, FOLDER_END, 0, 1).await
            );
            println!(
                "collaborative: {:?}",
                api.set_collaborative(playlist, true).await
            );
            let rootlist = api.rootlist().await?;
            for placed in rootlist.layout.iter().take(3) {
                let name = match placed.item {
                    vivace_core::model::Placement::Playlist(index) => {
                        let playlist = &rootlist.playlists[index];
                        format!(
                            "{} (collaborative: {})",
                            playlist.name, playlist.collaborative
                        )
                    }
                    vivace_core::model::Placement::Folder(index) => {
                        rootlist.folders[index].name.to_string()
                    }
                };
                println!("  {:?} {name} in folder {:?}", placed.item, placed.folder);
            }
            println!(
                "created spotify:playlist:{playlist}; clean up with folder-cleanup {playlist}"
            );
        }
        ["folder-cleanup", id] => {
            api.remove_from_rootlist(&[FOLDER_START, FOLDER_END])
                .await?;
            api.remove_playlist(Id::from_base62(id)?).await?;
            let rootlist = api.rootlist().await?;
            println!(
                "now {} folders, {} playlists",
                rootlist.folders.len(),
                rootlist.playlists.len()
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
            for show in &results.shows {
                println!("  show {} | {} | {}", show.id, show.name, show.publisher);
            }
        }
        ["watch", rest @ ..] => {
            let seconds = rest.first().map_or(Ok(5), |seconds| seconds.parse())?;
            watch(&engine, &mut events, seconds, false).await?;
        }
        ["queue-test", context, track, queued] => {
            let print_queues = async |events: &mut Events, seconds: u64| {
                let deadline = tokio::time::sleep(Duration::from_secs(seconds));
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        _ = &mut deadline => break,
                        event = events.recv() => match event {
                            Some(Event::Queue(queue)) => println!(
                                "queue: context <{}> current {:?} next {} (queued {})",
                                queue.context_uri,
                                queue.current.map(|item| item.uri()),
                                queue.next.len(),
                                queue.next.iter().filter(|next| next.queued).count()
                            ),
                            Some(Event::Item(item)) => println!("item: {}", item.uri()),
                            Some(_) => {}
                            None => break,
                        },
                    }
                }
            };
            println!("-- play {context}");
            engine.play_context(context, Start::Beginning, false, Repeat::Off)?;
            print_queues(&mut events, 6).await;
            let track = ItemRef::from_uri(track).ok_or("track uri")?;
            println!("-- play the single track {}", track.uri());
            engine.play_tracks(&[track], 0, Repeat::Off)?;
            print_queues(&mut events, 6).await;
            let queued = ItemRef::from_uri(queued).ok_or("queued uri")?;
            println!("-- queue {}", queued.uri());
            engine.add_to_queue(queued)?;
            print_queues(&mut events, 4).await;
            engine.pause()?;
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

/// What the measuring sink heard.
#[derive(Default)]
struct Meter {
    squares: f64,
    samples: u64,
    /// How often the player stopped the output, as it does between tracks
    /// that do not run into each other.
    stops: u32,
}

impl Meter {
    fn level_db(&self) -> f64 {
        10.0 * (self.squares / self.samples.max(1) as f64).log10()
    }
}

/// A silent sink that measures what passes through it.
struct Metered {
    silence: Silence,
    meter: Arc<std::sync::Mutex<Meter>>,
}

impl Sink for Metered {
    fn stop(&mut self) -> SinkResult<()> {
        if let Ok(mut meter) = self.meter.lock() {
            meter.stops += 1;
        }
        self.silence.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        if let (Ok(samples), Ok(mut meter)) = (packet.samples(), self.meter.lock()) {
            meter.squares += samples.iter().map(|sample| sample * sample).sum::<f64>();
            meter.samples += samples.len() as u64;
        }
        self.silence.write(packet, converter)
    }
}

/// Checks the playback settings by ear, as it were: the loudness of each
/// volume level, whether gapless playback keeps the output running from
/// one track to the next, and whether autoplay goes on after an album.
async fn options_test(base: Config) -> Fallible<()> {
    // Random Access Memories: Instant Crush, Lose Yourself to Dance and
    // the last track, Contact.
    let album = Id::from_base62("4m2880jivSbbyEGAKfITCa")?;
    let uri = Kind::Album.uri(album);

    let connect = |config: Config| async move {
        let meter = Arc::new(std::sync::Mutex::new(Meter::default()));
        let sink_meter = meter.clone();
        let config = Config {
            sink: Some(Arc::new(move || {
                Box::new(Metered {
                    silence: Silence::default(),
                    meter: sink_meter.clone(),
                })
            })),
            ..config
        };
        let (engine, events) = Engine::connect(config).await?;
        Ok::<_, Box<dyn std::error::Error>>((engine, events, meter))
    };
    /// Waits until playback is a second in.
    async fn started(events: &mut Events) -> Option<Event> {
        wait_for(
            events,
            15,
            |event| matches!(event, Event::Position(position) if *position > 1_000),
        )
        .await
    }
    // Waits for an event, at most `seconds`.
    async fn wait_for(
        events: &mut Events,
        seconds: u64,
        mut wanted: impl FnMut(&Event) -> bool,
    ) -> Option<Event> {
        let deadline = tokio::time::sleep(Duration::from_secs(seconds));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => return None,
                event = events.recv() => match event {
                    Some(event) if wanted(&event) => return Some(event),
                    Some(_) => {}
                    None => return None,
                },
            }
        }
    }

    let tracks = {
        let (engine, _, _) = connect(base.clone()).await?;
        engine.api().album(album).await?.tracks
    };

    println!("-- loudness of Instant Crush from 1:00, normalised");
    for level in [VolumeLevel::Quiet, VolumeLevel::Normal, VolumeLevel::Loud] {
        let (engine, mut events, meter) = connect(Config {
            normalise: true,
            volume_level: level,
            ..base.clone()
        })
        .await?;
        engine.play_context(&uri, Start::Index(1), false, Repeat::Off)?;
        started(&mut events).await.ok_or("did not start")?;
        engine.seek(60_000)?;
        wait_for(
            &mut events,
            10,
            |event| matches!(event, Event::Position(position) if *position > 61_000),
        )
        .await;
        *meter.lock().map_err(|_| "poisoned")? = Meter::default();
        tokio::time::sleep(Duration::from_secs(8)).await;
        let level_db = meter.lock().map_err(|_| "poisoned")?.level_db();
        println!("{level:?}: {level_db:.1} dB");
    }

    println!("-- from the end of Give Life Back to Music into The Game of Love");
    for gapless in [true, false] {
        let (engine, mut events, meter) = connect(Config {
            gapless,
            ..base.clone()
        })
        .await?;
        engine.play_context(&uri, Start::Index(0), false, Repeat::Off)?;
        started(&mut events).await.ok_or("did not start")?;
        engine.seek(tracks[0].duration_ms.saturating_sub(4_000))?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        meter.lock().map_err(|_| "poisoned")?.stops = 0;
        let next = ItemRef::track(tracks[1].id);
        let reached = wait_for(&mut events, 15, |event| *event == Event::Item(next)).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let stops = meter.lock().map_err(|_| "poisoned")?.stops;
        println!(
            "gapless {gapless}: next track {}, output stopped {stops} times",
            reached.is_some()
        );
    }

    println!("-- after the album's last track");
    let last = tracks.len() as u32 - 1;
    for autoplay in [true, false] {
        let (engine, mut events, _) = connect(Config {
            autoplay,
            ..base.clone()
        })
        .await?;
        engine.play_context(&uri, Start::Index(last), false, Repeat::Off)?;
        started(&mut events).await.ok_or("did not start")?;
        engine.seek(tracks[last as usize].duration_ms.saturating_sub(3_000))?;
        let ended = wait_for(&mut events, 20, |event| {
            matches!(event, Event::Stopped)
                || matches!(event, Event::Item(item) if item.id != tracks[last as usize].id)
        })
        .await;
        println!("autoplay {autoplay}: then {ended:?}");
    }
    Ok(())
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

/// Plays Liked Songs, or an episode as a list of one as search results do,
/// drops the connection as Spotify's server does now and
/// then, and reconnects with the same device as the app does. Playback must
/// carry on by itself from where it was.
async fn drop_test(mut config: Config, episode: Option<&str>) -> Fallible<()> {
    // The app keeps its device id across reconnects.
    config.persistent_device = true;
    let (engine, mut events) = Engine::connect(config.clone()).await?;
    match episode {
        Some(id) => {
            engine.play_tracks(&[ItemRef::episode(Id::from_base62(id)?)], 0, Repeat::Off)?
        }
        None => play_liked(&engine)?,
    }
    let before = playing_past(&mut events, 8000).await?;
    println!("-> playing at {before} ms; dropping the connection");
    engine.drop_connection();
    if wait_for(&mut events, 20, |event| *event == Event::Disconnected)
        .await
        .is_none()
    {
        return Err("the session didn't end".into());
    }
    // As the app does, the next session picks up where this one left off,
    // playing if this one was playing until the end.
    config.resume = Some(Resume {
        session_id: engine.session_id(),
        play: engine.playing_at().is_some(),
        left_off: engine.left_off(),
    });
    drop((engine, events));
    tokio::time::sleep(Duration::from_secs(5)).await;

    let (engine, mut events) = Engine::connect(config).await?;
    println!("-> reconnected");
    let carried_on = |event: &Event| matches!(event, Event::Position(position_ms) if *position_ms > before + 3000);
    let resumed = wait_for(&mut events, 15, carried_on).await;
    engine.pause()?;
    match resumed {
        Some(event) => println!("PASS: playback carried on by itself ({event:?})"),
        None => println!("FAIL: playback didn't carry on after the reconnect"),
    }
    Ok(())
}

/// Plays Liked Songs, pauses, and drops the connection while paused, which
/// nothing on the device notices by itself. Once reconnected as the app
/// does, play must start it again where it stopped.
async fn drop_paused_test(mut config: Config) -> Fallible<()> {
    // The app keeps its device id across reconnects.
    config.persistent_device = true;
    let (engine, mut events) = Engine::connect(config.clone()).await?;
    play_liked(&engine)?;
    let item = started(&mut events).await?;
    playing_past(&mut events, 8000).await?;
    let paused_at = pause(&engine, &mut events).await?;
    println!("-> paused {item:?} at {paused_at} ms; dropping the connection");
    engine.drop_connection();
    if wait_for(&mut events, 20, |event| *event == Event::Disconnected)
        .await
        .is_none()
    {
        println!("FAIL: the dropped connection went unnoticed");
        return Ok(());
    }
    config.resume = Some(Resume {
        session_id: engine.session_id(),
        play: false,
        left_off: engine.left_off(),
    });
    drop((engine, events));
    tokio::time::sleep(Duration::from_secs(5)).await;

    let (engine, mut events) = Engine::connect(config).await?;
    println!("-> reconnected; play");
    wait_for(&mut events, 3, |_| false).await;
    engine.play()?;
    let resumed = wait_for(&mut events, 20, |event| {
        matches!(event, Event::Playback(Playback { playing: true, .. }))
    })
    .await;
    let same_item = events_item(&mut events, item).await;
    engine.pause()?;
    match resumed {
        Some(Event::Playback(Playback { position_ms, .. }))
            if near(position_ms, paused_at, 3000) && same_item =>
        {
            println!("PASS: {item:?} started again at {position_ms} ms")
        }
        Some(event) => {
            println!("FAIL: played, but not where it stopped ({event:?}, same item: {same_item})")
        }
        None => println!("FAIL: play did nothing"),
    }
    Ok(())
}

/// Takes the account's playback, moves it to `device` the way the device
/// picker does and prints what Spotify answers, then gives it back.
async fn speaker_test(config: Config, device: &str, mode: Option<&str>) -> Fallible<()> {
    let (engine, mut events) = Engine::connect(config).await?;
    let api = engine.api();
    let mut connect = None;
    watch_connect(&mut events, &mut connect, 3).await;
    let original = remote_playback(&connect).map(|remote| remote.device.clone());
    println!("-> state was on {original:?}");
    let paused_context = mode.and_then(|mode| mode.strip_prefix("paused-context="));
    if let Some(uri) = paused_context.or(mode.and_then(|mode| mode.strip_prefix("context="))) {
        println!("-> playing the second item of {uri}");
        engine.play_context(uri, Start::Index(1), false, Repeat::Off)?;
        watch_connect(&mut events, &mut connect, 10).await;
        if paused_context.is_some() {
            engine.pause()?;
            watch_connect(&mut events, &mut connect, 3).await;
        }
    } else if mode != Some("idle") {
        println!("-> taking playback here");
        engine.transfer_here()?;
        watch_connect(&mut events, &mut connect, 5).await;
    }
    if matches!(mode, Some("play" | "next")) {
        engine.play()?;
        watch_connect(&mut events, &mut connect, 5).await;
    }
    if mode == Some("next") {
        engine.next()?;
        watch_connect(&mut events, &mut connect, 8).await;
    }
    let started = Instant::now();
    println!("-> transfer to {device}");
    let result = api.transfer(device).await;
    println!("transfer answered {result:?} in {:?}", started.elapsed());
    watch_connect(&mut events, &mut connect, 30).await;
    let on_target = remote_playback(&connect)
        .filter(|remote| *remote.device == *device)
        .map(|remote| remote.playing);
    if on_target == Some(true) {
        println!("-> pausing {device}");
        api.remote(device, RemoteCommand::Pause).await?;
        watch_connect(&mut events, &mut connect, 3).await;
    }
    if let Some(original) = original {
        println!("-> giving playback back to {original}");
        if on_target.is_some() {
            api.remote(device, RemoteCommand::Pause).await.ok();
        }
        engine.transfer_here().ok();
        watch_connect(&mut events, &mut connect, 4).await;
        engine.pause().ok();
        println!("give back: {:?}", api.transfer(&original).await);
        watch_connect(&mut events, &mut connect, 5).await;
        if remote_playback(&connect).is_some_and(|remote| remote.playing) {
            api.remote(&original, RemoteCommand::Pause).await?;
            watch_connect(&mut events, &mut connect, 2).await;
        }
    }
    Ok(())
}

/// Playback on another device, as the account's state last said.
fn remote_playback(connect: &Option<Connect>) -> Option<&Remote> {
    connect.as_ref()?.remote.as_ref()
}

/// Prints how the account's playback changes for `seconds`, keeping the
/// latest state in `connect`.
async fn watch_connect(events: &mut Events, connect: &mut Option<Connect>, seconds: u64) {
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return,
            event = events.recv() => match event {
                Some(Event::Connect(state)) => {
                    let active = state.devices.iter().find(|device| device.is_active);
                    println!(
                        "connect: active {:?}, remote playing {:?}",
                        active.map(|device| &device.name),
                        state.remote.as_ref().map(|remote| remote.playing),
                    );
                    *connect = Some(state);
                }
                Some(event @ (Event::Playback(_) | Event::Stopped)) => println!("{event:?}"),
                Some(_) => {}
                None => return,
            },
        }
    }
}

async fn idle_test(mut config: Config, mode: Option<&str>, list: bool) -> Fallible<()> {
    // The app keeps its device id across reconnects.
    config.persistent_device = true;
    let (mut engine, mut events) = Engine::connect(config.clone()).await?;
    if list {
        // A plain list of tracks, as search results or a show's episodes
        // play: librespot names it with a placeholder Spotify can't resolve.
        let liked = engine.api().saved().await?.tracks;
        let tracks: Vec<ItemRef> = liked
            .iter()
            .take(10)
            .map(|saved| ItemRef::track(saved.id))
            .collect();
        engine.play_tracks(&tracks, 0, Repeat::Off)?;
    } else {
        play_liked(&engine)?;
    }
    let item = started(&mut events).await?;
    playing_past(&mut events, 8000).await?;
    let paused_at = pause(&engine, &mut events).await?;
    println!("-> paused {item:?} at {paused_at} ms; losing playback");
    engine.lose_playback()?;
    wait_for(&mut events, 3, |_| false).await;

    if mode == Some("drop") {
        println!("-> dropping the connection");
        engine.drop_connection();
        if wait_for(&mut events, 20, |event| *event == Event::Disconnected)
            .await
            .is_none()
        {
            return Err("the session didn't end".into());
        }
        // Spotify forgets a paused session's state within hours; an id
        // that names no session stands for that.
        config.resume = Some(Resume {
            session_id: "forgotten".into(),
            play: false,
            left_off: engine.left_off(),
        });
        drop((engine, events));
        tokio::time::sleep(Duration::from_secs(5)).await;
        (engine, events) = Engine::connect(config).await?;
        let mut connect = None;
        watch_connect(&mut events, &mut connect, 3).await;
        if connect.is_some_and(|connect| connect.devices.iter().any(|device| device.is_active)) {
            println!("SKIP: the account still has playback, so play has it anyway");
            return Ok(());
        }
        println!("-> reconnected with nothing active");
    }
    if mode == Some("transfer") {
        println!("-> choose this computer");
        engine.transfer_here()?;
        let taken = wait_for(&mut events, 20, |event| {
            matches!(event, Event::Playback(Playback { playing: false, .. }))
        })
        .await;
        match taken {
            Some(Event::Playback(Playback { position_ms, .. }))
                if near(position_ms, paused_at, 1500) => {}
            other => {
                println!("FAIL: choosing this computer didn't take it back paused ({other:?})");
                return Ok(());
            }
        }
    }
    println!(
        "-> left off: {:?}",
        engine.left_off().map(|left_off| left_off.playing)
    );
    println!("-> play");
    engine.play()?;
    let resumed = wait_for(&mut events, 20, |event| {
        matches!(event, Event::Playback(Playback { playing: true, .. }))
    })
    .await;
    let same_item = events_item(&mut events, item).await;
    engine.pause()?;
    match resumed {
        Some(Event::Playback(Playback { position_ms, .. }))
            if near(position_ms, paused_at, 3000) && same_item =>
        {
            println!("PASS: {item:?} started again at {position_ms} ms")
        }
        Some(event) => {
            println!("FAIL: played, but not where it stopped ({event:?}, same item: {same_item})")
        }
        None => println!("FAIL: play did nothing"),
    }
    Ok(())
}

async fn settings_test(mut config: Config, paused: bool) -> Fallible<()> {
    // The app keeps its device id across sessions.
    config.persistent_device = true;
    config.normalise = true;
    let (engine, mut events) = Engine::connect(config.clone()).await?;
    play_liked(&engine)?;
    let item = started(&mut events).await?;
    let mut before = playing_past(&mut events, 8000).await?;
    if paused {
        before = pause(&engine, &mut events).await?;
    }
    let Some(left_off) = engine.left_off() else {
        println!("FAIL: nothing to carry over");
        return Ok(());
    };
    println!(
        "-> at {before} ms, playing {}; changing the volume level",
        left_off.playing
    );
    drop((engine, events));

    config.volume_level = VolumeLevel::Loud;
    let (engine, mut events) = Engine::connect(config).await?;
    engine.pick_up(left_off)?;
    let started = wait_for(&mut events, 20, |event| matches!(event, Event::Playback(_))).await;
    let Some(Event::Playback(Playback {
        playing,
        position_ms,
    })) = started
    else {
        println!("FAIL: nothing carried on");
        return Ok(());
    };
    let near = near(position_ms, before, 2500);
    let went_on = wait_for(&mut events, 8, |event| {
        matches!(event, Event::Position(at) if *at > position_ms + 4000)
            || matches!(event, Event::Stopped | Event::Playback(_))
    })
    .await;
    let same_item = events_item(&mut events, item).await;
    let kept_playing = matches!(went_on, Some(Event::Position(_)));
    engine.pause()?;
    if !near || !same_item || playing == paused || (!paused && !kept_playing) {
        println!(
            "FAIL: playing {playing} at {position_ms} ms, same item {same_item}, then {went_on:?}"
        );
    } else {
        println!("PASS: {item:?} carried on at {position_ms} ms, playing {playing}");
    }
    Ok(())
}

fn play_liked(engine: &Engine) -> vivace_core::Result<()> {
    engine.play_context(
        &engine.liked_songs_uri(),
        Start::Beginning,
        false,
        Repeat::Off,
    )
}

/// Waits for the player to announce what it plays.
async fn started(events: &mut Events) -> Fallible<ItemRef> {
    match wait_for(events, 30, |event| matches!(event, Event::Item(_))).await {
        Some(Event::Item(item)) => Ok(item),
        _ => Err("nothing started".into()),
    }
}

/// Waits for playback to get past `position_ms`, and says where it got.
async fn playing_past(events: &mut Events, position_ms: u32) -> Fallible<u32> {
    let past = |event: &Event| matches!(event, Event::Position(at) if *at > position_ms);
    match wait_for(events, 30, past).await {
        Some(Event::Position(at)) => Ok(at),
        _ => Err("the track didn't start".into()),
    }
}

/// Pauses, and says where playback stopped.
async fn pause(engine: &Engine, events: &mut Events) -> Fallible<u32> {
    engine.pause()?;
    let paused = |event: &Event| matches!(event, Event::Playback(Playback { playing: false, .. }));
    match wait_for(events, 10, paused).await {
        Some(Event::Playback(Playback { position_ms, .. })) => Ok(position_ms),
        _ => Err("the track didn't pause".into()),
    }
}

/// Whether `position_ms` is where playback stopped, at `stopped_at`: up to
/// a second and a half before it, or up to `late_ms` after.
fn near(position_ms: u32, stopped_at: u32, late_ms: u32) -> bool {
    position_ms + 1500 >= stopped_at && position_ms <= stopped_at + late_ms
}

/// Whether the player is still on `item` a couple of seconds on.
async fn events_item(events: &mut Events, item: ItemRef) -> bool {
    let other = wait_for(
        events,
        2,
        |event| matches!(event, Event::Item(other) if *other != item),
    )
    .await;
    other.is_none()
}

/// Prints events for up to `seconds` until one matches, and returns it.
async fn wait_for(
    events: &mut Events,
    seconds: u64,
    matches: impl Fn(&Event) -> bool,
) -> Option<Event> {
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return None,
            event = events.recv() => {
                let event = event?;
                match &event {
                    Event::Queue(_) | Event::Connect(_) => {}
                    event => println!("{event:?}"),
                }
                if matches(&event) {
                    return Some(event);
                }
            }
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
                            "device: {} ({:?}) {}{}{}",
                            device.name,
                            device.kind,
                            device.id,
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
