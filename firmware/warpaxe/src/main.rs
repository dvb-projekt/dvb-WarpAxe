//! WarpAxe firmware, stage B: everything but the hashing.
//!
//! Boot order matters and is deliberate: diagnostics first (a crash in the
//! previous run is shown and, after a crash, the USB PHY stays on
//! USB-Serial/JTAG for a while so the board can always be reflashed),
//! then config, display, the network interfaces as a list (USB always,
//! Wi-Fi when configured), then the API.

mod api;
mod config;
mod diag;
mod display;
mod font;
mod net;

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

fn draw(d: &mut display::Display) {
    use display::*;
    let cfg = config::get();
    let n = net::state();
    d.clear(NAVY);
    d.text(4, 4, 2, WHITE, &format!("WARPAXE {}", cfg.hostname));
    let step = *STEP.lock().unwrap();
    let (usb, col) = if n.usb_mounted { ("USB UP", GREEN) } else { ("USB -", ORANGE) };
    d.text(4, 26, 2, col, &format!("{step}  {usb}"));
    d.text(
        4,
        48,
        1,
        WHITE,
        &format!(
            "USB {}  WIFI {}  UP {}S  HITS {}",
            n.usb_ip.unwrap_or_else(|| "-".into()),
            n.wifi_ip.unwrap_or_else(|| "-".into()),
            diag::uptime_s(),
            diag::HTTP_HITS.load(std::sync::atomic::Ordering::Relaxed)
        ),
    );
    d.text(4, 58, 1, GREY, &format!("{}  RESET {}  POOL {}:{}", api::VERSION, diag::reset_reason(), cfg.stratum_url, cfg.stratum_port));
    match diag::last_crash() {
        Some(c) => {
            d.text(4, 68, 1, RED, &format!("LAST CRASH {}", &c[..c.len().min(46)]));
            d.log_tail(78, 9, GREY);
        }
        None => d.log_tail(72, 10, GREY),
    }
    if d.flush().is_err() {
        log::warn!("display flush failed");
    }
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    diag::start_log_capture();
    diag::mark_boot();
    info!("{} boot, reset reason {}", api::VERSION, diag::reset_reason());

    let peripherals = Peripherals::take().expect("peripherals");
    let sysloop = EspSystemEventLoop::take().expect("event loop");
    let nvs = EspDefaultNvsPartition::take().expect("nvs");
    if let Err(e) = config::init(nvs.clone()) {
        log::error!("config init failed: {e}");
    }

    set_step("DISPLAY");
    let mut d = display::Display::new().ok();
    let crashed = diag::log_core_dump();
    if let Some(mut d) = d.take() {
        std::thread::Builder::new()
            .stack_size(8192)
            .spawn(move || loop {
                draw(&mut d);
                std::thread::sleep(Duration::from_millis(500));
            })
            .expect("display thread");
    }

    // After a crash: keep USB-Serial/JTAG for a while, then continue.
    let grace = diag::jtag_grace();
    if !grace.is_zero() {
        set_step("JTAG WINDOW");
        info!("last run crashed (core dump shown: {crashed}); {} s on USB-Serial/JTAG before USB OTG", grace.as_secs());
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

    let cfg = config::get();
    if !cfg.ssid.is_empty() {
        set_step("WIFI");
        net::wifi::start(peripherals.modem, sysloop.clone(), nvs.clone(), cfg.ssid.clone(), cfg.wifi_pass.clone());
    }

    set_step("API");
    let _server = match api::start() {
        Ok(s) => Some(s),
        Err(e) => {
            log::error!("api failed: {e}");
            None
        }
    };
    set_step("RUN");
    info!("running: http on usb {} and wifi when up", net::usb::ip_string());
    loop {
        std::thread::sleep(Duration::from_secs(5));
    }
}
