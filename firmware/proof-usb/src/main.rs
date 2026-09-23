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

fn draw(d: &mut display::Display, mac: [u8; 6], step: &str, err: Option<i32>) {
    use display::*;
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
            "IP 192.168.7.1  HITS {}  RX {}  TX {}  UP {}S",
            HITS.load(Ordering::Relaxed),
            usb_net::RX_FRAMES.load(Ordering::Relaxed),
            usb_net::TX_FRAMES.load(Ordering::Relaxed),
            uptime_s()
        ),
    );
    d.text(4, 58, 1, GREY, &format!("MAC {}  NO WIFI  GET /BOOTLOADER TO REFLASH", mac_str(mac)));
    d.log_tail(72, 8, GREY);
    if d.flush().is_err() {
        log::warn!("display flush failed");
    }
}

/// Show the failure and stay alive so the screen can be read.
fn stuck(d: &mut display::Display, mac: [u8; 6], step: &str, code: i32) -> ! {
    log::error!("step {step} failed: {code}");
    loop {
        draw(d, mac, step, Some(code));
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
    // Give the host three seconds to open the USB-Serial/JTAG console
    // before anything worth reading happens.
    std::thread::sleep(Duration::from_millis(3000));
    info!("WarpAxe USB proof: boot");
    info!("free heap {} B, free PSRAM {} B", unsafe { sys::esp_get_free_heap_size() }, unsafe {
        sys::heap_caps_get_free_size(sys::MALLOC_CAP_SPIRAM)
    });

    let mac = usb_net::base_mac();
    info!("step 1: display");
    let mut d = match display::Display::new() {
        Ok(d) => d,
        Err(e) => {
            log::error!("display init failed: {e:?}");
            loop {
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
    };
    info!("step 1 ok");
    draw(&mut d, mac, "1 DISPLAY OK", None);
    std::thread::sleep(Duration::from_millis(1500));

    if let Err(e) = usb_net::start_usb(mac) {
        stuck(&mut d, mac, "2 USB", e.code());
    }
    info!("usb: NCM + CDC up");
    draw(&mut d, mac, "2 USB OK", None);
    std::thread::sleep(Duration::from_millis(1500));

    if let Err(e) = usb_net::start_netif(mac) {
        stuck(&mut d, mac, "3 NETIF", e.code());
    }
    info!("netif 192.168.7.1/24 with DHCP server");
    draw(&mut d, mac, "3 NETIF OK", None);
    std::thread::sleep(Duration::from_millis(1500));

    let _server = match http_server(mac) {
        Ok(s) => s,
        Err(e) => stuck(&mut d, mac, "4 HTTP", e.0.code()),
    };
    info!("http: listening on 192.168.7.1:80");

    loop {
        draw(&mut d, mac, "4 HTTP OK", None);
        std::thread::sleep(Duration::from_millis(1000));
    }
}
