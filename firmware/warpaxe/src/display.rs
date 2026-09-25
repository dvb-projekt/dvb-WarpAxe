//! The T-Display-S3's ST7789 over the ESP32-S3's i80 (8-bit parallel) bus
//! (Nerd family). Bitaxe boards have no display; this module is a no-op
//! there once board profiles exist.
//!
//! driven with the IDF's `esp_lcd` — the same pieces Pmaxsd's driver uses.
//! Pins from `ESP-Miner-NerdQAxePlus/main/displays/displayDriver.h` and
//! LilyGO's schematic. No LVGL: one RGB565 framebuffer, one bitmap font.

use core::ffi::c_void;
use esp_idf_svc::sys::{self as sys, tusb};

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 170;

const PIN_PWR: i32 = 15;
const PIN_RD: i32 = 9;
const PIN_BACKLIGHT: i32 = 38;
const PIN_CS: i32 = 6;
const PIN_DC: i32 = 7;
const PIN_WR: i32 = 8;
const PIN_RST: i32 = 5;
const PIN_DATA: [i32; 8] = [39, 40, 41, 42, 45, 46, 47, 48];

pub const BLACK: u16 = 0x0000;
pub const WHITE: u16 = 0xFFFF;
pub const GREEN: u16 = 0x07E0;
pub const RED: u16 = 0xF800;
pub const ORANGE: u16 = 0xFD20;
pub const GREY: u16 = 0x8410;
pub const NAVY: u16 = 0x0010;

pub struct Lcd {
    panel: tusb::esp_lcd_panel_handle_t,
    /// 64-byte aligned, in PSRAM, never freed: the i80 DMA wants alignment.
    fb: &'static mut [u8],
}

macro_rules! check {
    ($e:expr) => {{
        let rc = $e;
        if rc != sys::ESP_OK {
            return Err(sys::EspError::from(rc).unwrap());
        }
    }};
}

fn gpio_out(pin: i32, level: bool) {
    unsafe {
        sys::gpio_reset_pin(pin);
        sys::gpio_set_direction(pin, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
        sys::gpio_set_level(pin, level as u32);
    }
}

// The panel handle is only ever used from the one thread that owns the
// Display; the raw pointer is what stops the compiler from seeing that.
unsafe impl Send for Lcd {}

impl Lcd {
    pub fn new() -> Result<Self, sys::EspError> {
        gpio_out(PIN_PWR, true);
        gpio_out(PIN_RD, true);
        gpio_out(PIN_BACKLIGHT, false);

        let mut bus: tusb::esp_lcd_i80_bus_handle_t = core::ptr::null_mut();
        let mut io: tusb::esp_lcd_panel_io_handle_t = core::ptr::null_mut();
        let mut panel: tusb::esp_lcd_panel_handle_t = core::ptr::null_mut();
        unsafe {
            let mut bus_cfg: tusb::esp_lcd_i80_bus_config_t = core::mem::zeroed();
            bus_cfg.dc_gpio_num = PIN_DC;
            bus_cfg.wr_gpio_num = PIN_WR;
            bus_cfg.clk_src = tusb::soc_periph_lcd_clk_src_t_LCD_CLK_SRC_DEFAULT;
            bus_cfg.data_gpio_nums[..8].copy_from_slice(&PIN_DATA);
            bus_cfg.bus_width = 8;
            bus_cfg.max_transfer_bytes = WIDTH * HEIGHT * 2;
            bus_cfg.__bindgen_anon_1.psram_trans_align = 64;
            bus_cfg.sram_trans_align = 4;
            check!(tusb::esp_lcd_new_i80_bus(&bus_cfg, &mut bus));

            let mut io_cfg: tusb::esp_lcd_panel_io_i80_config_t = core::mem::zeroed();
            io_cfg.cs_gpio_num = PIN_CS;
            io_cfg.pclk_hz = 10_000_000;
            io_cfg.trans_queue_depth = 10;
            io_cfg.dc_levels.set_dc_idle_level(0);
            io_cfg.dc_levels.set_dc_cmd_level(0);
            io_cfg.dc_levels.set_dc_dummy_level(0);
            io_cfg.dc_levels.set_dc_data_level(1);
            io_cfg.lcd_cmd_bits = 8;
            io_cfg.lcd_param_bits = 8;
            check!(tusb::esp_lcd_new_panel_io_i80(bus, &io_cfg, &mut io));

            let mut panel_cfg: tusb::esp_lcd_panel_dev_config_t = core::mem::zeroed();
            panel_cfg.reset_gpio_num = PIN_RST;
            panel_cfg.__bindgen_anon_1.rgb_ele_order =
                tusb::lcd_rgb_element_order_t_LCD_RGB_ELEMENT_ORDER_RGB;
            panel_cfg.bits_per_pixel = 16;
            check!(tusb::esp_lcd_new_panel_st7789(io, &panel_cfg, &mut panel));
            check!(tusb::esp_lcd_panel_reset(panel));
            check!(tusb::esp_lcd_panel_init(panel));
            check!(tusb::esp_lcd_panel_invert_color(panel, true));
            check!(tusb::esp_lcd_panel_swap_xy(panel, true));
            check!(tusb::esp_lcd_panel_mirror(panel, false, true));
            check!(tusb::esp_lcd_panel_set_gap(panel, 0, 35));
            check!(tusb::esp_lcd_panel_disp_on_off(panel, true));
        }
        // Internal RAM, like the first build that lit the screen; the
        // PSRAM-aligned variant is the prime suspect for the black screen.
        let len = WIDTH * HEIGHT * 2;
        let fb: &'static mut [u8] = Box::leak(vec![0u8; len].into_boxed_slice());
        let mut d = Lcd { panel, fb };
        d.clear(BLACK);
        d.flush()?;
        gpio_out(PIN_BACKLIGHT, true);
        Ok(d)
    }

    pub fn clear(&mut self, color: u16) {
        let [hi, lo] = color.to_be_bytes();
        for px in self.fb.chunks_exact_mut(2) {
            px[0] = hi;
            px[1] = lo;
        }
    }

    pub fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: u16) {
        let [hi, lo] = color.to_be_bytes();
        for yy in y..(y + h).min(HEIGHT) {
            for xx in x..(x + w).min(WIDTH) {
                let i = (yy * WIDTH + xx) * 2;
                self.fb[i] = hi;
                self.fb[i + 1] = lo;
            }
        }
    }

    /// Draw `text` at (x, y), each font pixel `scale` screen pixels.
    /// Last `n` captured IDF log lines, small, from `y` downwards.
    pub fn log_tail(&mut self, y: usize, n: usize, color: u16) {
        let count = unsafe { tusb::wa_log_count() } as usize;
        let first = count.saturating_sub(n);
        let mut row = 0;
        for i in first..count {
            let mut buf = [0u8; 56];
            unsafe {
                tusb::wa_log_line(
                    i as i32,
                    buf.as_mut_ptr() as *mut core::ffi::c_char,
                    buf.len(),
                )
            };
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let s = String::from_utf8_lossy(&buf[..end]).into_owned();
            self.text(4, y + row * 9, 1, color, &s);
            row += 1;
        }
    }

    pub fn text(&mut self, x: usize, y: usize, scale: usize, color: u16, text: &str) {
        let mut cx = x;
        for &c in text.as_bytes() {
            let g = crate::font::glyph(c);
            for (col, bits) in g.iter().enumerate() {
                for row in 0..crate::font::H {
                    if bits & (1 << row) != 0 {
                        self.rect(cx + col * scale, y + row * scale, scale, scale, color);
                    }
                }
            }
            cx += (crate::font::W + 1) * scale;
        }
    }

    /// Backlight on or off; the panel keeps its content.
    pub fn set_backlight(&mut self, on: bool) {
        gpio_out(PIN_BACKLIGHT, on);
    }
    pub fn flush(&mut self) -> Result<(), sys::EspError> {
        unsafe {
            check!(tusb::esp_lcd_panel_draw_bitmap(
                self.panel,
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                self.fb.as_ptr() as *const c_void,
            ));
        }
        Ok(())
    }
}

/// The Bitaxe's SSD1306 on the board's I²C bus.
pub struct Oled {
    panel: warpaxe_board::ssd1306::Ssd1306<crate::board::SharedI2c>,
    flip: Option<bool>,
    invert: Option<bool>,
}

impl Oled {
    pub fn new(bus: crate::board::SharedI2c, addr: u8) -> Result<Self, String> {
        let mut panel = warpaxe_board::ssd1306::Ssd1306::new(bus, addr);
        panel.init().map_err(|e| format!("{e:?}"))?;
        Ok(Oled {
            panel,
            flip: None,
            invert: None,
        })
    }
    /// Apply the config's orientation when it changed; live, so a
    /// PATCH of `flipscreen` / `invertscreen` shows on the next draw.
    pub fn orient(&mut self, flip: bool, invert: bool) {
        if self.flip != Some(flip) && self.panel.set_mirror(flip, flip).is_ok() {
            self.flip = Some(flip);
        }
        if self.invert != Some(invert) && self.panel.set_invert(invert).is_ok() {
            self.invert = Some(invert);
        }
    }
    pub fn clear(&mut self) {
        self.panel.clear();
    }
    pub fn text(&mut self, x: usize, y: usize, scale: usize, text: &str) {
        self.panel.text(x, y, scale, text);
    }
    pub fn flush(&mut self) -> Result<(), String> {
        self.panel.flush().map_err(|e| format!("{e:?}"))
    }
    pub fn set_power(&mut self, on: bool) {
        let _ = self.panel.set_power(on);
    }
}

/// Whichever display the board profile names.
pub enum Display {
    Lcd(Lcd),
    Oled(Oled),
}

/// Uptime second of the last thing that should wake the display:
/// boot, a button press, a write on the API.
static LAST_WAKE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static ASLEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Something happened the person wants to see: keep or bring the
/// display on.
pub fn touch() {
    LAST_WAKE.store(
        crate::diag::uptime_s() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
}

pub fn last_wake() -> u64 {
    u64::from(LAST_WAKE.load(std::sync::atomic::Ordering::Relaxed))
}

pub fn asleep() -> bool {
    ASLEEP.load(std::sync::atomic::Ordering::Relaxed)
}

impl Display {
    /// Panel dark (OLED off, LCD backlight off) or back on.
    pub fn set_awake(&mut self, awake: bool) {
        match self {
            Display::Lcd(l) => l.set_backlight(awake),
            Display::Oled(o) => o.set_power(awake),
        }
        ASLEEP.store(!awake, std::sync::atomic::Ordering::Relaxed);
    }

    /// The profile's display, or `None` when it is absent or failed.
    pub fn new() -> Option<Self> {
        use warpaxe_board::profile::Display as Kind;
        match crate::board::profile().display {
            Kind::St7789I80 => match Lcd::new() {
                Ok(l) => Some(Display::Lcd(l)),
                Err(e) => {
                    log::error!("display: st7789 failed: {e}");
                    None
                }
            },
            Kind::Ssd1306 { addr } => match crate::board::i2c().map(|b| Oled::new(b, addr)) {
                Some(Ok(o)) => Some(Display::Oled(o)),
                Some(Err(e)) => {
                    log::error!("display: ssd1306 failed: {e}");
                    None
                }
                None => {
                    log::error!("display: ssd1306 named but no i2c bus");
                    None
                }
            },
            Kind::None => None,
        }
    }
}
