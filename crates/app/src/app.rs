//! The controller: owns what is on screen and reacts to the user and the backend.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vivace_core::{
    Config, Connect, Device, DeviceKind, Playback, Quality, Repeat, Start, VolumeLevel,
    model::{Id, Image, Lyrics, Playlist, Progress, Track},
};

use crate::{
    CardData, DeviceIcon, DeviceItem, Devices, Dialog, Emblem as EmblemKind, HeaderData, Library,
    LibraryItem, LyricsView, MainWindow, Nav, Player, Queue, RepeatMode, Row, RowKind, SaveKind,
    Session, SettingsView, TrackAction, TrackData,
    backend::{self, Request, ResolvedQueue, Update},
    content::{Card, Dates, Emblem, Page, PageData, Saveable, Section, TrackSection},
    format,
    images::{Images, Shape},
    library::{self, Filter, Row as LibraryRow, Sort},
    media::{Attach, MediaCommand, MediaKeys},
    models::LazyModel,
    notify,
    settings::Settings,
    system,
    tray::{self, Tray},
};

/// How long closing waits for the backend to record where an episode was left.
const QUIT_TIMEOUT: Duration = Duration::from_secs(3);
const SEARCH_DELAY: Duration = Duration::from_millis(250);
const NOTICE_DURATION: Duration = Duration::from_secs(4);
const MEDIA_ATTACH_INTERVAL: Duration = Duration::from_millis(250);
const MEDIA_ATTACH_ATTEMPTS: u32 = 20;
const REMOTE_TICK: Duration = Duration::from_secs(1);
const REMOTE_VOLUME_DELAY: Duration = Duration::from_millis(300);
// Pages kept for instant back and forward navigation.
const CACHED_PAGES: usize = 12;
// Logical edge lengths of artwork, matching the components in `ui/`.
const THUMBNAIL: u32 = 40;
const PLAYER_COVER: u32 = 56;
const CARD_COVER: u32 = 152;
const HEADER_COVER: u32 = 192;

pub struct App {
    ui: slint::Weak<MainWindow>,
    requests: UnboundedSender<Request>,
    /// Says when the backend thread has finished.
    backend_finished: std::sync::mpsc::Receiver<()>,
    /// For services that report to the app from other threads.
    updates: UnboundedSender<Update>,
    media: RefCell<Option<MediaKeys>>,
    /// The notification-area icon, while closing the window keeps the app running.
    tray: RefCell<Option<Tray>>,
    /// Hide the window once media keys are attached: the app started at
    /// login with the window closed to the tray.
    start_hidden: Cell<bool>,
    /// Name each new song in a notification.
    notify_song_changes: Cell<bool>,
    images: Images,
    rows: Rc<LazyModel<Row>>,
    library: Rc<LazyModel<LibraryItem>>,
    queue: Rc<LazyModel<TrackData>>,
    state: RefCell<State>,
    /// Stand-in artwork for playlists without a picture; `None` while it is
    /// being fetched or when there is none.
    playlist_covers: RefCell<HashMap<Id, Option<Image>>>,
    /// How far the user got in the episodes shown; `None` while it is being
    /// looked up. Forgotten when another page opens, so it stays current.
    progress: RefCell<HashMap<Id, Option<Progress>>>,
    search_timer: Timer,
    media_timer: Timer,
    /// Advances the position shown for another device's playback.
    remote_timer: Timer,
    /// Holds back volume changes for another device until the slider rests.
    volume_timer: Timer,
    /// Fires when the next lyric line is due.
    lyrics_timer: Timer,
    /// When the position on screen was last set.
    position_shown: Cell<Instant>,
    this: Weak<Self>,
    notice_timer: Timer,
}

struct State {
    history: Vec<Visit>,
    /// Index of the visit on screen.
    position: usize,
    /// Identifies the latest page request; answers to older ones are dropped.
    token: u64,
    page: Option<Rc<PageData>>,
    layout: Vec<RowSource>,
    columns: usize,
    cache: Vec<(Page, Rc<PageData>)>,
    ever_connected: bool,

    /// Everything in Your Library, and what of it the sidebar lists.
    library: library::Contents,
    library_rows: Vec<LibraryRow>,
    library_filter: Filter,
    library_sort: Sort,
    /// The folder the sidebar shows the inside of.
    library_folder: Option<usize>,
    username: String,
    liked: HashSet<Id>,
    /// Episodes in Your Episodes.
    episodes: HashSet<Id>,
    /// Saved albums, followed artists and followed podcasts.
    saved: HashSet<Id>,

    now_playing: Option<Track>,
    playing: bool,
    shuffle: bool,
    repeat: Repeat,
    queue: Vec<Track>,
    queue_sequence: u64,
    /// URI and name of the context the user last started, to label the queue.
    started: Option<(String, String)>,

    /// Lyrics of the playing track once requested; the inner `None` covers
    /// both "still loading" and "has none".
    lyrics: Option<(Id, Option<Lyrics>)>,
    devices: Vec<Device>,
    /// What the open dialog is asking, to act on when it is confirmed.
    question: Option<Question>,
    /// Id of the other device that is playing, if one is.
    remote: Option<Box<str>>,
}

impl State {
    fn current_page(&self) -> &Page {
        &self.history[self.position].page
    }

    /// The playlists the user can add to: their own and collaborative ones.
    fn editable(&self) -> impl Iterator<Item = &Playlist> {
        self.library
            .playlists()
            .iter()
            .filter(|playlist| playlist.collaborative || *playlist.owner == *self.username)
    }

    /// The track shown in a row of the page, if that row is a track.
    fn track_at(&self, row: usize) -> Option<&Track> {
        let (Some(page), Some(&RowSource::Track { section, index })) =
            (&self.page, self.layout.get(row))
        else {
            return None;
        };
        match &page.sections[section] {
            Section::Tracks(tracks) => tracks.tracks.get(index).map(|entry| &entry.track),
            _ => None,
        }
    }

    fn is_saved(&self, item: Saveable) -> bool {
        match item {
            Saveable::Track(id) => self.liked.contains(&id),
            Saveable::Episode(id) => self.episodes.contains(&id),
            Saveable::Album(id) | Saveable::Artist(id) | Saveable::Show(id) => {
                self.saved.contains(&id)
            }
        }
    }
}

enum Question {
    Rename(Id),
    /// Remove a playlist, named for the confirmation message.
    Remove(Id, String),
    SignOut,
}

struct Visit {
    page: Page,
    scroll_y: f32,
}

/// Where a row of the page list takes its content from.
#[derive(Clone, Copy)]
enum RowSource {
    Header,
    Title {
        section: usize,
    },
    Columns {
        section: usize,
    },
    Track {
        section: usize,
        index: usize,
    },
    Cards {
        section: usize,
        start: usize,
        len: usize,
    },
    Message {
        section: usize,
    },
}

impl App {
    /// Wires the window to a freshly started backend and opens the home page.
    pub fn start(ui: &MainWindow, config: Config) -> Result<Rc<Self>, slint::PlatformError> {
        let (updates, receiver) = unbounded_channel();
        let (requests, backend_finished) = backend::spawn(config, updates.clone());

        let app = Rc::new_cyclic(|app: &Weak<Self>| Self {
            ui: ui.as_weak(),
            images: Images::new(requests.clone()),
            updates,
            media: RefCell::new(None),
            tray: RefCell::new(None),
            start_hidden: Cell::new(false),
            notify_song_changes: Cell::new(false),
            requests,
            backend_finished,
            rows: Rc::new(LazyModel::new(with(app, Self::row))),
            library: Rc::new(LazyModel::new(with(app, Self::library_item))),
            queue: Rc::new(LazyModel::new(with(app, Self::queue_item))),
            state: RefCell::new(State {
                history: vec![Visit {
                    page: Page::Home,
                    scroll_y: 0.0,
                }],
                position: 0,
                token: 0,
                page: None,
                layout: Vec::new(),
                columns: 2,
                cache: Vec::new(),
                ever_connected: false,
                library: library::Contents::default(),
                library_rows: Vec::new(),
                library_filter: Filter::All,
                library_sort: Sort::default(),
                library_folder: None,
                username: String::new(),
                liked: HashSet::new(),
                episodes: HashSet::new(),
                saved: HashSet::new(),
                now_playing: None,
                playing: false,
                shuffle: false,
                repeat: Repeat::Off,
                queue: Vec::new(),
                queue_sequence: 0,
                started: None,
                lyrics: None,
                devices: Vec::new(),
                question: None,
                remote: None,
            }),
            playlist_covers: RefCell::default(),
            progress: RefCell::default(),
            search_timer: Timer::default(),
            media_timer: Timer::default(),
            remote_timer: Timer::default(),
            volume_timer: Timer::default(),
            lyrics_timer: Timer::default(),
            position_shown: Cell::new(Instant::now()),
            this: app.clone(),
            notice_timer: Timer::default(),
        });

        ui.global::<Nav>().set_rows(ModelRc::from(app.rows.clone()));
        ui.global::<Library>()
            .set_items(ModelRc::from(app.library.clone()));
        ui.global::<Queue>()
            .set_items(ModelRc::from(app.queue.clone()));
        app.connect_callbacks(ui);
        app.show_current();

        slint::spawn_local(app.clone().listen(receiver))
            .map_err(|error| slint::PlatformError::Other(error.to_string()))?;
        Ok(app)
    }

    /// Hooks up the media keys once the native window exists, which on
    /// some platforms is shortly after the event loop starts.
    /// Ends the session, giving it a moment to record where the playing
    /// episode was left. Call once the window has closed.
    pub fn quit(&self) {
        if self.requests.send(Request::Quit).is_ok() {
            _ = self.backend_finished.recv_timeout(QUIT_TIMEOUT);
        }
    }

    /// Whether closing the window should leave the app running.
    pub fn keeps_running_in_tray(&self) -> bool {
        self.tray.borrow().is_some()
    }

    /// Hides the window as soon as media keys are attached, which needs it
    /// on screen; for a start at login with the window closed to the tray.
    pub fn start_in_tray(&self) {
        self.start_hidden.set(true);
    }

    /// Puts the icon in the notification area, or takes it away.
    fn set_tray(&self, on: bool) {
        if !on {
            self.tray.borrow_mut().take();
            return;
        }
        if self.tray.borrow().is_some() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        match Tray::new(&ui) {
            Ok(icon) => {
                *self.tray.borrow_mut() = Some(icon);
                self.update_tray_tooltip();
            }
            Err(error) => log::warn!("couldn't add the tray icon: {error}"),
        }
    }

    fn update_tray_tooltip(&self) {
        let tray = self.tray.borrow();
        let Some(tray) = tray.as_ref() else { return };
        let state = self.state.borrow();
        match &state.now_playing {
            Some(track) => tray.set_tooltip(&format!("{} · {}", track.name, artist_names(track))),
            None => tray.set_tooltip("Vivace"),
        }
    }

    pub fn attach_media_keys(self: &Rc<Self>) {
        let app = Rc::downgrade(self);
        let mut attempts = 0;
        self.media_timer
            .start(TimerMode::Repeated, MEDIA_ATTACH_INTERVAL, move || {
                let Some(app) = app.upgrade() else { return };
                let Some(ui) = app.ui.upgrade() else { return };
                attempts += 1;
                match MediaKeys::attach(ui.window(), app.updates.clone()) {
                    Attach::Ready(media) => *app.media.borrow_mut() = Some(media),
                    Attach::WindowNotReady if attempts < MEDIA_ATTACH_ATTEMPTS => return,
                    Attach::WindowNotReady | Attach::Unavailable => {}
                }
                app.media_timer.stop();
                if app.start_hidden.take()
                    && let Err(error) = ui.hide()
                {
                    log::warn!("couldn't hide the window: {error}");
                }
            });
    }

    async fn listen(self: Rc<Self>, mut updates: UnboundedReceiver<Update>) {
        while let Some(update) = updates.recv().await {
            self.apply(update);
        }
    }

    fn connect_callbacks(self: &Rc<Self>, ui: &MainWindow) {
        let nav = ui.global::<Nav>();
        nav.on_back(self.handler(|app| app.step(-1)));
        nav.on_forward(self.handler(|app| app.step(1)));
        nav.on_open_home(self.handler(|app| app.open(Page::Home)));
        nav.on_open_settings(self.handler(|app| app.open(Page::Settings)));

        let settings = ui.global::<SettingsView>();
        settings.set_version(env!("CARGO_PKG_VERSION").into());
        if let Ok(folder) = vivace_core::data_dir() {
            settings.set_data_folder(folder.display().to_string().into());
        }
        self.show_settings(&Settings::load());
        settings.on_set_quality({
            let app = Rc::downgrade(self);
            move |quality| {
                let quality = match quality {
                    0 => Quality::Low,
                    1 => Quality::Normal,
                    _ => Quality::VeryHigh,
                };
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.quality = quality);
                }
            }
        });
        settings.on_set_normalise({
            let app = Rc::downgrade(self);
            move |normalise| {
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.normalise = normalise);
                }
            }
        });
        settings.on_set_device_name({
            let app = Rc::downgrade(self);
            move |name| {
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.set_device_name(&name));
                }
            }
        });
        settings.on_set_volume_level({
            let app = Rc::downgrade(self);
            move |level| {
                let level = match level {
                    0 => VolumeLevel::Quiet,
                    2 => VolumeLevel::Loud,
                    _ => VolumeLevel::Normal,
                };
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.volume_level = level);
                }
            }
        });
        settings.on_set_gapless({
            let app = Rc::downgrade(self);
            move |on| {
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.gapless = on);
                }
            }
        });
        settings.on_set_autoplay({
            let app = Rc::downgrade(self);
            move |on| {
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.autoplay = on);
                }
            }
        });
        settings.on_set_start_at_login({
            let app = Rc::downgrade(self);
            move |on| {
                let Some(app) = app.upgrade() else { return };
                if let Err(error) = system::set_start_at_login(on) {
                    log::warn!("couldn't change the login entry: {error}");
                    app.notice(&format!("Couldn't change how Vivace starts: {error}"));
                }
                app.with_ui(|ui| {
                    ui.global::<SettingsView>()
                        .set_start_at_login(system::starts_at_login())
                });
            }
        });
        settings.set_tray_supported(tray::SUPPORTED);
        settings.set_notify_supported(notify::SUPPORTED);
        settings.on_set_notify_song_changes({
            let app = Rc::downgrade(self);
            move |on| {
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.notify_song_changes = on);
                }
            }
        });
        settings.on_set_close_to_tray({
            let app = Rc::downgrade(self);
            move |on| {
                if let Some(app) = app.upgrade() {
                    app.change_settings(|settings| settings.close_to_tray = on);
                }
            }
        });
        settings.on_sign_out(self.handler(Self::ask_to_sign_out));
        nav.on_play_page(self.handler(Self::play_page));
        nav.on_toggle_saved(self.handler(Self::toggle_page_saved));
        nav.on_search({
            let app = Rc::downgrade(self);
            move |_| {
                let Some(strong) = app.upgrade() else { return };
                // Wait for a pause in typing before asking Spotify.
                let app = app.clone();
                strong
                    .search_timer
                    .start(TimerMode::SingleShot, SEARCH_DELAY, move || {
                        if let Some(app) = app.upgrade() {
                            app.search();
                        }
                    });
            }
        });
        nav.on_play_track({
            let app = Rc::downgrade(self);
            move |row| {
                if let Some(app) = app.upgrade() {
                    app.play_row(row as usize);
                }
            }
        });
        nav.on_track_action({
            let app = Rc::downgrade(self);
            move |row, action| {
                if let Some(app) = app.upgrade() {
                    app.track_action(row as usize, action);
                }
            }
        });
        nav.on_add_to_playlist({
            let app = Rc::downgrade(self);
            move |row, playlist| {
                if let Some(app) = app.upgrade() {
                    app.add_to_playlist(row as usize, playlist);
                }
            }
        });
        nav.on_open_card({
            let app = Rc::downgrade(self);
            move |row, card| {
                if let Some(app) = app.upgrade() {
                    app.open_card(row as usize, card as usize);
                }
            }
        });
        nav.on_open_external(self.handler(|app| {
            let link = app
                .state
                .borrow()
                .page
                .as_ref()
                .and_then(|page| page.header.as_ref()?.external.clone());
            // Hands over to the browser, which offers Spotify's app if installed.
            if let Some(link) = link
                && let Err(error) = open::that_detached(&link)
            {
                log::warn!("couldn't open {link}: {error}");
                app.notice("Couldn't open Spotify");
            }
        }));
        nav.on_open_section({
            let app = Rc::downgrade(self);
            move |row| {
                if let Some(app) = app.upgrade() {
                    app.open_section(row as usize);
                }
            }
        });
        nav.on_columns_changed({
            let app = Rc::downgrade(self);
            move |columns| {
                if let Some(app) = app.upgrade() {
                    app.set_columns(columns.max(1) as usize);
                }
            }
        });

        let lyrics = ui.global::<LyricsView>();
        lyrics.on_shown(self.handler(|app| {
            app.load_lyrics();
            app.follow_lyrics();
        }));
        lyrics.on_seek({
            let app = Rc::downgrade(self);
            move |index| {
                if let Some(app) = app.upgrade() {
                    app.seek_to_lyric(index.max(0) as usize);
                }
            }
        });

        ui.global::<Devices>().on_transfer({
            let app = Rc::downgrade(self);
            move |index| {
                if let Some(app) = app.upgrade() {
                    app.transfer(index as usize);
                }
            }
        });

        let library = ui.global::<Library>();
        library.on_rename({
            let app = Rc::downgrade(self);
            move |index| {
                if let Some(app) = app.upgrade() {
                    app.ask_about_playlist(index as usize, true);
                }
            }
        });
        library.on_remove({
            let app = Rc::downgrade(self);
            move |index| {
                if let Some(app) = app.upgrade() {
                    app.remove_library_item(index as usize);
                }
            }
        });
        library.set_sorts(ModelRc::new(slint::VecModel::from(
            Sort::ALL
                .iter()
                .map(|sort| SharedString::from(sort.label()))
                .collect::<Vec<_>>(),
        )));
        library.on_set_filter({
            let app = Rc::downgrade(self);
            move |chip| {
                if let Some(app) = app.upgrade() {
                    app.state.borrow_mut().library_filter = Filter::from_chip(chip);
                    app.show_library();
                }
            }
        });
        library.on_set_sort({
            let app = Rc::downgrade(self);
            move |index| {
                let Some(app) = app.upgrade() else { return };
                let sort = usize::try_from(index)
                    .ok()
                    .and_then(|index| Sort::ALL.get(index).copied())
                    .unwrap_or_default();
                app.state.borrow_mut().library_sort = sort;
                Settings::update(|settings| settings.library_sort = sort);
                app.show_library();
            }
        });
        library.on_close_folder(self.handler(|app| {
            {
                let mut state = app.state.borrow_mut();
                let folder = state.library_folder;
                state.library_folder = folder.and_then(|folder| state.library.parent(folder));
            }
            app.show_library();
        }));
        ui.global::<Dialog>().on_confirmed({
            let app = Rc::downgrade(self);
            move |text| {
                if let Some(app) = app.upgrade() {
                    app.dialog_confirmed(text.trim());
                }
            }
        });

        ui.global::<Library>().on_open({
            let app = Rc::downgrade(self);
            move |index| {
                if let Some(app) = app.upgrade() {
                    app.open_library_item(index as usize);
                }
            }
        });

        let player = ui.global::<Player>();
        player.on_toggle(self.handler(|app| {
            let playing = app.state.borrow().playing;
            app.request(if playing {
                Request::Pause
            } else {
                Request::Play
            });
        }));
        player.on_next(self.handler(|app| app.request(Request::Next)));
        player.on_previous(self.handler(|app| app.request(Request::Previous)));
        player.on_toggle_shuffle(self.handler(|app| {
            let shuffle = !app.state.borrow().shuffle;
            app.set_shuffle(shuffle);
            app.request(Request::Shuffle(shuffle));
        }));
        player.on_cycle_repeat(self.handler(|app| {
            let repeat = match app.state.borrow().repeat {
                Repeat::Off => Repeat::Context,
                Repeat::Context => Repeat::Track,
                Repeat::Track => Repeat::Off,
            };
            app.set_repeat(repeat);
            app.request(Request::Repeat(repeat));
        }));
        player.on_toggle_liked(self.handler(|app| {
            let item = app.state.borrow().now_playing.as_ref().map(saveable);
            if let Some(item) = item {
                app.toggle_saved(item);
            }
        }));
        player.on_open_album(self.handler(|app| {
            let page = app.state.borrow().now_playing.as_ref().map(album_page);
            if let Some(page) = page {
                app.open(page);
            }
        }));
        player.on_open_artist(self.handler(|app| {
            let page = app
                .state
                .borrow()
                .now_playing
                .as_ref()
                .and_then(artist_page);
            if let Some(page) = page {
                app.open(page);
            }
        }));
        player.on_seek({
            let app = Rc::downgrade(self);
            move |position_ms| {
                if let Some(app) = app.upgrade() {
                    let position_ms = position_ms.max(0) as u32;
                    app.show_position(position_ms);
                    app.request(Request::Seek(position_ms));
                }
            }
        });
        player.on_set_volume({
            let app = Rc::downgrade(self);
            move |volume| {
                if let Some(app) = app.upgrade() {
                    app.with_ui(|ui| ui.global::<Player>().set_volume(volume));
                    app.send_volume(volume);
                }
            }
        });
    }

    /// Wraps a method as a callback that does nothing once the app is gone.
    fn handler(self: &Rc<Self>, action: impl Fn(&Self) + 'static) -> impl FnMut() + 'static {
        let app = Rc::downgrade(self);
        move || {
            if let Some(app) = app.upgrade() {
                action(&app);
            }
        }
    }

    fn request(&self, request: Request) {
        // The backend only goes away when the process is exiting.
        _ = self.requests.send(request);
    }

    fn with_ui(&self, action: impl FnOnce(&MainWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            action(&ui);
        }
    }

    fn shape(&self, size: u32, round: bool) -> Shape {
        let scale = self
            .ui
            .upgrade()
            .map_or(1.0, |ui| ui.window().scale_factor());
        Shape { size, scale, round }
    }

    // ---- backend updates -------------------------------------------------

    fn apply(&self, update: Update) {
        match update {
            Update::Connecting => {
                if self.state.borrow().ever_connected {
                    self.notice("Reconnecting to Spotify…");
                } else {
                    self.with_ui(|ui| {
                        let session = ui.global::<Session>();
                        session.set_status(
                            "Connecting to Spotify…\nIf a browser tab opens, approve access there."
                                .into(),
                        );
                    });
                }
            }
            Update::PlaylistChanged { playlist, notice } => {
                self.notice(&notice);
                self.request(Request::Library);
                // Reload the playlist if it is the one on screen.
                if *self.state.borrow().current_page() == Page::Playlist(playlist) {
                    self.show_current();
                }
            }
            Update::PlaylistRemoved { playlist, notice } => {
                self.notice(&notice);
                self.request(Request::Library);
                // Its page has nothing left to show.
                if *self.state.borrow().current_page() == Page::Playlist(playlist) {
                    self.open(Page::Home);
                }
            }
            Update::SignedOut => {
                {
                    // Nothing of the previous account stays on screen.
                    let mut state = self.state.borrow_mut();
                    state.ever_connected = false;
                    state.username.clear();
                    state.library = library::Contents::default();
                    state.library_rows.clear();
                    state.library_folder = None;
                    state.liked.clear();
                    state.episodes.clear();
                    state.saved.clear();
                    state.cache.clear();
                    state.page = None;
                    state.layout.clear();
                    state.history = vec![Visit {
                        page: Page::Home,
                        scroll_y: 0.0,
                    }];
                    state.position = 0;
                }
                self.library.reset(0);
                self.rows.reset(0);
                self.with_ui(|ui| {
                    ui.global::<SettingsView>().set_visible(false);
                    ui.global::<Session>().set_connected(false);
                    ui.global::<SettingsView>()
                        .set_username(SharedString::default());
                });
            }
            Update::Connected { username } => {
                self.with_ui(|ui| {
                    ui.global::<SettingsView>()
                        .set_username(username.as_str().into())
                });
                self.state.borrow_mut().username = username;
                self.state.borrow_mut().ever_connected = true;
                self.with_ui(|ui| ui.global::<Session>().set_connected(true));
                self.request(Request::Library);
                self.show_current();
            }
            Update::Failed(error) => {
                let message =
                    format!("Couldn't connect to Spotify: {error}\nTrying again shortly.");
                if self.state.borrow().ever_connected {
                    self.notice(&message);
                } else {
                    self.with_ui(|ui| ui.global::<Session>().set_status(message.into()));
                }
            }
            Update::Library {
                contents,
                liked,
                episodes,
            } => {
                {
                    let mut state = self.state.borrow_mut();
                    state.saved = contents
                        .albums
                        .iter()
                        .map(|(album, _)| album.id)
                        .chain(contents.artists.iter().map(|(artist, _)| artist.id))
                        .chain(contents.shows.iter().map(|(show, _)| show.id))
                        .collect();
                    // The open folder may be gone, or another one by now.
                    if state
                        .library_folder
                        .is_some_and(|folder| folder >= contents.rootlist.folders.len())
                    {
                        state.library_folder = None;
                    }
                    state.library = contents;
                    state.liked = liked.into_iter().collect();
                    state.episodes = episodes.into_iter().collect();
                }
                self.show_library();
                self.rows.refresh_all();
                self.queue.refresh_all();
                self.show_player_liked();
                self.show_editable();
            }
            Update::Page { token, result } => self.page_loaded(token, result),
            Update::Image { key, pixels } => {
                let url = key.url.clone();
                self.images.insert(key, pixels);
                self.image_arrived(&url);
            }
            Update::PlaylistCover { playlist, image } => {
                self.playlist_covers.borrow_mut().insert(playlist, image);
                self.library.refresh_all();
                self.refresh_cards();
            }
            Update::NowPlaying(track) => self.now_playing(track),
            Update::EpisodeProgress { episode, progress } => {
                self.progress.borrow_mut().insert(episode, Some(progress));
                self.refresh_track(episode);
            }
            // While another device plays, its state is what the player shows.
            Update::Playback(_) | Update::Position(_) | Update::Stopped | Update::Volume(_)
                if self.state.borrow().remote.is_some() => {}
            Update::Playback(Playback {
                playing,
                position_ms,
            }) => {
                self.state.borrow_mut().playing = playing;
                self.with_ui(|ui| ui.global::<Player>().set_playing(playing));
                self.show_position(position_ms);
                if let Some(media) = self.media.borrow_mut().as_mut() {
                    media.set_playback(playing, position_ms);
                }
            }
            Update::Media(command) => self.media_command(command),
            Update::Position(position_ms) => self.show_position(position_ms),
            Update::Lyrics { track, lyrics } => self.lyrics_loaded(track, lyrics),
            Update::Stopped => {
                self.state.borrow_mut().playing = false;
                self.with_ui(|ui| ui.global::<Player>().set_playing(false));
                self.follow_lyrics();
            }
            Update::Volume(volume) => {
                self.with_ui(|ui| ui.global::<Player>().set_volume(volume));
            }
            Update::Shuffle(shuffle) => self.set_shuffle(shuffle),
            Update::Repeat(repeat) => self.set_repeat(repeat),
            Update::Queue(queue) => self.queue_changed(queue),
            Update::Connect(connect) => self.connect_changed(connect),
            Update::LibraryChanged => self.request(Request::Library),
            Update::SaveFailed { item, saved } => {
                self.set_saved(item, !saved);
                self.notice("Couldn't update your library");
            }
            Update::Notice(message) => self.notice(&message),
        }
    }

    fn connect_changed(&self, connect: Connect) {
        let was_remote = self.state.borrow().remote.is_some();
        let local_active = connect
            .devices
            .iter()
            .any(|device| device.is_local && device.is_active);
        let items: Vec<DeviceItem> = connect
            .devices
            .iter()
            .map(|device| DeviceItem {
                name: device.name.as_ref().into(),
                kind: match device.kind {
                    DeviceKind::Computer => DeviceIcon::Computer,
                    DeviceKind::Phone => DeviceIcon::Phone,
                    DeviceKind::Speaker => DeviceIcon::Speaker,
                    DeviceKind::Tv => DeviceIcon::Tv,
                    DeviceKind::Other => DeviceIcon::Other,
                },
                active: device.is_active,
                local: device.is_local,
            })
            .collect();
        let remote_name = connect.remote.as_ref().and_then(|remote| {
            let device = connect
                .devices
                .iter()
                .find(|device| device.id == remote.device)?;
            Some(device.name.clone())
        });
        {
            let mut state = self.state.borrow_mut();
            state.remote = connect.remote.as_ref().map(|remote| remote.device.clone());
            state.devices = connect.devices;
            match &connect.remote {
                Some(remote) => state.playing = remote.playing,
                // Nothing plays anywhere; a local start will say otherwise.
                None if was_remote && !local_active => state.playing = false,
                None => {}
            }
        }

        let playing = self.state.borrow().playing;
        self.with_ui(|ui| {
            let devices = ui.global::<Devices>();
            devices.set_items(ModelRc::from(items.as_slice()));
            devices.set_remote_name(remote_name.as_deref().unwrap_or_default().into());

            let player = ui.global::<Player>();
            player.set_playing(playing);
            if let Some(remote) = &connect.remote {
                player.set_duration_ms(remote.duration_ms as i32);
                player.set_volume(remote.volume);
            }
        });
        if let Some(remote) = &connect.remote {
            self.show_position(remote.position_ms);
            self.set_shuffle(remote.shuffle);
            self.set_repeat(remote.repeat);
            if let Some(media) = self.media.borrow_mut().as_mut() {
                media.set_playback(remote.playing, remote.position_ms);
            }
        }

        // The device reports its position only when something changes, so
        // the seconds in between are counted here.
        if connect.remote.as_ref().is_some_and(|remote| remote.playing) {
            let app = self.this.clone();
            self.remote_timer
                .start(TimerMode::Repeated, REMOTE_TICK, move || {
                    let Some(app) = app.upgrade() else { return };
                    let Some(ui) = app.ui.upgrade() else { return };
                    let player = ui.global::<Player>();
                    let position = player.get_position_ms() + REMOTE_TICK.as_millis() as i32;
                    app.show_position(position.clamp(0, player.get_duration_ms()) as u32);
                });
        } else {
            self.remote_timer.stop();
        }
    }

    fn transfer(&self, index: usize) {
        let request = {
            let state = self.state.borrow();
            match state.devices.get(index) {
                Some(device) if !device.is_active => Request::Transfer {
                    device: device.id.clone(),
                    local: device.is_local,
                },
                _ => return,
            }
        };
        self.request(request);
    }

    /// Applies a volume change: at once here, once the slider rests elsewhere.
    fn send_volume(&self, volume: f32) {
        if self.state.borrow().remote.is_none() {
            return self.request(Request::Volume(volume));
        }
        let requests = self.requests.clone();
        self.volume_timer
            .start(TimerMode::SingleShot, REMOTE_VOLUME_DELAY, move || {
                _ = requests.send(Request::Volume(volume));
            });
    }

    fn notice(&self, message: &str) {
        self.with_ui(|ui| ui.global::<Session>().set_notice(message.into()));
        let ui = self.ui.clone();
        self.notice_timer
            .start(TimerMode::SingleShot, NOTICE_DURATION, move || {
                if let Some(ui) = ui.upgrade() {
                    ui.global::<Session>().set_notice(SharedString::default());
                }
            });
    }

    fn media_command(&self, command: MediaCommand) {
        let playing = self.state.borrow().playing;
        let seek = |position_ms: i64| {
            let position_ms = position_ms.clamp(0, i64::from(i32::MAX)) as u32;
            self.show_position(position_ms);
            Request::Seek(position_ms)
        };
        self.request(match command {
            MediaCommand::Play => Request::Play,
            MediaCommand::Pause => Request::Pause,
            MediaCommand::Toggle if playing => Request::Pause,
            MediaCommand::Toggle => Request::Play,
            MediaCommand::Next => Request::Next,
            MediaCommand::Previous => Request::Previous,
            MediaCommand::SeekTo(position) => {
                seek(i64::try_from(position.as_millis()).unwrap_or(i64::MAX))
            }
            MediaCommand::SeekBy(offset_ms) => {
                let position_ms = self
                    .ui
                    .upgrade()
                    .map_or(0, |ui| ui.global::<Player>().get_position_ms());
                seek(i64::from(position_ms).saturating_add(offset_ms))
            }
        });
    }

    fn set_shuffle(&self, shuffle: bool) {
        self.state.borrow_mut().shuffle = shuffle;
        self.with_ui(|ui| ui.global::<Player>().set_shuffle(shuffle));
    }

    fn set_repeat(&self, repeat: Repeat) {
        self.state.borrow_mut().repeat = repeat;
        self.with_ui(|ui| {
            ui.global::<Player>().set_repeat(match repeat {
                Repeat::Off => RepeatMode::Off,
                Repeat::Context => RepeatMode::All,
                Repeat::Track => RepeatMode::One,
            });
        });
    }

    fn now_playing(&self, track: Track) {
        let (previous, remote) = {
            let mut state = self.state.borrow_mut();
            let previous = state.now_playing.as_ref().map(|track| track.id);
            state.now_playing = Some(track.clone());
            (previous, state.remote.is_some())
        };
        self.with_ui(|ui| {
            let player = ui.global::<Player>();
            player.set_available(true);
            player.set_title(track.name.as_ref().into());
            player.set_artists(artist_names(&track).into());
            player.set_duration_ms(track.duration_ms as i32);
            player.set_cover(
                self.images
                    .get(track.cover.as_ref(), self.shape(PLAYER_COVER, false)),
            );
        });
        self.update_tray_tooltip();
        // A local track starts from the top; another device says where it is.
        if !remote {
            self.show_position(0);
        }
        // Only songs that start here, as with Spotify's own notifications.
        if !remote && previous != Some(track.id) && self.notify_song_changes.get() {
            notify::announce(track.name.to_string(), artist_names(&track));
        }
        if let Some(media) = self.media.borrow_mut().as_mut() {
            media.set_track(&track, &artist_names(&track));
        }
        self.load_lyrics();
        self.show_player_liked();
        for id in previous.into_iter().chain([track.id]) {
            self.refresh_track(id);
        }
    }

    /// Shows a playback position and keeps the lyrics in step with it.
    fn show_position(&self, position_ms: u32) {
        self.with_ui(|ui| ui.global::<Player>().set_position_ms(position_ms as i32));
        self.position_shown.set(Instant::now());
        self.follow_lyrics();
        self.follow_progress(position_ms);
    }

    /// Keeps the playing episode's progress current where it is shown.
    fn follow_progress(&self, position_ms: u32) {
        let episode = {
            let state = self.state.borrow();
            match &state.now_playing {
                Some(track) if track.is_episode() => track.id,
                _ => return,
            }
        };
        let shown = {
            let mut progress = self.progress.borrow_mut();
            match progress.get_mut(&episode) {
                Some(known) => {
                    *known = Some(Progress::InProgress { position_ms });
                    true
                }
                None => false,
            }
        };
        if shown {
            self.refresh_track(episode);
        }
    }

    /// How far the user got in an episode, looked up the first time it is asked for.
    fn episode_progress(&self, episode: Id) -> Option<Progress> {
        let mut progress = self.progress.borrow_mut();
        if let Some(known) = progress.get(&episode) {
            return *known;
        }
        progress.insert(episode, None);
        self.request(Request::EpisodeProgress(episode));
        None
    }

    /// Fetches the playing track's lyrics if the panel is open and lacks them.
    fn load_lyrics(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<LyricsView>();
        let track = {
            let mut state = self.state.borrow_mut();
            let track = match &state.now_playing {
                Some(track) if track.is_episode() => {
                    view.set_lines(ModelRc::default());
                    view.set_status("Podcasts have no lyrics".into());
                    return;
                }
                Some(track) => track.id,
                None => {
                    view.set_status("Play something to see its lyrics".into());
                    return;
                }
            };
            if !view.get_visible()
                || state
                    .lyrics
                    .as_ref()
                    .is_some_and(|(loaded, _)| *loaded == track)
            {
                return;
            }
            // Marks the request as made; the answer fills in the lines.
            state.lyrics = Some((track, None));
            track
        };
        view.set_lines(ModelRc::default());
        view.set_current(-1);
        view.set_status("Loading lyrics…".into());
        self.request(Request::Lyrics(track));
    }

    fn lyrics_loaded(&self, track: Id, lyrics: Option<Lyrics>) {
        {
            let mut state = self.state.borrow_mut();
            if state.now_playing.as_ref().map(|playing| playing.id) != Some(track) {
                return;
            }
            state.lyrics = Some((track, lyrics.clone()));
        }
        self.with_ui(|ui| {
            let view = ui.global::<LyricsView>();
            match &lyrics {
                Some(lyrics) => {
                    let lines: Vec<SharedString> = lyrics
                        .lines
                        .iter()
                        .map(|line| line.text.as_ref().into())
                        .collect();
                    view.set_lines(ModelRc::from(lines.as_slice()));
                    view.set_synced(lyrics.synced);
                }
                None => view.set_status("No lyrics for this song".into()),
            }
        });
        self.follow_lyrics();
    }

    /// Highlights the line being sung and wakes up again for the next one.
    fn follow_lyrics(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let view = ui.global::<LyricsView>();
        self.lyrics_timer.stop();
        if !view.get_visible() {
            return;
        }

        let state = self.state.borrow();
        let Some((_, Some(lyrics))) = &state.lyrics else {
            return;
        };
        if !lyrics.synced {
            return view.set_current(-1);
        }
        // The position on screen moves once a second; add what has passed since.
        let shown = ui.global::<Player>().get_position_ms().max(0) as u32;
        let elapsed = if state.playing {
            self.position_shown.get().elapsed().as_millis() as u32
        } else {
            0
        };
        let position = shown.saturating_add(elapsed);

        let upcoming = lyrics
            .lines
            .partition_point(|line| line.start_ms <= position);
        view.set_current(upcoming as i32 - 1);
        if let (true, Some(next)) = (state.playing, lyrics.lines.get(upcoming)) {
            let wait = Duration::from_millis(u64::from(next.start_ms - position));
            let app = self.this.clone();
            self.lyrics_timer
                .start(TimerMode::SingleShot, wait, move || {
                    if let Some(app) = app.upgrade() {
                        app.follow_lyrics();
                    }
                });
        }
    }

    fn seek_to_lyric(&self, index: usize) {
        let position_ms = {
            let state = self.state.borrow();
            match &state.lyrics {
                Some((_, Some(lyrics))) if lyrics.synced => {
                    lyrics.lines.get(index).map(|line| line.start_ms)
                }
                _ => None,
            }
        };
        if let Some(position_ms) = position_ms {
            self.show_position(position_ms);
            self.request(Request::Seek(position_ms));
        }
    }

    fn queue_changed(&self, queue: ResolvedQueue) {
        let (len, queued, context) = {
            let mut state = self.state.borrow_mut();
            if queue.sequence < state.queue_sequence {
                return;
            }
            state.queue_sequence = queue.sequence;
            state.queue = queue.tracks;
            let context = match &state.started {
                Some((uri, name)) if **uri == *queue.context_uri => name.clone(),
                _ => String::new(),
            };
            (state.queue.len(), queue.queued, context)
        };
        self.queue.reset(len);
        self.with_ui(|ui| {
            let panel = ui.global::<Queue>();
            panel.set_queued(queued as i32);
            panel.set_context(context.into());
        });
    }

    /// Redraws everything that shows the image at `url`.
    fn image_arrived(&self, url: &str) {
        let shows = |image: &Option<Image>| image.as_ref().is_some_and(|image| image.url() == url);
        let (rows, library, queue, player) = {
            let state = self.state.borrow();
            let rows: Vec<usize> = match &state.page {
                Some(page) => state
                    .layout
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| match **source {
                        RowSource::Header => page
                            .header
                            .as_ref()
                            .is_some_and(|header| shows(&header.image)),
                        RowSource::Track { section, index } => match &page.sections[section] {
                            Section::Tracks(tracks) => shows(&tracks.tracks[index].track.cover),
                            _ => false,
                        },
                        RowSource::Cards {
                            section,
                            start,
                            len,
                        } => match &page.sections[section] {
                            Section::Cards(cards) => cards.cards[start..start + len]
                                .iter()
                                .any(|card| shows(&self.card_image(card, false))),
                            _ => false,
                        },
                        _ => false,
                    })
                    .map(|(row, _)| row)
                    .collect(),
                None => Vec::new(),
            };
            let library: Vec<usize> = state
                .library_rows
                .iter()
                .enumerate()
                .filter(|(_, row)| shows(&self.library_image(&state, **row, false)))
                .map(|(index, _)| index)
                .collect();
            let queue: Vec<usize> = state
                .queue
                .iter()
                .enumerate()
                .filter(|(_, track)| shows(&track.cover))
                .map(|(index, _)| index)
                .collect();
            let player = state
                .now_playing
                .as_ref()
                .filter(|track| shows(&track.cover))
                .cloned();
            (rows, library, queue, player)
        };
        rows.into_iter().for_each(|row| self.rows.refresh(row));
        library
            .into_iter()
            .for_each(|row| self.library.refresh(row));
        queue.into_iter().for_each(|row| self.queue.refresh(row));
        if let Some(track) = player {
            let cover = self
                .images
                .get(track.cover.as_ref(), self.shape(PLAYER_COVER, false));
            self.with_ui(|ui| ui.global::<Player>().set_cover(cover));
        }
    }

    // ---- navigation ------------------------------------------------------

    fn open(&self, page: Page) {
        {
            let mut state = self.state.borrow_mut();
            if state.history[state.position].page == page {
                return;
            }
            self.remember_scroll(&mut state);
            let position = state.position + 1;
            state.history.truncate(position);
            state.history.push(Visit {
                page,
                scroll_y: 0.0,
            });
            state.position = position;
        }
        self.show_current();
    }

    fn step(&self, offset: isize) {
        {
            let mut state = self.state.borrow_mut();
            let Some(position) = state.position.checked_add_signed(offset) else {
                return;
            };
            if position >= state.history.len() {
                return;
            }
            self.remember_scroll(&mut state);
            state.position = position;
        }
        self.show_current();
    }

    fn remember_scroll(&self, state: &mut State) {
        if let Some(ui) = self.ui.upgrade() {
            let position = state.position;
            state.history[position].scroll_y = ui.global::<Nav>().get_scroll_y();
        }
    }

    /// Shows the current visit: at once if cached, and always refreshed from Spotify.
    fn show_current(&self) {
        // Episodes may have moved on elsewhere since they were last shown.
        self.progress.borrow_mut().clear();
        let (page, token, cached, scroll_y, can_go_back, can_go_forward) = {
            let mut state = self.state.borrow_mut();
            state.token += 1;
            let visit = &state.history[state.position];
            let cached = state
                .cache
                .iter()
                .find(|(page, _)| *page == visit.page)
                .map(|(_, data)| data.clone());
            (
                visit.page.clone(),
                state.token,
                cached,
                visit.scroll_y,
                state.position > 0,
                state.position + 1 < state.history.len(),
            )
        };

        let is_settings = page == Page::Settings;
        let is_cached = cached.is_some() || is_settings;
        if let Some(data) = cached {
            self.set_page(data, scroll_y);
        }
        self.with_ui(|ui| {
            ui.global::<SettingsView>().set_visible(is_settings);
            let nav = ui.global::<Nav>();
            nav.set_can_go_back(can_go_back);
            nav.set_can_go_forward(can_go_forward);
            nav.set_home_selected(page == Page::Home);
            nav.set_loading(!is_cached);
            // The search field mirrors the page: empty away from results.
            let query = match &page {
                Page::Search(query) | Page::SearchAll(query, _) => query.as_str(),
                _ => "",
            };
            if nav.get_query() != query {
                nav.set_query(query.into());
            }
        });
        self.library.refresh_all();
        self.show_editable();
        if !is_settings {
            self.request(Request::Page { token, page });
        }
    }

    fn page_loaded(&self, token: u64, result: Result<PageData, String>) {
        let (data, scroll_y) = {
            let mut state = self.state.borrow_mut();
            if token != state.token {
                return;
            }
            let data = Rc::new(result.unwrap_or_else(|error| PageData {
                header: None,
                context: None,
                listed_order: false,
                sections: vec![Section::Message(format!(
                    "Couldn't load this page\n{error}"
                ))],
            }));
            let page = state.history[state.position].page.clone();
            let scroll_y = state.history[state.position].scroll_y;

            let unchanged = state.page.as_ref().is_some_and(|shown| **shown == *data);
            state.cache.retain(|(cached, _)| *cached != page);
            state.cache.push((page, data.clone()));
            if state.cache.len() > CACHED_PAGES {
                state.cache.remove(0);
            }
            if unchanged {
                drop(state);
                self.with_ui(|ui| ui.global::<Nav>().set_loading(false));
                return;
            }
            (data, scroll_y)
        };
        self.set_page(data, scroll_y);
        self.with_ui(|ui| ui.global::<Nav>().set_loading(false));
    }

    fn set_page(&self, data: Rc<PageData>, scroll_y: f32) {
        let len = {
            let mut state = self.state.borrow_mut();
            state.layout = layout(&data, state.columns);
            state.page = Some(data);
            state.layout.len()
        };
        self.rows.reset(len);
        self.with_ui(|ui| ui.global::<Nav>().set_scroll_y(scroll_y));
    }

    fn set_columns(&self, columns: usize) {
        let len = {
            let mut state = self.state.borrow_mut();
            if state.columns == columns {
                return;
            }
            state.columns = columns;
            let Some(page) = state.page.clone() else {
                return;
            };
            state.layout = layout(&page, columns);
            state.layout.len()
        };
        self.rows.reset(len);
    }

    fn search(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let query = ui.global::<Nav>().get_query().trim().to_owned();
        if query.is_empty() {
            return;
        }
        {
            // Refining a search replaces it instead of piling up history.
            let mut state = self.state.borrow_mut();
            let position = state.position;
            if let Page::Search(current) = &mut state.history[position].page {
                if *current == query {
                    return;
                }
                *current = query;
                state.history[position].scroll_y = 0.0;
                state.history.truncate(position + 1);
                drop(state);
                return self.show_current();
            }
        }
        self.open(Page::Search(query));
    }

    fn open_library_item(&self, index: usize) {
        let page = {
            let mut state = self.state.borrow_mut();
            let Some(&row) = state.library_rows.get(index) else {
                return;
            };
            let library = &state.library;
            match row {
                LibraryRow::LikedSongs => Page::Liked,
                LibraryRow::YourEpisodes => Page::YourEpisodes,
                LibraryRow::Playlist(index) => Page::Playlist(library.playlists()[index].id),
                LibraryRow::Album(index) => Page::Album(library.albums[index].0.id),
                LibraryRow::Artist(index) => Page::Artist(library.artists[index].0.id),
                LibraryRow::Show(index) => Page::Show(library.shows[index].0.id),
                LibraryRow::Folder(folder) => {
                    state.library_folder = Some(folder);
                    drop(state);
                    return self.show_library();
                }
            }
        };
        self.open(page);
    }

    /// Lists the library as the sidebar's filter, order and folder ask.
    fn show_library(&self) {
        let (len, chip, sort, folder) = {
            let mut state = self.state.borrow_mut();
            let rows = state.library.rows(
                state.library_filter,
                state.library_sort,
                state.library_folder,
            );
            state.library_rows = rows;
            let folder = state
                .library_folder
                .map(|folder| state.library.rootlist.folders[folder].name.to_string())
                .unwrap_or_default();
            let sort = Sort::ALL
                .iter()
                .position(|sort| *sort == state.library_sort)
                .unwrap_or_default();
            (
                state.library_rows.len(),
                state.library_filter.chip(),
                sort as i32,
                folder,
            )
        };
        self.with_ui(|ui| {
            let library = ui.global::<Library>();
            library.set_filter(chip);
            library.set_sort(sort);
            library.set_folder(folder.into());
        });
        self.library.reset(len);
    }

    /// Takes an entry out of the library: playlists after asking, albums,
    /// artists and podcasts at once, as in Spotify's apps.
    fn remove_library_item(&self, index: usize) {
        let item = {
            let state = self.state.borrow();
            let library = &state.library;
            match state.library_rows.get(index) {
                Some(LibraryRow::Album(index)) => Saveable::Album(library.albums[*index].0.id),
                Some(LibraryRow::Artist(index)) => Saveable::Artist(library.artists[*index].0.id),
                Some(LibraryRow::Show(index)) => Saveable::Show(library.shows[*index].0.id),
                Some(LibraryRow::Playlist(_)) => {
                    drop(state);
                    return self.ask_about_playlist(index, false);
                }
                _ => return,
            }
        };
        self.set_saved(item, false);
        self.request(Request::SetSaved { item, saved: false });
    }

    /// Opens the page a section's "Show all" link leads to.
    fn open_section(&self, row: usize) {
        let target = {
            let state = self.state.borrow();
            let (Some(page), Some(&RowSource::Title { section })) =
                (&state.page, state.layout.get(row))
            else {
                return;
            };
            section_more(&page.sections[section]).cloned()
        };
        if let Some(target) = target {
            self.open(target);
        }
    }

    fn open_card(&self, row: usize, card: usize) {
        let target = {
            let state = self.state.borrow();
            let (
                Some(page),
                Some(&RowSource::Cards {
                    section,
                    start,
                    len,
                }),
            ) = (&state.page, state.layout.get(row))
            else {
                return;
            };
            match &page.sections[section] {
                Section::Cards(cards) if card < len => cards.cards[start + card].target.clone(),
                _ => return,
            }
        };
        self.open(target);
    }

    // ---- playback and track actions --------------------------------------

    fn play_page(&self) {
        let request = {
            let mut state = self.state.borrow_mut();
            let Some(page) = state.page.clone() else {
                return;
            };
            let Some(uri) = &page.context else {
                return;
            };
            if page.listed_order {
                let Some(tracks) = page.sections.iter().find_map(|section| match section {
                    Section::Tracks(tracks) => Some(tracks),
                    _ => None,
                }) else {
                    return;
                };
                play_listed(&mut state, &page, tracks, 0)
            } else {
                state.started = Some((uri.clone(), page_title(&page)));
                Request::PlayContext {
                    uri: uri.clone(),
                    start: Start::Beginning,
                    shuffle: state.shuffle,
                    repeat: state.repeat,
                }
            }
        };
        self.request(request);
    }

    fn play_row(&self, row: usize) {
        let request = {
            let mut state = self.state.borrow_mut();
            let (Some(page), Some(&RowSource::Track { section, index })) =
                (state.page.clone(), state.layout.get(row))
            else {
                return;
            };
            let Section::Tracks(tracks) = &page.sections[section] else {
                return;
            };
            let track = &tracks.tracks[index].track;
            if !track.playable {
                return;
            }
            match &page.context {
                Some(uri) if !page.listed_order => {
                    state.started = Some((uri.clone(), page_title(&page)));
                    Request::PlayContext {
                        uri: uri.clone(),
                        start: Start::Item(track.item()),
                        shuffle: state.shuffle,
                        repeat: state.repeat,
                    }
                }
                _ => play_listed(&mut state, &page, tracks, index),
            }
        };
        self.request(request);
    }

    fn track_action(&self, row: usize, action: TrackAction) {
        let track = {
            let state = self.state.borrow();
            let (Some(page), Some(&RowSource::Track { section, index })) =
                (&state.page, state.layout.get(row))
            else {
                return;
            };
            match &page.sections[section] {
                Section::Tracks(tracks) => tracks.tracks[index].track.clone(),
                _ => return,
            }
        };
        match action {
            TrackAction::ToggleLiked => self.toggle_saved(saveable(&track)),
            TrackAction::TogglePlayed if track.is_episode() => {
                let played = self.episode_progress(track.id) == Some(Progress::Completed);
                let progress = if played {
                    Progress::NotStarted
                } else {
                    Progress::Completed
                };
                // Shown at once; the backend corrects it if Spotify refuses.
                self.progress.borrow_mut().insert(track.id, Some(progress));
                self.refresh_track(track.id);
                self.request(Request::SetProgress {
                    episode: track.id,
                    progress,
                });
            }
            TrackAction::TogglePlayed => {}
            TrackAction::AddToQueue => self.request(Request::AddToQueue(track.item())),
            TrackAction::OpenAlbum => self.open(album_page(&track)),
            TrackAction::OpenArtist => {
                if let Some(page) = artist_page(&track) {
                    self.open(page);
                }
            }
            TrackAction::RemoveFromPlaylist => {
                let page = self.state.borrow().current_page().clone();
                if let Page::Playlist(playlist) = page {
                    self.request(Request::RemoveFromPlaylist {
                        playlist,
                        track: track.item(),
                    });
                }
            }
        }
    }

    /// Opens the dialog to rename the playlist at a library index, or to
    /// confirm taking it out of the library.
    fn ask_about_playlist(&self, index: usize, rename: bool) {
        let (playlist, name, owned) = {
            let state = self.state.borrow();
            let Some(LibraryRow::Playlist(playlist)) = state.library_rows.get(index) else {
                return;
            };
            let playlist = &state.library.playlists()[*playlist];
            (
                playlist.id,
                playlist.name.to_string(),
                *playlist.owner == *state.username,
            )
        };
        let question = if rename {
            Question::Rename(playlist)
        } else {
            Question::Remove(playlist, name.clone())
        };
        self.state.borrow_mut().question = Some(question);
        self.with_ui(|ui| {
            let dialog = ui.global::<Dialog>();
            dialog.set_has_input(rename);
            dialog.set_text(if rename {
                name.as_str().into()
            } else {
                SharedString::default()
            });
            if rename {
                dialog.set_title("Rename playlist".into());
                dialog.set_message(SharedString::default());
                dialog.set_confirm_label("Save".into());
            } else if owned {
                dialog.set_title(format!("Delete {name}?").into());
                dialog
                    .set_message("Spotify keeps deleted playlists recoverable for a while.".into());
                dialog.set_confirm_label("Delete".into());
            } else {
                dialog.set_title(format!("Remove {name} from Your Library?").into());
                dialog.set_message(SharedString::default());
                dialog.set_confirm_label("Remove".into());
            }
            dialog.set_visible(true);
        });
    }

    fn ask_to_sign_out(&self) {
        self.state.borrow_mut().question = Some(Question::SignOut);
        self.with_ui(|ui| {
            let dialog = ui.global::<Dialog>();
            dialog.set_has_input(false);
            dialog.set_text(SharedString::default());
            dialog.set_title("Sign out?".into());
            dialog.set_message(
                "You will have to approve access in your browser to sign in again.".into(),
            );
            dialog.set_confirm_label("Sign out".into());
            dialog.set_visible(true);
        });
    }

    /// Stores a change to the settings and reconnects with it, which is
    /// the only way the player picks it up.
    fn change_settings(&self, change: impl FnOnce(&mut Settings)) {
        let before = Settings::load();
        let settings = Settings::update(change);
        self.show_settings(&settings);
        if settings.config_differs(&before) {
            self.request(Request::Configure(settings.config()));
        }
    }

    fn show_settings(&self, settings: &Settings) {
        self.state.borrow_mut().library_sort = settings.library_sort;
        self.set_tray(settings.close_to_tray && tray::SUPPORTED);
        self.notify_song_changes
            .set(settings.notify_song_changes && notify::SUPPORTED);
        self.with_ui(|ui| {
            let view = ui.global::<SettingsView>();
            view.set_quality(match settings.quality {
                Quality::Low => 0,
                Quality::Normal => 1,
                Quality::VeryHigh => 2,
            });
            view.set_normalise(settings.normalise);
            view.set_volume_level(match settings.volume_level {
                VolumeLevel::Quiet => 0,
                VolumeLevel::Normal => 1,
                VolumeLevel::Loud => 2,
            });
            view.set_gapless(settings.gapless);
            view.set_autoplay(settings.autoplay);
            view.set_device_name(settings.device_name().into());
            view.set_start_at_login(system::starts_at_login());
            view.set_close_to_tray(settings.close_to_tray && tray::SUPPORTED);
            view.set_notify_song_changes(settings.notify_song_changes);
        });
    }

    /// Carries out what the dialog asked about; `text` is the typed answer.
    fn dialog_confirmed(&self, text: &str) {
        self.with_ui(|ui| ui.global::<Dialog>().set_visible(false));
        let question = self.state.borrow_mut().question.take();
        match question {
            Some(Question::Rename(playlist)) if !text.is_empty() => {
                self.request(Request::RenamePlaylist {
                    playlist,
                    name: text.to_owned(),
                })
            }
            Some(Question::Remove(playlist, name)) => {
                self.request(Request::RemovePlaylist { playlist, name })
            }
            Some(Question::SignOut) => self.request(Request::SignOut),
            Some(Question::Rename(_)) | None => {}
        }
    }

    /// Adds the track in `row` to an editable playlist, or to a new one
    /// named after the track when `choice` is negative.
    fn add_to_playlist(&self, row: usize, choice: i32) {
        let request = {
            let state = self.state.borrow();
            let Some(track) = state.track_at(row) else {
                return;
            };
            let chosen = usize::try_from(choice)
                .ok()
                .and_then(|index| state.editable().nth(index));
            match (chosen, choice < 0) {
                (Some(playlist), _) => Request::AddToPlaylist {
                    playlist: Some(playlist.id),
                    name: playlist.name.to_string(),
                    track: track.item(),
                },
                (None, true) => Request::AddToPlaylist {
                    playlist: None,
                    name: track.name.to_string(),
                    track: track.item(),
                },
                (None, false) => return,
            }
        };
        self.request(request);
    }

    /// Publishes which playlists accept tracks and whether the page is one.
    fn show_editable(&self) {
        let (names, page_editable) = {
            let state = self.state.borrow();
            let names: Vec<SharedString> = state
                .editable()
                .map(|playlist| playlist.name.as_ref().into())
                .collect();
            let page_editable = match state.current_page() {
                Page::Playlist(id) => state.editable().any(|playlist| playlist.id == *id),
                _ => false,
            };
            (names, page_editable)
        };
        self.with_ui(|ui| {
            let nav = ui.global::<Nav>();
            nav.set_editable_playlists(ModelRc::from(names.as_slice()));
            nav.set_page_editable(page_editable);
        });
    }

    /// Saves or removes the album or artist the page is about.
    fn toggle_page_saved(&self) {
        let item = {
            let state = self.state.borrow();
            state
                .page
                .as_ref()
                .and_then(|page| page.header.as_ref()?.save)
        };
        if let Some(item) = item {
            self.toggle_saved(item);
        }
    }

    fn toggle_saved(&self, item: Saveable) {
        let saved = !self.state.borrow().is_saved(item);
        self.set_saved(item, saved);
        self.request(Request::SetSaved { item, saved });
    }

    /// Shows an item as saved or not, ahead of Spotify confirming it.
    fn set_saved(&self, item: Saveable, saved: bool) {
        {
            let mut state = self.state.borrow_mut();
            let set = match item {
                Saveable::Track(_) => &mut state.liked,
                Saveable::Episode(_) => &mut state.episodes,
                Saveable::Album(_) | Saveable::Artist(_) | Saveable::Show(_) => &mut state.saved,
            };
            if saved {
                set.insert(item.id());
            } else {
                set.remove(&item.id());
            }
        }
        match item {
            Saveable::Track(track) | Saveable::Episode(track) => {
                self.refresh_track(track);
                self.show_player_liked();
                // The collection's library entry counts its items.
                let collection = if matches!(item, Saveable::Episode(_)) {
                    LibraryRow::YourEpisodes
                } else {
                    LibraryRow::LikedSongs
                };
                let entry = self
                    .state
                    .borrow()
                    .library_rows
                    .iter()
                    .position(|row| *row == collection);
                if let Some(entry) = entry {
                    self.library.refresh(entry);
                }
            }
            // The header is the page's first row.
            Saveable::Album(_) | Saveable::Artist(_) | Saveable::Show(_) => self.rows.refresh(0),
        }
    }

    fn show_player_liked(&self) {
        let (liked, episode) = {
            let state = self.state.borrow();
            state.now_playing.as_ref().map_or((false, false), |track| {
                (state.is_saved(saveable(track)), track.is_episode())
            })
        };
        self.with_ui(|ui| {
            let player = ui.global::<Player>();
            player.set_liked(liked);
            player.set_episode(episode);
        });
    }

    /// Redraws the rows showing `track`, wherever they are.
    fn refresh_track(&self, track: Id) {
        let (rows, queue) = {
            let state = self.state.borrow();
            let rows: Vec<usize> = match &state.page {
                Some(page) => state
                    .layout
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| match **source {
                        RowSource::Track { section, index } => match &page.sections[section] {
                            Section::Tracks(tracks) => tracks.tracks[index].track.id == track,
                            _ => false,
                        },
                        _ => false,
                    })
                    .map(|(row, _)| row)
                    .collect(),
                None => Vec::new(),
            };
            let queue: Vec<usize> = state
                .queue
                .iter()
                .enumerate()
                .filter(|(_, queued)| queued.id == track)
                .map(|(index, _)| index)
                .collect();
            (rows, queue)
        };
        rows.into_iter().for_each(|row| self.rows.refresh(row));
        queue.into_iter().for_each(|row| self.queue.refresh(row));
    }

    // ---- row builders ----------------------------------------------------

    fn row(&self, index: usize) -> Option<Row> {
        let state = self.state.borrow();
        let page = state.page.as_ref()?;
        Some(match *state.layout.get(index)? {
            RowSource::Header => {
                let header = page.header.as_ref()?;
                Row {
                    kind: RowKind::Header,
                    header: HeaderData {
                        kind: header.kind.into(),
                        title: header.title.as_str().into(),
                        subtitle: header.subtitle.as_str().into(),
                        cover: self.images.get(
                            header.image.as_ref(),
                            self.shape(HEADER_COVER, header.round),
                        ),
                        round: header.round,
                        emblem: emblem(header.emblem),
                        playable: page.context.is_some(),
                        save: match header.save {
                            Some(Saveable::Album(_)) => SaveKind::Album,
                            // An audiobook is saved like an album, though
                            // Spotify keeps it with the followed shows.
                            Some(Saveable::Show(_)) if header.external.is_some() => SaveKind::Album,
                            // A podcast is followed, like an artist.
                            Some(Saveable::Artist(_) | Saveable::Show(_)) => SaveKind::Artist,
                            Some(Saveable::Track(_) | Saveable::Episode(_)) | None => {
                                SaveKind::None
                            }
                        },
                        saved: header.save.is_some_and(|item| state.is_saved(item)),
                        external: header.external.is_some(),
                    },
                    ..Row::default()
                }
            }
            RowSource::Title { section } => Row {
                kind: RowKind::Section,
                text: match &page.sections[section] {
                    Section::Tracks(tracks) => tracks.title.unwrap_or_default().into(),
                    Section::Cards(cards) => cards.title.as_str().into(),
                    Section::Message(_) => SharedString::default(),
                },
                link: if section_more(&page.sections[section]).is_some() {
                    "Show all".into()
                } else {
                    SharedString::default()
                },
                ..Row::default()
            },
            RowSource::Columns { section } => {
                let Section::Tracks(tracks) = &page.sections[section] else {
                    return None;
                };
                Row {
                    kind: RowKind::Columns,
                    text: tracks.dates.label().into(),
                    dated: tracks.dates != Dates::None,
                    album_view: tracks.album_view,
                    ..Row::default()
                }
            }
            RowSource::Track { section, index } => {
                let Section::Tracks(tracks) = &page.sections[section] else {
                    return None;
                };
                let entry = tracks.tracks.get(index)?;
                Row {
                    kind: RowKind::Track,
                    album_view: tracks.album_view,
                    dated: tracks.dates != Dates::None,
                    track: TrackData {
                        number: index as i32 + 1,
                        added: format::date(entry.added_at).into(),
                        ..self.track_data(&state, &entry.track)
                    },
                    ..Row::default()
                }
            }
            RowSource::Cards {
                section,
                start,
                len,
            } => {
                let Section::Cards(cards) = &page.sections[section] else {
                    return None;
                };
                let cards: Vec<CardData> = cards
                    .cards
                    .get(start..start + len)?
                    .iter()
                    .map(|card| CardData {
                        title: card.title.as_str().into(),
                        subtitle: card.subtitle.as_str().into(),
                        cover: self.images.get(
                            self.card_image(card, true).as_ref(),
                            self.shape(CARD_COVER, card.round),
                        ),
                        round: card.round,
                        emblem: EmblemKind::None,
                    })
                    .collect();
                Row {
                    kind: RowKind::Cards,
                    cards: ModelRc::from(cards.as_slice()),
                    ..Row::default()
                }
            }
            RowSource::Message { section } => Row {
                kind: RowKind::Message,
                text: match &page.sections[section] {
                    Section::Message(message) => message.as_str().into(),
                    _ => SharedString::default(),
                },
                ..Row::default()
            },
        })
    }

    fn track_data(&self, state: &State, track: &Track) -> TrackData {
        let (progress, remaining, played) = match track
            .is_episode()
            .then(|| self.episode_progress(track.id))
            .flatten()
        {
            Some(Progress::InProgress { position_ms }) if position_ms < track.duration_ms => (
                position_ms as f32 / track.duration_ms as f32,
                format::time_left(track.duration_ms - position_ms).into(),
                false,
            ),
            Some(Progress::Completed) => (0.0, "Played".into(), true),
            _ => (0.0, SharedString::default(), false),
        };
        TrackData {
            title: track.name.as_ref().into(),
            artists: artist_names(track).into(),
            album: track.album.name.as_ref().into(),
            duration: format::duration(track.duration_ms).into(),
            added: SharedString::default(),
            cover: self
                .images
                .get(track.cover.as_ref(), self.shape(THUMBNAIL, false)),
            number: 0,
            explicit: track.explicit,
            playable: track.playable,
            current: state
                .now_playing
                .as_ref()
                .is_some_and(|playing| playing.id == track.id),
            liked: state.is_saved(saveable(track)),
            episode: track.is_episode(),
            progress,
            remaining,
            played,
        }
    }

    fn library_item(&self, index: usize) -> Option<LibraryItem> {
        let state = self.state.borrow();
        let row = *state.library_rows.get(index)?;
        let current = &state.history[state.position].page;
        let library = &state.library;
        let cover = self.images.get(
            self.library_image(&state, row, true).as_ref(),
            self.shape(THUMBNAIL, matches!(row, LibraryRow::Artist(_))),
        );
        let item = LibraryItem {
            cover,
            ..LibraryItem::default()
        };
        Some(match row {
            LibraryRow::LikedSongs => LibraryItem {
                title: "Liked Songs".into(),
                subtitle: format!("Playlist · {}", format::count(state.liked.len(), "song")).into(),
                emblem: EmblemKind::LikedSongs,
                selected: *current == Page::Liked,
                ..item
            },
            LibraryRow::YourEpisodes => LibraryItem {
                title: "Your Episodes".into(),
                subtitle: format::count(state.episodes.len(), "episode").into(),
                emblem: EmblemKind::YourEpisodes,
                selected: *current == Page::YourEpisodes,
                ..item
            },
            LibraryRow::Folder(folder) => LibraryItem {
                title: library.rootlist.folders[folder].name.as_ref().into(),
                subtitle: format!(
                    "Folder · {}",
                    format::count(library.folder_size(folder), "item")
                )
                .into(),
                emblem: EmblemKind::Folder,
                ..item
            },
            LibraryRow::Playlist(index) => {
                let playlist = &library.playlists()[index];
                let owned = *playlist.owner == *state.username;
                LibraryItem {
                    title: playlist.name.as_ref().into(),
                    subtitle: format!("Playlist · {}", playlist.owner).into(),
                    selected: *current == Page::Playlist(playlist.id),
                    owned,
                    removable: true,
                    remove_label: if owned {
                        "Delete"
                    } else {
                        "Remove from Your Library"
                    }
                    .into(),
                    ..item
                }
            }
            LibraryRow::Album(index) => {
                let album = &library.albums[index].0;
                let artists: Vec<&str> = album.artists.iter().map(|artist| &*artist.name).collect();
                LibraryItem {
                    title: album.name.as_ref().into(),
                    subtitle: format!("Album · {}", artists.join(", ")).into(),
                    selected: *current == Page::Album(album.id),
                    removable: true,
                    remove_label: "Remove from Your Library".into(),
                    ..item
                }
            }
            LibraryRow::Artist(index) => {
                let artist = &library.artists[index].0;
                LibraryItem {
                    title: artist.name.as_ref().into(),
                    subtitle: "Artist".into(),
                    round: true,
                    selected: *current == Page::Artist(artist.id),
                    removable: true,
                    remove_label: "Unfollow".into(),
                    ..item
                }
            }
            LibraryRow::Show(index) => {
                let show = &library.shows[index].0;
                LibraryItem {
                    title: show.name.as_ref().into(),
                    subtitle: format!(
                        "{} · {}",
                        if show.audiobook {
                            "Audiobook"
                        } else {
                            "Podcast"
                        },
                        show.publisher
                    )
                    .into(),
                    selected: *current == Page::Show(show.id),
                    removable: true,
                    remove_label: "Unfollow".into(),
                    ..item
                }
            }
        })
    }

    /// The artwork of a library entry; collections and folders have none.
    fn library_image(&self, state: &State, row: LibraryRow, fetch: bool) -> Option<Image> {
        let library = &state.library;
        match row {
            LibraryRow::LikedSongs | LibraryRow::YourEpisodes | LibraryRow::Folder(_) => None,
            LibraryRow::Playlist(index) => self.playlist_image(&library.playlists()[index], fetch),
            LibraryRow::Album(index) => library.albums[index].0.cover.clone(),
            LibraryRow::Artist(index) => library.artists[index].0.image.clone(),
            LibraryRow::Show(index) => library.shows[index].0.image.clone(),
        }
    }

    /// A card's artwork, falling back to a stand-in for picture-less playlists.
    fn card_image(&self, card: &Card, fetch: bool) -> Option<Image> {
        match (&card.image, &card.target) {
            (None, Page::Playlist(id)) => self.playlist_cover(*id, fetch),
            (image, _) => image.clone(),
        }
    }

    fn playlist_image(&self, playlist: &Playlist, fetch: bool) -> Option<Image> {
        playlist
            .image
            .clone()
            .or_else(|| self.playlist_cover(playlist.id, fetch))
    }

    /// Looks up a playlist's stand-in artwork, requesting it once if `fetch` is set.
    fn playlist_cover(&self, playlist: Id, fetch: bool) -> Option<Image> {
        let mut covers = self.playlist_covers.borrow_mut();
        if let Some(cover) = covers.get(&playlist) {
            return cover.clone();
        }
        if fetch {
            covers.insert(playlist, None);
            self.request(Request::PlaylistCover(playlist));
        }
        None
    }

    /// Redraws the card rows of the page.
    fn refresh_cards(&self) {
        let rows: Vec<usize> = {
            let state = self.state.borrow();
            let is_cards = |source: &RowSource| matches!(source, RowSource::Cards { .. });
            let rows = state.layout.iter().enumerate();
            rows.filter(|(_, source)| is_cards(source))
                .map(|(row, _)| row)
                .collect()
        };
        rows.into_iter().for_each(|row| self.rows.refresh(row));
    }

    fn queue_item(&self, index: usize) -> Option<TrackData> {
        let state = self.state.borrow();
        let track = state.queue.get(index)?;
        Some(TrackData {
            current: index == 0,
            ..self.track_data(&state, track)
        })
    }
}

/// Adapts a method into a row builder that yields nothing once the app is gone.
fn with<T>(
    app: &Weak<App>,
    build: fn(&App, usize) -> Option<T>,
) -> impl Fn(usize) -> Option<T> + 'static
where
    T: 'static,
{
    let app = app.clone();
    move |index| build(&*app.upgrade()?, index)
}

/// Flattens a page into list rows, wrapping cards `columns` to a row.
fn layout(page: &PageData, columns: usize) -> Vec<RowSource> {
    let mut rows = Vec::new();
    if page.header.is_some() {
        rows.push(RowSource::Header);
    }
    for (section, content) in page.sections.iter().enumerate() {
        match content {
            Section::Tracks(tracks) => {
                if tracks.title.is_some() {
                    rows.push(RowSource::Title { section });
                }
                rows.push(RowSource::Columns { section });
                rows.extend(
                    (0..tracks.tracks.len()).map(|index| RowSource::Track { section, index }),
                );
            }
            Section::Cards(cards) => {
                rows.push(RowSource::Title { section });
                let columns = columns.max(1);
                // A shelf shows one row; a grid wraps until every card is shown.
                let shown = if cards.wrap {
                    cards.cards.len()
                } else {
                    columns.min(cards.cards.len())
                };
                rows.extend((0..shown).step_by(columns).map(|start| RowSource::Cards {
                    section,
                    start,
                    len: columns.min(shown - start),
                }));
            }
            Section::Message(_) => rows.push(RowSource::Message { section }),
        }
    }
    rows
}

/// Where a track's album link leads: its album, or an episode's show.
fn album_page(track: &Track) -> Page {
    if track.is_episode() {
        Page::Show(track.album.id)
    } else {
        Page::Album(track.album.id)
    }
}

/// Where a track's artist link leads: its first artist, or an episode's show.
fn artist_page(track: &Track) -> Option<Page> {
    let artist = track.artists.first()?;
    Some(if track.is_episode() {
        Page::Show(artist.id)
    } else {
        Page::Artist(artist.id)
    })
}

fn page_title(page: &PageData) -> String {
    page.header
        .as_ref()
        .map(|header| header.title.clone())
        .unwrap_or_default()
}

fn artist_names(track: &Track) -> String {
    let names: Vec<&str> = track.artists.iter().map(|artist| &*artist.name).collect();
    names.join(", ")
}

/// How a track is kept in the library: liked, or saved to Your Episodes.
fn saveable(track: &Track) -> Saveable {
    if track.is_episode() {
        Saveable::Episode(track.id)
    } else {
        Saveable::Track(track.id)
    }
}

fn emblem(emblem: Option<Emblem>) -> EmblemKind {
    match emblem {
        None => EmblemKind::None,
        Some(Emblem::LikedSongs) => EmblemKind::LikedSongs,
        Some(Emblem::YourEpisodes) => EmblemKind::YourEpisodes,
    }
}

/// Where a section's "Show all" link leads, if it has one.
fn section_more(section: &Section) -> Option<&Page> {
    match section {
        Section::Tracks(tracks) => tracks.more.as_ref(),
        Section::Cards(cards) => cards.more.as_ref(),
        Section::Message(_) => None,
    }
}

/// Plays a page's tracks as a plain list from `index`, naming the queue after
/// the page when the list stands in for its context.
fn play_listed(state: &mut State, page: &PageData, tracks: &TrackSection, index: usize) -> Request {
    state.started = page.listed_order.then(|| (String::new(), page_title(page)));
    Request::PlayTracks {
        tracks: tracks
            .tracks
            .iter()
            .map(|entry| entry.track.item())
            .collect(),
        index: index as u32,
        repeat: state.repeat,
    }
}
