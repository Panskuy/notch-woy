//! Draws the app icon and writes `assets/notch-woy.ico`, which `build.rs`
//! embeds in the exe: `cargo test make_icon -- --ignored`.

use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, SpreadMode, Transform};

/// Sizes Windows picks from: taskbar, tray, Explorer views, Alt+Tab.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let k = r * 0.552_284_8;
    let (rt, b) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(rt - r, y);
    pb.cubic_to(rt - r + k, y, rt, y + r - k, rt, y + r);
    pb.line_to(rt, b - r);
    pb.cubic_to(rt, b - r + k, rt - r + k, b, rt - r, b);
    pb.line_to(x + r, b);
    pb.cubic_to(x + r - k, b, x, b - r + k, x, b - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// A purple-to-pink tile with the black island near its top, drawn
/// directly at `size` so small sizes stay crisp.
fn draw(size: u32) -> Pixmap {
    let s = size as f32;
    let mut pixmap = Pixmap::new(size, size).unwrap();
    let inset = (s * 0.06).max(1.0);
    let tile = rounded(inset, inset, s - 2.0 * inset, s - 2.0 * inset, s * 0.22).unwrap();
    let gradient = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(s, s),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(124, 92, 255, 255)),
            GradientStop::new(1.0, Color::from_rgba8(255, 94, 156, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    let paint = Paint { shader: gradient, anti_alias: true, ..Default::default() };
    pixmap.fill_path(&tile, &paint, FillRule::Winding, Transform::identity(), None);

    let (w, h) = (s * 0.58, (s * 0.2).max(3.0));
    let (x, y) = ((s - w) / 2.0, s * 0.24);
    let pill = rounded(x, y, w, h, h / 2.0).unwrap();
    let mut black = Paint::default();
    black.set_color_rgba8(0, 0, 0, 255);
    black.anti_alias = true;
    pixmap.fill_path(&pill, &black, FillRule::Winding, Transform::identity(), None);

    // The playing indicator inside the island, where there is room for it.
    if size >= 32 {
        let mut green = Paint::default();
        green.set_color_rgba8(48, 209, 88, 255);
        green.anti_alias = true;
        let bar_w = h * 0.14;
        for (i, k) in [0.45, 0.75, 0.55].into_iter().enumerate() {
            let bar_h = h * k;
            let bx = x + w - h * 0.5 - (2 - i) as f32 * bar_w * 1.8;
            let bar = rounded(bx, y + (h - bar_h) / 2.0, bar_w, bar_h, bar_w / 2.0).unwrap();
            pixmap.fill_path(&bar, &green, FillRule::Winding, Transform::identity(), None);
        }
    }
    pixmap
}

/// An .ico holding one PNG per size (supported since Windows Vista).
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, png) in images {
        // 256 is stored as 0.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

#[test]
#[ignore]
fn make_icon() {
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&size| (size, draw(size).encode_png().unwrap())).collect();
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(format!("{dir}/notch-woy.ico"), ico(&images)).unwrap();
    // A large copy to look at.
    draw(256).save_png(concat!(env!("CARGO_MANIFEST_DIR"), "/target/icon_preview.png")).unwrap();
}
