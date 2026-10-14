//! Regenerates the icon files from `assets/icon.svg`: `assets/icon.png`,
//! the window icon, `assets/icon.ico`, the Windows executable's icon, and
//! `installer/welcome.bmp`, the side panel of the installer's first and
//! last pages.
//!
//!   cargo run -p vivace --example icon

use std::io::Cursor;

use resvg::{tiny_skia, usvg};

const SIZE: u32 = 128;
// Sizes Windows picks from for the taskbar, Explorer views and Alt+Tab.
const ICO_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];
// NSIS's welcome-page panel, and the icon's size on it.
const PANEL: (u32, u32) = (164, 314);
const PANEL_ICON: u32 = 96;
// The app's background colour, top and bottom of the panel.
const PANEL_TOP: [u8; 3] = [0x1d, 0x1f, 0x27];
const PANEL_BOTTOM: [u8; 3] = [0x0d, 0x0e, 0x12];

/// Draws the icon at `size` pixels.
fn render(tree: &usvg::Tree, size: u32) -> Result<image::RgbaImage, Box<dyn std::error::Error>> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or("the icon has no size")?;
    let scale = size as f32 / tree.size().width();
    resvg::render(
        tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // tiny-skia keeps colours premultiplied by alpha; image files do not.
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Ok(image::RgbaImage::from_raw(size, size, pixels).ok_or("the icon has an unexpected size")?)
}

/// The installer's side panel: the app's dark background with the icon a
/// third of the way down. BMP has no transparency, so the icon is blended
/// onto the background here.
fn panel(icon: &image::RgbaImage) -> image::RgbImage {
    let (width, height) = PANEL;
    let left = (width - icon.width()) / 2;
    let top = height / 3 - icon.height() / 2;
    image::RgbImage::from_fn(width, height, |x, y| {
        let fraction = y as f32 / (height - 1) as f32;
        let mut pixel: [f32; 3] = std::array::from_fn(|channel| {
            f32::from(PANEL_TOP[channel]) * (1.0 - fraction)
                + f32::from(PANEL_BOTTOM[channel]) * fraction
        });
        let inside =
            (left..left + icon.width()).contains(&x) && (top..top + icon.height()).contains(&y);
        if inside {
            let over = icon.get_pixel(x - left, y - top).0;
            let alpha = f32::from(over[3]) / 255.0;
            for (channel, value) in pixel.iter_mut().enumerate() {
                *value = f32::from(over[channel]) * alpha + *value * (1.0 - alpha);
            }
        }
        image::Rgb(pixel.map(|value| value.round() as u8))
    })
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
    let svg = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.svg"))?;
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default())?;

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.png");
    render(&tree, SIZE)?.save(path)?;
    println!("wrote {path}");

    let mut images = Vec::new();
    for size in ICO_SIZES {
        let mut png = Vec::new();
        render(&tree, size)?.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)?;
        images.push((size, png));
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.ico");
    std::fs::write(path, ico(&images))?;
    println!("wrote {path}");

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../installer/welcome.bmp");
    panel(&render(&tree, PANEL_ICON)?).save(path)?;
    println!("wrote {path}");
    Ok(())
}
