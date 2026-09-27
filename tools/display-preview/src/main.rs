//! Host preview of the device display: the firmware's own `screens.rs`
//! and `gfx` draw into a frame buffer, fed with sample values; every page
//! and special screen is written as a raw frame (RGB565 big-endian
//! 320x170, or SSD1306 pages 128x32) for tools/frame2png.py.

#![allow(dead_code)]

#[path = "../../../firmware/warpaxe/src/gfx/mod.rs"]
mod gfx;
#[path = "../../../firmware/warpaxe/src/screens.rs"]
mod screens;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static GAMMA: AtomicBool = AtomicBool::new(false);
static FRAME: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
static UPTIME: AtomicU64 = AtomicU64::new(11_535);

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
            crate::gfx::Canvas { fb: &mut self.fb, w: WIDTH as i32, h: HEIGHT as i32 }
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
                if on { self.fb[i] |= b } else { self.fb[i] &= !b }
            }
        }
        pub fn bitmap(&mut self, bits: &[u8]) {
            for y in 0..32 {
                for x in 0..128 {
                    let on = bits.get(y * 16 + x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
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
                                    self.pixel((cx + col * scale + dx) as i32, (y + row * scale + dy) as i32, true);
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
    pub fn recent_ghs(n: usize) -> Vec<f32> {
        let base = if crate::GAMMA.load(std::sync::atomic::Ordering::Relaxed) { 1310.0 } else { 6520.0 };
        (0..n.min(360))
            .map(|i| {
                let t = i as f32;
                base * (1.0 + 0.012 * (t / 17.0).sin() + 0.006 * (t / 5.3).cos())
            })
            .collect()
    }
}

mod market {
    #[derive(Clone)]
    pub struct Market {
        pub price: Option<(String, f64)>,
        pub fees: Option<(f64, f64, f64)>,
    }
    pub fn get() -> Option<Market> {
        Some(Market { price: Some(("EUR".into(), 74_245.0)), fees: Some((2.0, 1.0, 1.0)) })
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
    }
    pub fn get() -> Config {
        Config {
            display_language: "de".into(),
            pid_target_temp: 58,
            overheat_temp: 70,
            flipscreen: false,
            invertscreen: false,
            core_voltage: 1250,
            autofanspeed: true,
            fanspeed: 100,
            stratum_user: "bc1qtgmlr7cw7wkflaxp28zjzxyfrnvxq657vfeqz0.NerdQaxe++".into(),
            fallback_user: String::new(),
            ssid: "Heimnetz".into(),
        }
    }
    pub fn best_diff() -> u64 {
        11_200_000
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
    }
    pub fn gamma() -> bool {
        crate::GAMMA.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn state() -> BoardState {
        if gamma() {
            BoardState { temp_c: Some(62.3), vr_temp_c: Some(55), vin_mv: Some(5120.0), vout_mv: Some(1140.0), iout_ma: Some(20500.0), power_w: Some(23.4), fan_pct: 100, fan_rpm: 5020, fan2_pct: None, fan2_rpm: None }
        } else {
            BoardState { temp_c: Some(56.2), vr_temp_c: Some(50), vin_mv: Some(12050.0), vout_mv: Some(1238.0), iout_ma: Some(9140.0), power_w: Some(109.5), fan_pct: 59, fan_rpm: 1831, fan2_pct: Some(25), fan2_rpm: Some(0) }
        }
    }
    pub fn profile() -> &'static Profile {
        let name = if gamma() { "bitaxe-gamma" } else { "nerdqaxe-plusplus" };
        Profile::by_name(name).expect("profile")
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
    }
    pub fn state() -> MinerState {
        let g = crate::board::gamma();
        MinerState {
            hashrate_ghs: if g { 1312.4 } else { 6524.8 },
            frequency_hz: if g { 640_000_000 } else { 800_000_000 },
            best_session_diff: 42_900_000,
            last_block: None,
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
        let ip = if crate::board::gamma() { "192.168.178.42" } else { "192.168.178.41" };
        NetState { wifi_ip: Some(ip.into()), usb_ip: None, wifi_rssi: Some(-66) }
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
    }
    pub fn state() -> StratumState {
        StratumState {
            pool: "pool.warppool.org:3334".into(),
            tls: !crate::board::gamma(),
            using_fallback: false,
            authorized: true,
            shares_accepted: 1234,
            shares_rejected: 0,
            difficulty: 21_780.0,
            block_height: 968_857,
            network_difficulty: 132.76e12,
            scriptsig: "dvbprojekt-WarpPool".into(),
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
    let mut d = display::Display::Lcd(display::Lcd { fb: vec![0; 320 * 170 * 2] });
    let mut ui = screens::Ui::new();
    let names = ["cockpit", "bitcoin", "device", "pool", "log"];
    for (i, n) in names.iter().enumerate() {
        ui.frame(&mut d);
        save(&dir, &format!("lcd-{i}-{n}"));
        ui.key(screens::Key::Next);
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

    // OLED: every slide of the rotation.
    GAMMA.store(true, Ordering::Relaxed);
    let mut o = display::Display::Oled(display::Oled { fb: [0; 512] });
    let mut ui = screens::Ui::new();
    for i in 0..9 {
        ui.frame(&mut o);
        save(&dir, &format!("oled-{i}"));
        ui.key(screens::Key::Home);
    }
    println!("frames in {dir}");
}
