//! What the device display shows after the splash.
//!
//! LCD (Nerd boards, 320×170): the cockpit, and four more pages a press of
//! the page key (GPIO14) steps through: Bitcoin, device, pool, log. The
//! boot key (GPIO0) goes back to the cockpit; a minute without a key press
//! does too. OLED (Bitaxe, 128×32): the hashrate large on the left, a
//! rotating value on the right, and three full-width info slides per
//! round; the boot key steps on.
//!
//! Special screens cover every page while their state lasts: firmware
//! update, block found (animated; a key press dismisses it), overheat,
//! chips off.

use std::time::{Duration, Instant};

use crate::display::{self, Display, Lcd, Oled};
use crate::gfx::fonts::*;
use crate::gfx::{self, mix, Align, Canvas};
use crate::{board, clock, config, diag, history, market, miner, net, stratum};

const W: i32 = display::WIDTH as i32;
const H: i32 = display::HEIGHT as i32;
/// Back to the cockpit after this long without a key press.
const HOME_AFTER_S: u64 = 60;
/// A found block stays on screen this long unless a key dismisses it.
const BLOCK_SHOW_S: u64 = 600;
const PAGES: usize = 5;
const OLED_STEP: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Event {
    Ota(Option<u8>),
    Block(u64),
    Overheat,
    ChipsOff,
    /// The setup hotspot is up: how to join it.
    Setup,
}

#[derive(Clone, Copy)]
struct Star {
    angle: f32,
    dist: f32,
    speed: f32,
    color: u16,
}

pub struct Ui {
    page: usize,
    last_key_s: u64,
    base: Vec<u8>,
    event: Option<Event>,
    event_t0: Instant,
    acked_block: Option<u64>,
    stars: Vec<Star>,
    rng: u32,
    oled_i: usize,
    oled_t: Instant,
    last_acc: u64,
    blink: u8,
}

/// Key presses from the display thread.
pub enum Key {
    /// GPIO14 on the Nerd boards.
    Next,
    /// GPIO0 (boot key).
    Home,
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
    let units = ["", "K", "M", "G", "T", "P", "E"];
    let mut v = v.max(0.0);
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let d = if v >= 100.0 {
        1
    } else if v >= 10.0 {
        1
    } else {
        2
    };
    format!("{}{}", num(v, if i == 0 { 0 } else { d }), units[i])
}

fn uptime(s: u64) -> String {
    let (d, h, m) = (s / 86_400, s / 3600 % 24, s / 60 % 60);
    if d > 0 {
        format!("{d} d {h} h")
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
    } else {
        (num(ghs, 0), "GH/s")
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
    let hot = if cfg.overheat_temp > 0 {
        f32::from(cfg.overheat_temp)
    } else {
        p.max_temp_c
    };
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
            page: 0,
            last_key_s: 0,
            base: Vec::new(),
            event: None,
            event_t0: Instant::now(),
            acked_block: None,
            stars: Vec::new(),
            rng: 0x9E37_79B9,
            oled_i: 0,
            oled_t: Instant::now(),
            last_acc: 0,
            blink: 0,
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
        if let Some(Event::Block(h)) = self.event {
            self.acked_block = Some(h);
            *display::DEMO.lock().unwrap() = None;
            return;
        }
        match k {
            Key::Next => self.page = (self.page + 1) % PAGES,
            Key::Home => {
                self.page = 0;
                self.oled_i = (self.oled_i + 1) % OLED_SLIDES;
                self.oled_t = Instant::now();
            }
        }
    }

    fn current_event(&self) -> Option<Event> {
        let now = diag::uptime_s();
        if let Some((ev, until)) = display::DEMO.lock().unwrap().clone() {
            if now < until {
                return Some(match ev.as_str() {
                    "block" => Event::Block(stratum::state().block_height.max(1)),
                    "overheat" => Event::Overheat,
                    "off" => Event::ChipsOff,
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
            return Some(Event::ChipsOff);
        }
        if net::hotspot().is_some() {
            return Some(Event::Setup);
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
        if self.page != 0 && diag::uptime_s().saturating_sub(self.last_key_s) > HOME_AFTER_S {
            self.page = 0;
        }
        match d {
            Display::Lcd(l) => self.lcd(l),
            Display::Oled(o) => self.oled(o),
        }
    }

    // ---------------------------------------------------------------- LCD

    fn lcd(&mut self, l: &mut Lcd) -> Duration {
        let cfg = config::get();
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
            None => {
                l.blit(&self.base);
                let mut c = l.canvas();
                match self.page {
                    0 => cockpit(&mut c),
                    1 => bitcoin(&mut c),
                    2 => device(&mut c),
                    3 => pool(&mut c),
                    _ => log_page(&mut c),
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
            let sub = format!("Block {}", num(height as f64, 0));
            fade_text(c, &VAL12, W / 2, 106, gfx::FG, &sub, a);
            let hint = l("Press a key to close", "Taste drücken zum Schließen");
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

    fn oled(&mut self, o: &mut Oled) -> Duration {
        let cfg = config::get();
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
            None => {
                if self.oled_t.elapsed() >= OLED_STEP {
                    self.oled_i = (self.oled_i + 1) % OLED_SLIDES;
                    self.oled_t = Instant::now();
                }
                let acc = stratum::state().shares_accepted;
                if acc != self.last_acc {
                    self.blink = if self.last_acc == 0 { 0 } else { 1 };
                    self.last_acc = acc;
                } else if self.blink > 0 {
                    self.blink -= 1;
                }
                oled_slide(o, self.oled_i);
                if self.blink > 0 {
                    for y in 0..3 {
                        for x in 125..128 {
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
static SPLASH_OLED: &[u8] = include_bytes!("splash/oled_128x32.bits");

/// The wordmark top left of every page, the page title after it.
fn badge(c: &mut Canvas) {
    c.sprite(9, 2, 66, 20, BADGE);
}

fn page_head(c: &mut Canvas, title: &str, n: usize) {
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
    dots(c, 264, 11, n);
    c.rect(12, 25, 298, 1, gfx::WHITE, 16);
}

fn dots(c: &mut Canvas, x: i32, y: i32, n: usize) {
    for i in 0..PAGES {
        let (col, a) = if i == n {
            (gfx::PLASMA, 255)
        } else {
            (gfx::WHITE, 40)
        };
        c.rect(x + i as i32 * 9, y, 5, 5, col, a);
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

fn cockpit(c: &mut Canvas) {
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
    let hash_lbl = "HASHRATE";
    c.text(&LABEL11, 12, 34, gfx::DIM, hash_lbl, Align::Left);
    let (v, unit) = hashrate(m.hashrate_ghs);
    let w = c.text(&HERO, 11, 70, gfx::FG, &v, Align::Left);
    c.text(&UNIT13, 16 + w, 70, gfx::MUTE, unit, Align::Left);

    // Last hour, 10 s samples.
    let mut hist = history::recent_ghs(360);
    // Samples before the first estimate (right after boot) are zero:
    // start the line at the first real one.
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
                let v = if v.is_finite() { *v } else { lo };
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

    // Right column.
    let rx = 222;
    c.rect(rx - 8, 8, 1, 124, gfx::WHITE, 12);
    dots(c, 270, 10, 0);
    let big = |c: &mut Canvas, label: &str, val: &str, unit: &str, y: i32, col: u16| {
        c.text(&LABEL11, rx, y, gfx::DIM, label, Align::Left);
        let w = c.text(&VAL16, rx, y + 17, col, val, Align::Left);
        c.text(&LABEL11, rx + w + 3, y + 17, gfx::MUTE, unit, Align::Left);
    };
    let t = b.temp_c;
    big(
        c,
        "CHIP",
        &t.map_or("–".into(), |t| num(f64::from(t), 1)),
        "°C",
        28,
        t.map_or(gfx::FG, temp_color),
    );
    c.rect(rx, 50, 88, 3, gfx::WHITE, 20);
    if let Some(t) = t {
        let (_, hot) = limits();
        let f = ((t - 30.0) / (hot - 30.0)).clamp(0.0, 1.0);
        c.rect(rx, 50, (88.0 * f) as i32, 3, temp_color(t), 255);
    }
    let pw = b.power_w.unwrap_or(0.0);
    let (lbl_p, lbl_e) = (l("POWER", "LEISTUNG"), l("EFFICIENCY", "EFFIZIENZ"));
    big(c, lbl_p, &num(f64::from(pw), 1), "W", 66, gfx::FG);
    let eff = if m.hashrate_ghs > 1.0 {
        f64::from(pw) / (m.hashrate_ghs / 1000.0)
    } else {
        0.0
    };
    big(
        c,
        lbl_e,
        &if eff > 0.0 { num(eff, 1) } else { "–".into() },
        "J/TH",
        100,
        gfx::FG,
    );

    // Status bar.
    c.rect(3, 144, W - 3, 26, gfx::BLACK, 90);
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
    let host = st.pool.split(':').next().unwrap_or("").to_string();
    let w = c.text(&MONO11, 20, 160, gfx::MUTE, &host, Align::Left);
    let mut left_end = 20 + w;
    if st.tls || st.sv2 {
        lock_icon(c, 24 + w, 161, gfx::OK);
        left_end += 11;
    }
    let ip = n.wifi_ip.or(n.usb_ip).unwrap_or_else(|| "–".into());
    let ip_x = 310 - MONO11.width(&ip);
    c.text(&MONO11, 310, 160, gfx::DIM, &ip, Align::Right);
    // Shares centred in the gap between pool and IP, if they fit.
    let shares = format!("{}/{}", st.shares_accepted, st.shares_rejected);
    if MONO11.width(&shares) + 16 <= ip_x - left_end {
        c.text(
            &MONO11,
            (left_end + ip_x) / 2,
            160,
            gfx::FG,
            &shares,
            Align::Center,
        );
    }
}

fn bitcoin(c: &mut Canvas) {
    let st = stratum::state();
    page_head(c, "Bitcoin", 1);
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

fn device(c: &mut Canvas) {
    let b = board::state();
    let m = miner::state();
    let cfg = config::get();
    let p = board::profile();
    page_head(c, l("Device", "Gerät"), 2);
    let (x0, r, r2, e) = (12, 156, 170, 308);
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
    let vr = b.vr_temp_c.map_or("–".into(), |t| format!("{t} °C"));
    kv(c, l("Regulator", "Regler"), &vr, 62, gfx::FG, x0, r);
    kv(
        c,
        l("Clock", "Takt"),
        &format!("{} MHz", m.frequency_hz / 1_000_000),
        80,
        gfx::FG,
        x0,
        r,
    );
    let meas = b
        .vout_mv
        .map(|v| p.core_mv_for_vout(v / 1000.0))
        .unwrap_or(0);
    kv(
        c,
        l("Core", "Kern"),
        &format!("{}/{} mV", cfg.core_voltage, meas),
        98,
        gfx::FG,
        x0,
        r,
    );
    kv(
        c,
        l("Uptime", "Laufzeit"),
        &uptime(diag::uptime_s()),
        116,
        gfx::FG,
        x0,
        r,
    );
    let vin = b.vin_mv.map_or("–".into(), |v| {
        format!("{} V", num(f64::from(v) / 1000.0, 2))
    });
    kv(c, l("Input", "Eingang"), &vin, 44, gfx::FG, r2, e);
    let amps = b.iout_ma.map_or("–".into(), |v| {
        format!("{} A", num(f64::from(v) / 1000.0, 2))
    });
    kv(c, l("Current", "Strom"), &amps, 62, gfx::FG, r2, e);
    let pw = b
        .power_w
        .map_or("–".into(), |v| format!("{} W", num(f64::from(v), 1)));
    kv(c, l("Power", "Leistung"), &pw, 80, gfx::FG, r2, e);
    let fan = |rpm: u16, pct: u8| {
        if rpm == 0 && pct > 0 {
            l("not connected", "nicht belegt").to_string()
        } else {
            format!("{rpm} rpm")
        }
    };
    let two = b.fan2_rpm.is_some();
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
    let line = if cfg.autofanspeed {
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

fn pool(c: &mut Canvas) {
    let st = stratum::state();
    let m = miner::state();
    let n = net::state();
    let cfg = config::get();
    page_head(c, "Pool", 3);
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
        &si(st.difficulty),
        100,
        gfx::FG,
        12,
        308,
    );
    let best = format!(
        "{} / {}",
        si(m.best_session_diff as f64),
        si(config::best_diff() as f64)
    );
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
    let ip = n
        .wifi_ip
        .clone()
        .or(n.usb_ip.clone())
        .unwrap_or_else(|| "–".into());
    c.text(&MONO9, 12, 160, gfx::FG, &ip, Align::Left);
    if let Some(r) = n.wifi_rssi {
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

fn log_page(c: &mut Canvas) {
    page_head(c, "Log", 4);
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

fn event_screen(c: &mut Canvas, ev: Event, t: f32) {
    let pulse = 0.5 + 0.5 * (t * 2.5).sin();
    match ev {
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
                    "Lüfter 100 % · Freigabe im Web-UI unter System",
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
        Event::ChipsOff => {
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
                l("CHIPS OFF", "CHIPS AUS"),
                Align::Center,
            );
            let l = l(
                "Device stays reachable · switch on in the web UI",
                "Gerät bleibt erreichbar · Einschalten im Web-UI",
            );
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
            let ip = n.wifi_ip.or(n.usb_ip).unwrap_or_default();
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
            let join = l("Join {n}", "Verbinden mit {n}").replace("{n}", &ssid);
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

const OLED_SLIDES: usize = 9;

fn oled_text(o: &mut Oled, x: i32, y: i32, s: &str) {
    o.text(x.max(0) as usize, y.max(0) as usize, 1, s);
}

fn oled_center(o: &mut Oled, y: i32, s: &str) {
    let w = (s.len() * 6) as i32 - 1;
    oled_text(o, (128 - w) / 2, y, s);
}

fn oled_slide(o: &mut Oled, i: usize) {
    let m = miner::state();
    let st = stratum::state();
    let b = board::state();
    let n = net::state();
    if i == 8 {
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
    if i >= 5 {
        let (a, b2) = match i {
            5 => (
                st.pool.split(':').next().unwrap_or("").to_uppercase(),
                n.wifi_ip.or(n.usb_ip).unwrap_or_default(),
            ),
            6 => (
                format!("BLOCK {}", st.block_height),
                format!("DIFF {}", si(st.network_difficulty).replace(',', ".")),
            ),
            _ => (
                n.wifi_rssi
                    .map_or("WLAN -".into(), |r| format!("WLAN {r} DBM")),
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
    let w = OLED24.draw(0, 25, &v, |x, y, a| {
        if a >= 8 {
            o.pixel(x, y, true)
        }
    });
    oled_text(o, w + 3, 18, unit);
    for y in 2..30 {
        o.pixel(79, y, true);
    }
    let pw = b.power_w.unwrap_or(0.0);
    let (label, val) = match i {
        0 => ("TEMP", b.temp_c.map_or("-".into(), |t| format!("{t:.0}C"))),
        1 => ("POWER", format!("{pw:.1}W")),
        2 => (
            "J/TH",
            if ths > 0.01 {
                format!("{:.1}", f64::from(pw) / ths)
            } else {
                "-".into()
            },
        ),
        3 => ("SHARES", format!("{}", st.shares_accepted)),
        _ => ("BEST", si(m.best_session_diff as f64).replace(',', ".")),
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
        Event::ChipsOff => {
            oled_center(o, 6, &oled_str(l("CHIPS OFF", "CHIPS AUS")));
            let n = net::state();
            oled_center(o, 19, &n.wifi_ip.or(n.usb_ip).unwrap_or_default());
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
