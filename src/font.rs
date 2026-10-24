//! Text rasterizing over `ab_glyph`, which reads glyph outlines from the
//! font file only when a glyph is drawn. (fontdue, used before, decoded
//! every glyph of Segoe UI up front: ~40 MB for two faces.)

use ab_glyph::{point, Font as _, FontVec, PxScale, ScaleFont as _};

pub struct Font {
    inner: FontVec,
    /// Sizes here are per em (1 em = `px`), but `ab_glyph` scales by the
    /// font's ascent-to-descent height; this converts one to the other.
    em_to_scale: f32,
}

/// A rendered glyph, positioned relative to its pen point on the baseline.
pub struct Glyph {
    /// Offset of the bitmap's top-left from the pen point (top is negative
    /// above the baseline).
    pub left: i32,
    pub top: i32,
    pub width: usize,
    pub height: usize,
    pub advance: f32,
    /// One coverage byte per pixel, row-major.
    pub coverage: Vec<u8>,
}

impl Font {
    pub fn load(path: &str) -> Option<Self> {
        let inner = FontVec::try_from_vec(std::fs::read(path).ok()?).ok()?;
        let em_to_scale = inner.height_unscaled() / inner.units_per_em()?;
        Some(Self { inner, em_to_scale })
    }

    fn scale(&self, px: f32) -> PxScale {
        PxScale::from(px * self.em_to_scale)
    }

    /// Horizontal advance of `ch` at `px` per em.
    pub fn advance(&self, ch: char, px: f32) -> f32 {
        self.inner.as_scaled(self.scale(px)).h_advance(self.inner.glyph_id(ch))
    }

    pub fn rasterize(&self, ch: char, px: f32) -> Glyph {
        let id = self.inner.glyph_id(ch);
        let advance = self.inner.as_scaled(self.scale(px)).h_advance(id);
        let positioned = id.with_scale_and_position(self.scale(px), point(0.0, 0.0));
        // Spaces and the like have no outline.
        let Some(outlined) = self.inner.outline_glyph(positioned) else {
            return Glyph { left: 0, top: 0, width: 0, height: 0, advance, coverage: Vec::new() };
        };
        // Whole-pixel bounds around the outline.
        let bounds = outlined.px_bounds();
        let (width, height) = (bounds.width() as usize, bounds.height() as usize);
        let mut coverage = vec![0u8; width * height];
        outlined.draw(|x, y, c| {
            if let Some(px) = coverage.get_mut(y as usize * width + x as usize) {
                *px = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        Glyph { left: bounds.min.x as i32, top: bounds.min.y as i32, width, height, advance, coverage }
    }
}
