//! The T-Display-S3's ST7789 over the ESP32-S3's i80 (8-bit parallel) bus
//! (Nerd family). Bitaxe boards have no display; this module is a no-op
//! there once board profiles exist.
//!
//! driven with the IDF's `esp_lcd` — the same pieces Pmaxsd's driver uses.
//! Pins from `ESP-Miner-NerdQAxePlus/main/displays/displayDriver.h` and
//! LilyGO's schematic. No LVGL: one RGB565 framebuffer, one bitmap font.

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};
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
pub const GREY: u16 = 0x8410;
/// Brand colours of the web UI: `--btc` (Bitaxe) and `--plasma` (Nerd).
pub const BTC: u16 = 0xF483;
pub const PLASMA: u16 = 0x071F;

/// Width in pixels of `text` in the 5×7 font at `scale`.
pub fn text_width(text: &str, scale: usize) -> usize {
    (text.len() * (crate::font::W + 1) * scale).saturating_sub(scale)
}

/// Set by the i80 driver when a frame has left the buffer. `flush` waits
/// for it: the next frame is drawn into the same buffer, and drawing
/// while the DMA still reads it put half-old, half-new (and now and then
/// colour-garbled) frames on the panel.
static FRAME_SENT: AtomicBool = AtomicBool::new(true);

unsafe extern "C" fn frame_sent(
    _io: tusb::esp_lcd_panel_io_handle_t,
    _edata: *mut tusb::esp_lcd_panel_io_event_data_t,
    _ctx: *mut c_void,
) -> bool {
    FRAME_SENT.store(true, Ordering::Release);
    false
}

/// `len` zeroed bytes of PSRAM for the time of `f`: room to unpack a
/// picture that is drawn and then not needed again. Asked for from PSRAM
/// by name, because a plain allocation below 16 KB comes from the
/// internal heap, which is the scarce one. `None` when PSRAM has no such
/// piece.
pub fn with_psram_scratch<R>(len: usize, f: impl FnOnce(&mut [u8]) -> R) -> Option<R> {
    let p = unsafe { sys::heap_caps_calloc(1, len, sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT) };
    if p.is_null() {
        return None;
    }
    // Nobody else knows the block, and it is freed only after `f`.
    let r = f(unsafe { core::slice::from_raw_parts_mut(p as *mut u8, len) });
    unsafe { sys::heap_caps_free(p) };
    Some(r)
}

/// Rows per transfer: the frame goes out in bands through a small buffer
/// in internal RAM (320 × 10 × 2 = 6.4 KB), as LVGL-based stock firmware
/// does. Straight from the PSRAM frame buffer the i80 DMA now and then
/// came up short while Wi-Fi and the miner used PSRAM too: a byte slipped
/// and the rest of the frame showed in wrong colours, with a moving edge
/// where it started (seen on the NerdQAxe++, 2026-09-28).
const BAND: usize = 10;

pub struct Lcd {
    panel: tusb::esp_lcd_panel_handle_t,
    /// DMA-capable internal RAM for one band; `None` if it could not be
    /// had, then the frame goes out from PSRAM as before.
    bounce: Option<&'static mut [u8]>,
    /// 64-byte aligned, in PSRAM, never freed: the i80 DMA wants alignment.
    fb: &'static mut [u8],
    flip: bool,
    invert: bool,
    /// The backlight driver's level, 0 (off) to `BL_STEPS` (AW9364), or
    /// the percentage last set (PWM boards).
    bl_level: u8,
    /// The backlight hangs on an AW9364 (pulse steps), else it is dimmed
    /// by PWM.
    bl_pulses: bool,
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

/// The backlight driver's current levels (AW9364).
const BL_STEPS: u8 = 16;

/// The driver level for a brightness in percent: 0 is off, anything
/// else one of the 16 levels, 1 at the least.
fn backlight_level(pct: u8) -> u8 {
    if pct == 0 {
        return 0;
    }
    let lvl = (u16::from(pct.min(100)) * u16::from(BL_STEPS) + 50) / 100;
    (lvl as u8).clamp(1, BL_STEPS)
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
            // One full frame plus headroom: sized exactly, the DMA link list
            // came up one descriptor short now and then after alignment
            // ("gdma_link_mount_buffers: no more").
            bus_cfg.max_transfer_bytes = WIDTH * HEIGHT * 2 + 8192;
            bus_cfg.__bindgen_anon_1.psram_trans_align = 64;
            bus_cfg.sram_trans_align = 4;
            check!(tusb::esp_lcd_new_i80_bus(&bus_cfg, &mut bus));

            let mut io_cfg: tusb::esp_lcd_panel_io_i80_config_t = core::mem::zeroed();
            io_cfg.cs_gpio_num = PIN_CS;
            io_cfg.pclk_hz = 10_000_000;
            io_cfg.trans_queue_depth = 10;
            io_cfg.on_color_trans_done = Some(frame_sent);
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
        // A plain allocation: at 108,800 bytes it is above the 16 KB
        // threshold and lands in PSRAM (the field's comment says so too).
        // The PSRAM-aligned variant was the prime suspect for the black
        // screen of the first builds; this one lit it.
        let len = WIDTH * HEIGHT * 2;
        let fb: &'static mut [u8] = Box::leak(vec![0u8; len].into_boxed_slice());
        let bounce = unsafe {
            let n = WIDTH * BAND * 2;
            let p = sys::heap_caps_malloc(n, sys::MALLOC_CAP_DMA | sys::MALLOC_CAP_INTERNAL);
            (!p.is_null()).then(|| core::slice::from_raw_parts_mut(p as *mut u8, n))
        };
        if bounce.is_none() {
            log::warn!("display: no internal RAM for the transfer band, sending from PSRAM");
        }
        let mut d = Lcd {
            panel,
            bounce,
            fb,
            flip: false,
            invert: false,
            bl_level: 0,
            bl_pulses: crate::board::profile().backlight_aw9364(),
        };
        d.clear(BLACK);
        d.flush()?;
        // Off since the start of the init, far longer than 2.5 ms: the
        // driver starts at its top level.
        gpio_out(PIN_BACKLIGHT, true);
        d.bl_level = if d.bl_pulses { BL_STEPS } else { 100 };
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
    /// The frame buffer as a drawing surface.
    pub fn canvas(&mut self) -> crate::gfx::Canvas<'_> {
        crate::gfx::Canvas {
            fb: self.fb,
            w: WIDTH as i32,
            h: HEIGHT as i32,
        }
    }

    pub fn blit(&mut self, frame: &[u8]) {
        let n = frame.len().min(self.fb.len());
        self.fb[..n].copy_from_slice(&frame[..n]);
    }

    /// A whole frame stored packed (raw DEFLATE), unpacked straight into
    /// the frame buffer: no second buffer, and the 1.3 KB the unpacking
    /// works with are on this thread's stack for the call. A stream that
    /// is not exactly one frame cannot come out of the build (host test
    /// crates/inflate/tests/splash.rs); should it, the frame is black.
    pub fn blit_packed(&mut self, packed: &[u8]) {
        if warpaxe_inflate::inflate(packed, self.fb) != Ok(self.fb.len()) {
            self.clear(BLACK);
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

    /// `flipscreen` / `invertscreen` on the ST7789: 180° is both mirror
    /// axes reversed (the 170-line window sits centred in the 240-line
    /// memory, so the 35-line gap stays); the panel's colours are
    /// inverted by default, so `invert` switches that off.
    pub fn orient(&mut self, flip: bool, invert: bool) {
        if flip != self.flip {
            let rc = unsafe { tusb::esp_lcd_panel_mirror(self.panel, flip, !flip) };
            if rc == sys::ESP_OK {
                self.flip = flip;
            }
        }
        if invert != self.invert {
            let rc = unsafe { tusb::esp_lcd_panel_invert_color(self.panel, !invert) };
            if rc == sys::ESP_OK {
                self.invert = invert;
            }
        }
    }

    /// Backlight on or off; the panel keeps its content.
    pub fn set_backlight(&mut self, on: bool) {
        self.set_backlight_pct(if on { 100 } else { 0 });
    }

    /// Backlight in percent. The LCD's backlight driver (AW9364) does not
    /// dim by PWM: it counts low pulses on its enable pin, each one a
    /// step down of its 16 current levels (from level 1 round to 16), and
    /// a low longer than 2.5 ms switches it off; switched on it starts at
    /// level 16. A PWM made it step round and round, so 75 % could come
    /// out darker than 50 %. Here the percentage picks one of the 16
    /// levels. The driver cannot be read back, so every change starts
    /// from a known state: off for 3 ms, on (level 16), 1 ms to settle,
    /// then the pulses down to the level, sent with interrupts off so
    /// none is stretched or lost. A missed pulse is then put right by the
    /// next change instead of lasting. The 4 ms dark is not seen.
    pub fn set_backlight_pct(&mut self, pct: u8) {
        if !self.bl_pulses {
            return self.set_backlight_pwm(pct.min(100));
        }
        let target = backlight_level(pct);
        if target == self.bl_level {
            return;
        }
        let pin = PIN_BACKLIGHT;
        unsafe { sys::gpio_set_level(pin, 0) };
        self.bl_level = 0;
        if target == 0 {
            return;
        }
        unsafe {
            sys::esp_rom_delay_us(3_000);
            sys::gpio_set_level(pin, 1);
            sys::esp_rom_delay_us(1_000);
        }
        let pulses = BL_STEPS - target;
        esp_idf_svc::hal::interrupt::free(|| {
            for _ in 0..pulses {
                unsafe {
                    sys::gpio_set_level(pin, 0);
                    sys::esp_rom_delay_us(2);
                    sys::gpio_set_level(pin, 1);
                    sys::esp_rom_delay_us(2);
                }
            }
        });
        self.bl_level = target;
    }
    /// Backlight in percent on a board without the AW9364 (Naja Duo): 100
    /// and 0 are the plain pin, anything between a 5 kHz PWM with the
    /// duty in step with the percentage. The LEDC is set up on each
    /// change; `gpio_out` takes the pin back for 0 and 100.
    fn set_backlight_pwm(&mut self, pct: u8) {
        if pct == self.bl_level {
            return;
        }
        self.bl_level = pct;
        match pct {
            0 => gpio_out(PIN_BACKLIGHT, false),
            100.. => gpio_out(PIN_BACKLIGHT, true),
            p => {
                let timer = sys::ledc_timer_config_t {
                    speed_mode: sys::ledc_mode_t_LEDC_LOW_SPEED_MODE,
                    duty_resolution: sys::ledc_timer_bit_t_LEDC_TIMER_8_BIT,
                    timer_num: sys::ledc_timer_t_LEDC_TIMER_0,
                    freq_hz: 5_000,
                    clk_cfg: sys::soc_periph_ledc_clk_src_legacy_t_LEDC_AUTO_CLK,
                    deconfigure: false,
                };
                let duty = (u32::from(p) * 255 / 100).max(1);
                let chan = sys::ledc_channel_config_t {
                    gpio_num: PIN_BACKLIGHT,
                    speed_mode: sys::ledc_mode_t_LEDC_LOW_SPEED_MODE,
                    channel: sys::ledc_channel_t_LEDC_CHANNEL_0,
                    timer_sel: sys::ledc_timer_t_LEDC_TIMER_0,
                    duty,
                    ..Default::default()
                };
                let ok = unsafe {
                    sys::ledc_timer_config(&timer) == sys::ESP_OK
                        && sys::ledc_channel_config(&chan) == sys::ESP_OK
                };
                if !ok {
                    // Better full than dark: the clock must stay readable.
                    log::warn!("display: backlight PWM failed, full brightness");
                    gpio_out(PIN_BACKLIGHT, true);
                    self.bl_level = 100;
                }
            }
        }
    }

    pub fn flush(&mut self) -> Result<(), sys::EspError> {
        snapshot(self.fb);
        match self.bounce.as_deref_mut() {
            Some(band) => {
                for y in (0..HEIGHT).step_by(BAND) {
                    let rows = BAND.min(HEIGHT - y);
                    let n = WIDTH * rows * 2;
                    let at = y * WIDTH * 2;
                    band[..n].copy_from_slice(&self.fb[at..at + n]);
                    send(self.panel, y, rows, band.as_ptr())?;
                }
            }
            None => send(self.panel, 0, HEIGHT, self.fb.as_ptr())?,
        }
        Ok(())
    }
}

/// Draw `rows` full-width rows at `y` from `data` and wait until the DMA
/// has read them (the caller reuses the buffer): ~1 ms per band at
/// 10 MHz, given up after 50 ms rather than hang the display thread.
fn send(
    panel: tusb::esp_lcd_panel_handle_t,
    y: usize,
    rows: usize,
    data: *const u8,
) -> Result<(), sys::EspError> {
    FRAME_SENT.store(false, Ordering::Release);
    unsafe {
        check!(tusb::esp_lcd_panel_draw_bitmap(
            panel,
            0,
            y as i32,
            WIDTH as i32,
            (y + rows) as i32,
            data as *const c_void,
        ));
    }
    for _ in 0..50 {
        if FRAME_SENT.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Ok(())
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
    pub fn bitmap(&mut self, bits: &[u8]) {
        self.panel.bitmap(bits);
    }
    pub fn pixel(&mut self, x: i32, y: i32, on: bool) {
        if (0..128).contains(&x) && (0..32).contains(&y) {
            self.panel.set_pixel(x as usize, y as usize, on);
        }
    }
    pub fn clear(&mut self) {
        self.panel.clear();
    }
    pub fn text(&mut self, x: usize, y: usize, scale: usize, text: &str) {
        self.panel.text(x, y, scale, text);
    }
    pub fn flush(&mut self) -> Result<(), String> {
        snapshot(self.panel.frame());
        // Against burn-in: the picture steps one position along the
        // orbit every ten minutes (unshifted for the first ten).
        let orbit = &warpaxe_board::ssd1306::ORBIT;
        let (dx, dy) = orbit[(crate::diag::uptime_s() / 600) as usize % orbit.len()];
        self.panel.set_shift(dx, dy);
        self.panel.flush().map_err(|e| format!("{e:?}"))
    }
    pub fn set_power(&mut self, on: bool) {
        let _ = self.panel.set_power(on);
    }
    /// Whether the panel took it (an I²C write on the shared bus).
    pub fn set_brightness(&mut self, pct: u8) -> bool {
        self.panel.set_brightness(pct).is_ok()
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

/// Uptime second of the last key press, on the device or by the API's
/// remote key. Night mode wakes for this alone: a write on the API (a
/// tuner's PATCH every few minutes) must not light the display at night.
static LAST_KEY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A key was pressed: wake the display, at night too.
pub fn touch_key() {
    LAST_KEY.store(
        crate::diag::uptime_s() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
    touch();
}

/// Seconds since the last key press; `u64::MAX` before the first.
pub fn since_key() -> u64 {
    match LAST_KEY.load(std::sync::atomic::Ordering::Relaxed) {
        0 => u64::MAX,
        k => crate::diag::uptime_s().saturating_sub(u64::from(k)),
    }
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

    /// Brightness of an awake panel, percent (night mode); 100 is the
    /// normal light. `false` when the OLED did not take it, so the
    /// caller sends it again.
    pub fn set_brightness(&mut self, pct: u8) -> bool {
        match self {
            Display::Lcd(l) => {
                l.set_backlight_pct(pct.max(1));
                true
            }
            Display::Oled(o) => o.set_brightness(pct.max(1)),
        }
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

/// Firmware update progress for the display: 0 = none, 1 = erasing,
/// 2..=102 = written percent + 2.
pub static OTA_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// A special screen shown for a while on request (`POST
/// /api/v1/display/demo`), to see it without the real event.
pub static DEMO: std::sync::Mutex<Option<(String, u64)>> = std::sync::Mutex::new(None);

pub fn demo(event: &str, secs: u64) {
    *DEMO.lock().unwrap() = Some((event.to_string(), crate::diag::uptime_s() + secs));
    touch();
}

/// A copy of the next frame sent to the panel, on request (`GET
/// /api/v1/display/frame`, for looking at the screen remotely).
static SNAP_WANTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SNAP: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

fn snapshot(frame: &[u8]) {
    if SNAP_WANTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        *SNAP.lock().unwrap() = Some(frame.to_vec());
    }
}

/// The next frame the display sends, waiting up to two seconds. None at
/// once while the display is asleep: nothing is drawn then, and the
/// wait would hold the HTTP task for the whole two seconds.
pub fn capture() -> Option<Vec<u8>> {
    if asleep() {
        return None;
    }
    *SNAP.lock().unwrap() = None;
    SNAP_WANTED.store(true, std::sync::atomic::Ordering::Relaxed);
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Some(f) = SNAP.lock().unwrap().take() {
            return Some(f);
        }
    }
    SNAP_WANTED.store(false, std::sync::atomic::Ordering::Relaxed);
    None
}

/// A key press from the API (`POST /api/v1/display/key?k=next|home`):
/// 0 none, 1 next, 2 home.
pub static REMOTE_KEY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
