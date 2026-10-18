//! Host preview of the device display: the firmware's own `screens.rs`
//! and `gfx` draw into a frame buffer, fed with sample values; every page
//! and special screen is written as a raw frame (RGB565 big-endian
//! 320x170, or SSD1306 pages 128x32) for tools/frame2png.py.

#![allow(dead_code)]

#[path = "../../../firmware/warpaxe/src/display_i18n.rs"]
mod display_i18n;
#[path = "../../../firmware/warpaxe/src/gfx/mod.rs"]
mod gfx;
#[path = "../../../firmware/warpaxe/src/screens.rs"]
mod screens;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static GAMMA: AtomicBool = AtomicBool::new(false);
static HOTSPOT: AtomicBool = AtomicBool::new(false);
static FRAME: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
static UPTIME: AtomicU64 = AtomicU64::new(11_535);

/// PREVIEW_STRESS=1: the longest values each place can show (12 d 23 h,
/// six-figure shares and rpm, a 999.99 M difficulty), to see nothing
/// runs into its neighbour.
fn stress() -> bool {
    std::env::var("PREVIEW_STRESS").is_ok()
}

mod display {
    pub const WIDTH: usize = 320;
    pub const HEIGHT: usize = 170;
    pub static OTA_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    pub static DEMO: std::sync::Mutex<Option<(String, u64)>> = std::sync::Mutex::new(None);
    pub fn touch() {}
    pub enum Display {
        Lcd(Lcd),
        Oled(Oled),
    }
    pub struct Lcd {
        pub fb: Vec<u8>,
    }
    impl Lcd {
        pub fn orient(&mut self, _f: bool, _i: bool) {}
        pub fn canvas(&mut self) -> crate::gfx::Canvas<'_> {
            crate::gfx::Canvas {
                fb: &mut self.fb,
                w: WIDTH as i32,
                h: HEIGHT as i32,
            }
        }
        pub fn blit(&mut self, f: &[u8]) {
            let n = f.len().min(self.fb.len());
            self.fb[..n].copy_from_slice(&f[..n]);
        }
        pub fn flush(&mut self) -> Result<(), String> {
            *crate::FRAME.lock().unwrap() = self.fb.clone();
            Ok(())
        }
    }
    pub struct Oled {
        pub fb: [u8; 512],
    }
    impl Oled {
        pub fn orient(&mut self, _f: bool, _i: bool) {}
        pub fn clear(&mut self) {
            self.fb = [0; 512];
        }
        pub fn pixel(&mut self, x: i32, y: i32, on: bool) {
            if (0..128).contains(&x) && (0..32).contains(&y) {
                let i = (y as usize / 8) * 128 + x as usize;
                let b = 1u8 << (y % 8);
                if on {
                    self.fb[i] |= b
                } else {
                    self.fb[i] &= !b
                }
            }
        }
        pub fn bitmap(&mut self, bits: &[u8]) {
            for y in 0..32 {
                for x in 0..128 {
                    let on = bits
                        .get(y * 16 + x / 8)
                        .is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
                    self.pixel(x as i32, y as i32, on);
                }
            }
        }
        pub fn text(&mut self, x: usize, y: usize, scale: usize, s: &str) {
            let mut cx = x;
            for &c in s.as_bytes() {
                let g = warpaxe_board::font::glyph(c);
                for (col, bits) in g.iter().enumerate() {
                    for row in 0..7 {
                        if bits & (1 << row) != 0 {
                            for dy in 0..scale {
                                for dx in 0..scale {
                                    self.pixel(
                                        (cx + col * scale + dx) as i32,
                                        (y + row * scale + dy) as i32,
                                        true,
                                    );
                                }
                            }
                        }
                    }
                }
                cx += 6 * scale;
            }
        }
        pub fn flush(&mut self) -> Result<(), String> {
            *crate::FRAME.lock().unwrap() = self.fb.to_vec();
            Ok(())
        }
    }
}

mod diag {
    pub fn uptime_s() -> u64 {
        crate::UPTIME.load(std::sync::atomic::Ordering::Relaxed)
    }
}

mod clock {
    pub fn local_hm() -> Option<(u8, u8)> {
        Some((17, 42))
    }
}

mod history {
    fn curve(n: usize, base: f32) -> Vec<f32> {
        (0..n.min(360))
            .map(|i| {
                let t = i as f32;
                base * (1.0 + 0.012 * (t / 17.0).sin() + 0.006 * (t / 5.3).cos())
            })
            .collect()
    }
    pub fn recent_ghs(n: usize) -> Vec<f32> {
        curve(
            n,
            if crate::GAMMA.load(std::sync::atomic::Ordering::Relaxed) {
                1310.0
            } else {
                6520.0
            },
        )
    }
    pub fn recent_power_w(n: usize) -> Vec<f32> {
        curve(n, 109.5)
    }
    pub fn recent_eff_jth(n: usize) -> Vec<f32> {
        curve(n, 16.8)
    }
}

mod market {
    #[derive(Clone)]
    pub struct Market {
        pub price: Option<(String, f64)>,
        pub fees: Option<(f64, f64, f64)>,
    }
    pub fn get() -> Option<Market> {
        Some(Market {
            price: Some((
                "EUR".into(),
                if crate::stress() { 999_999.0 } else { 74_245.0 },
            )),
            fees: Some((2.0, 1.0, 1.0)),
        })
    }
}

mod config {
    #[derive(Clone)]
    pub struct Config {
        pub display_language: String,
        pub pid_target_temp: u8,
        pub overheat_temp: u8,
        pub flipscreen: bool,
        pub invertscreen: bool,
        pub core_voltage: u16,
        pub autofanspeed: bool,
        pub fanspeed: u8,
        pub stratum_user: String,
        pub fallback_user: String,
        pub ssid: String,
        pub frequency: u16,
        pub btc_price: bool,
        pub display_pages: Vec<String>,
        pub display_start_page: String,
        pub cockpit_big: String,
        pub cockpit_slots: Vec<String>,
        pub cockpit_footer: String,
        pub oled_slides: Vec<String>,
        pub oled_interval: u8,
    }
    /// A comma-separated list from the environment, empty when unset.
    fn list(var: &str) -> Vec<String> {
        std::env::var(var)
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }
    pub fn get() -> Config {
        Config {
            display_language: std::env::var("PREVIEW_LANG").unwrap_or_else(|_| "en".into()),
            // The display's choosable parts, as the API would set them:
            // PREVIEW_PAGES=cockpit,pool PREVIEW_START=pool PREVIEW_BIG=power
            // PREVIEW_SLOTS=vr,fan,uptime PREVIEW_FOOTER=shares-uptime
            // PREVIEW_OLED=temp,logo
            btc_price: std::env::var("PREVIEW_PRICE").is_ok(),
            display_pages: list("PREVIEW_PAGES"),
            display_start_page: std::env::var("PREVIEW_START").unwrap_or_default(),
            cockpit_big: std::env::var("PREVIEW_BIG").unwrap_or_default(),
            cockpit_slots: list("PREVIEW_SLOTS"),
            cockpit_footer: std::env::var("PREVIEW_FOOTER").unwrap_or_default(),
            oled_slides: list("PREVIEW_OLED"),
            oled_interval: 4,
            pid_target_temp: 58,
            overheat_temp: 70,
            flipscreen: false,
            invertscreen: false,
            core_voltage: 1250,
            autofanspeed: true,
            fanspeed: 100,
            stratum_user: "bc1qexampleaddressxxxxxxxxxxxxxxxxxxxxxxxx.NerdQaxe++".into(),
            fallback_user: String::new(),
            ssid: "HomeNet".into(),
            frequency: if crate::board::profile().name == "nerdnos" {
                300
            } else {
                800
            },
        }
    }
    pub fn best_diff() -> u64 {
        if crate::stress() {
            999_990_000
        } else {
            11_200_000
        }
    }
    pub fn best_diff_f() -> f64 {
        0.0381
    }
}

mod cpuminer {
    pub struct Info {
        pub hw_khs: f64,
        pub sw_khs: f64,
        pub candidates: u32,
        pub best_diff: f64,
    }
    pub fn info() -> Info {
        Info {
            hw_khs: 214.6,
            sw_khs: 76.9,
            candidates: 41,
            best_diff: 0.0127,
        }
    }
    pub fn die_temp_c() -> Option<f32> {
        Some(56.4)
    }
    pub fn cpu_mhz() -> u32 {
        240
    }
}

mod board {
    pub use warpaxe_board::Profile;
    pub struct BoardState {
        pub temp_c: Option<f32>,
        pub vr_temp_c: Option<i32>,
        pub vin_mv: Option<f32>,
        pub vout_mv: Option<f32>,
        pub iout_ma: Option<f32>,
        pub power_w: Option<f32>,
        pub fan_pct: u8,
        pub fan_rpm: u16,
        pub fan2_pct: Option<u8>,
        pub fan2_rpm: Option<u16>,
        pub overheat_latched: bool,
        pub overheat_cause: Option<&'static str>,
        pub power_estimated: bool,
        pub has_vr: bool,
    }
    pub fn gamma() -> bool {
        crate::GAMMA.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn state() -> BoardState {
        if profile().name == "nerdnos" {
            return BoardState {
                temp_c: Some(73.5),
                vr_temp_c: None,
                vin_mv: None,
                vout_mv: Some(1392.0),
                iout_ma: None,
                power_w: Some(8.0),
                fan_pct: 0,
                fan_rpm: 0,
                fan2_pct: None,
                fan2_rpm: None,
                overheat_latched: false,
                overheat_cause: None,
                power_estimated: true,
                has_vr: false,
            };
        }
        if gamma() {
            BoardState {
                temp_c: Some(62.3),
                vr_temp_c: Some(55),
                vin_mv: Some(5120.0),
                vout_mv: Some(1140.0),
                iout_ma: Some(20500.0),
                power_w: Some(23.4),
                fan_pct: 100,
                fan_rpm: 5020,
                fan2_pct: None,
                fan2_rpm: None,
                overheat_latched: false,
                overheat_cause: None,
                power_estimated: profile().name == "nerdnos",
                has_vr: profile().name != "nerdnos",
            }
        } else {
            BoardState {
                temp_c: Some(56.2),
                vr_temp_c: Some(50),
                vin_mv: Some(12050.0),
                vout_mv: Some(1238.0),
                iout_ma: Some(9140.0),
                power_w: Some(if crate::stress() { 1099.5 } else { 109.5 }),
                fan_pct: 59,
                fan_rpm: if crate::stress() { 65_535 } else { 1831 },
                fan2_pct: Some(25),
                fan2_rpm: Some(0),
                overheat_latched: false,
                overheat_cause: None,
                power_estimated: profile().name == "nerdnos",
                has_vr: profile().name != "nerdnos",
            }
        }
    }
    pub struct Chip {
        pub small_cores: u16,
    }
    pub fn chip() -> Chip {
        let n = format!("{:?}", profile().asic);
        Chip {
            small_cores: if n.contains("1397") { 672 } else { 2040 },
        }
    }
    pub fn profile() -> &'static Profile {
        // PREVIEW_BOARD=nerdoctaxe-gamma renders another Nerd board's titles.
        let own = if gamma() {
            "bitaxe-gamma"
        } else {
            "nerdqaxe-plusplus"
        };
        let name: &str = &*Box::leak(
            std::env::var("PREVIEW_BOARD")
                .unwrap_or_else(|_| own.into())
                .into_boxed_str(),
        );
        Profile::by_name(name).expect("profile")
    }
    pub fn devkit() -> bool {
        profile().asic_count == 0 && profile().name != "undetected"
    }
    pub fn overheat() -> bool {
        false
    }
    pub fn shutdown() -> bool {
        false
    }
}

mod miner {
    pub struct MinerState {
        pub hashrate_ghs: f64,
        pub frequency_hz: u32,
        pub best_session_diff: u64,
        pub last_block: Option<(u64, u64)>,
        pub clock_step: Option<u8>,
        pub chips: u32,
    }
    pub fn state() -> MinerState {
        let g = crate::board::gamma();
        let nos = crate::board::profile().name == "nerdnos";
        MinerState {
            hashrate_ghs: if nos {
                148.5
            } else if g {
                1312.4
            } else {
                6524.8
            },
            frequency_hz: if nos {
                300_000_000
            } else if g {
                640_000_000
            } else {
                800_000_000
            },
            best_session_diff: 42_900_000,
            last_block: None,
            clock_step: None,
            chips: crate::board::profile().asic_count.into(),
        }
    }
}

mod net {
    pub struct NetState {
        pub wifi_ip: Option<String>,
        pub usb_ip: Option<String>,
        pub wifi_rssi: Option<i32>,
    }
    pub fn state() -> NetState {
        let ip = if crate::board::gamma() {
            "192.168.178.42"
        } else {
            "192.168.178.41"
        };
        NetState {
            wifi_ip: Some(ip.into()),
            usb_ip: None,
            wifi_rssi: Some(-66),
        }
    }
    pub fn hotspot() -> Option<(String, String)> {
        crate::HOTSPOT
            .load(std::sync::atomic::Ordering::Relaxed)
            .then(|| ("WarpAxe-6C98".into(), "4829175036".into()))
    }
}

mod stratum {
    pub struct StratumState {
        pub pool: String,
        pub tls: bool,
        pub using_fallback: bool,
        pub authorized: bool,
        pub shares_accepted: u64,
        pub shares_rejected: u64,
        pub difficulty: f64,
        pub block_height: u64,
        pub network_difficulty: f64,
        pub scriptsig: String,
        pub sv2: bool,
    }
    pub fn state() -> StratumState {
        StratumState {
            // Gamma on V1 with TLS, the Nerd boards on Sv2, as the test miners run.
            pool: if crate::board::gamma() {
                "pool.warppool.org:3334"
            } else {
                "pool.warppool.org:3336"
            }
            .into(),
            tls: crate::board::gamma(),
            using_fallback: false,
            authorized: true,
            shares_accepted: if crate::stress() { 999_999 } else { 1234 },
            shares_rejected: if crate::stress() { 999 } else { 0 },
            difficulty: if crate::stress() {
                999_990_000.0
            } else {
                21_780.0
            },
            block_height: if crate::stress() { 9_999_999 } else { 968_857 },
            network_difficulty: 132.76e12,
            scriptsig: "dvbprojekt-WarpPool".into(),
            sv2: !crate::board::gamma(),
        }
    }
}

fn save(dir: &str, name: &str) {
    let f = FRAME.lock().unwrap().clone();
    std::fs::write(format!("{dir}/{name}.raw"), f).expect("write frame");
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&dir).expect("out dir");

    // LCD: five pages, then the special screens.
    let mut d = display::Display::Lcd(display::Lcd {
        fb: vec![0; 320 * 170 * 2],
    });
    if stress() {
        UPTIME.store(12 * 86_400 + 23 * 3600 + 59 * 60, Ordering::Relaxed);
    }
    let mut ui = screens::Ui::new();
    // The configured pages (PREVIEW_PAGES), the start page first.
    let names = warpaxe_displaycfg::pages(&config::get().display_pages);
    let start = warpaxe_displaycfg::start_index(&names, &config::get().display_start_page);
    for k in 0..names.len() {
        let i = (start + k) % names.len();
        ui.frame(&mut d);
        save(&dir, &format!("lcd-{i}-{}", names[i]));
        ui.key(screens::Key::Next);
    }
    // PREVIEW_COCKPIT_ONLY=1: the cockpit alone (the combination runs).
    if std::env::var("PREVIEW_COCKPIT_ONLY").is_ok() {
        println!("frames in {dir}");
        return;
    }
    for ev in ["overheat", "off", "ota"] {
        *display::DEMO.lock().unwrap() = Some((ev.into(), u64::MAX));
        ui.frame(&mut d);
        save(&dir, &format!("lcd-ev-{ev}"));
    }
    *display::DEMO.lock().unwrap() = Some(("block".into(), u64::MAX));
    let start = std::time::Instant::now();
    let mut shot = 0;
    for at in [0.6f32, 1.9, 3.0] {
        while start.elapsed().as_secs_f32() < at {
            ui.frame(&mut d);
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        ui.frame(&mut d);
        save(&dir, &format!("lcd-ev-block-{shot}"));
        shot += 1;
    }
    *display::DEMO.lock().unwrap() = None;
    HOTSPOT.store(true, Ordering::Relaxed);
    ui.frame(&mut d);
    save(&dir, "lcd-ev-setup");
    HOTSPOT.store(false, Ordering::Relaxed);

    // OLED: every slide of the rotation.
    GAMMA.store(true, Ordering::Relaxed);
    let mut o = display::Display::Oled(display::Oled { fb: [0; 512] });
    let mut ui = screens::Ui::new();
    let slides = warpaxe_displaycfg::oled_slides(&config::get().oled_slides);
    for (i, name) in slides.iter().enumerate() {
        ui.frame(&mut o);
        save(&dir, &format!("oled-{i}-{name}"));
        ui.key(screens::Key::Home);
    }
    *display::DEMO.lock().unwrap() = Some(("overheat".into(), u64::MAX));
    ui.frame(&mut o);
    save(&dir, "oled-ev-overheat");
    *display::DEMO.lock().unwrap() = None;
    HOTSPOT.store(true, Ordering::Relaxed);
    let start = std::time::Instant::now();
    for (page, at) in [0.2f32, 3.2, 6.2].into_iter().enumerate() {
        while start.elapsed().as_secs_f32() < at {
            ui.frame(&mut o);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        ui.frame(&mut o);
        save(&dir, &format!("oled-ev-setup-{page}"));
    }
    println!("frames in {dir}");
}
