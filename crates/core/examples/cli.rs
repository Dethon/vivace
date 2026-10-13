//! Developer tool exercising the engine against a real account.
//!
//!   cli library | home | playlist <id> | album <id> | artist <id> | search <query>
//!   cli watch [seconds]     prints Connect events without taking over playback
//!   cli play <context uri> [seconds]     plays through the default output
//!   cli play-silent <context uri> [seconds]     decodes without sound

use std::sync::Arc;
use std::time::{Duration, Instant};

use librespot_playback::{
    SAMPLES_PER_SECOND,
    audio_backend::{Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};

use vivace_core::{
    Config, Engine, Event, Events, Repeat, Start,
    model::{Id, Track},
};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> Fallible<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let config = match args.first() {
        Some(&"play-silent") => Config {
            sink: Some(Arc::new(|| Box::new(Silence::default()))),
            ..Config::default()
        },
        _ => Config::default(),
    };

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
                    "  {} | {} | {} tracks | by {} | image {:?}",
                    playlist.id, playlist.name, playlist.length, playlist.owner, playlist.image
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
        ["playlist", id] => {
            let details = api.playlist(Id::from_base62(id)?).await?;
            println!("{:?}", details.playlist);
            println!(
                "{} entries after {:?}",
                details.entries.len(),
                started.elapsed()
            );
            let ids: Vec<Id> = details.entries.iter().map(|entry| entry.track).collect();
            let tracks: Vec<Track> = api.tracks(&ids).await?.into_iter().flatten().collect();
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
            let results = api.search(query).await?;
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
        ["play" | "play-silent", uri, rest @ ..] => {
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
                    engine.add_to_queue(Id::from_base62("4uLU6hMCjMI75M1A2tKUQC")?)?;
                    engine.next()?;
                    engine.set_shuffle(true)?;
                }
                Some(Event::Queue(queue)) => println!(
                    "queue: context {} | current {:?} | {} next ({} queued by hand), first {:?}",
                    queue.context_uri,
                    queue.current,
                    queue.next.len(),
                    queue.next.iter().filter(|item| item.queued).count(),
                    queue.next.iter().take(3).map(|item| item.track).collect::<Vec<_>>()
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
