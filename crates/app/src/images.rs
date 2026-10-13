//! Artwork: downloaded and scaled off the UI thread, cached on it.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use slint::{Rgba8Pixel, SharedPixelBuffer};
use tokio::sync::mpsc::UnboundedSender;
use vivace_core::{Api, model::Image};

use crate::backend::Request;

// Decoded pixels kept for reuse. Evicted images stay alive while on screen.
const BUDGET_BYTES: usize = 24 * 1024 * 1024;
// Corner radius as a share of the edge, within limits that suit both
// thumbnails and page headers.
const CORNER_FRACTION: f32 = 0.05;
const MIN_CORNER: f32 = 3.0;
const MAX_CORNER: f32 = 10.0;
// Album covers come in three sizes sharing an id suffix; the smallest is
// enough for thumbnails and a tenth of the download.
const COVER_300: &str = "/image/ab67616d00001e02";
const COVER_64: &str = "/image/ab67616d00004851";

/// An image at the size and shape it is drawn.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageKey {
    pub url: Box<str>,
    /// Edge length in physical pixels.
    pub size: u32,
    pub round: bool,
}

/// How an image is drawn: edge length in logical pixels, the window's scale
/// factor, and whether it is a circle.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub size: u32,
    pub scale: f32,
    pub round: bool,
}

pub struct Images {
    requests: UnboundedSender<Request>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    loaded: HashMap<ImageKey, Entry>,
    // Requested and not yet answered, or known to have failed.
    unavailable: HashSet<ImageKey>,
    bytes: usize,
    clock: u64,
}

struct Entry {
    image: slint::Image,
    bytes: usize,
    used: u64,
}

impl Images {
    pub fn new(requests: UnboundedSender<Request>) -> Self {
        Self {
            requests,
            state: RefCell::default(),
        }
    }

    /// Returns the image if it is cached; otherwise requests it and returns
    /// an empty one. [`Images::insert`] delivers it later.
    pub fn get(&self, image: Option<&Image>, shape: Shape) -> slint::Image {
        let Some(image) = image else {
            return slint::Image::default();
        };
        let key = ImageKey {
            url: image.url().into(),
            size: (shape.size as f32 * shape.scale).round() as u32,
            round: shape.round,
        };

        let mut state = self.state.borrow_mut();
        state.clock += 1;
        let clock = state.clock;
        if let Some(entry) = state.loaded.get_mut(&key) {
            entry.used = clock;
            return entry.image.clone();
        }
        if state.unavailable.insert(key.clone()) {
            // The backend outlives the window; a failed send means shutdown.
            _ = self.requests.send(Request::Image(key));
        }
        slint::Image::default()
    }

    /// Stores a finished download; `None` marks the image as failed.
    pub fn insert(&self, key: ImageKey, pixels: Option<SharedPixelBuffer<Rgba8Pixel>>) {
        let Some(pixels) = pixels else {
            return;
        };
        let mut state = self.state.borrow_mut();
        state.unavailable.remove(&key);
        state.clock += 1;
        let entry = Entry {
            bytes: pixels.as_bytes().len(),
            image: slint::Image::from_rgba8(pixels),
            used: state.clock,
        };
        state.bytes += entry.bytes;
        if let Some(replaced) = state.loaded.insert(key, entry) {
            state.bytes -= replaced.bytes;
        }

        while state.bytes > BUDGET_BYTES {
            let Some(oldest) = state
                .loaded
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = state.loaded.remove(&oldest) {
                state.bytes -= evicted.bytes;
            }
        }
    }
}

/// Downloads `key`'s image and renders it at its final size and shape.
pub async fn load(api: &Api, key: &ImageKey) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let bytes = match small_variant(key) {
        Some(url) => match api.fetch(&url).await {
            Ok(bytes) => bytes,
            Err(_) => api.fetch(&key.url).await.ok()?,
        },
        None => api.fetch(&key.url).await.ok()?,
    };
    let (size, round) = (key.size, key.round);
    tokio::task::spawn_blocking(move || render(&bytes, size, round))
        .await
        .ok()?
}

fn small_variant(key: &ImageKey) -> Option<String> {
    (key.size <= 64 && key.url.contains(COVER_300)).then(|| key.url.replace(COVER_300, COVER_64))
}

fn render(bytes: &[u8], size: u32, round: bool) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let mut scaled = decoded
        .resize_to_fill(size, size, image::imageops::FilterType::Triangle)
        .into_rgba8();
    let radius = if round {
        size as f32 / 2.0
    } else {
        (size as f32 * CORNER_FRACTION).clamp(MIN_CORNER, MAX_CORNER)
    };
    round_corners(&mut scaled, radius);
    Some(SharedPixelBuffer::clone_from_slice(
        scaled.as_raw(),
        size,
        size,
    ))
}

/// Fades out the pixels outside a rounded rectangle of the given radius.
fn round_corners(image: &mut image::RgbaImage, radius: f32) {
    let (width, height) = (image.width() as f32, image.height() as f32);
    let radius = radius.min(width / 2.0).min(height / 2.0);
    let reach = radius.ceil() as u32;
    for y in 0..image.height() {
        for x in 0..image.width() {
            // Only the four corner squares can lie outside the shape.
            let in_corner_x = x < reach || x >= image.width() - reach;
            let in_corner_y = y < reach || y >= image.height() - reach;
            if !(in_corner_x && in_corner_y) {
                continue;
            }
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = (radius - px).max(px - (width - radius)).max(0.0);
            let dy = (radius - py).max(py - (height - radius)).max(0.0);
            let coverage = (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
            if coverage < 1.0 {
                let alpha = &mut image.get_pixel_mut(x, y).0[3];
                *alpha = (f32::from(*alpha) * coverage) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opaque(size: u32) -> image::RgbaImage {
        image::RgbaImage::from_pixel(size, size, image::Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn rounding_clears_corners_and_keeps_the_rest() {
        let mut image = opaque(40);
        round_corners(&mut image, 8.0);
        assert_eq!(image.get_pixel(0, 0).0[3], 0);
        assert_eq!(image.get_pixel(39, 39).0[3], 0);
        assert_eq!(image.get_pixel(20, 0).0[3], 255);
        assert_eq!(image.get_pixel(0, 20).0[3], 255);
        assert_eq!(image.get_pixel(20, 20).0[3], 255);
        assert_eq!(image.get_pixel(8, 8).0[3], 255);
    }

    #[test]
    fn full_radius_makes_a_circle() {
        let mut image = opaque(40);
        round_corners(&mut image, 20.0);
        assert_eq!(image.get_pixel(2, 2).0[3], 0);
        assert_eq!(image.get_pixel(20, 1).0[3], 255);
        assert_eq!(image.get_pixel(6, 6).0[3], 255);
    }

    #[test]
    fn thumbnails_use_the_small_cover_variant() {
        let key = |size| ImageKey {
            url: "https://i.scdn.co/image/ab67616d00001e02abcdef".into(),
            size,
            round: false,
        };
        assert_eq!(
            small_variant(&key(40)).as_deref(),
            Some("https://i.scdn.co/image/ab67616d00004851abcdef")
        );
        assert_eq!(small_variant(&key(152)), None);
    }
}
