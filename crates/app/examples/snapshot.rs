//! Renders the window with sample data to a PNG, without a display.
//!
//!   cargo run -p vivace --example snapshot -- out.png [width height]
//!
//! `SNAPSHOT_QUEUE=1` opens the queue panel; `SNAPSHOT_CLICK=x,y` and
//! `SNAPSHOT_RIGHT_CLICK=x,y` click at that point first, e.g. to open a popup.
//! `SNAPSHOT_TIMING=1` also prints how long a full frame takes to draw.

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, Image, LogicalPosition, ModelRc, PhysicalSize, PlatformError, Rgba8Pixel,
    SharedPixelBuffer, VecModel,
};
use vivace::{
    CardData, DeviceIcon, DeviceItem, Devices, HeaderData, Library, LibraryItem, MainWindow, Nav,
    Player, Queue, RepeatMode, Row, RowKind, SaveKind, Session, TrackData,
};

// Frames drawn when timing is requested.
const FRAMES: u32 = 30;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn cover(seed: u32, size: u32) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    for (index, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (index as u32 % size, index as u32 / size);
        *pixel = Rgba8Pixel {
            r: (60 + seed * 53 % 160 + x * 60 / size) as u8,
            g: (40 + seed * 97 % 140 + y * 80 / size) as u8,
            b: (90 + seed * 31 % 150) as u8,
            a: 255,
        };
    }
    Image::from_rgba8(buffer)
}

fn track(number: i32) -> TrackData {
    const TITLES: [&str; 6] = [
        "Give Life Back to Music",
        "うみねこのなく頃に",
        "Instant Crush (feat. Julian Casablancas)",
        "A very long track title that has to be elided somewhere along the row",
        "Touch",
        "Doin' it Right",
    ];
    let index = number as usize % TITLES.len();
    TrackData {
        title: TITLES[index].into(),
        artists: ["Daft Punk", "zts", "Daft Punk, Julian Casablancas"][index % 3].into(),
        album: "Random Access Memories".into(),
        duration: format!("{}:{:02}", 3 + number % 4, number * 7 % 60).into(),
        added: "Oct 3, 2026".into(),
        cover: if number % 5 == 4 {
            Image::default()
        } else {
            cover(number as u32, 40)
        },
        number,
        explicit: number % 4 == 2,
        playable: number != 5,
        current: number == 3,
        liked: number % 3 == 0,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: snapshot <out.png> [width height]")?;
    let width = args.next().map_or(Ok(1180), |value| value.parse())?;
    let height = args.next().map_or(Ok(760), |value| value.parse())?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone())))?;
    let ui = MainWindow::new()?;
    window.set_size(PhysicalSize::new(width, height));

    ui.global::<Session>().set_connected(true);

    let library: Vec<LibraryItem> = (0..9)
        .map(|index| LibraryItem {
            title: ["Liked Songs", "Late Night Drive", "Road Trip", "Focus"][index % 4].into(),
            subtitle: ["Playlist · 83 songs", "Playlist · listener"][index % 2].into(),
            cover: if index == 2 {
                Image::default()
            } else {
                cover(index as u32 + 20, 40)
            },
            liked_songs: index == 0,
            owned: index % 2 == 1,
            removable: index > 0,
            selected: index == 1,
        })
        .collect();
    ui.global::<Library>()
        .set_items(ModelRc::new(VecModel::from(library)));

    let cards = |seed: u32, round: bool| -> ModelRc<CardData> {
        let cards: Vec<CardData> = (0..5)
            .map(|index| CardData {
                title: ["Discovery", "Homework", "A release with quite a long name"][index % 3]
                    .into(),
                subtitle: ["2001 · Album", "Artist"][index % 2].into(),
                cover: if index == 3 {
                    Image::default()
                } else {
                    cover(seed + index as u32, 152)
                },
                round,
                liked_songs: false,
            })
            .collect();
        ModelRc::new(VecModel::from(cards))
    };
    let mut rows = vec![
        Row {
            kind: RowKind::Header,
            header: HeaderData {
                kind: "Playlist".into(),
                title: "Late Night Drive".into(),
                subtitle: "listener · 187 songs".into(),
                cover: cover(7, 192),
                round: false,
                liked_songs: false,
                playable: true,
                save: SaveKind::Album,
                saved: true,
            },
            ..Row::default()
        },
        Row {
            kind: RowKind::Columns,
            dated: true,
            ..Row::default()
        },
    ];
    rows.extend((1..=6).map(|number| Row {
        kind: RowKind::Track,
        dated: true,
        track: track(number),
        ..Row::default()
    }));
    rows.push(Row {
        kind: RowKind::Section,
        text: "Albums".into(),
        ..Row::default()
    });
    rows.push(Row {
        kind: RowKind::Cards,
        cards: cards(40, false),
        ..Row::default()
    });
    rows.push(Row {
        kind: RowKind::Section,
        text: "Artists".into(),
        ..Row::default()
    });
    rows.push(Row {
        kind: RowKind::Cards,
        cards: cards(60, true),
        ..Row::default()
    });
    let nav = ui.global::<Nav>();
    nav.set_rows(ModelRc::new(VecModel::from(rows)));
    nav.set_can_go_back(true);
    let playlists = ["Late Night Drive", "Focus"].map(slint::SharedString::from);
    nav.set_editable_playlists(ModelRc::new(VecModel::from(playlists.to_vec())));
    nav.set_page_editable(true);

    let player = ui.global::<Player>();
    player.set_available(true);
    player.set_title("Instant Crush (feat. Julian Casablancas)".into());
    player.set_artists("Daft Punk, Julian Casablancas".into());
    player.set_cover(cover(3, 56));
    player.set_playing(true);
    player.set_liked(true);
    player.set_position_ms(83_000);
    player.set_duration_ms(337_000);
    player.set_volume(0.7);
    player.set_shuffle(true);
    player.set_repeat(RepeatMode::One);

    let queue = ui.global::<Queue>();
    queue.set_visible(std::env::var_os("SNAPSHOT_QUEUE").is_some());
    queue.set_items(ModelRc::new(VecModel::from(
        (3..12).map(track).collect::<Vec<_>>(),
    )));
    queue.set_queued(2);
    queue.set_context("Late Night Drive".into());

    let devices: Vec<DeviceItem> = [
        ("Vivace", DeviceIcon::Computer, false, true),
        ("Cocina", DeviceIcon::Speaker, true, false),
        ("Pixel 9", DeviceIcon::Phone, false, false),
        ("Salón", DeviceIcon::Tv, false, false),
    ]
    .into_iter()
    .map(|(name, kind, active, local)| DeviceItem {
        name: name.into(),
        kind,
        active,
        local,
    })
    .collect();
    ui.global::<Devices>()
        .set_items(ModelRc::new(VecModel::from(devices)));
    ui.global::<Devices>().set_remote_name("Cocina".into());

    ui.show()?;
    slint::platform::update_timers_and_animations();
    for (variable, button) in [
        ("SNAPSHOT_CLICK", PointerEventButton::Left),
        ("SNAPSHOT_RIGHT_CLICK", PointerEventButton::Right),
    ] {
        let point = std::env::var(variable).ok();
        let Some((x, y)) = point.as_deref().and_then(|point| point.split_once(',')) else {
            continue;
        };
        let position = LogicalPosition::new(x.trim().parse()?, y.trim().parse()?);
        for event in [
            WindowEvent::PointerMoved { position },
            WindowEvent::PointerPressed { position, button },
            WindowEvent::PointerReleased { position, button },
        ] {
            ui.window().dispatch_event(event);
        }
        slint::platform::update_timers_and_animations();
    }
    if std::env::var_os("SNAPSHOT_TIMING").is_some() {
        // A rough measure of how long drawing the whole window takes.
        let started = std::time::Instant::now();
        for _ in 0..FRAMES {
            ui.window().take_snapshot()?;
        }
        println!(
            "full frame: {:.2} ms",
            started.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES)
        );
    }
    let snapshot = ui.window().take_snapshot()?;
    image::save_buffer(
        &path,
        snapshot.as_bytes(),
        snapshot.width(),
        snapshot.height(),
        image::ColorType::Rgba8,
    )?;
    println!("wrote {path} ({}x{})", snapshot.width(), snapshot.height());
    Ok(())
}
