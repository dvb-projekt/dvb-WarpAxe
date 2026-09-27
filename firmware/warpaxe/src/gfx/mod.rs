//! Drawing on the LCD's RGB565 frame (big-endian, as the panel takes it):
//! alpha-blended rectangles, anti-aliased lines and discs, and the
//! pre-rasterised fonts.

pub mod font;
#[rustfmt::skip]
pub mod fonts;

use font::Font;

/// RGB565 from 8-bit channels.
pub const fn rgb(r: u8, g: u8, b: u8) -> u16 {
    ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)
}

// The web UI's palette.
pub const VOID: u16 = rgb(0x05, 0x06, 0x0B);
pub const WARP: u16 = rgb(0x7B, 0x5C, 0xFF);
pub const PLASMA: u16 = rgb(0x00, 0xE0, 0xFF);
pub const BTC: u16 = rgb(0xF7, 0x93, 0x1A);
pub const FG: u16 = rgb(0xE8, 0xEE, 0xF7);
pub const MUTE: u16 = rgb(0x9C, 0xA8, 0xBD);
pub const DIM: u16 = rgb(0x6B, 0x73, 0x90);
pub const OK: u16 = rgb(0x50, 0xE3, 0xA4);
pub const WARN: u16 = rgb(0xF5, 0xB9, 0x42);
pub const ERR: u16 = rgb(0xFF, 0x5A, 0x5A);
pub const WHITE: u16 = 0xFFFF;
pub const BLACK: u16 = 0;

fn split(c: u16) -> (i32, i32, i32) {
    (
        i32::from(c >> 11),
        i32::from((c >> 5) & 0x3f),
        i32::from(c & 0x1f),
    )
}

/// `a` over `b`, alpha 0-255.
pub fn mix(a: u16, b: u16, alpha: u8) -> u16 {
    let (ar, ag, ab) = split(a);
    let (br, bg, bb) = split(b);
    let t = i32::from(alpha);
    let l = |x: i32, y: i32| (y + (x - y) * t / 255) as u16;
    (l(ar, br) << 11) | (l(ag, bg) << 5) | l(ab, bb)
}

pub struct Canvas<'a> {
    pub fb: &'a mut [u8],
    pub w: i32,
    pub h: i32,
}

pub enum Align {
    Left,
    Center,
    Right,
}

impl Canvas<'_> {
    #[inline]
    pub fn get(&self, x: i32, y: i32) -> u16 {
        let i = ((y * self.w + x) * 2) as usize;
        u16::from_be_bytes([self.fb[i], self.fb[i + 1]])
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, c: u16) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let i = ((y * self.w + x) * 2) as usize;
        self.fb[i..i + 2].copy_from_slice(&c.to_be_bytes());
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: u16, alpha: u8) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || alpha == 0 {
            return;
        }
        let under = self.get(x, y);
        self.set(
            x,
            y,
            if alpha == 255 {
                c
            } else {
                mix(c, under, alpha)
            },
        );
    }

    pub fn fill(&mut self, c: u16) {
        let [hi, lo] = c.to_be_bytes();
        for p in self.fb.chunks_exact_mut(2) {
            p[0] = hi;
            p[1] = lo;
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u16, alpha: u8) {
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                self.blend(xx, yy, c, alpha);
            }
        }
    }

    /// A rectangle fading from `a0` at the top to `a1` at the bottom.
    pub fn vfade(&mut self, x: i32, y: i32, w: i32, h: i32, c: u16, a0: u8, a1: u8) {
        for r in 0..h.max(1) {
            let a = i32::from(a0) + (i32::from(a1) - i32::from(a0)) * r / h.max(1);
            self.rect(x, y + r, w, 1, c, a as u8);
        }
    }

    /// Anti-aliased line (Wu), `alpha` scales the whole line.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: u16, alpha: u8) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as i32;
        let steep = dy.abs() > dx.abs();
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let (x, y) = (x0 + dx * t, y0 + dy * t);
            let a = f32::from(alpha);
            if steep {
                let f = x - x.floor();
                self.blend(x.floor() as i32, y.round() as i32, c, (a * (1.0 - f)) as u8);
                self.blend(x.floor() as i32 + 1, y.round() as i32, c, (a * f) as u8);
            } else {
                let f = y - y.floor();
                self.blend(x.round() as i32, y.floor() as i32, c, (a * (1.0 - f)) as u8);
                self.blend(x.round() as i32, y.floor() as i32 + 1, c, (a * f) as u8);
            }
        }
    }

    /// Filled disc with a soft edge.
    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, c: u16, alpha: u8) {
        let (x0, x1) = ((cx - r - 1.0) as i32, (cx + r + 1.0) as i32);
        let (y0, y1) = ((cy - r - 1.0) as i32, (cy + r + 1.0) as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, (cov * f32::from(alpha)) as u8);
                }
            }
        }
    }

    /// Circle outline of `width` px with soft edges.
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: u16, alpha: u8) {
        let reach = r + width;
        let (x0, x1) = (
            (cx - reach - 1.0).max(0.0) as i32,
            (cx + reach + 1.0).min(self.w as f32) as i32,
        );
        let (y0, y1) = (
            (cy - reach - 1.0).max(0.0) as i32,
            (cy + reach + 1.0).min(self.h as f32) as i32,
        );
        for y in y0..y1 {
            for x in x0..x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let cov = (width / 2.0 + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, (cov * f32::from(alpha)) as u8);
                }
            }
        }
    }

    /// A sprite of RGB565 (big-endian) + 8-bit alpha per pixel.
    pub fn sprite(&mut self, x: i32, y: i32, w: i32, h: i32, data: &[u8]) {
        for r in 0..h {
            for col in 0..w {
                let i = ((r * w + col) * 3) as usize;
                if let Some(px) = data.get(i..i + 3) {
                    self.blend(x + col, y + r, u16::from_be_bytes([px[0], px[1]]), px[2]);
                }
            }
        }
    }

    /// Text with the pen on the baseline at `y`; returns its width.
    pub fn text(&mut self, f: &Font, x: i32, y: i32, c: u16, s: &str, align: Align) -> i32 {
        let w = f.width(s);
        let x = match align {
            Align::Left => x,
            Align::Center => x - w / 2,
            Align::Right => x - w,
        };
        f.draw(x, y, s, |px, py, a| self.blend(px, py, c, a * 17));
        w
    }
}
