//! Drives the real app without a display or sound and saves screenshots.
//!
//! Signs in with the stored credentials, walks through a few pages and
//! writes one PNG per step:
//!
//!   cargo run -p vivace --example live -- <output directory>
//!
//! With `LIVE_PLAYLIST_EDIT=1` it instead stops after the playlist page,
//! adds a track to a new playlist, renames that playlist and deletes it. With
//! `LIVE_NO_PLAYBACK=1` it only browses, leaving other devices undisturbed.
//! With `LIVE_PODCAST=1` it opens a followed podcast, plays its newest
//! episode from where it was left, and saves it to Your Episodes and back.
//! With `LIVE_SETTINGS=1` it changes settings and signs out; afterwards the
//! saved login is gone, so keep a copy of it to restore.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use librespot_playback::{
    SAMPLES_PER_SECOND,
    audio_backend::{Sink, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{EventLoopProxy, Platform, WindowAdapter};
use slint::{ComponentHandle, EventLoopError, Model, PhysicalSize, PlatformError};
use vivace::{
    App, Config, Dialog, Library, LyricsView, MainWindow, Nav, Player, Queue, RowKind,
    SettingsView, TrackAction,
};

// What the playlist-editing scenario renames its playlist to before deleting it.
const TEST_PLAYLIST: &str = "Vivace live test";
// Name fragments of the steps that start, pause or seek playback.
const PLAYING_STEPS: [&str; 4] = ["playing", "paused", "lyrics", "stopped"];
const WIDTH: u32 = 1280;
// Where a podcast page's Follow button sits in the window.
const FOLLOW_BUTTON: (f32, f32) = (671.0, 255.0);
// The visible part of the settings page's device name field.
const DEVICE_NAME_FIELD: (f32, f32) = (500.0, 670.0);
const HEIGHT: u32 = 800;

enum Event {
    Quit,
    Invoke(Box<dyn FnOnce() + Send>),
}

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    events: Receiver<Event>,
    sender: Sender<Event>,
    started: Instant,
}

struct Proxy(Sender<Event>);

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        self.0
            .send(Event::Quit)
            .map_err(|_| EventLoopError::EventLoopTerminated)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), EventLoopError> {
        self.0
            .send(Event::Invoke(event))
            .map_err(|_| EventLoopError::EventLoopTerminated)
    }
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(self.sender.clone())))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let mut buffer = vec![PremultipliedRgbaColor::default(); (WIDTH * HEIGHT) as usize];
        loop {
            slint::platform::update_timers_and_animations();
            // Rendering is what makes lists ask for their rows and artwork.
            self.window.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, WIDTH as usize);
            });
            let wait = slint::platform::duration_until_next_timer_update()
                .unwrap_or(Duration::from_millis(250))
                .min(Duration::from_millis(250));
            match self.events.recv_timeout(wait) {
                Ok(Event::Invoke(event)) => event(),
                Ok(Event::Quit) | Err(RecvTimeoutError::Disconnected) => return Ok(()),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// Discards audio at playback speed.
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

type Step = (&'static str, u64, Box<dyn Fn(&MainWindow)>);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: nothing has started a thread yet.
    unsafe { vivace::limit_player_threads() };
    vivace::tune_allocator();
    vivace::init_logging();
    let directory = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("usage: live <output directory>")?,
    );
    std::fs::create_dir_all(&directory)?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let (sender, events) = channel();
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        events,
        sender,
        started: Instant::now(),
    }))?;

    let ui = MainWindow::new()?;
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    let config = Config {
        sink: Some(Arc::new(|| Box::new(Silence::default()))),
        ..Config::default()
    };
    let app = App::start(&ui, config)?;
    ui.show()?;

    // Each step runs its action, waits, then is photographed.
    let first_row = |ui: &MainWindow, kind: RowKind| {
        ui.global::<Nav>()
            .get_rows()
            .iter()
            .position(|row| row.kind == kind)
            .unwrap_or(0) as i32
    };
    let steps: Vec<Step> = vec![
        ("1-home", 5000, Box::new(|_| {})),
        (
            "2-playlist",
            3000,
            // The first playlist, after Liked Songs and Your Episodes.
            Box::new(|ui| ui.global::<Library>().invoke_open(2)),
        ),
        (
            "3-playing",
            6000,
            Box::new(move |ui| {
                ui.global::<Queue>().set_visible(true);
                let row = first_row(ui, RowKind::Track) + 2;
                ui.global::<Nav>().invoke_play_track(row);
            }),
        ),
        (
            "4-paused",
            1500,
            Box::new(|ui| ui.global::<Player>().invoke_toggle()),
        ),
        (
            "5-search",
            4000,
            Box::new(|ui| {
                ui.global::<Queue>().set_visible(false);
                ui.global::<Nav>().set_query("daft punk".into());
                ui.global::<Nav>().invoke_search("daft punk".into());
            }),
        ),
        (
            "5b-all-albums",
            4000,
            Box::new(|ui| {
                let rows = ui.global::<Nav>().get_rows();
                let title = rows
                    .iter()
                    .position(|row| row.kind == RowKind::Section && row.text == "Albums");
                match title {
                    Some(row) => ui.global::<Nav>().invoke_open_section(row as i32),
                    None => println!("no albums among the results"),
                }
            }),
        ),
        (
            "5c-back",
            1500,
            Box::new(|ui| ui.global::<Nav>().invoke_back()),
        ),
        (
            "6-artist",
            4000,
            Box::new(move |ui| {
                let row = first_row(ui, RowKind::Cards);
                ui.global::<Nav>().invoke_open_card(row, 0);
            }),
        ),
        (
            "7-album",
            4000,
            Box::new(move |ui| {
                let row = first_row(ui, RowKind::Cards);
                ui.global::<Nav>().invoke_open_card(row, 0);
            }),
        ),
        (
            "8-back",
            1500,
            Box::new(|ui| ui.global::<Nav>().invoke_back()),
        ),
        (
            "9-liked",
            3000,
            Box::new(|ui| ui.global::<Library>().invoke_open(0)),
        ),
        (
            "9b-shortcuts",
            1500,
            // Alt+Left goes back to the album and Ctrl+K puts the cursor in
            // the search field. Transport and volume keys are left out: when
            // the user is listening elsewhere they would act on that device.
            Box::new(|ui| {
                shortcut(
                    ui,
                    slint::platform::Key::Alt,
                    slint::platform::Key::LeftArrow.into(),
                );
                shortcut(ui, slint::platform::Key::Control, "k".into());
            }),
        ),
        (
            "9c-library-albums",
            1000,
            Box::new(|ui| ui.global::<Library>().invoke_set_filter(1)),
        ),
        (
            "9d-library-podcasts",
            1000,
            Box::new(|ui| ui.global::<Library>().invoke_set_filter(3)),
        ),
        (
            "9e-library-alphabetical",
            1000,
            // The order is saved, so the next step puts it back.
            Box::new(|ui| {
                ui.global::<Library>().invoke_set_filter(-1);
                ui.global::<Library>().invoke_set_sort(2);
            }),
        ),
        (
            "9f-library-custom",
            500,
            Box::new(|ui| ui.global::<Library>().invoke_set_sort(0)),
        ),
        (
            "9g-audiobook-search",
            4000,
            Box::new(|ui| {
                ui.global::<Nav>().set_query("el principito".into());
                ui.global::<Nav>().invoke_search("el principito".into());
            }),
        ),
        (
            "9h-audiobook",
            4000,
            Box::new(|ui| {
                let rows = ui.global::<Nav>().get_rows();
                let title = rows
                    .iter()
                    .position(|row| row.kind == RowKind::Section && row.text == "Audiobooks");
                match title {
                    Some(row) => ui.global::<Nav>().invoke_open_card(row as i32 + 1, 0),
                    None => println!("no audiobooks among the results"),
                }
            }),
        ),
        (
            "10-lyrics-search",
            4000,
            Box::new(|ui| {
                ui.global::<Nav>()
                    .set_query("never gonna give you up".into());
                ui.global::<Nav>()
                    .invoke_search("never gonna give you up".into());
            }),
        ),
        (
            "11-lyrics-loading",
            3000,
            Box::new(move |ui| {
                ui.global::<Nav>()
                    .invoke_play_track(first_row(ui, RowKind::Track));
                ui.global::<LyricsView>().set_visible(true);
                ui.global::<LyricsView>().invoke_shown();
            }),
        ),
        (
            "12-lyrics",
            4000,
            Box::new(|ui| ui.global::<Player>().invoke_seek(44_000)),
        ),
        (
            "13-stopped",
            1000,
            Box::new(|ui| ui.global::<Player>().invoke_toggle()),
        ),
    ];

    // Opt-in because it writes to the account: adds the playlist page's first
    // track to a new playlist, renames that playlist and deletes it. If a
    // step fails, remove what is left with `cli delete-playlist`.
    let mut steps = steps;
    if std::env::var_os("LIVE_PLAYLIST_EDIT").is_some() {
        // The test playlist is named after the track it is made from; it is
        // looked up by its later names so no other playlist is ever touched.
        let library_index = |ui: &MainWindow, name: &str| {
            let items = ui.global::<Library>().get_items();
            items
                .iter()
                .position(|item| item.title == name && item.owned)
                .map(|index| index as i32)
        };
        steps.truncate(2);
        steps.push((
            "3-added-to-new-playlist",
            4000,
            Box::new(move |ui| {
                let row = first_row(ui, RowKind::Track);
                ui.global::<Nav>().invoke_add_to_playlist(row, -1);
            }),
        ));
        steps.push((
            "4-rename-dialog",
            1000,
            Box::new(move |ui| {
                let rows = ui.global::<Nav>().get_rows();
                let created = rows
                    .iter()
                    .find(|row| row.kind == RowKind::Track)
                    .map(|row| row.track.title);
                match created.and_then(|name| library_index(ui, &name)) {
                    Some(index) => ui.global::<Library>().invoke_rename(index),
                    None => println!("the new playlist is not in the library"),
                }
            }),
        ));
        steps.push((
            "5-renamed",
            4000,
            Box::new(|ui| {
                if ui.global::<Dialog>().get_visible() {
                    ui.global::<Dialog>().invoke_confirmed(TEST_PLAYLIST.into());
                }
            }),
        ));
        steps.push((
            "6-delete-dialog",
            1000,
            Box::new(move |ui| match library_index(ui, TEST_PLAYLIST) {
                Some(index) => ui.global::<Library>().invoke_remove(index),
                None => println!("the renamed playlist is not in the library"),
            }),
        ));
        steps.push((
            "7-deleted",
            4000,
            Box::new(|ui| {
                if ui.global::<Dialog>().get_visible() {
                    ui.global::<Dialog>().invoke_confirmed("".into());
                }
            }),
        ));
    }

    // Opens the first followed podcast from Home and plays its latest episode.
    if std::env::var_os("LIVE_PODCAST").is_some() {
        steps.truncate(1);
        steps.push((
            "p2-show",
            4000,
            Box::new(|ui| {
                let rows = ui.global::<Nav>().get_rows();
                let shelf = rows
                    .iter()
                    .position(|row| row.kind == RowKind::Section && row.text == "Your podcasts");
                match shelf {
                    Some(title) => ui.global::<Nav>().invoke_open_card(title as i32 + 1, 0),
                    None => println!("no podcasts shelf on Home"),
                }
            }),
        ));
        steps.push((
            "p3-episode-playing",
            7000,
            // The header's play button, which starts the newest episode
            // where it was left.
            Box::new(|ui| {
                ui.global::<Queue>().set_visible(true);
                ui.global::<Nav>().invoke_play_page();
            }),
        ));
        steps.push((
            "p4-paused",
            1500,
            Box::new(|ui| ui.global::<Player>().invoke_toggle()),
        ));
        // Marks the next episode played and unplayed again.
        steps.push((
            "p4b-marked-played",
            2500,
            Box::new(move |ui| {
                let row = first_row(ui, RowKind::Track) + 1;
                ui.global::<Nav>()
                    .invoke_track_action(row, TrackAction::TogglePlayed);
            }),
        ));
        steps.push((
            "p4c-marked-unplayed",
            2500,
            Box::new(move |ui| {
                let row = first_row(ui, RowKind::Track) + 1;
                ui.global::<Nav>()
                    .invoke_track_action(row, TrackAction::TogglePlayed);
            }),
        ));
        // Saves the episode to Your Episodes, where it comes first, and takes
        // it out again from there.
        steps.push((
            "p5-saved",
            3000,
            Box::new(move |ui| {
                ui.global::<Nav>()
                    .invoke_track_action(first_row(ui, RowKind::Track), TrackAction::ToggleLiked);
            }),
        ));
        steps.push((
            "p6-your-episodes",
            4000,
            Box::new(|ui| ui.global::<Library>().invoke_open(1)),
        ));
        steps.push((
            "p7-unsaved",
            3000,
            Box::new(move |ui| {
                ui.global::<Nav>()
                    .invoke_track_action(first_row(ui, RowKind::Track), TrackAction::ToggleLiked);
            }),
        ));
        // Plays on, so closing has a position to record.
        steps.push((
            "p8-resumed",
            5000,
            Box::new(|ui| ui.global::<Player>().invoke_toggle()),
        ));
    }

    // Changes a setting, which reconnects, and then signs out. Signing in
    // again needs the browser, so run this with a copy of the credentials.
    // Clicks and types as a user would, on the folder, a podcast's Follow
    // button and the device name. Net zero for the account: follows twice
    // and puts the default device name back.
    if std::env::var_os("LIVE_INTERACTIONS").is_some() {
        let library_index = |ui: &MainWindow, name: &str| {
            let items = ui.global::<Library>().get_items();
            items
                .iter()
                .position(|item| item.title == name)
                .map(|index| index as i32)
        };
        steps.truncate(1);
        steps.push((
            "i2-folder",
            1500,
            Box::new(move |ui| match library_index(ui, "Vivace test folder") {
                Some(index) => ui.global::<Library>().invoke_open(index),
                None => println!("no test folder: run cli folder-setup first"),
            }),
        ));
        steps.push((
            "i3-folder-up",
            1000,
            Box::new(|ui| ui.global::<Library>().invoke_close_folder()),
        ));
        steps.push((
            "i4-show",
            4000,
            Box::new(
                move |ui| match library_index(ui, "A followed podcast") {
                    Some(index) => ui.global::<Library>().invoke_open(index),
                    None => println!("the podcast is not in the library"),
                },
            ),
        ));
        for name in ["i5-follow-clicked", "i6-follow-clicked-again"] {
            steps.push((
                name,
                2500,
                Box::new(|ui| click(ui, FOLLOW_BUTTON.0, FOLLOW_BUTTON.1)),
            ));
        }
        steps.push((
            "i7-settings",
            1000,
            Box::new(|ui| ui.global::<Nav>().invoke_open_settings()),
        ));
        // Types a name into the field and presses Enter, which reconnects
        // under that name; then empties it, back to the default.
        steps.push((
            "i8-named",
            8000,
            Box::new(|ui| {
                click(ui, DEVICE_NAME_FIELD.0, DEVICE_NAME_FIELD.1);
                shortcut(ui, slint::platform::Key::Control, "a".into());
                type_text(ui, "Vivace typed");
                type_text(ui, &slint::SharedString::from(slint::platform::Key::Return));
            }),
        ));
        steps.push((
            "i9-default-name",
            8000,
            Box::new(|ui| {
                print_own_device(ui);
                click(ui, DEVICE_NAME_FIELD.0, DEVICE_NAME_FIELD.1);
                shortcut(ui, slint::platform::Key::Control, "a".into());
                type_text(
                    ui,
                    &slint::SharedString::from(slint::platform::Key::Backspace),
                );
                type_text(ui, &slint::SharedString::from(slint::platform::Key::Return));
            }),
        ));
        steps.push(("i10-checked", 500, Box::new(print_own_device)));
    }

    if std::env::var_os("LIVE_SETTINGS").is_some() {
        steps.truncate(1);
        steps.push((
            "s2-settings",
            1000,
            Box::new(|ui| ui.global::<Nav>().invoke_open_settings()),
        ));
        steps.push((
            "s3-normal-quality",
            6000,
            Box::new(|ui| ui.global::<SettingsView>().invoke_set_quality(1)),
        ));
        steps.push((
            "s4-renamed-device",
            6000,
            Box::new(|ui| {
                ui.global::<SettingsView>().invoke_set_quality(2);
                ui.global::<SettingsView>()
                    .invoke_set_device_name("Vivace test".into());
            }),
        ));
        steps.push((
            "s5-sign-out-dialog",
            6000,
            Box::new(|ui| {
                ui.global::<SettingsView>()
                    .invoke_set_device_name("Vivace".into());
                ui.global::<SettingsView>().invoke_sign_out();
            }),
        ));
        steps.push((
            "s6-signed-out",
            4000,
            Box::new(|ui| ui.global::<Dialog>().invoke_confirmed("".into())),
        ));
    }

    // Starting playback takes it away from whichever device has it, so the
    // steps that play can be left out while someone is listening.
    if std::env::var_os("LIVE_NO_PLAYBACK").is_some() {
        steps.retain(|(name, ..)| !PLAYING_STEPS.iter().any(|playing| name.contains(playing)));
    }

    let steps = Rc::new(steps);
    let index = Rc::new(Cell::new(0));
    let timer = Rc::new(slint::Timer::default());
    let pending: Rc<RefCell<Option<&'static str>>> = Rc::default();
    let tick = {
        let (ui, timer, steps) = (ui.as_weak(), timer.clone(), steps.clone());
        move || {
            let Some(ui) = ui.upgrade() else { return };
            if let Some(name) = pending.take() {
                let path = directory.join(format!("{name}.png"));
                match ui.window().take_snapshot() {
                    Ok(shot) => {
                        let saved = image::save_buffer(
                            &path,
                            shot.as_bytes(),
                            shot.width(),
                            shot.height(),
                            image::ColorType::Rgba8,
                        );
                        let rss = memory_stats::memory_stats()
                            .map_or(0.0, |usage| usage.physical_mem as f64 / 1_048_576.0);
                        println!("{name}: {saved:?} rss {rss:.1} MB");
                    }
                    Err(error) => println!("{name}: {error}"),
                }
            }
            let Some((name, wait, action)) = steps.get(index.get()) else {
                // Leaves the event loop; the run is over.
                _ = slint::quit_event_loop();
                return;
            };
            index.set(index.get() + 1);
            action(&ui);
            pending.replace(Some(name));
            timer.set_interval(Duration::from_millis(*wait));
        }
    };
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(1), tick);

    slint::run_event_loop()?;
    // Closes as the app does, recording where an episode was left.
    let closing = std::time::Instant::now();
    app.quit();
    println!("closed in {:?}", closing.elapsed());
    Ok(())
}

/// Presses a key while holding a modifier, as a user would.
fn shortcut(ui: &MainWindow, modifier: slint::platform::Key, key: slint::SharedString) {
    use slint::platform::WindowEvent;
    let window = ui.window();
    let modifier: slint::SharedString = modifier.into();
    window.dispatch_event(WindowEvent::KeyPressed {
        text: modifier.clone(),
    });
    window.dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
    window.dispatch_event(WindowEvent::KeyReleased { text: key });
    window.dispatch_event(WindowEvent::KeyReleased { text: modifier });
}

/// Clicks the left mouse button at a point of the window.
fn click(ui: &MainWindow, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let position = slint::LogicalPosition::new(x, y);
    let window = ui.window();
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

/// Types text, one key at a time.
fn type_text(ui: &MainWindow, text: &str) {
    use slint::platform::WindowEvent;
    for character in text.chars() {
        let key = slint::SharedString::from(character.to_string().as_str());
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: key.clone() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: key });
    }
}

/// Prints what Spotify's device list calls this computer.
fn print_own_device(ui: &MainWindow) {
    let devices = ui.global::<vivace::Devices>().get_items();
    let own = devices.iter().find(|device| device.local);
    println!(
        "this computer in the device list: {:?}",
        own.map(|device| device.name.to_string())
    );
}
