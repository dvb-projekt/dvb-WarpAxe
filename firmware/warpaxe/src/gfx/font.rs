//! Pre-rasterised fonts (tools/make-fonts.py): 4-bit coverage per pixel.

pub struct Glyph {
    pub c: char,
    /// Pen advance, 1/16 px.
    pub adv: u16,
    /// Bitmap offset from the pen position on the baseline.
    pub dx: i8,
    pub dy: i8,
    pub w: u8,
    pub h: u8,
    /// Index of the first pixel in `Font::data` (two pixels per byte).
    pub off: u32,
}

pub struct Font {
    pub data: &'static [u8],
    /// Sorted by `c`.
    pub glyphs: &'static [Glyph],
}

impl Font {
    fn glyph(&self, c: char) -> Option<&Glyph> {
        self.glyphs
            .binary_search_by(|g| g.c.cmp(&c))
            .ok()
            .map(|i| &self.glyphs[i])
            .or_else(|| {
                // Unknown characters: '?' if the font has one.
                self.glyphs
                    .binary_search_by(|g| g.c.cmp(&'?'))
                    .ok()
                    .map(|i| &self.glyphs[i])
            })
    }

    /// Width of `s` in pixels.
    pub fn width(&self, s: &str) -> i32 {
        let sum: i32 = s
            .chars()
            .map(|c| self.glyph(c).map_or(0, |g| i32::from(g.adv)))
            .sum();
        (sum + 8) / 16
    }

    /// Draw `s` with the pen starting at (x, baseline y); `px(x, y,
    /// coverage 0-15)` gets every covered pixel. Returns the width.
    pub fn draw(&self, x: i32, y: i32, s: &str, mut px: impl FnMut(i32, i32, u8)) -> i32 {
        let mut pen16 = x * 16;
        for c in s.chars() {
            let pen = (pen16 + 8) >> 4;
            let Some(g) = self.glyph(c) else { continue };
            let (w, h) = (usize::from(g.w), usize::from(g.h));
            for row in 0..h {
                for col in 0..w {
                    let i = g.off as usize + row * w + col;
                    let byte = self.data.get(i / 2).copied().unwrap_or(0);
                    let a = if i % 2 == 0 { byte >> 4 } else { byte & 0x0f };
                    if a > 0 {
                        px(
                            pen + i32::from(g.dx) + col as i32,
                            y + i32::from(g.dy) + row as i32,
                            a,
                        );
                    }
                }
            }
            pen16 += i32::from(g.adv);
        }
        ((pen16 + 8) >> 4) - x
    }
}
