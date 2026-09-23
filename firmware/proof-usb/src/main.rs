//! WarpAxe stage-B hardware proof on a LilyGO T-Display-S3.
//!
//! What it proves, in order: the ST7789 lights up over the i80 bus; the
//! USB-C socket comes up as a CDC-NCM network adapter plus a CDC-ACM
//! console (so the USB-Serial/JTAG console is gone, on purpose); the
//! board hands the host an address by DHCP and answers HTTP on
//! 192.168.7.1; and `GET /bootloader` puts the chip into ROM download
//! mode so it can be re-flashed without a finger on BOOT. No Wi-Fi.

mod display;
mod font;
mod usb_net;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use esp_idf_svc::http::server::{Configuration, EspHttpServer};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::{EspIOError, Write};
use esp_idf_svc::sys;
use log::info;

static HITS: AtomicU32 = AtomicU32::new(0);
static BOOT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn uptime_s() -> u64 {
    BOOT.get().map_or(0, |t| t.elapsed().as_secs())
}

fn mac_str(m: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

fn status_json(mac: [u8; 6]) -> String {
    format!(
        concat!(
            "{{\"device\":\"warpaxe-proof-usb\",\"board\":\"LilyGO T-Display-S3\",",
            "\"mac\":\"{}\",\"uptime_s\":{},\"usb_mounted\":{},",
            "\"rx_frames\":{},\"tx_frames\":{},\"http_hits\":{},\"wifi\":false}}\n"
        ),
        mac_str(mac),
        uptime_s(),
        usb_net::mounted(),
        usb_net::RX_FRAMES.load(Ordering::Relaxed),
        usb_net::TX_FRAMES.load(Ordering::Relaxed),
        HITS.load(Ordering::Relaxed),
    )
}

fn http_server(mac: [u8; 6]) -> Result<EspHttpServer<'static>, EspIOError> {
    let mut server = EspHttpServer::new(&Configuration::default())?;
    server
        .fn_handler("/", Method::Get, move |req| {
            HITS.fetch_add(1, Ordering::Relaxed);
            let body = status_json(mac);
            let mut r = req.into_response(200, Some("OK"), &[("Content-Type", "application/json")])?;
            r.write_all(body.as_bytes())?;
            Ok::<(), EspIOError>(())
        })
        ?;
    server
        .fn_handler("/api/system/info", Method::Get, move |req| {
            HITS.fetch_add(1, Ordering::Relaxed);
            // The few AxeOS fields a tuner reads first; enough to be seen.
            let body = format!(
                "{{\"hostname\":\"warpaxe-proof\",\"macAddr\":\"{}\",\"version\":\"proof-usb 0.1\",\"ASICModel\":\"none\",\"uptimeSeconds\":{},\"hashRate\":0}}\n",
                mac_str(mac),
                uptime_s()
            );
            let mut r = req.into_response(200, Some("OK"), &[("Content-Type", "application/json")])?;
            r.write_all(body.as_bytes())?;
            Ok::<(), EspIOError>(())
        })
        ?;
    server
        .fn_handler("/bootloader", Method::Get, |req| {
            let mut r = req.into_ok_response()?;
            r.write_all(b"rebooting into ROM download mode; flash me now\n")?;
            r.flush()?;
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                usb_net::reboot_to_bootloader();
            });
            Ok::<(), EspIOError>(())
        })
        ?;
    Ok(server)
}

static STEP: std::sync::Mutex<(&'static str, Option<i32>)> = std::sync::Mutex::new(("0 BOOT", None));

fn set_step(step: &'static str) {
    *STEP.lock().unwrap() = (step, None);
    info!("step: {step}");
}

fn draw(d: &mut display::Display, mac: [u8; 6]) {
    use display::*;
    let (step, err) = *STEP.lock().unwrap();
    d.clear(NAVY);
    d.text(4, 4, 2, WHITE, "WARPAXE USB PROOF");
    match err {
        Some(code) => d.text(4, 26, 2, RED, &format!("ERR {step} {code}")),
        None => {
            let (txt, col) = if usb_net::mounted() {
                ("MOUNTED", GREEN)
            } else {
                ("WAITING", ORANGE)
            };
            d.text(4, 26, 2, col, &format!("{step} USB {txt}"));
        }
    }
    d.text(
        4,
        48,
        1,
        WHITE,
        &format!(
            "192.168.7.1 HITS {} RX {} TX {} DROP {} FAIL {} UP {}S",
            HITS.load(Ordering::Relaxed),
            usb_net::RX_FRAMES.load(Ordering::Relaxed),
            usb_net::TX_FRAMES.load(Ordering::Relaxed),
            usb_net::TX_DROPPED.load(Ordering::Relaxed),
            usb_net::TX_FAILED.load(Ordering::Relaxed),
            uptime_s()
        ),
    );
    d.text(4, 58, 1, GREY, &format!("MAC {}  NO WIFI  B ON CDC = BOOTLOADER", mac_str(mac)));
    d.log_tail(72, 10, GREY);
    if d.flush().is_err() {
        log::warn!("display flush failed");
    }
}

/// Show the failure and stay alive so the screen can be read.
fn stuck(step: &'static str, code: i32) -> ! {
    log::error!("step {step} failed: {code}");
    *STEP.lock().unwrap() = (step, Some(code));
    loop {
        std::thread::sleep(Duration::from_millis(1000));
    }
}

fn main() {
    sys::link_patches();
    // First thing, before any log or driver: backlight on. A lit but empty
    // screen means main() runs at all.
    unsafe {
        sys::gpio_reset_pin(38);
        sys::gpio_set_direction(38, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
        sys::gpio_set_level(38, 1);
    }
    esp_idf_svc::log::EspLogger::initialize_default();
    unsafe { sys::tusb::wa_log_capture_start() };
    let _ = BOOT.set(Instant::now());
    std::thread::sleep(Duration::from_millis(1000));
    info!("WarpAxe USB proof: boot");
    info!("free heap {} B, free PSRAM {} B", unsafe { sys::esp_get_free_heap_size() }, unsafe {
        sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM)
    });

    let mac = usb_net::base_mac();
    set_step("1 DISPLAY");
    let mut d = match display::Display::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("display init failed: {e:?}");
            loop {
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
    };
    // The screen is painted by its own thread from here on, so a hang in
    // any later step still shows the log tail.
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || loop {
            draw(&mut d, mac);
            std::thread::sleep(Duration::from_millis(500));
        })
        .expect("display thread");
    // A crash in the previous run leaves a core dump in flash: show it,
    // and hold so it can be photographed.
    let crashed = unsafe { sys::tusb::wa_coredump_log() } != 0;
    set_step("1 DISPLAY OK");
    std::thread::sleep(Duration::from_millis(if crashed { 45_000 } else { 1500 }));

    set_step("2 USB");
    if let Err(e) = usb_net::start_usb(mac) {
        stuck("2 USB", e.code());
    }
    set_step("2 USB OK");
    // Wait for the host to open the NCM data interface (up to 20 s), so the
    // netif's link-up chatter has somewhere to go. Without a host we go on
    // anyway; transmit drops until the link exists.
    for _ in 0..40 {
        if usb_net::net_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    info!("ncm link ready: {}", usb_net::net_ready());

    set_step("3 NETIF");
    if let Err(e) = usb_net::start_netif(mac) {
        stuck("3 NETIF", e.code());
    }
    set_step("3 NETIF OK");
    std::thread::sleep(Duration::from_millis(1500));

    set_step("4 HTTP");
    let _server = match http_server(mac) {
        Ok(s) => s,
        Err(e) => stuck("4 HTTP", e.0.code()),
    };
    set_step("4 HTTP OK");
    loop {
        std::thread::sleep(Duration::from_millis(1000));
    }
}
