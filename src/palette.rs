//! Picks a background tint from album art.

use tiny_skia::Pixmap;

pub type Rgb = [u8; 3];

/// The art's most prominent color, muted so it reads as a soft wash under
/// white text rather than a loud block of color.
pub fn tint(art: &Pixmap) -> Rgb {
    // 3 bits per channel. Weighted toward colorful pixels so a vivid accent
    // beats a large gray or black area.
    let mut buckets = vec![(0.0f32, [0.0f32; 3]); 512];
    for px in art.pixels() {
        if px.alpha() < 128 {
            continue;
        }
        let c = px.demultiply();
        let rgb = [c.red(), c.green(), c.blue()];
        let (_, s, l) = rgb_to_hsl(rgb);
        let weight = 0.2 + s * (1.0 - (2.0 * l - 1.0).abs());
        let idx = (rgb[0] as usize >> 5) << 6 | (rgb[1] as usize >> 5) << 3 | rgb[2] as usize >> 5;
        let bucket = &mut buckets[idx];
        bucket.0 += weight;
        for i in 0..3 {
            bucket.1[i] += rgb[i] as f32 * weight;
        }
    }

    let Some((weight, sum)) = buckets
        .into_iter()
        .filter(|(w, _)| *w > 0.0)
        .max_by(|a, b| a.0.total_cmp(&b.0))
    else {
        return [60, 60, 60];
    };
    let dominant = sum.map(|v| (v / weight).round() as u8);

    let (h, s, l) = rgb_to_hsl(dominant);
    hsl_to_rgb(h, s.min(0.4), l.clamp(0.32, 0.42))
}

fn rgb_to_hsl(rgb: Rgb) -> (f32, f32, f32) {
    let [r, g, b] = rgb.map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Rgb {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r, g, b].map(|v| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Color;

    fn distance(a: Rgb, b: Rgb) -> f32 {
        let d = |i: usize| a[i] as f32 - b[i] as f32;
        (d(0).powi(2) + d(1).powi(2) + d(2).powi(2)).sqrt()
    }

    #[test]
    fn hsl_round_trips() {
        for rgb in [[255, 0, 0], [12, 200, 90], [30, 30, 30], [250, 240, 10], [90, 20, 200]] {
            let (h, s, l) = rgb_to_hsl(rgb);
            let back = hsl_to_rgb(h, s, l);
            assert!(distance(rgb, back) < 2.0, "{rgb:?} -> {back:?}");
        }
    }

    #[test]
    fn vivid_accent_beats_gray_area() {
        // 70% gray, 30% red: the tint should still be red.
        let mut art = Pixmap::new(10, 10).unwrap();
        art.fill(Color::from_rgba8(128, 128, 128, 255));
        art.fill_rect(
            tiny_skia::Rect::from_xywh(0.0, 0.0, 10.0, 3.0).unwrap(),
            &tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(220, 20, 20, 255)),
                ..Default::default()
            },
            tiny_skia::Transform::identity(),
            None,
        );
        let tint = tint(&art);
        assert!(tint[0] > tint[1] && tint[0] > tint[2], "{tint:?}");
        // Muted: not the raw saturated red.
        let (_, s, l) = rgb_to_hsl(tint);
        assert!(s <= 0.41 && (0.31..=0.43).contains(&l), "{tint:?}");
    }
}
