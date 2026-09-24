//! SSD1306 128×32 OLED over I²C (the Bitaxe display), with its own
//! frame buffer: 4 pages of 128 bytes, each byte a column of 8 pixels,
//! bit 0 at the top. Text is the shared 5×7 font.

use crate::font;
use crate::i2c::{Error, I2c};

/// Default I²C address (SA0 low).
pub const ADDR: u8 = 0x3C;
/// Panel width, pixels.
pub const WIDTH: usize = 128;
/// Panel height, pixels.
pub const HEIGHT: usize = 32;
const PAGES: usize = HEIGHT / 8;
const CTRL_CMD: u8 = 0x00;
const CTRL_DATA: u8 = 0x40;

/// The panel init for a 128×32 module: off, clock, 32-row multiplex,
/// charge pump on, horizontal addressing, segment and COM remap so
/// (0, 0) is top-left with the connector at the top, COM pins for 32
/// rows, contrast, precharge, VCOM, normal display, on.
const INIT: [u8; 26] = [
    0xAE, 0xD5, 0x80, 0xA8, 0x1F, 0xD3, 0x00, 0x40, 0x8D, 0x14, 0x20, 0x00, 0xA1, 0xC8, 0xDA, 0x02,
    0x81, 0x8F, 0xD9, 0xF1, 0xDB, 0x40, 0xA4, 0xA6, 0x2E, 0xAF,
];

/// The driver with its frame buffer.
pub struct Ssd1306<I> {
    bus: I,
    addr: u8,
    fb: [u8; WIDTH * PAGES],
}

impl<I: I2c> Ssd1306<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, addr: u8) -> Self {
        Ssd1306 {
            bus,
            addr,
            fb: [0; WIDTH * PAGES],
        }
    }

    fn command(&mut self, cmds: &[u8]) -> Result<(), Error<I::Error>> {
        let mut buf = [0u8; 32];
        buf[0] = CTRL_CMD;
        buf[1..=cmds.len()].copy_from_slice(cmds);
        Ok(self.bus.write(self.addr, &buf[..=cmds.len()])?)
    }

    /// Send the init sequence and a cleared frame.
    pub fn init(&mut self) -> Result<(), Error<I::Error>> {
        self.command(&INIT)?;
        self.clear();
        self.flush()
    }

    /// Mirror the panel: `x` flips left-right, `y` top-bottom, for a
    /// module mounted the other way round.
    pub fn set_mirror(&mut self, x: bool, y: bool) -> Result<(), Error<I::Error>> {
        self.command(&[if x { 0xA0 } else { 0xA1 }, if y { 0xC0 } else { 0xC8 }])
    }

    /// Everything off.
    pub fn clear(&mut self) {
        self.fb = [0; WIDTH * PAGES];
    }

    /// One pixel; outside the panel is ignored.
    pub fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let i = (y / 8) * WIDTH + x;
        let bit = 1 << (y % 8);
        if on {
            self.fb[i] |= bit;
        } else {
            self.fb[i] &= !bit;
        }
    }

    /// Draw `text` at (x, y), each font pixel `scale` panel pixels;
    /// 6 × scale pixels per character.
    pub fn text(&mut self, x: usize, y: usize, scale: usize, text: &str) {
        let mut cx = x;
        for &c in text.as_bytes() {
            let g = font::glyph(c);
            for (col, bits) in g.iter().enumerate() {
                for row in 0..font::H {
                    if bits & (1 << row) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                self.set_pixel(cx + col * scale + dx, y + row * scale + dy, true);
                            }
                        }
                    }
                }
            }
            cx += (font::W + 1) * scale;
        }
    }

    /// Send the frame buffer: the full column and page window, then the
    /// four pages as data writes.
    pub fn flush(&mut self) -> Result<(), Error<I::Error>> {
        self.command(&[0x21, 0, (WIDTH - 1) as u8, 0x22, 0, (PAGES - 1) as u8])?;
        for page in 0..PAGES {
            let mut buf = [0u8; WIDTH + 1];
            buf[0] = CTRL_DATA;
            buf[1..].copy_from_slice(&self.fb[page * WIDTH..(page + 1) * WIDTH]);
            self.bus.write(self.addr, &buf)?;
        }
        Ok(())
    }

    /// The frame buffer, for tests and for a host preview.
    pub fn frame(&self) -> &[u8] {
        &self.fb
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    #[test]
    fn init_sends_the_sequence_then_a_blank_frame() {
        let mut d = Ssd1306::new(Bus::sink(), ADDR);
        d.init().unwrap();
        let log = d.bus.log;
        let mut first = vec![CTRL_CMD];
        first.extend_from_slice(&INIT);
        assert_eq!(log[0], Tx::Write(ADDR, first));
        assert_eq!(
            log[1],
            Tx::Write(ADDR, vec![CTRL_CMD, 0x21, 0, 127, 0x22, 0, 3])
        );
        assert_eq!(log.len(), 2 + PAGES);
        for page in &log[2..] {
            match page {
                Tx::Write(_, b) => {
                    assert_eq!(b.len(), WIDTH + 1);
                    assert_eq!(b[0], CTRL_DATA);
                    assert!(b[1..].iter().all(|&x| x == 0));
                }
                _ => panic!("data write expected"),
            }
        }
    }

    #[test]
    fn text_lands_in_the_frame_buffer_column_wise() {
        let mut d = Ssd1306::new(Bus::sink(), ADDR);
        d.text(0, 0, 1, "I");
        // 'I' = [0x00, 0x41, 0x7F, 0x41, 0x00] straight into page 0.
        assert_eq!(&d.frame()[..6], &[0x00, 0x41, 0x7F, 0x41, 0x00, 0x00]);
        // Same glyph at y = 4 straddles pages 0 and 1.
        d.clear();
        d.text(0, 4, 1, "I");
        assert_eq!(d.frame()[2], 0x7F << 4);
        assert_eq!(d.frame()[WIDTH + 2], 0x7F >> 4);
        // Scale 2 doubles the pitch: the next character starts at x = 12.
        d.clear();
        d.text(0, 0, 2, "II");
        assert_ne!(d.frame()[12 + 4], 0);
    }

    #[test]
    fn pixels_outside_the_panel_are_ignored() {
        let mut d = Ssd1306::new(Bus::sink(), ADDR);
        d.set_pixel(WIDTH, 0, true);
        d.set_pixel(0, HEIGHT, true);
        assert!(d.frame().iter().all(|&b| b == 0));
        d.set_pixel(127, 31, true);
        assert_eq!(d.frame()[3 * WIDTH + 127], 0x80);
    }
}
