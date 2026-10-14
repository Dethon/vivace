//! Regenerates `assets/icon.png` from the `AppIcon` component.
//!
//!   cargo run -p vivace --example icon

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};
use vivace::AppIcon;

const SIZE: u32 = 128;
const CORNER_FRACTION: f32 = 0.22;

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

/// Fades out the pixels outside a rounded square of the given radius.
fn round_corners(image: &mut image::RgbaImage, radius: f32) {
    let size = image.width() as f32;
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let dx = (radius - px).max(px - (size - radius)).max(0.0);
        let dy = (radius - py).max(py - (size - radius)).max(0.0);
        let coverage = (radius - dx.hypot(dy) + 0.5).clamp(0.0, 1.0);
        pixel.0[3] = (f32::from(pixel.0[3]) * coverage) as u8;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone())))?;

    let icon = AppIcon::new()?;
    window.set_size(PhysicalSize::new(SIZE, SIZE));
    icon.show()?;
    let snapshot = icon.window().take_snapshot()?;
    let mut image = image::RgbaImage::from_raw(SIZE, SIZE, snapshot.as_bytes().to_vec())
        .ok_or("the snapshot has an unexpected size")?;
    round_corners(&mut image, SIZE as f32 * CORNER_FRACTION);

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.png");
    image.save(path)?;
    println!("wrote {path}");
    Ok(())
}
