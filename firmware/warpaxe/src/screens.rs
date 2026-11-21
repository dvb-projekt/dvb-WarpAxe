//! What the device display shows after the splash.
//!
//! LCD (Nerd boards, 320×170): the cockpit, and four more pages a press of
//! the page key (GPIO14) steps through: Bitcoin, device, pool, log. The
//! boot key (GPIO0) goes back to the start page; a minute without a key
//! press does too. OLED (Bitaxe, 128×32): the hashrate large on the left,
//! a rotating value on the right, and three full-width info slides per
//! round; the boot key steps on.
//!
//! Which pages the key steps through (and which is the start page), what
//! the cockpit's places show and which OLED slides rotate come from the
//! config, by the names in `warpaxe_displaycfg`; the layouts themselves
//! are fixed here. The factory settings are the screens of v0.1.30.
//!
//! Special screens cover every page while their state lasts: firmware
//! update, block found (animated; a key press dismisses it), overheat,
//! chips off.

use std::time::{Duration, Instant};

use crate::display::{self, Display, Lcd, Oled};
use crate::gfx::fonts::*;
use crate::gfx::{self, mix, Align, Canvas};
use crate::{board, clock, config, diag, history, market, miner, net, stratum};
use warpaxe_displaycfg as dcfg;

const W: i32 = display::WIDTH as i32;
const H: i32 = display::HEIGHT as i32;
/// Back to the cockpit after this long without a key press.
const HOME_AFTER_S: u64 = 60;
/// A found block stays on screen this long unless a key dismisses it.
const BLOCK_SHOW_S: u64 = 600;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Event {
    Ota(Option<u8>),
    Block(u64),
    Overheat,
    /// The chips are off: switched off by hand, or (`no_pool`) because
    /// no pool answers.
    ChipsOff {
        no_pool: bool,
    },
    /// The setup hotspot is up: how to join it.
    Setup,
    /// A self-test running, or its result for a while after (run id).
    SelfTest(u32),
}

#[derive(Clone, Copy)]
struct Star {
    angle: f32,
    dist: f32,
    speed: f32,
    color: u16,
}

pub struct Ui {
    /// Index into the configured page list (`pages`); `usize::MAX`
    /// until the first frame, which puts it on the start page.
    page: usize,
    last_key_s: u64,
    base: Vec<u8>,
    event: Option<Event>,
    event_t0: Instant,
    acked_block: Option<u64>,
    /// A self-test screen dismissed by a key: (run id, it was finished).
    acked_selftest: Option<(u32, bool)>,
    stars: Vec<Star>,
    rng: u32,
    oled_i: usize,
    oled_t: Instant,
    last_acc: u64,
    blink: u8,
    /// Night mode, dimmed (set by the display thread): the clock screen
    /// instead of the pages, and a key press only wakes the full display.
    pub night: bool,
}

/// Key presses from the display thread.
pub enum Key {
    /// GPIO14 on the Nerd boards.
    Next,
    /// GPIO0 (boot key).
    Home,
}

/// The LCD pages as the key steps through them, and the start page's
/// index among them, from a config read by the caller (once per frame).
fn pages_of(cfg: &config::Config) -> (Vec<&'static str>, usize) {
    let p = dcfg::pages(&cfg.display_pages);
    let start = dcfg::start_index(&p, &cfg.display_start_page);
    (p, start)
}

/// The OLED slides that rotate, and how long each stays.
fn oled_rotation_of(cfg: &config::Config) -> (Vec<&'static str>, Duration) {
    let step = u64::from(dcfg::oled_interval_s(cfg.oled_interval));
    (
        dcfg::oled_slides(&cfg.oled_slides),
        Duration::from_secs(step),
    )
}

/// The display language; Japanese and Chinese fall back to English (the
/// display fonts carry Latin script only).
fn lang() -> &'static str {
    match config::get().display_language.as_str() {
        "de" => "de",
        "es" => "es",
        "fr" => "fr",
        "it" => "it",
        "pt" => "pt",
        _ => "en",
    }
}

/// A display text in the display language: English and German inline,
/// the others from `display_i18n` (English where a text is missing).
fn l(en: &'static str, de: &'static str) -> &'static str {
    match lang() {
        "en" => en,
        "de" => de,
        other => crate::display_i18n::lookup(other, en).unwrap_or(en),
    }
}

/// For the 5x7 OLED font: capitals, accents dropped (German ones spelt
/// out as AE/OE/UE/SS).
fn oled_str(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let rep = match c {
            'ä' | 'Ä' => "AE",
            'ö' | 'Ö' => "OE",
            'ü' | 'Ü' => "UE",
            'ß' => "SS",
            'à' | 'á' | 'â' | 'ã' | 'À' | 'Á' | 'Â' | 'Ã' => "A",
            'ç' | 'Ç' => "C",
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => "E",
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => "I",
            'ñ' | 'Ñ' => "N",
            'ò' | 'ó' | 'ô' | 'õ' | 'Ò' | 'Ó' | 'Ô' | 'Õ' => "O",
            'ù' | 'ú' | 'û' | 'Ù' | 'Ú' | 'Û' => "U",
            _ => {
                out.push(c.to_ascii_uppercase());
                continue;
            }
        };
        out.push_str(rep);
    }
    out
}

/// A number with `d` decimals, in the display language's style.
fn num(v: f64, d: usize) -> String {
    let s = format!("{v:.d$}");
    let (int, frac) = s.split_once('.').map_or((s.as_str(), ""), |(a, b)| (a, b));
    let (neg, int) = int.strip_prefix('-').map_or((false, int), |i| (true, i));
    let (sep, dec) = match lang() {
        "en" => (',', '.'),
        "fr" => (' ', ','),
        _ => ('.', ','),
    };
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(sep);
        }
        grouped.push(c);
    }
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push(dec);
        out.push_str(frac);
    }
    out
}

/// 42.9M, 4.29G, 132.76T style.
fn si(v: f64) -> String {
    let (n, u) = si_parts(v);
    format!("{n}{u}")
}

/// The number and the prefix of [`si`] apart: ("42.9", "M"), for a
/// value face that has digits only (VAL16) with the letter in the unit
/// face beside it.
fn si_parts(v: f64) -> (String, &'static str) {
    let units = ["", "K", "M", "G", "T", "P", "E"];
    let mut v = v.max(0.0);
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let d = if v >= 10.0 { 1 } else { 2 };
    (num(v, if i == 0 { 0 } else { d }), units[i])
}

/// J/TH from the board's power and the ten-minute rate; `None` without
/// a power reading or before the first estimate.
fn eff_jth(m: &miner::MinerState, b: &board::BoardState) -> Option<f64> {
    let w = b.power_w?;
    (m.hashrate_ghs > 1.0).then(|| f64::from(w) / (m.hashrate_ghs / 1000.0))
}

/// "12 d 23 h" / "3 h 12 min", the day letter in the display language
/// (j in French, g in Italian, T in German).
fn uptime(s: u64) -> String {
    let (d, h, m) = (s / 86_400, s / 3600 % 24, s / 60 % 60);
    if d > 0 {
        format!("{d} {} {h} h", l("d", "T"))
    } else {
        format!("{h} h {m} min")
    }
}

fn short_worker(user: &str) -> String {
    match user.split_once('.') {
        Some((addr, name)) if addr.len() > 12 => {
            format!("{}…{}.{}", &addr[..4], &addr[addr.len() - 6..], name)
        }
        _ => user.to_string(),
    }
}

/// (value, unit) for a hashrate in GH/s.
fn hashrate(ghs: f64) -> (String, &'static str) {
    // No estimate yet (the first 30 s after a start): a dash, not a 0
    // that reads like a dead chain.
    if ghs <= 0.0 {
        return ("–".into(), "TH/s");
    }
    if ghs >= 1000.0 {
        (num(ghs / 1000.0, 2), "TH/s")
    } else if board::devkit() && ghs < 0.001 {
        (num(ghs * 1e6, 2), "kH/s")
    } else if board::devkit() && ghs < 1.0 {
        (num(ghs * 1e3, 2), "MH/s")
    } else {
        // Two decimals like the kH/s of the devkit: a single BM1397 moves in
        // the second place, not the hundreds.
        (num(ghs, 2), "GH/s")
    }
}

fn limits() -> (f32, f32) {
    let cfg = config::get();
    let p = board::profile();
    let target = if cfg.pid_target_temp > 0 {
        f32::from(cfg.pid_target_temp)
    } else {
        p.fan_target_c
    };
    let hot = p.chip_limit_c(cfg.overheat_temp);
    (target, hot)
}

fn temp_color(t: f32) -> u16 {
    let (target, hot) = limits();
    if t >= hot - 2.0 {
        gfx::ERR
    } else if t >= target + 5.0 {
        gfx::WARN
    } else {
        gfx::OK
    }
}

fn rssi_color(r: i32) -> u16 {
    if r >= -67 {
        gfx::OK
    } else if r >= -75 {
        gfx::WARN
    } else {
        gfx::ERR
    }
}

/// The Ethernet link as the display words it: "100 Mbit/s" (or "10"),
/// `none` before the speed is known (the LCD's dash is not in the OLED
/// font, which draws it as "?").
fn eth_speed(n: &net::NetState, none: &str) -> String {
    n.eth_speed_mbit
        .map_or(none.into(), |s| format!("{s} Mbit/s"))
}

fn family_color() -> u16 {
    if board::profile().name.starts_with("bitaxe") {
        gfx::BTC
    } else {
        gfx::PLASMA
    }
}

impl Ui {
    pub fn new() -> Self {
        Ui {
            page: usize::MAX,
            last_key_s: 0,
            base: Vec::new(),
            event: None,
            event_t0: Instant::now(),
            acked_block: None,
            acked_selftest: None,
            stars: Vec::new(),
            rng: 0x9E37_79B9,
            oled_i: 0,
            oled_t: Instant::now(),
            last_acc: 0,
            blink: 0,
            night: false,
        }
    }

    fn rand(&mut self) -> f32 {
        // xorshift32, enough for star positions.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    pub fn key(&mut self, k: Key) {
        self.last_key_s = diag::uptime_s();
        // At night the first press brings the full display back; it
        // does not step a page as well.
        if self.night {
            return;
        }
        if let Some(Event::Block(h)) = self.event {
            self.acked_block = Some(h);
            *display::DEMO.lock().unwrap() = None;
            return;
        }
        if let Some(Event::SelfTest(id)) = self.event {
            let done = crate::selftest::shown().is_some_and(|s| !s.running);
            self.acked_selftest = Some((id, done));
            *display::DEMO.lock().unwrap() = None;
            return;
        }
        let cfg = config::get();
        match k {
            Key::Next => {
                let (p, home) = pages_of(&cfg);
                self.page = if self.page >= p.len() {
                    home
                } else {
                    (self.page + 1) % p.len()
                };
            }
            Key::Home => {
                self.page = pages_of(&cfg).1;
                let (slides, _) = oled_rotation_of(&cfg);
                self.oled_i = (self.oled_i + 1) % slides.len();
                self.oled_t = Instant::now();
            }
        }
    }

    /// An alert is up that night mode must not keep dark or dim: a
    /// found block, an overheat stop, a firmware update, a self-test.
    pub fn alert(&self) -> bool {
        matches!(
            self.current_event(),
            Some(Event::Block(_) | Event::Overheat | Event::Ota(_) | Event::SelfTest(_))
        )
    }

    fn current_event(&self) -> Option<Event> {
        let now = diag::uptime_s();
        if let Some((ev, until)) = display::DEMO.lock().unwrap().clone() {
            if now < until {
                return Some(match ev.as_str() {
                    "block" => Event::Block(stratum::state().block_height.max(1)),
                    "overheat" => Event::Overheat,
                    "off" => Event::ChipsOff { no_pool: false },
                    "nopool" => Event::ChipsOff { no_pool: true },
                    _ => Event::Ota(Some(((now % 20) * 5) as u8)),
                });
            }
        }
        match display::OTA_STATE.load(std::sync::atomic::Ordering::Relaxed) {
            0 => {}
            1 => return Some(Event::Ota(None)),
            v => return Some(Event::Ota(Some(v - 2))),
        }
        if let Some((height, at)) = miner::state().last_block {
            if now.saturating_sub(at) < BLOCK_SHOW_S && self.acked_block != Some(height) {
                return Some(Event::Block(height));
            }
        }
        if board::overheat() {
            return Some(Event::Overheat);
        }
        if board::shutdown() {
            return Some(Event::ChipsOff { no_pool: false });
        }
        if let Some(s) = crate::selftest::shown() {
            // Dismissed while running: the result still shows once it is in.
            let acked = self
                .acked_selftest
                .is_some_and(|(id, done)| id == s.id && (done || s.running));
            if !acked {
                return Some(Event::SelfTest(s.id));
            }
        }
        if net::hotspot().is_some() {
            return Some(Event::Setup);
        }
        // After the setup screen: a miner without Wi-Fi has no pool
        // either, and the hotspot's name and password are shown nowhere
        // else.
        if board::no_pool() {
            return Some(Event::ChipsOff { no_pool: true });
        }
        None
    }

    /// Draw one frame; returns how long until the next one.
    pub fn frame(&mut self, d: &mut Display) -> Duration {
        let ev = self.current_event();
        if ev != self.event {
            if matches!(ev, Some(Event::Block(_))) && !matches!(self.event, Some(Event::Block(_))) {
                self.stars.clear();
                display::touch();
            }
            if ev.is_some() {
                display::touch();
            }
            self.event = ev;
            self.event_t0 = Instant::now();
        }
        // The config once per frame; the pages and the OLED rotation come
        // from this copy.
        let cfg = config::get();
        let (_, home) = pages_of(&cfg);
        if self.page != home && diag::uptime_s().saturating_sub(self.last_key_s) > HOME_AFTER_S {
            self.page = home;
        }
        match d {
            Display::Lcd(l) => self.lcd(l, &cfg),
            Display::Oled(o) => self.oled(o, &cfg),
        }
    }

    // ---------------------------------------------------------------- LCD

    fn lcd(&mut self, l: &mut Lcd, cfg: &config::Config) -> Duration {
        l.orient(
            cfg.flipscreen ^ crate::board::profile().display_flip,
            cfg.invertscreen,
        );
        if self.base.is_empty() {
            self.base = background();
        }
        let t = self.event_t0.elapsed().as_secs_f32();
        let next = match self.event {
            Some(Event::Block(h)) => {
                self.block_found(&mut l.canvas(), h, t);
                Duration::from_millis(40)
            }
            Some(ev) => {
                l.blit(&self.base);
                event_screen(&mut l.canvas(), ev, t);
                Duration::from_millis(80)
            }
            None if self.night => {
                night_screen(&mut l.canvas());
                Duration::from_millis(1000)
            }
            None => {
                l.blit(&self.base);
                let mut c = l.canvas();
                // The list may have shrunk under a PATCH: back to the start page.
                let (p, home) = pages_of(cfg);
                if self.page >= p.len() {
                    self.page = home.min(p.len() - 1);
                }
                let (pos, n) = (self.page, p.len());
                match p[pos] {
                    "bitcoin" => bitcoin(&mut c, pos, n),
                    "device" => device(&mut c, pos, n),
                    "pool" => pool(&mut c, pos, n),
                    "log" => log_page(&mut c, pos, n),
                    "clock" => clock_page(&mut c, pos, n),
                    _ => cockpit(&mut c, pos, n),
                }
                Duration::from_millis(500)
            }
        };
        if let Err(e) = l.flush() {
            log::warn!("display: {e}");
        }
        next
    }

    /// Warp jump: stars streak out of the centre, a flash, then the news.
    fn block_found(&mut self, c: &mut Canvas, height: u64, t: f32) {
        c.fill(gfx::VOID);
        let (cx, cy) = (W as f32 / 2.0, H as f32 / 2.0);
        if self.stars.is_empty() {
            for _ in 0..90 {
                let s = self.new_star(true);
                self.stars.push(s);
            }
        }
        // Accelerate for two seconds, then cruise.
        let warp = if t < 2.0 { 0.3 + t * t } else { 4.3 };
        let reach = (W * W + H * H) as f32;
        for i in 0..self.stars.len() {
            let s = self.stars[i];
            let len = (s.dist * 0.08 * warp).clamp(1.0, 60.0);
            let (dx, dy) = (s.angle.cos(), s.angle.sin());
            let alpha = (s.dist / 120.0).clamp(0.25, 1.0);
            c.line(
                cx + dx * s.dist,
                cy + dy * s.dist * 0.6,
                cx + dx * (s.dist + len),
                cy + dy * (s.dist + len) * 0.6,
                s.color,
                (alpha * 255.0) as u8,
            );
            let mut s = s;
            s.dist += s.speed * warp * (1.0 + s.dist / 60.0);
            let far = (dx * s.dist).powi(2) + (dy * s.dist * 0.6).powi(2) > reach / 4.0;
            self.stars[i] = if far { self.new_star(false) } else { s };
        }
        // Shock rings every 1.2 s once the jump is done.
        if t > 1.6 {
            let phase = (t - 1.6) % 1.2;
            let r = phase * 160.0;
            ring_fast(c, cx, cy, r, gfx::BTC, (200.0 * (1.0 - phase / 1.2)) as u8);
        }
        // White flash at the jump.
        if (1.6..2.2).contains(&t) {
            let a = 1.0 - (t - 1.6) / 0.6;
            c.rect(0, 0, W, H, gfx::WHITE, (a * 230.0) as u8);
        }
        if t > 1.7 {
            let a = ((t - 1.7) / 0.6).clamp(0.0, 1.0);
            let pulse = 0.5 + 0.5 * (t * 3.0).sin();
            let title_col = mix(gfx::rgb(0xFF, 0xD2, 0x7A), gfx::BTC, (pulse * 255.0) as u8);
            c.rect(0, 52, W, 72, gfx::VOID, (a * 170.0) as u8);
            let title = l("BLOCK FOUND", "BLOCK GEFUNDEN");
            fade_text(c, &TITLE, W / 2, 86, title_col, title, a);
            let sub = format!("{} {}", l("Block", "Block"), num(height as f64, 0));
            fade_text(c, &VAL12, W / 2, 106, gfx::FG, &sub, a);
            let hint = l("Press a key to close", "Zum Schließen Taste drücken");
            fade_text(c, &LABEL11, W / 2, 120, gfx::MUTE, hint, a * 0.8);
        }
    }

    fn new_star(&mut self, spread: bool) -> Star {
        let angle = self.rand() * std::f32::consts::TAU;
        let dist = if spread {
            self.rand() * 150.0
        } else {
            self.rand() * 8.0
        };
        let speed = 0.6 + self.rand() * 1.4;
        let k = self.rand();
        let color = if k < 0.55 {
            gfx::FG
        } else if k < 0.8 {
            gfx::PLASMA
        } else {
            gfx::BTC
        };
        Star {
            angle,
            dist,
            speed,
            color,
        }
    }

    // --------------------------------------------------------------- OLED

    fn oled(&mut self, o: &mut Oled, cfg: &config::Config) -> Duration {
        o.orient(
            cfg.flipscreen ^ crate::board::profile().display_flip,
            cfg.invertscreen,
        );
        o.clear();
        let t = self.event_t0.elapsed().as_secs_f32();
        let next = match self.event {
            Some(ev) => {
                oled_event(o, ev, t, &mut self.rng);
                Duration::from_millis(100)
            }
            None if self.night => {
                oled_slide(o, "clock");
                Duration::from_millis(1000)
            }
            None => {
                let (slides, step) = oled_rotation_of(cfg);
                if self.oled_t.elapsed() >= step {
                    self.oled_i = (self.oled_i + 1) % slides.len();
                    self.oled_t = Instant::now();
                }
                if self.oled_i >= slides.len() {
                    self.oled_i = 0;
                }
                let acc = stratum::state().shares_accepted;
                if acc != self.last_acc {
                    self.blink = if self.last_acc == 0 { 0 } else { 1 };
                    self.last_acc = acc;
                } else if self.blink > 0 {
                    self.blink -= 1;
                }
                oled_slide(o, slides[self.oled_i]);
                if self.blink > 0 {
                    // Inside the burn-in orbit's safe box (x 1..126, y 1..29).
                    for y in 1..4 {
                        for x in 124..127 {
                            o.pixel(x, y, true);
                        }
                    }
                }
                Duration::from_millis(500)
            }
        };
        if let Err(e) = o.flush() {
            log::warn!("display: {e}");
        }
        next
    }
}

// ------------------------------------------------------------- LCD pages

fn background() -> Vec<u8> {
    let mut v = vec![0u8; (W * H * 2) as usize];
    let mut c = Canvas {
        fb: &mut v,
        w: W,
        h: H,
    };
    // The web UI's ground: void with a faint violet glow top right,
    // dithered (4×4 Bayer) so RGB565 shows no steps.
    const BAYER: [[i32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    for y in 0..H {
        for x in 0..W {
            let dx = (x - 250) as f32 / 260.0;
            let dy = (y + 40) as f32 / 190.0;
            let glow = (1.0 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            let g2 = glow * glow;
            let r = 5.0 + 20.0 * g2;
            let gr = 6.0 + 14.0 * g2;
            let b = 11.0 + 42.0 * g2;
            let d = (BAYER[(y & 3) as usize][(x & 3) as usize] as f32 / 16.0 - 0.5) * 8.0;
            let q = |v: f32, step: f32| ((v + d * step / 8.0).clamp(0.0, 255.0)) as u8;
            c.set(x, y, gfx::rgb(q(r, 8.0), q(gr, 4.0), q(b, 8.0)));
        }
    }
    c.rect(0, 0, 3, H, family_color(), 255);
    v
}

/// Text centred at `x` in the first font of `fonts` it fits in (`max`
/// px): long translations step down a size instead of running off the
/// screen.
fn fit_text(
    c: &mut Canvas,
    fonts: &[&gfx::font::Font],
    x: i32,
    y: i32,
    col: u16,
    s: &str,
    max: i32,
) {
    let f = fonts
        .iter()
        .find(|f| f.width(s) <= max)
        .unwrap_or(&fonts[fonts.len() - 1]);
    c.text(f, x, y, col, s, Align::Center);
}

fn fade_text(c: &mut Canvas, f: &gfx::font::Font, x: i32, y: i32, col: u16, s: &str, a: f32) {
    let w = f.width(s);
    let a = a.clamp(0.0, 1.0);
    f.draw(x - w / 2, y, s, |px, py, cov| {
        c.blend(px, py, col, (f32::from(cov) * 17.0 * a) as u8)
    });
}

/// A ring drawn by walking its circumference (cheap enough per frame).
fn ring_fast(c: &mut Canvas, cx: f32, cy: f32, r: f32, col: u16, alpha: u8) {
    if r < 1.0 {
        return;
    }
    let steps = (r * std::f32::consts::TAU * 1.5) as i32;
    for i in 0..steps {
        let a = i as f32 / steps as f32 * std::f32::consts::TAU;
        let (x, y) = (cx + r * a.cos(), cy + r * a.sin() * 0.6);
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        let al = f32::from(alpha);
        c.blend(ix, iy, col, (al * (1.0 - fx) * (1.0 - fy)) as u8);
        c.blend(ix + 1, iy, col, (al * fx * (1.0 - fy)) as u8);
        c.blend(ix, iy + 1, col, (al * (1.0 - fx) * fy) as u8);
        c.blend(ix + 1, iy + 1, col, (al * fx * fy) as u8);
    }
}

fn clock_text() -> Option<String> {
    clock::local_hm().map(|(h, m)| format!("{h:02}:{m:02}"))
}

static BADGE: &[u8] = include_bytes!("splash/badge_66x20.rgb565a");
/// The OLED's wordmark: the boot splash (main.rs) and the "logo" slide.
pub static SPLASH_OLED: &[u8] = include_bytes!("splash/oled_128x32.bits");

/// The wordmark top left of every page, the page title after it.
fn badge(c: &mut Canvas) {
    c.sprite(9, 2, 66, 20, BADGE);
}

fn page_head(c: &mut Canvas, title: &str, pos: usize, total: usize) {
    badge(c);
    c.text(
        &LABEL12,
        82,
        17,
        gfx::PLASMA,
        &title.to_uppercase(),
        Align::Left,
    );
    if let Some(t) = clock_text() {
        c.text(&MONO11, 252, 18, gfx::MUTE, &t, Align::Right);
    }
    dots(c, 305, 11, pos, total);
    c.rect(12, 25, 298, 1, gfx::WHITE, 16);
}

/// One dot per active page, the current one lit, the last ending at
/// `x_end`; none when there is only one page.
fn dots(c: &mut Canvas, x_end: i32, y: i32, pos: usize, total: usize) {
    if total < 2 {
        return;
    }
    let x0 = x_end - 5 - (total as i32 - 1) * 9;
    for i in 0..total {
        let (col, a) = if i == pos {
            (gfx::PLASMA, 255)
        } else {
            (gfx::WHITE, 40)
        };
        c.rect(x0 + i as i32 * 9, y, 5, 5, col, a);
    }
}

fn kv(c: &mut Canvas, label: &str, val: &str, y: i32, col: u16, x0: i32, x1: i32) {
    kv_in(c, label, val, y, col, x0, x1, &VAL12);
}

/// Label left, value right in `x0..x1`; where a long translation would
/// make them touch, the label and then the value step down a size.
#[allow(clippy::too_many_arguments)]
fn kv_in(
    c: &mut Canvas,
    label: &str,
    val: &str,
    y: i32,
    col: u16,
    x0: i32,
    x1: i32,
    val_font: &gfx::font::Font,
) {
    let room = x1 - x0 - 6;
    let mut lf: &gfx::font::Font = &LABEL11;
    let mut vf: &gfx::font::Font = val_font;
    if lf.width(label) + vf.width(val) > room {
        lf = &LABEL10;
    }
    if lf.width(label) + vf.width(val) > room {
        vf = &MONO9;
    }
    c.text(lf, x0, y, gfx::MUTE, label, Align::Left);
    c.text(vf, x1, y, col, val, Align::Right);
}

fn lock_icon(c: &mut Canvas, x: i32, y: i32, col: u16) {
    c.ring(x as f32 + 3.5, y as f32 - 5.0, 2.4, 1.2, col, 255);
    c.rect(x, y - 5, 7, 6, col, 255);
}

fn cockpit(c: &mut Canvas, pos: usize, total: usize) {
    let cfg = config::get();
    let m = miner::state();
    let st = stratum::state();
    let b = board::state();
    let n = net::state();
    let p = board::profile();
    badge(c);
    c.text(
        &LABEL12,
        82,
        16,
        family_color(),
        &p.model.to_uppercase(),
        Align::Left,
    );
    if let Some(t) = clock_text() {
        c.text(&MONO11, 262, 16, gfx::MUTE, &t, Align::Right);
    }

    // The large number, and under it its last hour (10 s samples): the
    // curve shows the same quantity as the number above it, where the
    // history has it (power and efficiency are sampled with the rate).
    // An estimated power (no reading on the board) is marked with a tilde,
    // on the power and the efficiency; the hashrate is measured.
    let big = dcfg::big(&cfg.cockpit_big);
    let est = if b.power_estimated && big != "hashrate" {
        "~"
    } else {
        ""
    };
    let (label, value, unit, hist): (&str, String, &str, Vec<f32>) = match big {
        "power" => (
            l("POWER", "LEISTUNG"),
            b.power_w.map_or("–".into(), |w| num(f64::from(w), 1)),
            "W",
            history::recent_power_w(360),
        ),
        "efficiency" => (
            l("EFFICIENCY", "EFFIZIENZ"),
            eff_jth(&m, &b).map_or("–".into(), |e| num(e, 1)),
            "J/TH",
            history::recent_eff_jth(360),
        ),
        _ => {
            let (v, u) = hashrate(m.hashrate_ghs);
            ("HASHRATE", v, u, history::recent_ghs(360))
        }
    };
    c.text(&LABEL11, 12, 34, gfx::DIM, label, Align::Left);
    let mut x = 11;
    // HERO has digits only: the estimate's tilde in the unit face.
    if !est.is_empty() && value != "–" {
        x += c.text(&UNIT13, x, 70, gfx::MUTE, "~", Align::Left) + 1;
    }
    let w = c.text(&HERO, x, 70, gfx::FG, &value, Align::Left);
    c.text(&UNIT13, x + 5 + w, 70, gfx::MUTE, unit, Align::Left);
    sparkline(c, hist);

    // Right column: three places from the config.
    let rx = 222;
    c.rect(rx - 8, 8, 1, 124, gfx::WHITE, 12);
    dots(c, 311, 10, pos, total);
    let live = Live {
        m: &m,
        st: &st,
        b: &b,
        n: &n,
        cfg: &cfg,
    };
    for (i, id) in dcfg::slots(&cfg.cockpit_slots, board::devkit())
        .iter()
        .enumerate()
    {
        slot(c, id, [28, 66, 100][i], &live);
    }
    footer(c, dcfg::footer(&cfg.cockpit_footer), &live);
}

/// The state the cockpit's places and footer draw from.
struct Live<'a> {
    m: &'a miner::MinerState,
    st: &'a stratum::StratumState,
    b: &'a board::BoardState,
    n: &'a net::NetState,
    cfg: &'a config::Config,
}

/// The last hour of `hist` under the large number, as a filled line
/// between the lowest and highest sample; nothing before two samples.
fn sparkline(c: &mut Canvas, mut hist: Vec<f32>) {
    // Samples before the first reading (right after boot) are zero or
    // NaN: start the line at the first real one.
    if let Some(first) = hist.iter().position(|v| v.is_finite() && *v > 0.0) {
        hist.drain(..first);
    }
    let (x0, y0, gw, gh) = (12i32, 82i32, 196i32, 44i32);
    for k in 0..=2 {
        c.rect(x0, y0 + k * gh / 2, gw, 1, gfx::WHITE, 14);
    }
    let vals: Vec<f32> = hist
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    if vals.len() >= 2 {
        let lo = vals.iter().copied().fold(f32::MAX, f32::min);
        let hi = vals.iter().copied().fold(0.0f32, f32::max);
        let pad = ((hi - lo) * 0.15).max(hi * 0.01);
        let (lo, hi) = (lo - pad, hi + pad);
        let pts: Vec<(f32, f32)> = hist
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = x0 as f32 + i as f32 * gw as f32 / (hist.len() - 1).max(1) as f32;
                let v = if v.is_finite() && *v > 0.0 { *v } else { lo };
                (x, (y0 + gh) as f32 - (v - lo) / (hi - lo) * gh as f32)
            })
            .collect();
        for w2 in pts.windows(2) {
            let (a, b2) = (w2[0], w2[1]);
            for x in a.0 as i32..=b2.0 as i32 {
                let f = if b2.0 > a.0 {
                    (x as f32 - a.0) / (b2.0 - a.0)
                } else {
                    0.0
                };
                let y = a.1 + (b2.1 - a.1) * f;
                c.vfade(x, y as i32, 1, y0 + gh - y as i32, gfx::PLASMA, 80, 0);
            }
        }
        for w2 in pts.windows(2) {
            c.line(w2[0].0, w2[0].1, w2[1].0, w2[1].1, gfx::PLASMA, 255);
        }
        if let Some(e) = pts.last() {
            c.disc(e.0, e.1, 2.2, gfx::FG, 255);
        }
    }
    c.text(&LABEL11, x0 + gw, y0 - 3, gfx::DIM, "1 h", Align::Right);
}

/// One place of the cockpit's right column at `y`: a label, the value
/// in the digit face and its unit beside it; the temperatures with a
/// bar against their limit. What the board does not have is a dash.
fn slot(c: &mut Canvas, id: &str, y: i32, v: &Live) {
    let rx = 222;
    let (m, st, b, cfg) = (v.m, v.st, v.b, v.cfg);
    let dash = || "–".to_string();
    let est = if b.power_estimated { "~" } else { "" };
    let (label, value, unit, col): (&str, String, String, u16) = match id {
        "vr" => {
            let t = b.vr_temp_c;
            let hot = board::profile().vr_stop_c;
            (
                l("REGULATOR", "REGLER"),
                t.map_or_else(dash, |t| t.to_string()),
                "°C".into(),
                t.map_or(gfx::FG, |t| {
                    if t as f32 >= hot - 5.0 {
                        gfx::ERR
                    } else {
                        gfx::FG
                    }
                }),
            )
        }
        "power" => (
            l("POWER", "LEISTUNG"),
            b.power_w
                .map_or_else(dash, |w| format!("{est}{}", num(f64::from(w), 1))),
            "W".into(),
            gfx::FG,
        ),
        "efficiency" => (
            l("EFFICIENCY", "EFFIZIENZ"),
            eff_jth(m, b).map_or_else(dash, |e| format!("{est}{}", num(e, 1))),
            "J/TH".into(),
            gfx::FG,
        ),
        "fan" => (
            l("FAN", "LÜFTER"),
            if board::profile().has_fan() {
                num(f64::from(b.fan_rpm), 0)
            } else {
                dash()
            },
            "rpm".into(),
            gfx::FG,
        ),
        "fanpct" => (
            l("FAN", "LÜFTER"),
            if board::profile().has_fan() {
                b.fan_pct.to_string()
            } else {
                dash()
            },
            "%".into(),
            gfx::FG,
        ),
        // Accepted in the value face, the rejected count after the slash
        // beside it.
        "shares" => (
            "SHARES",
            num(st.shares_accepted as f64, 0),
            format!("/{}", st.shares_rejected),
            gfx::FG,
        ),
        "bestdiff" => {
            let (n, u) = if board::devkit() {
                (num(config::best_diff_f(), 3), "")
            } else {
                si_parts(config::best_diff() as f64)
            };
            (l("BEST DIFF", "BESTE DIFF"), n, u.into(), gfx::FG)
        }
        "height" => {
            let h = st.block_height;
            (
                l("BLOCK", "BLOCK"),
                if h > 0 { num(h as f64, 0) } else { dash() },
                String::new(),
                gfx::BTC,
            )
        }
        // Only with the price line on (the miner asks mempool.space then).
        "price" => {
            let price = if cfg.btc_price {
                market::get().and_then(|mk| mk.price)
            } else {
                None
            };
            let (n, u) = price.map_or_else(
                || (dash(), String::new()),
                |(cur, p)| {
                    let sym = match cur.as_str() {
                        "EUR" => "€".to_string(),
                        "USD" => "$".to_string(),
                        other => other.to_string(),
                    };
                    (num(p, 0), sym)
                },
            );
            ("BTC", n, u, gfx::FG)
        }
        "pooldiff" => {
            let d = st.difficulty;
            let (n, u) = if d <= 0.0 {
                (dash(), "")
            } else if d < 1.0 {
                (num(d, 3), "")
            } else {
                si_parts(d)
            };
            (l("POOL DIFF", "POOL-DIFF"), n, u.into(), gfx::FG)
        }
        // "12 d 23 h" in the text face (VAL12): the digit face would need
        // "12/23:59" plus a unit, which does not fit the column.
        "uptime" => (
            l("UPTIME", "LAUFZEIT"),
            uptime(diag::uptime_s()),
            String::new(),
            gfx::FG,
        ),
        "vin" => (
            l("INPUT", "EINGANG"),
            b.vin_mv
                .map_or_else(dash, |mv| num(f64::from(mv) / 1000.0, 2)),
            "V".into(),
            gfx::FG,
        ),
        // Accepted shares per hour since boot (the chipless board's
        // yield) after the first five minutes.
        "sharesph" => {
            let up = diag::uptime_s();
            (
                "SHARES/H",
                if up >= 300 {
                    num(st.shares_accepted as f64 * 3600.0 / up as f64, 1)
                } else {
                    dash()
                },
                String::new(),
                gfx::FG,
            )
        }
        _ => {
            let t = b.temp_c;
            (
                "CHIP",
                t.map_or_else(dash, |t| num(f64::from(t), 1)),
                "°C".into(),
                t.map_or(gfx::FG, temp_color),
            )
        }
    };
    // A long translation of the label steps down a size.
    let room = 310 - rx;
    let lf: &gfx::font::Font = [&LABEL11, &LABEL10, &MONO9]
        .into_iter()
        .find(|f| f.width(label) <= room)
        .unwrap_or(&MONO9);
    c.text(lf, rx, y, gfx::DIM, label, Align::Left);
    // 88 px for value and unit: a six-figure share count loses its
    // thousands separators first, then the unit steps down a size,
    // then it goes.
    let mut value = value;
    if VAL16.width(&value) + 3 + LABEL11.width(&unit) > room && id == "shares" {
        value = value.replace([',', '.', ' '], "");
    }
    let vf: &gfx::font::Font = if id == "uptime" { &VAL12 } else { &VAL16 };
    let w = c.text(vf, rx, y + 17, col, &value, Align::Left);
    let uf: &gfx::font::Font = if w + 3 + LABEL11.width(&unit) <= room {
        &LABEL11
    } else {
        &MONO9
    };
    if w + 3 + uf.width(&unit) <= room {
        c.text(uf, rx + w + 3, y + 17, gfx::MUTE, &unit, Align::Left);
    }
    // The temperatures' bar: from 30 °C to the limit.
    let bar = match id {
        "vr" => b
            .vr_temp_c
            .map(|t| (t as f32, board::profile().vr_stop_c, col)),
        "chip" => b.temp_c.map(|t| (t, limits().1, temp_color(t))),
        _ => None,
    };
    if matches!(id, "vr" | "chip") {
        c.rect(rx, y + 22, 88, 3, gfx::WHITE, 20);
    }
    if let Some((t, hot, col)) = bar {
        let f = ((t - 30.0) / (hot - 30.0)).clamp(0.0, 1.0);
        c.rect(rx, y + 22, (88.0 * f) as i32, 3, col, 255);
    }
}

/// The status bar along the bottom: two or three of pool, shares, IP,
/// uptime and worker, by `kind` (`warpaxe_displaycfg::FOOTERS`).
fn footer(c: &mut Canvas, kind: &str, v: &Live) {
    let (st, n, cfg) = (v.st, v.n, v.cfg);
    c.rect(3, 144, W - 3, 26, gfx::BLACK, 90);
    let (left, mid, right) = match kind {
        "pool-ip" => ("pool", None, "ip"),
        "shares-uptime" => ("shares", None, "uptime"),
        "pool-uptime" => ("pool", None, "uptime"),
        "ip-uptime" => ("ip", None, "uptime"),
        "worker-ip" => ("worker", None, "ip"),
        _ => ("pool", Some("shares"), "ip"),
    };
    let ip = n.ip().unwrap_or_else(|| "–".into());
    let user = if st.using_fallback {
        &cfg.fallback_user
    } else {
        &cfg.stratum_user
    };
    let text = |item: &str| -> (String, u16) {
        match item {
            "shares" => (
                format!("{}/{}", st.shares_accepted, st.shares_rejected),
                gfx::FG,
            ),
            "uptime" => (uptime(diag::uptime_s()), gfx::MUTE),
            "worker" => (short_worker(user), gfx::MUTE),
            _ => (ip.clone(), gfx::DIM),
        }
    };
    // Left: the pool with its state dot and lock, or a plain text.
    let left_end = if left == "pool" {
        let up = st.authorized;
        c.disc(
            12.0,
            157.0,
            3.0,
            if up {
                if st.using_fallback {
                    gfx::WARN
                } else {
                    gfx::OK
                }
            } else {
                gfx::ERR
            },
            255,
        );
        let host = warpaxe_stratum::dial::host_of(&st.pool).to_string();
        let w = c.text(&MONO11, 20, 160, gfx::MUTE, &host, Align::Left);
        let mut end = 20 + w;
        if st.tls || st.sv2 {
            lock_icon(c, 24 + w, 161, gfx::OK);
            end += 11;
        }
        end
    } else {
        let (s, col) = text(left);
        12 + c.text(&MONO11, 12, 160, col, &s, Align::Left)
    };
    let (rs, rcol) = text(right);
    let right_x = 310 - MONO11.width(&rs);
    c.text(&MONO11, 310, 160, rcol, &rs, Align::Right);
    // The middle item centred in the gap, if it fits.
    if let Some(mid) = mid {
        let (ms, mcol) = text(mid);
        if MONO11.width(&ms) + 16 <= right_x - left_end {
            c.text(
                &MONO11,
                (left_end + right_x) / 2,
                160,
                mcol,
                &ms,
                Align::Center,
            );
        }
    }
}

fn bitcoin(c: &mut Canvas, pos: usize, total: usize) {
    let st = stratum::state();
    page_head(c, "Bitcoin", pos, total);
    c.text(
        &LABEL11,
        12,
        42,
        gfx::DIM,
        l("BLOCK HEIGHT", "BLOCKHÖHE"),
        Align::Left,
    );
    let h = st.block_height;
    c.text(
        &TITLE,
        12,
        72,
        gfx::BTC,
        &if h > 0 {
            num(h as f64, 0)
        } else {
            "–".into()
        },
        Align::Left,
    );
    if h > 0 {
        let epoch = h / 210_000;
        let next = (epoch + 1) * 210_000;
        let left = next - h;
        let frac = (h - epoch * 210_000) as f32 / 210_000.0;
        c.text(&LABEL11, 180, 42, gfx::DIM, "HALVING", Align::Left);
        let blocks = format!("{} {}", num(left as f64, 0), l("blocks", "Blöcke"));
        c.text(&VAL12, 180, 58, gfx::FG, &blocks, Align::Left);
        let months = (left as f64 / 144.0 / 30.44).round();
        let sub = format!(
            "≈ {} {} · {} %",
            months,
            l("months", "Monate"),
            (frac * 100.0).round()
        );
        c.text(&LABEL11, 180, 72, gfx::MUTE, &sub, Align::Left);
        c.rect(12, 82, 296, 4, gfx::WHITE, 20);
        c.rect(12, 82, (296.0 * frac) as i32, 4, gfx::BTC, 255);
    }
    let nd = st.network_difficulty;
    kv(
        c,
        l("Network difficulty", "Netzwerk-Difficulty"),
        &if nd > 0.0 { si(nd) } else { "–".into() },
        104,
        gfx::FG,
        12,
        308,
    );
    let nh = nd * 4_294_967_296.0 / 600.0;
    kv(
        c,
        l("Network hashrate", "Netzwerk-Hashrate"),
        &if nd > 0.0 {
            format!("≈ {}H/s", si(nh))
        } else {
            "–".into()
        },
        120,
        gfx::FG,
        12,
        308,
    );
    if let Some(mk) = market::get() {
        c.rect(3, 132, W - 3, 38, gfx::WARN, 18);
        if let Some((cur, p)) = &mk.price {
            let sym = match cur.as_str() {
                "EUR" => " €",
                "USD" => " $",
                other => other,
            };
            let s = if sym.len() <= 4 && sym.starts_with(' ') {
                format!("BTC {}{}", num(*p, 0), sym)
            } else {
                format!("BTC {} {}", num(*p, 0), sym)
            };
            c.text(&VAL12, 12, 158, gfx::FG, &s, Align::Left);
        }
        if let Some((a, b2, c2)) = mk.fees {
            let s = format!(
                "{} {} · {} · {} sat/vB",
                l("Fees", "Gebühren"),
                num(c2, 0),
                num(b2, 0),
                num(a, 0)
            );
            c.text(&MONO9, 308, 158, gfx::MUTE, &s, Align::Right);
        }
    }
}

// ------------------------------------------------- clock and night mode

/// Night mode is on and the local time lies in its window (false while
/// the clock is not set).
pub fn night_now(cfg: &config::Config) -> bool {
    cfg.night_mode
        && clock::local().is_some_and(|t| {
            dcfg::in_night(t.minute_of_day(), cfg.night_start, cfg.night_end)
        })
}

/// The day's name, full, in the display language (0 = Sunday).
fn weekday(w: u8) -> &'static str {
    match w {
        1 => l("Monday", "Montag"),
        2 => l("Tuesday", "Dienstag"),
        3 => l("Wednesday", "Mittwoch"),
        4 => l("Thursday", "Donnerstag"),
        5 => l("Friday", "Freitag"),
        6 => l("Saturday", "Samstag"),
        _ => l("Sunday", "Sonntag"),
    }
}

/// The month, short, in the display language (0 = January).
fn month(m: u8) -> &'static str {
    match m {
        0 => l("Jan", "Jan"),
        1 => l("Feb", "Feb"),
        2 => l("Mar", "Mär"),
        3 => l("Apr", "Apr"),
        4 => l("May", "Mai"),
        5 => l("Jun", "Jun"),
        6 => l("Jul", "Jul"),
        7 => l("Aug", "Aug"),
        8 => l("Sep", "Sep"),
        9 => l("Oct", "Okt"),
        10 => l("Nov", "Nov"),
        _ => l("Dec", "Dez"),
    }
}

/// "Nov 14" in English, "14. Nov" in German, "14 nov" in the others.
fn date_short(t: &clock::Local) -> String {
    let m = month(t.mon);
    match lang() {
        "en" => format!("{m} {}", t.mday),
        "de" => format!("{}. {m}", t.mday),
        _ => format!("{} {}", t.mday, m.to_lowercase()),
    }
}

/// The time as the clock font draws it (dashes while it is not set),
/// and "AM"/"PM" on the 12-hour clock.
fn hhmm(t: Option<&clock::Local>) -> (String, Option<&'static str>) {
    let Some(t) = t else {
        return ("––:––".into(), None);
    };
    let cfg = config::get();
    if dcfg::twelve_hour(&cfg.clock_format, &cfg.timezone) {
        let (h, pm) = dcfg::hour12(t.hour);
        (format!("{h}:{:02}", t.min), Some(if pm { "PM" } else { "AM" }))
    } else {
        (format!("{:02}:{:02}", t.hour, t.min), None)
    }
}

/// The clock page: the time large, the date beside it, the hashrate,
/// and the block (with price and fee when market data is on) below.
fn clock_page(c: &mut Canvas, pos: usize, total: usize) {
    let t = clock::local();
    badge(c);
    c.text(
        &LABEL12,
        82,
        17,
        gfx::PLASMA,
        &l("Time", "Uhr").to_uppercase(),
        Align::Left,
    );
    dots(c, 305, 11, pos, total);
    c.rect(12, 25, 298, 1, gfx::WHITE, 16);

    // The time; the colon in the brand colour.
    let col = if t.is_some() { gfx::FG } else { gfx::DIM };
    let (s, ampm) = hhmm(t.as_ref());
    let (hh, mm) = s.split_at(s.find(':').unwrap_or(2));
    let mut x = 10;
    x += c.text(&CLOCK, x, 96, col, hh, Align::Left);
    x += c.text(&CLOCK, x, 96, gfx::WARP, ":", Align::Left);
    c.text(&CLOCK, x, 96, col, &mm[1..], Align::Left);
    // How far the minute has run; AM/PM at its end on the 12-hour clock.
    let bar = if ampm.is_some() { 184 } else { 206 };
    c.rect(12, 104, bar, 3, gfx::WHITE, 20);
    if let Some(t) = &t {
        let w = bar * i32::from(t.sec) / 59;
        c.rect(12, 104, w, 3, gfx::PLASMA, 255);
    }
    if let Some(a) = ampm {
        c.text(&LABEL11, 218, 108, gfx::MUTE, a, Align::Right);
    }

    // Beside it: the day, the date, the week, the hashrate.
    let x0 = 232;
    match &t {
        Some(t) => {
            let wd = weekday(t.wday).to_uppercase();
            c.text(&LABEL11, x0, 46, gfx::MUTE, &wd, Align::Left);
            c.text(&VAL12, x0, 63, gfx::FG, &date_short(t), Align::Left);
            let week = dcfg::iso_week(t.year, t.yday, t.wday);
            let wk = format!("{} · {} {week}", t.year, l("Week", "KW"));
            c.text(&LABEL10, x0, 77, gfx::DIM, &wk, Align::Left);
        }
        None => {
            c.text(&LABEL10, x0, 52, gfx::MUTE, l("NO TIME YET", "KEINE UHRZEIT"), Align::Left);
            c.text(&LABEL10, x0, 66, gfx::DIM, l("Check NTP", "NTP prüfen"), Align::Left);
        }
    }
    let m = miner::state();
    let ok = m.hashrate_ghs > 0.0;
    c.rect(x0, 87, 5, 5, if ok { gfx::OK } else { gfx::WARN }, 255);
    let (v, unit) = hashrate(m.hashrate_ghs);
    let w = c.text(&VAL12, x0 + 9, 93, gfx::FG, &v, Align::Left);
    c.text(&MONO9, x0 + 12 + w, 93, gfx::MUTE, unit, Align::Left);

    // Below: the block, then price and fee, or pool and uptime.
    c.rect(12, 116, 296, 1, gfx::WHITE, 16);
    let st = stratum::state();
    let h = st.block_height;
    let block = if h > 0 { num(h as f64, 0) } else { "–".into() };
    let col3 = |c: &mut Canvas, x: i32, label: &str, val: &str, vcol: u16| {
        c.text(&LABEL10, x, 131, gfx::MUTE, &label.to_uppercase(), Align::Left);
        c.text(&VAL12, x, 149, vcol, val, Align::Left);
    };
    col3(c, 12, l("Block", "Block"), &block, gfx::BTC);
    match market::get() {
        Some(mk) => {
            if let Some((cur, p)) = &mk.price {
                let sym = match cur.as_str() {
                    "EUR" => "€",
                    "USD" => "$",
                    other => other,
                };
                col3(c, 112, "BTC", &format!("{} {sym}", num(*p, 0)), gfx::FG);
            }
            if let Some((next, _, _)) = mk.fees {
                let f = format!("{} sat/vB", num(next, 0));
                col3(c, 212, l("Fee", "Gebühr"), &f, gfx::FG);
            }
        }
        None => {
            // The host without a "pool." or "stratum." in front, cut to
            // the column (about 92 px) so it never runs into the next.
            let host = warpaxe_stratum::dial::host_of(&st.pool);
            let host = host
                .strip_prefix("pool.")
                .or_else(|| host.strip_prefix("stratum."))
                .unwrap_or(host);
            let mut pool = String::new();
            for ch in host.chars() {
                pool.push(ch);
                if VAL12.width(&pool) > 92 {
                    pool.pop();
                    pool.push('…');
                    break;
                }
            }
            col3(c, 112, "Pool", if pool.is_empty() { "–" } else { &pool }, gfx::FG);
            col3(c, 212, l("Uptime", "Laufzeit"), &uptime(diag::uptime_s()), gfx::FG);
        }
    }
}

/// Night mode: the time, the date and one line, dim, on black.
fn night_screen(c: &mut Canvas) {
    c.fill(gfx::BLACK);
    let t = clock::local();
    let (s, ampm) = hhmm(t.as_ref());
    let (hh, mm) = s.split_at(s.find(':').unwrap_or(2));
    let suffix_w = ampm.map_or(0, |a| 6 + LABEL12.width(a));
    let width = CLOCK.width(&s) + suffix_w;
    let mut x = (W - width) / 2;
    x += c.text(&CLOCK, x, 98, gfx::MUTE, hh, Align::Left);
    x += c.text(&CLOCK, x, 98, gfx::WARP, ":", Align::Left);
    x += c.text(&CLOCK, x, 98, gfx::MUTE, &mm[1..], Align::Left);
    if let Some(a) = ampm {
        c.text(&LABEL12, x + 6, 98, gfx::DIM, a, Align::Left);
    }
    if let Some(t) = &t {
        let d = format!("{} · {}", weekday(t.wday), date_short(t));
        c.text(&LABEL12, W / 2, 128, gfx::DIM, &d, Align::Center);
    }
    let m = miner::state();
    let (v, unit) = hashrate(m.hashrate_ghs);
    let mut line = format!("{v} {unit}");
    if let Some(tc) = board::state().temp_c {
        line.push_str(&format!(" · {} °C", num(f64::from(tc), 0)));
    }
    c.text(&MONO9, W / 2, 152, gfx::DIM, &line, Align::Center);
}

/// The OLED clock: the time large, the day and date and the hashrate
/// beside it.
fn oled_clock(o: &mut Oled, m: &miner::MinerState) {
    let t = clock::local();
    let (s, ampm) = hhmm(t.as_ref());
    let s = s.replace('–', "-");
    let w = OLED24.draw(1, 25, &s, |x, y, a| {
        if a >= 8 {
            o.pixel(x, y, true)
        }
    });
    let time_w = w;
    if let Some(a) = ampm {
        oled_text(o, w + 2, 19, a);
    }
    let date = match &t {
        Some(t) if lang() == "en" => {
            let wd: String = oled_str(weekday(t.wday)).chars().take(3).collect();
            format!("{wd} {}/{}", t.mon + 1, t.mday)
        }
        Some(t) => {
            // Portuguese needs three letters: SEGunda and SEXta.
            let n = if lang() == "pt" { 3 } else { 2 };
            let wd: String = oled_str(weekday(t.wday)).chars().take(n).collect();
            format!("{wd} {}.{}.", t.mday, t.mon + 1)
        }
        None => "NO NTP".into(),
    };
    // Where it would touch the time, the date goes without the day.
    let room = 128 - (time_w + 4);
    let date = if (date.len() * 6) as i32 - 1 > room {
        date.split_once(' ').map_or(date.clone(), |(_, d)| d.to_string())
    } else {
        date
    };
    let w = (date.len() * 6) as i32 - 1;
    oled_text(o, 127 - w, 5, &date);
    let ths = m.hashrate_ghs / 1000.0;
    let hr = if m.hashrate_ghs <= 0.0 {
        "-".to_string()
    } else if ths >= 1.0 {
        format!("{ths:.2}TH")
    } else {
        format!("{:.0}GH", m.hashrate_ghs)
    };
    let w = (hr.len() * 6) as i32 - 1;
    oled_text(o, 127 - w, 19, &hr);
}

fn device(c: &mut Canvas, pos: usize, total: usize) {
    let b = board::state();
    let m = miner::state();
    let cfg = config::get();
    let p = board::profile();
    page_head(c, l("Device", "Gerät"), pos, total);
    let (x0, r, r2, e) = (12, 156, 170, 308);
    // The devkit: no regulator, rail, fan or chip voltage to show; the
    // ESP and its two hash workers instead.
    if board::devkit() {
        let i = crate::cpuminer::info();
        let t = b
            .temp_c
            .map_or("–".into(), |t| format!("{} °C", num(f64::from(t), 1)));
        kv(
            c,
            "ESP32-S3",
            &t,
            44,
            b.temp_c.map_or(gfx::FG, temp_color),
            x0,
            r,
        );
        kv(
            c,
            l("Clock", "Takt"),
            &format!("{} MHz", crate::cpuminer::cpu_mhz()),
            62,
            gfx::FG,
            x0,
            r,
        );
        kv(
            c,
            l("Uptime", "Laufzeit"),
            &uptime(diag::uptime_s()),
            80,
            gfx::FG,
            x0,
            r,
        );
        let (_, hot) = limits();
        kv(c, "Max", &format!("{hot:.0} °C"), 98, gfx::FG, x0, r);
        kv(
            c,
            "SHA",
            &format!("{} kH/s", num(i.hw_khs, 1)),
            44,
            gfx::FG,
            r2,
            e,
        );
        kv(
            c,
            "CPU",
            &format!("{} kH/s", num(i.sw_khs, 1)),
            62,
            gfx::FG,
            r2,
            e,
        );
        kv(
            c,
            l("Candidates", "Kandidaten"),
            &num(f64::from(i.candidates), 0),
            80,
            gfx::FG,
            r2,
            e,
        );
        kv(
            c,
            l("Best diff", "Beste Diff"),
            &num(i.best_diff, 3),
            98,
            gfx::FG,
            r2,
            e,
        );
        c.rect(12, 130, 298, 1, gfx::WHITE, 16);
        c.text(
            &LABEL10,
            12,
            150,
            gfx::MUTE,
            l(
                "ESP32-S3R8 · SHA engine + CPU core · experimental",
                "ESP32-S3R8 · SHA-Einheit + CPU-Kern · experimentell",
            ),
            Align::Left,
        );
        c.text(
            &MONO9,
            12,
            164,
            gfx::DIM,
            &format!("WarpAxe {}", env!("CARGO_PKG_VERSION")),
            Align::Left,
        );
        let _ = (&m, &cfg);
        return;
    }
    let t = b
        .temp_c
        .map_or("–".into(), |t| format!("{} °C", num(f64::from(t), 1)));
    kv(
        c,
        "Chip",
        &t,
        44,
        b.temp_c.map_or(gfx::FG, temp_color),
        x0,
        r,
    );
    // A board without a regulator (the NerdNOS: a fixed rail, no
    // sensors but the thermistor) shows what it has instead of dashes:
    // the ESP32's own die, the effective rate, the Wi-Fi and the clock a
    // restart begins at.
    let novr = !b.has_vr;
    if novr {
        let die = crate::cpuminer::die_temp_c()
            .map_or("–".into(), |t| format!("{} °C", num(f64::from(t), 1)));
        kv(c, "ESP32", &die, 62, gfx::FG, x0, r);
    } else {
        let vr = b.vr_temp_c.map_or("–".into(), |t| format!("{t} °C"));
        kv(c, l("Regulator", "Regler"), &vr, 62, gfx::FG, x0, r);
    }
    // After an overheat step the actual clock stands next to the
    // configured one: "275/300 MHz" (a restart begins at 300 again).
    let clock = match &m.clock_step {
        Some(_) => format!(
            "{}/{} MHz",
            m.frequency_hz / 1_000_000,
            crate::config::get().frequency
        ),
        None => format!("{} MHz", m.frequency_hz / 1_000_000),
    };
    kv(c, l("Clock", "Takt"), &clock, 80, gfx::FG, x0, r);
    let meas = b
        .vout_mv
        .map(|v| p.core_mv_for_vout(v / 1000.0))
        .unwrap_or(0);
    // A fixed rail has no set point: only the reading.
    let core = if novr {
        format!("{meas} mV")
    } else {
        format!("{}/{} mV", cfg.core_voltage, meas)
    };
    kv(c, l("Core", "Kern"), &core, 98, gfx::FG, x0, r);
    kv(
        c,
        l("Uptime", "Laufzeit"),
        &uptime(diag::uptime_s()),
        116,
        gfx::FG,
        x0,
        r,
    );
    if novr {
        // Delivered against small cores × clock, as the API's yieldPercent.
        let exp = m.expected_hs(board::chip().small_cores) / 1e9;
        let eff = if exp > 0.0 && m.hashrate_ghs > 0.0 {
            format!("{} %", num((m.hashrate_ghs / exp * 100.0).min(999.0), 0))
        } else {
            "–".into()
        };
        kv(c, l("Effective", "Ausbeute"), &eff, 44, gfx::FG, r2, e);
        let n = net::state();
        // The cable when it carries the address, else the Wi-Fi level.
        if n.eth_connected {
            kv(c, "Ethernet", &eth_speed(&n, "–"), 62, gfx::OK, r2, e);
        } else {
            let (w, wc) = n.wifi_rssi.map_or(("–".into(), gfx::FG), |r| {
                (format!("{r} dBm"), rssi_color(r))
            });
            kv(c, l("Wi-Fi", "WLAN"), &w, 62, wc, r2, e);
        }
    } else {
        let vin = b.vin_mv.map_or("–".into(), |v| {
            format!("{} V", num(f64::from(v) / 1000.0, 2))
        });
        kv(c, l("Input", "Eingang"), &vin, 44, gfx::FG, r2, e);
        let amps = b.iout_ma.map_or("–".into(), |v| {
            format!("{} A", num(f64::from(v) / 1000.0, 2))
        });
        kv(c, l("Current", "Strom"), &amps, 62, gfx::FG, r2, e);
    }
    let pw = b.power_w.map_or("–".into(), |v| {
        format!(
            "{}{} W",
            if b.power_estimated { "~" } else { "" },
            num(f64::from(v), 1)
        )
    });
    kv(c, l("Power", "Leistung"), &pw, 80, gfx::FG, r2, e);
    let has_fan = board::profile().has_fan();
    let fan = |rpm: u16, pct: u8| {
        if !has_fan {
            "–".to_string()
        } else if rpm == 0 && pct > 0 {
            l("not connected", "nicht belegt").to_string()
        } else {
            format!("{rpm} rpm")
        }
    };
    let two = b.fan2_rpm.is_some();
    if !has_fan {
        // No fan to drive: the clock a restart begins at (after an
        // overheat step it differs from the running one).
        kv(
            c,
            l("Restart clock", "Neustart-Takt"),
            &format!("{} MHz", cfg.frequency),
            98,
            gfx::FG,
            r2,
            e,
        );
    } else {
        kv(
            c,
            if two {
                l("Fan M2", "Lüfter M2")
            } else {
                l("Fan", "Lüfter")
            },
            &fan(b.fan_rpm, b.fan_pct),
            98,
            gfx::FG,
            r2,
            e,
        );
    }
    if let (Some(rpm), Some(pct)) = (b.fan2_rpm, b.fan2_pct) {
        let v = fan(rpm, pct);
        let label = l("Fan M1", "Lüfter M1");
        if rpm == 0 && pct > 0 {
            // "not connected" in the narrower label face: it would run
            // into the label in the value face.
            kv_in(c, label, &v, 116, gfx::DIM, r2, e, &LABEL11);
        } else {
            kv(c, label, &v, 116, gfx::FG, r2, e);
        }
    }
    c.rect(12, 130, 298, 1, gfx::WHITE, 16);
    let (target, hot) = limits();
    let line = if !has_fan {
        l("Shutdown {h} °C", "Abschaltung {h} °C")
    } else if cfg.autofanspeed {
        l(
            "Fans automatic · target {t} °C · shutdown {h} °C",
            "Lüfter automatisch · Ziel {t} °C · Abschaltung {h} °C",
        )
    } else {
        l(
            "Fans fixed {p} % · shutdown {h} °C",
            "Lüfter fest {p} % · Abschaltung {h} °C",
        )
    }
    .replace("{t}", &format!("{target:.0}"))
    .replace("{h}", &format!("{hot:.0}"))
    .replace("{p}", &cfg.fanspeed.to_string());
    let f = [&LABEL10, &MONO9]
        .into_iter()
        .find(|f| f.width(&line) <= W - 24)
        .unwrap_or(&MONO9);
    c.text(f, 12, 150, gfx::MUTE, &line, Align::Left);
    c.text(
        &MONO9,
        12,
        164,
        gfx::DIM,
        &format!("WarpAxe {}", env!("CARGO_PKG_VERSION")),
        Align::Left,
    );
}

fn pool(c: &mut Canvas, pos: usize, total: usize) {
    let st = stratum::state();
    let m = miner::state();
    let n = net::state();
    let cfg = config::get();
    page_head(c, "Pool", pos, total);
    let w = c.text(&VAL12, 12, 46, gfx::FG, &st.pool, Align::Left);
    let mut x = 16 + w;
    if st.tls || st.sv2 {
        lock_icon(c, x, 46, gfx::OK);
        x += 11;
        let what = if st.sv2 { "V2" } else { "TLS" };
        x += c.text(&LABEL11, x, 46, gfx::OK, what, Align::Left) + 6;
    }
    if st.using_fallback {
        c.text(
            &LABEL11,
            x,
            46,
            gfx::WARN,
            l("FALLBACK", "AUSWEICH"),
            Align::Left,
        );
    }
    let user = if st.using_fallback {
        &cfg.fallback_user
    } else {
        &cfg.stratum_user
    };
    c.text(&MONO9, 12, 62, gfx::MUTE, &short_worker(user), Align::Left);
    kv(
        c,
        "Shares",
        &format!(
            "{} / {}",
            num(st.shares_accepted as f64, 0),
            st.shares_rejected
        ),
        84,
        gfx::FG,
        12,
        308,
    );
    kv(
        c,
        l("Pool difficulty", "Pool-Difficulty"),
        &if st.difficulty > 0.0 && st.difficulty < 1.0 {
            num(st.difficulty, 3)
        } else {
            si(st.difficulty)
        },
        100,
        gfx::FG,
        12,
        308,
    );
    let best = if board::devkit() {
        // The devkit's shares sit below 1: decimals, session / all-time.
        format!(
            "{} / {}",
            num(crate::cpuminer::info().best_diff, 3),
            num(config::best_diff_f(), 3)
        )
    } else {
        format!(
            "{} / {}",
            si(m.best_session_diff as f64),
            si(config::best_diff() as f64)
        )
    };
    kv(
        c,
        l(
            "Best diff (session / all-time)",
            "Beste Diff (Sitzung / gesamt)",
        ),
        &best,
        116,
        gfx::FG,
        12,
        308,
    );
    kv(
        c,
        "Scriptsig",
        &if st.scriptsig.is_empty() {
            "–".into()
        } else {
            st.scriptsig.clone()
        },
        132,
        gfx::MUTE,
        12,
        308,
    );
    c.rect(12, 142, 298, 1, gfx::WHITE, 16);
    let ip = n.ip().unwrap_or_else(|| "–".into());
    c.text(&MONO9, 12, 160, gfx::FG, &ip, Align::Left);
    // Which way the address came: the cable with its speed, or the
    // network with its level.
    if n.eth_connected {
        c.text(
            &MONO9,
            308,
            160,
            gfx::OK,
            &format!("Ethernet · {}", eth_speed(&n, "–")),
            Align::Right,
        );
    } else if let Some(r) = n.wifi_rssi {
        c.text(
            &MONO9,
            308,
            160,
            rssi_color(r),
            &format!("{} · {r} dBm", cfg.ssid),
            Align::Right,
        );
    }
}

fn log_page(c: &mut Canvas, pos: usize, total: usize) {
    page_head(c, l("Log", "Log"), pos, total);
    let count = unsafe { esp_idf_svc::sys::tusb::wa_log_count() } as usize;
    let first = count.saturating_sub(11);
    for (row, i) in (first..count).enumerate() {
        let mut buf = [0u8; 120];
        unsafe {
            esp_idf_svc::sys::tusb::wa_log_line(
                i as i32,
                buf.as_mut_ptr() as *mut core::ffi::c_char,
                buf.len(),
            )
        };
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let line = String::from_utf8_lossy(&buf[..end]).into_owned();
        // "I (12345) warpaxe::stratum: stratum: text": module, then text.
        let body = line.split_once(") ").map_or(line.as_str(), |(_, r)| r);
        let (head, text) = body.split_once(": ").unwrap_or(("", body));
        let module = head.rsplit("::").next().unwrap_or(head);
        let text = text.strip_prefix(&format!("{module}: ")).unwrap_or(text);
        let y = 38 + row as i32 * 12;
        let lvl = line.chars().next().unwrap_or('I');
        let col = match lvl {
            'E' => gfx::ERR,
            'W' => gfx::WARN,
            _ => gfx::MUTE,
        };
        let w = c.text(
            &MONO9,
            12,
            y,
            gfx::PLASMA,
            &format!("{module}:"),
            Align::Left,
        );
        let max = ((W - 24 - w) / 6).max(4) as usize;
        let text: String = text.chars().take(max).collect();
        c.text(
            &MONO9,
            16 + w,
            y,
            if row + 1 == count - first {
                gfx::FG
            } else {
                col
            },
            &text,
            Align::Left,
        );
    }
}

/// A check's short name for the display.
fn check_name(ch: warpaxe_board::selftest::Check) -> &'static str {
    use warpaxe_board::selftest::Check;
    match ch {
        Check::Chips => l("Chips", "Chips"),
        Check::Hashrate => l("Hashrate", "Hashrate"),
        Check::Domains => l("Domains", "Domänen"),
        Check::Errors => l("Errors", "Fehler"),
        Check::CoreVoltage => l("Core voltage", "Kernspannung"),
        Check::Regulator => l("Regulator", "Regler"),
        Check::Phases => l("Phases", "Phasen"),
        Check::Fan => l("Fan", "Lüfter"),
        Check::Temperature => l("Temperature", "Temperatur"),
        Check::InputVoltage => l("Input", "Eingang"),
    }
}

/// A check's step name while running.
fn step_name(step: &str, progress: u8) -> String {
    match step {
        "hashrate" => {
            let s = crate::selftest::WINDOW_S;
            let done = u32::from(progress.saturating_sub(5)) * s / 80;
            l("Hash counters {s} s", "Hash-Zähler {s} s")
                .replace("{s}", &format!("{}", s.saturating_sub(done.min(s))))
        }
        "fan" => l("Fan test", "Lüftertest").to_string(),
        "judge" => l("Evaluating", "Auswertung").to_string(),
        _ => l("Board", "Board").to_string(),
    }
}

/// The self-test on the LCD: progress while it runs, then PASS or FAIL
/// with every check in two columns.
fn selftest_screen(c: &mut Canvas, t: f32) {
    use warpaxe_board::selftest::Status;
    let Some(s) = crate::selftest::shown() else {
        return;
    };
    c.fill(gfx::rgb(0x0b, 0x0c, 0x14));
    badge(c);
    c.text(
        &LABEL12,
        82,
        17,
        gfx::PLASMA,
        &l("SELF-TEST", "SELBSTTEST").to_uppercase(),
        Align::Left,
    );
    if let Some(tc) = clock_text() {
        c.text(&MONO11, 308, 18, gfx::MUTE, &tc, Align::Right);
    }
    c.rect(12, 25, 298, 1, gfx::WHITE, 16);
    if s.running {
        let pulse = 0.5 + 0.5 * (t * 2.5).sin();
        c.disc(
            W as f32 / 2.0,
            58.0,
            5.0 + 2.0 * pulse,
            gfx::PLASMA,
            (120.0 + 135.0 * pulse) as u8,
        );
        c.text(
            &TITLE,
            W / 2,
            96,
            gfx::FG,
            &format!("{} %", s.progress),
            Align::Center,
        );
        // The bar.
        c.rect(40, 110, 240, 6, gfx::WHITE, 24);
        c.rect(
            40,
            110,
            240 * i32::from(s.progress) / 100,
            6,
            gfx::PLASMA,
            255,
        );
        fit_text(
            c,
            &[&LABEL11, &LABEL10],
            W / 2,
            134,
            gfx::MUTE,
            &step_name(s.step, s.progress),
            W - 16,
        );
        fit_text(
            c,
            &[&LABEL10, &MONO9],
            W / 2,
            156,
            gfx::DIM,
            l("Mining continues", "Das Schürfen läuft weiter"),
            W - 16,
        );
        return;
    }
    let Some(r) = s.report else {
        c.text(
            &TITLE,
            W / 2,
            80,
            gfx::WARN,
            l("CANCELLED", "ABGEBROCHEN"),
            Align::Center,
        );
        let why = match s.aborted {
            Some("pointChanged") => l(
                "Clock or voltage changed during the test",
                "Takt oder Spannung im Test geändert",
            ),
            Some("chainStopped") => l(
                "The ASICs went off during the test",
                "Die ASICs gingen während des Tests aus",
            ),
            _ => l(
                "Cancelled in the web UI",
                "In der Weboberfläche abgebrochen",
            ),
        };
        fit_text(
            c,
            &[&LABEL11, &LABEL10, &MONO9],
            W / 2,
            108,
            gfx::MUTE,
            why,
            W - 16,
        );
        c.text(
            &LABEL10,
            W / 2,
            156,
            gfx::DIM,
            l("Press a key to close", "Zum Schließen Taste drücken"),
            Align::Center,
        );
        return;
    };
    let (word, col) = if r.passed() {
        ("PASS", gfx::OK)
    } else {
        ("FAIL", gfx::ERR)
    };
    c.text(&TITLE, 12, 56, col, word, Align::Left);
    let line = if r.passed() {
        l(
            "{n} of {m} checks passed",
            "{n} von {m} Prüfungen bestanden",
        )
        .replace("{n}", &r.judged().to_string())
        .replace("{m}", &r.judged().to_string())
    } else {
        l(
            "{n} of {m} checks failed",
            "{n} von {m} Prüfungen nicht bestanden",
        )
        .replace("{n}", &r.failed().to_string())
        .replace("{m}", &r.judged().to_string())
    };
    let wx = TITLE.width(word) + 24;
    let lf: &gfx::font::Font = if LABEL11.width(&line) <= W - 12 - wx {
        &LABEL11
    } else {
        &LABEL10
    };
    c.text(lf, wx, 56, gfx::MUTE, &line, Align::Left);
    c.rect(12, 64, 298, 1, gfx::WHITE, 16);
    for (i, o) in r.outcomes.iter().enumerate() {
        let (col_x, row) = if i < 5 {
            (12, i as i32)
        } else {
            (168, i as i32 - 5)
        };
        let y = 82 + row * 15;
        let (mark_col, label_col) = match o.status {
            Status::Pass => (gfx::OK, gfx::FG),
            Status::Fail => (gfx::ERR, gfx::FG),
            Status::Skip => (gfx::DIM, gfx::DIM),
        };
        let (x, yb) = (col_x as f32, y as f32);
        match o.status {
            Status::Pass => {
                c.line(x, yb - 5.0, x + 3.0, yb - 1.0, mark_col, 255);
                c.line(x + 3.0, yb - 1.0, x + 9.0, yb - 9.0, mark_col, 255);
            }
            Status::Fail => {
                c.line(x, yb - 9.0, x + 8.0, yb - 1.0, mark_col, 255);
                c.line(x, yb - 1.0, x + 8.0, yb - 9.0, mark_col, 255);
            }
            Status::Skip => c.rect(col_x, y - 6, 8, 2, mark_col, 255),
        }
        let mut name = check_name(o.check).to_string();
        if o.status == Status::Fail {
            if let Some(ch) = o.chip {
                name.push_str(&format!(" C{ch}"));
            }
            if let Some(d) = o.domain {
                name.push_str(&format!("D{d}"));
            }
            if let Some(p) = o.phase {
                name.push_str(&format!(" P{p}"));
            }
        }
        let f: &gfx::font::Font = if LABEL11.width(&name) <= 118 {
            &LABEL11
        } else {
            &LABEL10
        };
        c.text(f, col_x + 14, y, label_col, &name, Align::Left);
    }
    c.text(
        &LABEL10,
        W / 2,
        163,
        gfx::DIM,
        l("Press a key to close", "Zum Schließen Taste drücken"),
        Align::Center,
    );
}

fn event_screen(c: &mut Canvas, ev: Event, t: f32) {
    let pulse = 0.5 + 0.5 * (t * 2.5).sin();
    match ev {
        Event::SelfTest(_) => selftest_screen(c, t),
        Event::Overheat => {
            c.fill(gfx::rgb(0x1a, 0x07, 0x09));
            let a = (80.0 + 175.0 * pulse) as u8;
            for (x, y, w, h) in [
                (0, 0, W, 4),
                (0, H - 4, W, 4),
                (0, 0, 4, H),
                (W - 4, 0, 4, H),
            ] {
                c.rect(x, y, w, h, gfx::ERR, a);
            }
            let b = board::state();
            let power = b.overheat_cause == Some("power");
            c.text(
                &TITLE,
                W / 2,
                62,
                gfx::ERR,
                if power {
                    l("REGULATOR FAULT", "REGLERFEHLER")
                } else {
                    l("OVERHEAT", "ÜBERHITZUNG")
                },
                Align::Center,
            );
            let tc = b.temp_c.map_or("–".into(), |t| num(f64::from(t), 1));
            let l1 = if power {
                l(
                    "Regulator fault · core voltage off",
                    "Reglerfehler · Kernspannung aus",
                )
                .to_string()
            } else {
                l(
                    "Chip {t} °C · core voltage off",
                    "Chip {t} °C · Kernspannung aus",
                )
                .replace("{t}", &tc)
            };
            fit_text(
                c,
                &[&VAL12, &LABEL11, &LABEL10],
                W / 2,
                90,
                gfx::FG,
                &l1,
                W - 16,
            );
            let l2 = if b.overheat_latched {
                l(
                    "Fans 100 % · clear it in the web UI under System",
                    "Lüfter 100 % · Freigabe unter System (Weboberfläche)",
                )
            } else if power {
                l(
                    "Fans 100 % · tries again after a minute",
                    "Lüfter 100 % · neuer Versuch nach einer Minute",
                )
            } else {
                l(
                    "Fans 100 % · starts again once cooled",
                    "Lüfter 100 % · läuft nach dem Abkühlen wieder an",
                )
            };
            fit_text(
                c,
                &[&LABEL11, &LABEL10, &MONO9],
                W / 2,
                112,
                gfx::MUTE,
                l2,
                W - 16,
            );
        }
        Event::ChipsOff { no_pool } => {
            c.fill(gfx::rgb(0x0b, 0x0c, 0x14));
            c.disc(
                W as f32 / 2.0,
                40.0,
                5.0 + 2.0 * pulse,
                gfx::WARN,
                (120.0 + 135.0 * pulse) as u8,
            );
            c.text(
                &TITLE,
                W / 2,
                84,
                gfx::WARN,
                if no_pool {
                    l("NO POOL", "KEIN POOL")
                } else {
                    l("ASICS OFF", "ASICS AUS")
                },
                Align::Center,
            );
            let l = if no_pool {
                l(
                    "ASICs off until a pool answers",
                    "ASICs aus, bis ein Pool antwortet",
                )
            } else {
                l(
                    "Device stays reachable · switch on in the web UI",
                    "Gerät bleibt erreichbar · Einschalten in der Weboberfläche",
                )
            };
            fit_text(
                c,
                &[&LABEL11, &LABEL10, &MONO9],
                W / 2,
                106,
                gfx::MUTE,
                l,
                W - 16,
            );
            let n = net::state();
            let ip = n.ip().unwrap_or_default();
            c.text(&VAL12, W / 2, 128, gfx::FG, &ip, Align::Center);
        }
        Event::Setup => {
            c.fill(gfx::rgb(0x0b, 0x0c, 0x1c));
            let (ssid, pass) = net::hotspot().unwrap_or_default();
            c.text(
                &TITLE,
                W / 2,
                30,
                gfx::FG,
                l("WI-FI SETUP", "WLAN-EINRICHTUNG"),
                Align::Center,
            );
            let join = l("Join {n}", "Mit {n} verbinden").replace("{n}", &ssid);
            fit_text(
                c,
                &[&VAL12, &LABEL11, &LABEL10],
                W / 2,
                58,
                gfx::FG,
                &join,
                W - 16,
            );
            c.text(
                &LABEL11,
                W / 2,
                80,
                gfx::MUTE,
                l("Password · 10 digits", "Passwort · 10 Ziffern"),
                Align::Center,
            );
            // Two groups of five with a gap, no space character: easier to
            // read off and nothing extra to type.
            let (a, b) = pass.split_at(pass.len() / 2);
            c.text(&TITLE, W / 2 - 6, 110, gfx::WARN, a, Align::Right);
            c.text(&TITLE, W / 2 + 6, 110, gfx::WARN, b, Align::Left);
            fit_text(
                c,
                &[&LABEL11, &LABEL10, &MONO9],
                W / 2,
                140,
                gfx::MUTE,
                l(
                    "then open http://192.168.4.1",
                    "dann http://192.168.4.1 öffnen",
                ),
                W - 16,
            );
        }
        Event::Ota(pct) => {
            c.fill(gfx::rgb(0x0b, 0x0c, 0x1c));
            c.text(
                &TITLE,
                W / 2,
                64,
                gfx::FG,
                l("FIRMWARE UPDATE", "FIRMWARE-UPDATE"),
                Align::Center,
            );
            c.rect(60, 84, 200, 6, gfx::WHITE, 26);
            let f = pct.map_or(0.0, |p| f32::from(p) / 100.0);
            let fw = (200.0 * f) as i32;
            for x in 0..fw {
                let col = mix(gfx::PLASMA, gfx::WARP, (x * 255 / 200) as u8);
                c.rect(60 + x, 84, 1, 6, col, 255);
            }
            // Shimmer running along the bar.
            let sx = 60.0 + (t * 120.0) % 220.0 - 20.0;
            for k in 0..20 {
                let x = sx as i32 + k;
                if x >= 60 && x < 60 + fw {
                    let a = (255.0 * (1.0 - (k as f32 - 10.0).abs() / 10.0) * 0.6) as u8;
                    c.rect(x, 84, 1, 6, gfx::WHITE, a);
                }
            }
            let line = match pct {
                None => l(
                    "Preparing flash · do not power off",
                    "Speicher wird vorbereitet · nicht ausschalten",
                )
                .to_string(),
                Some(p) => l("{p} % · do not power off", "{p} % · nicht ausschalten")
                    .replace("{p}", &p.to_string()),
            };
            fit_text(
                c,
                &[&LABEL11, &LABEL10, &MONO9],
                W / 2,
                110,
                gfx::MUTE,
                &line,
                W - 16,
            );
        }
        Event::Block(_) => {}
    }
}

// ------------------------------------------------------------------ OLED

fn oled_text(o: &mut Oled, x: i32, y: i32, s: &str) {
    o.text(x.max(0) as usize, y.max(0) as usize, 1, s);
}

fn oled_center(o: &mut Oled, y: i32, s: &str) {
    let w = (s.len() * 6) as i32 - 1;
    oled_text(o, (128 - w) / 2, y, s);
}

/// One slide by its name (`warpaxe_displaycfg::OLED_SLIDES`).
fn oled_slide(o: &mut Oled, name: &str) {
    let m = miner::state();
    let st = stratum::state();
    let b = board::state();
    let n = net::state();
    if name == "logo" {
        // The wordmark, with model and version as on the splash.
        o.bitmap(SPLASH_OLED);
        let line = format!(
            "{} {}",
            board::profile().model.to_uppercase(),
            env!("CARGO_PKG_VERSION")
        );
        let line: String = line.chars().take(21).collect();
        oled_center(o, 23, &line);
        return;
    }
    if name == "clock" {
        oled_clock(o, &m);
        return;
    }
    if matches!(name, "pool" | "block" | "wifi") {
        let (a, b2) = match name {
            "pool" => (
                warpaxe_stratum::dial::host_of(&st.pool).to_uppercase(),
                n.ip().unwrap_or_default(),
            ),
            "block" => (
                format!("BLOCK {}", st.block_height),
                format!("DIFF {}", si(st.network_difficulty).replace(',', ".")),
            ),
            _ => (
                if n.eth_connected {
                    format!("ETHERNET {}", oled_str(&eth_speed(&n, "-")))
                } else {
                    n.wifi_rssi
                        .map_or("WLAN -".into(), |r| format!("WLAN {r} DBM"))
                },
                format!(
                    "UP {}",
                    uptime(diag::uptime_s())
                        .to_uppercase()
                        .replace(' ', "")
                        .replace("MIN", "M")
                ),
            ),
        };
        let a: String = a.chars().take(21).collect();
        oled_center(o, 6, &a);
        oled_center(o, 19, &b2);
        return;
    }
    // Hashrate large on the left.
    let ths = m.hashrate_ghs / 1000.0;
    let (v, unit) = if m.hashrate_ghs <= 0.0 {
        ("-".to_string(), "TH/S")
    } else if ths >= 1.0 {
        (format!("{ths:.2}"), "TH/S")
    } else {
        (format!("{:.0}", m.hashrate_ghs), "GH/S")
    };
    let w = OLED24.draw(1, 25, &v, |x, y, a| {
        if a >= 8 {
            o.pixel(x, y, true)
        }
    });
    oled_text(o, w + 3, 18, unit);
    for y in 2..30 {
        o.pixel(79, y, true);
    }
    let pw = b.power_w.unwrap_or(0.0);
    let (label, val) = match name {
        "power" => ("POWER", format!("{pw:.1}W")),
        "efficiency" => (
            "J/TH",
            if ths > 0.01 {
                format!("{:.1}", f64::from(pw) / ths)
            } else {
                "-".into()
            },
        ),
        "shares" => ("SHARES", format!("{}", st.shares_accepted)),
        "best" => ("BEST", si(m.best_session_diff as f64).replace(',', ".")),
        _ => ("TEMP", b.temp_c.map_or("-".into(), |t| format!("{t:.0}C"))),
    };
    oled_text(o, 84, 5, label);
    oled_text(o, 84, 18, &val);
}

fn oled_event(o: &mut Oled, ev: Event, t: f32, rng: &mut u32) {
    let blink = (t * 2.0) as i32 % 2 == 0;
    match ev {
        Event::Block(h) => {
            // 1-bit warp: dots flying out of the centre.
            for _ in 0..40 {
                *rng ^= *rng << 13;
                *rng ^= *rng >> 17;
                *rng ^= *rng << 5;
                let a = (*rng % 628) as f32 / 100.0;
                let r = ((*rng >> 10) % 70) as f32 * (0.5 + (t % 1.0));
                o.pixel(
                    (64.0 + r * a.cos()) as i32,
                    (16.0 + r * a.sin() * 0.4) as i32,
                    true,
                );
            }
            if t > 1.0 {
                for y in 3..29 {
                    for x in 8..120 {
                        o.pixel(x, y, false);
                    }
                }
                oled_center(o, 6, &{
                    let t = oled_str(l("BLOCK FOUND", "BLOCK GEFUNDEN"));
                    if blink {
                        format!("!! {t} !!")
                    } else {
                        t
                    }
                });
                oled_center(o, 19, &format!("{h}"));
            }
        }
        Event::Overheat => {
            oled_center(
                o,
                6,
                &if blink {
                    oled_str(l("OVERHEAT", "ÜBERHITZUNG"))
                } else {
                    String::new()
                },
            );
            let tc = board::state()
                .temp_c
                .map_or("-".into(), |t| format!("{t:.0}C"));
            oled_center(
                o,
                19,
                &format!("{tc} {}", oled_str(l("CORE OFF", "KERN AUS"))),
            );
        }
        Event::ChipsOff { no_pool } => {
            let title = if no_pool {
                l("NO POOL", "KEIN POOL")
            } else {
                l("ASICS OFF", "ASICS AUS")
            };
            oled_center(o, 6, &oled_str(title));
            let n = net::state();
            oled_center(o, 19, &n.ip().unwrap_or_default());
        }
        Event::Setup => {
            // Two lines only: name, password and address take turns.
            let (ssid, pass) = net::hotspot().unwrap_or_default();
            let (a, b) = match (t / 3.0) as i32 % 3 {
                0 => (oled_str(l("WI-FI SETUP", "WLAN-EINRICHTUNG")), ssid),
                1 => (oled_str(l("PASSWORD", "PASSWORT")), pass),
                _ => (
                    oled_str(l("THEN OPEN", "DANN ÖFFNEN")),
                    "192.168.4.1".into(),
                ),
            };
            oled_center(o, 6, &a);
            oled_center(o, 19, &b);
        }
        Event::SelfTest(_) => {
            use warpaxe_board::selftest::Status;
            let Some(s) = crate::selftest::shown() else {
                return;
            };
            let title = oled_str(l("SELF-TEST", "SELBSTTEST"));
            if s.running {
                oled_center(o, 6, &format!("{title} {} %", s.progress));
                oled_center(o, 19, &oled_str(&step_name(s.step, s.progress)));
            } else if let Some(r) = s.report {
                let word = if r.passed() { "PASS" } else { "FAIL" };
                oled_center(
                    o,
                    6,
                    &if blink || r.passed() {
                        format!("{title} {word}")
                    } else {
                        title.clone()
                    },
                );
                let line = match r.outcomes.iter().find(|x| x.status == Status::Fail) {
                    None => format!("{}/{} OK", r.judged(), r.judged()),
                    Some(x) => {
                        let mut n = oled_str(check_name(x.check));
                        if let Some(ch) = x.chip {
                            n.push_str(&format!(" C{ch}"));
                        }
                        if let Some(d) = x.domain {
                            n.push_str(&format!("D{d}"));
                        }
                        n
                    }
                };
                oled_center(o, 19, &line);
            } else {
                oled_center(o, 6, &title);
                oled_center(o, 19, &oled_str(l("CANCELLED", "ABGEBROCHEN")));
            }
        }
        Event::Ota(p) => {
            oled_center(o, 4, &oled_str(l("FIRMWARE UPDATE", "FIRMWARE-UPDATE")));
            let f = p.map_or(0, |p| i32::from(p));
            for x in 14..114 {
                o.pixel(x, 18, true);
                o.pixel(x, 24, true);
            }
            for x in 14..14 + f {
                for y in 19..24 {
                    o.pixel(x, y, true);
                }
            }
        }
    }
}
