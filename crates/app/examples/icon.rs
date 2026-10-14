//! Regenerates `assets/icon.png`, the window icon, and `assets/icon.ico`,
//! the Windows executable's icon, from the `AppIcon` component.
//!
//!   cargo run -p vivace --example icon

use std::io::Cursor;
use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, PhysicalSize, PlatformError};
use vivace::AppIcon;

const SIZE: u32 = 128;
// Sizes Windows picks from for the taskbar, Explorer views and Alt+Tab.
const ICO_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
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

/// Draws the icon at `size` pixels with rounded corners.
fn render(
    window: &MinimalSoftwareWindow,
    icon: &AppIcon,
    size: u32,
) -> Result<image::RgbaImage, Box<dyn std::error::Error>> {
    icon.set_size(size as f32);
    window.set_size(PhysicalSize::new(size, size));
    icon.show()?;
    let snapshot = icon.window().take_snapshot()?;
    let mut image = image::RgbaImage::from_raw(size, size, snapshot.as_bytes().to_vec())
        .ok_or("the snapshot has an unexpected size")?;
    round_corners(&mut image, size as f32 * CORNER_FRACTION);
    Ok(image)
}

/// An `.ico` file holding one PNG image per size, which Windows reads
/// since Vista.
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&0u16.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        // 256 is written as 0.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        file.extend_from_slice(&[side, side, 0, 0]);
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&32u16.to_le_bytes());
        file.extend_from_slice(&(png.len() as u32).to_le_bytes());
        file.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        file.extend_from_slice(png);
    }
    file
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone())))?;
    let icon = AppIcon::new()?;

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.png");
    render(&window, &icon, SIZE)?.save(path)?;
    println!("wrote {path}");

    let mut images = Vec::new();
    for size in ICO_SIZES {
        let mut png = Vec::new();
        render(&window, &icon, size)?
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
        images.push((size, png));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.ico");
    std::fs::write(path, ico(&images))?;
    println!("wrote {path}");
    Ok(())
}
