//! Renders `assets/icon.svg` into `assets/app.ico` (PNG-compressed entries).
//!
//! Run after changing the SVG: `cargo run --example make_icon`

use std::fs;

use resvg::{tiny_skia, usvg};

const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    let svg = fs::read("assets/icon.svg").expect("read assets/icon.svg");
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).expect("parse svg");

    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| {
            let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("pixmap");
            let scale = size as f32 / tree.size().width();
            resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            (size, pixmap.encode_png().expect("encode png"))
        })
        .collect();

    // ICONDIR, then one ICONDIRENTRY per image, then the PNG data.
    let mut ico = Vec::new();
    ico.extend(0u16.to_le_bytes());
    ico.extend(1u16.to_le_bytes());
    ico.extend((images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        ico.extend([dim, dim, 0, 0]);
        ico.extend(1u16.to_le_bytes()); // color planes
        ico.extend(32u16.to_le_bytes()); // bits per pixel
        ico.extend((png.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &images {
        ico.extend(png);
    }
    fs::write("assets/app.ico", &ico).expect("write assets/app.ico");
    fs::write("assets/icon-256.png", &images.last().unwrap().1).expect("write preview png");
    println!("wrote assets/app.ico ({} bytes, sizes {:?})", ico.len(), SIZES);
}
