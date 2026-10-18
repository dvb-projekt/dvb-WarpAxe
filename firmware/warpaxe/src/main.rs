#![recursion_limit = "512"]
//! WarpAxe firmware: the ESP32-S3 side of the miner.
//!
//! Boot order matters and is deliberate: diagnostics first (a crash in the
//! previous run is shown and, after a crash, the USB PHY stays on
//! USB-Serial/JTAG for a while so the board can always be reflashed),
//! then config, display, the network interfaces as a list (USB always,
//! Wi-Fi when configured), then the API.

mod api;
mod board;
mod clock;
mod config;
mod cpuminer;
mod diag;
mod display;
mod display_i18n;
mod font;
mod gfx;
mod history;
mod link;
mod market;
mod miner;
mod net;
mod ntc;
mod screens;
mod stratum;
mod swarm;
mod uart;
mod update;

use std::sync::Mutex;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use log::info;

static STEP: Mutex<&'static str> = Mutex::new("boot");

fn set_step(s: &'static str) {
    *STEP.lock().unwrap() = s;
    info!("step: {s}");
}

/// Seconds after boot the splash stays up (the display thread starts
/// about a second in, so it shows for four to five).
const SPLASH_S: u64 = 6;
static SPLASH_LCD: &[u8] = include_bytes!("splash/lcd_320x170.rgb565");
/// The makers' wordmarks (docs/brand/nerd and docs/brand/bitaxe, used with
/// their authors' and the trade mark holders' consent): width, height,
/// RGB565 + alpha.
static MARK_NQPP: (i32, i32, &[u8]) = (
    138,
    30,
    include_bytes!("splash/mark_nerdqaxe-plusplus_138x30.rgb565a"),
);
static MARK_OCTAXE: (i32, i32, &[u8]) = (
    143,
    34,
    include_bytes!("splash/mark_nerdoctaxe_143x34.rgb565a"),
);
static MARK_NERDAXE: (i32, i32, &[u8]) = (
    143,
    34,
    include_bytes!("splash/mark_nerdaxe_143x34.rgb565a"),
);
static MARK_GAIA: (i32, i32, &[u8]) = (
    108,
    40,
    include_bytes!("splash/mark_nerdaxe-gaia_108x40.rgb565a"),
);
static MARK_NERDNOS: (i32, i32, &[u8]) =
    (51, 40, include_bytes!("splash/mark_nerdnos_51x40.rgb565a"));
static MARK_NERDQX: (i32, i32, &[u8]) =
    (86, 38, include_bytes!("splash/mark_nerdqx_86x38.rgb565a"));
static MARK_BITAXE: (i32, i32, &[u8]) =
    (92, 40, include_bytes!("splash/mark_bitaxe_92x40.rgb565a"));
static SPLASH_OLED: &[u8] = include_bytes!("splash/oled_128x32.bits");

/// One frame: the splash for the first seconds, then the screens.
/// Returns how long until the next frame.
fn draw(d: &mut display::Display, ui: &mut screens::Ui) -> Duration {
    if diag::uptime_s() < SPLASH_S {
        draw_splash(d);
        return Duration::from_millis(500);
    }
    ui.frame(d)
}

/// The wordmark with the model and firmware version under it, in the
/// board family's colour (as the web UI's type plate).
fn draw_splash(d: &mut display::Display) {
    let cfg = config::get();
    let profile = board::profile();
    let version = env!("CARGO_PKG_VERSION");
    match d {
        display::Display::Lcd(l) => {
            use display::*;
            l.orient(
                cfg.flipscreen ^ crate::board::profile().display_flip,
                cfg.invertscreen,
            );
            l.blit(SPLASH_LCD);
            let col = if profile.name.starts_with("bitaxe") {
                BTC
            } else {
                PLASMA
            };
            let mark = match profile.name {
                "nerdqaxe-plusplus" | "nerdqaxe-plusplus-rev7" => Some(MARK_NQPP),
                "nerdoctaxe-gamma" => Some(MARK_OCTAXE),
                "nerdaxe-gamma" => Some(MARK_NERDAXE),
                "nerdaxe-gaia" => Some(MARK_GAIA),
                "nerdnos" => Some(MARK_NERDNOS),
                "q1370" | "q1373" => Some(MARK_NERDQX),
                "bitaxe-naja-duo" | "bitaxe-gamma-hex" => Some(MARK_BITAXE),
                _ => None,
            };
            if let Some((w, h, data)) = mark {
                // Centred where the model name would stand.
                l.canvas()
                    .sprite((WIDTH as i32 - w) / 2, 125 - h / 2, w, h, data);
            } else {
                let model = profile.model.to_ascii_uppercase();
                l.text((WIDTH - text_width(&model, 2)) / 2, 122, 2, col, &model);
            }
            let line = format!("FIRMWARE {version}");
            l.text((WIDTH - text_width(&line, 1)) / 2, 148, 1, GREY, &line);
        }
        display::Display::Oled(o) => {
            o.orient(
                cfg.flipscreen ^ crate::board::profile().display_flip,
                cfg.invertscreen,
            );
            o.bitmap(SPLASH_OLED);
            let line = format!("{} {version}", profile.model.to_ascii_uppercase());
            let w = display::text_width(&line, 1).min(128);
            o.text((128 - w) / 2, 23, 1, &line);
        }
    }
    if let Err(e) = match d {
        display::Display::Lcd(l) => l.flush().map_err(|e| e.to_string()),
        display::Display::Oled(o) => o.flush(),
    } {
        log::warn!("display: splash: {e}");
    }
}

fn main() {
    sys::link_patches();
    diag::init_logger();
    diag::start_log_capture();
    diag::mark_boot();
    info!(
        "{} boot, reset reason {}",
        api::VERSION,
        diag::reset_reason()
    );

    let peripherals = Peripherals::take().expect("peripherals");
    let sysloop = EspSystemEventLoop::take().expect("event loop");
    let nvs = EspDefaultNvsPartition::take().expect("nvs");
    if let Err(e) = config::init(nvs.clone()) {
        log::error!("config init failed: {e}");
    }
    // On the main task, before any thread with a PSRAM stack exists: it
    // reads the otadata partition from flash.
    update::init_ota_state();

    // The fallback for boards that never report a chain (the devkit, chips
    // off on purpose): the image counts as good after three minutes up.
    std::thread::Builder::new()
        .stack_size(3072)
        .spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(180));
            update::mark_app_valid("three minutes up");
        })
        .ok();
    set_step("BOARD BUS");
    board::init_bus(peripherals.i2c0);
    // Chips in reset and the core rail off before USB, Wi-Fi and the rest
    // take their seconds: a reset leaves the board as it was.
    board::quiesce();
    set_step("DISPLAY");
    let mut d = display::Display::new();
    let crashed = diag::log_core_dump();
    if let Some(mut d) = d.take() {
        diag::spawn_psram("display", 16 * 1024, move || {
            // Keys, active low: GPIO0 (boot key) on every board; GPIO14
            // (page key) next to the LCD on the Nerd boards.
            let page_key = matches!(d, display::Display::Lcd(_));
            let keys: &[i32] = if page_key { &[0, 14] } else { &[0] };
            for &k in keys {
                unsafe {
                    sys::gpio_set_direction(k, sys::gpio_mode_t_GPIO_MODE_INPUT);
                    sys::gpio_set_pull_mode(k, sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY);
                }
            }
            display::touch();
            let mut ui = screens::Ui::new();
            let mut awake = true;
            let mut was_down = [false; 2];
            let mut next_frame = std::time::Instant::now();
            loop {
                for (i, &k) in keys.iter().enumerate() {
                    let down = unsafe { sys::gpio_get_level(k) } == 0;
                    if down && !was_down[i] {
                        // A press that wakes the display does nothing else.
                        if awake {
                            ui.key(if k == 14 {
                                screens::Key::Next
                            } else {
                                screens::Key::Home
                            });
                        }
                        display::touch();
                        next_frame = std::time::Instant::now();
                    }
                    was_down[i] = down;
                }
                match display::REMOTE_KEY.swap(0, std::sync::atomic::Ordering::Relaxed) {
                    1 => ui.key(screens::Key::Next),
                    2 => ui.key(screens::Key::Home),
                    _ => {}
                }
                let timeout_s = u64::from(config::get().display_timeout) * 60;
                let idle = diag::uptime_s().saturating_sub(display::last_wake());
                let want_awake = timeout_s == 0 || idle < timeout_s;
                if want_awake != awake {
                    awake = want_awake;
                    d.set_awake(awake);
                }
                if awake && std::time::Instant::now() >= next_frame {
                    next_frame = std::time::Instant::now() + draw(&mut d, &mut ui);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .expect("display thread");
    }

    // After a crash: keep USB-Serial/JTAG for a while, then continue.
    let grace = diag::jtag_grace();
    if !grace.is_zero() {
        set_step("JTAG WINDOW");
        info!(
            "last run crashed (core dump shown: {crashed}); {} s on USB-Serial/JTAG before USB OTG",
            grace.as_secs()
        );
        std::thread::sleep(grace);
    }

    set_step("USB");
    if let Err(e) = net::usb::start_usb() {
        log::error!("usb start failed: {e}");
    } else {
        // The console follows the PHY: logs are readable on the CDC port.
        unsafe { sys::tusb::wa_usb_console_to_cdc() };
    }
    // esp_netif_init is one-shot; the event loop already exists.
    unsafe {
        let rc = sys::esp_netif_init();
        if rc != sys::ESP_OK && rc != sys::ESP_ERR_INVALID_STATE {
            log::error!("esp_netif_init: {rc}");
        }
    }
    // Wait (bounded) for the host to open the NCM link before the netif
    // starts talking, so its link-up chatter is not dropped for nothing.
    for _ in 0..20 {
        if net::usb::link_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Err(e) = net::usb::start_netif() {
        log::error!("usb netif failed: {e}");
    }

    // Always: without a network in the config (or one that cannot be
    // reached after boot) Wi-Fi opens the setup hotspot.
    let cfg = config::get();
    set_step("WIFI");
    net::wifi::start(
        peripherals.modem,
        sysloop.clone(),
        nvs.clone(),
        cfg.ssid.clone(),
        cfg.wifi_pass.clone(),
        net::mdns::label(&cfg.hostname),
    );
    net::mdns::start(cfg.hostname.clone());

    set_step("API");
    let _server = match api::start() {
        Ok(s) => Some(s),
        Err(e) => {
            log::error!("api failed: {e}");
            None
        }
    };
    set_step("STRATUM");
    stratum::start();
    set_step("BOARD");
    board::start();
    history::start();
    clock::start();
    market::start();
    update::start();
    set_step("CHAIN");
    if board::devkit() {
        // A T-Display-S3 without chips: no chain, the ESP32-S3 hashes by
        // itself. Never on a board with chips (board::devkit).
        cpuminer::start();
    } else if board::profile().name == "undetected" {
        // First start without a known board: no chain until one is chosen
        // in the web UI (board::first_start).
        log::warn!("chain: not started, the board is not known yet");
    } else {
        match uart::ChipUart::new(
            peripherals.uart1,
            peripherals.pins.gpio17,
            peripherals.pins.gpio18,
        ) {
            Ok(u) => miner::start(u),
            Err(e) => log::error!("chain uart failed: {e}"),
        }
    }
    set_step("RUN");
    info!(
        "running: http on usb {} and wifi when up",
        net::usb::ip_string()
    );
    loop {
        std::thread::sleep(Duration::from_secs(5));
    }
}
