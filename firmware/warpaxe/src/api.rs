//! HTTP API. Two faces on one server: the AxeOS-compatible routes a tuner
//! already speaks (`/api/system/info`, `PATCH /api/system`, restart, OTA)
//! and WarpAxe's own `/api/v1/*`. No hashing yet, so the mining fields
//! are honest zeros.

use std::sync::atomic::Ordering;
use std::time::Duration;

use esp_idf_svc::http::server::{Configuration, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::{EspIOError, Read, Write};
use esp_idf_svc::ota::EspOta;
use serde_json::json;

use crate::{config, diag, net};

pub const VERSION: &str = concat!("warpaxe ", env!("CARGO_PKG_VERSION"));

fn mac_str(m: [u8; 6]) -> String {
    format!("{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}", m[0], m[1], m[2], m[3], m[4], m[5])
}

/// The AxeOS `/api/system/info` shape, as far as a tuner reads it.
pub fn system_info() -> serde_json::Value {
    let cfg = config::get();
    let n = net::state();
    json!({
        "power": 0.0, "voltage": 0.0, "current": 0.0,
        "temp": 0.0, "vrTemp": 0.0,
        "hashRate": 0.0, "expectedHashrate": 0.0,
        "bestDiff": "0", "bestSessionDiff": "0",
        "freeHeap": diag::free_heap(),
        "coreVoltage": cfg.core_voltage, "coreVoltageActual": 0,
        "frequency": cfg.frequency,
        "ssid": cfg.ssid, "macAddr": mac_str(net::usb::base_mac()),
        "hostname": cfg.hostname,
        "wifiStatus": if n.wifi_connected { "Connected!" } else { "Not connected" },
        "wifiRSSI": n.wifi_rssi.unwrap_or(0),
        "usbStatus": if n.usb_mounted { "Connected!" } else { "Not connected" },
        "sharesAccepted": 0, "sharesRejected": 0,
        "uptimeSeconds": diag::uptime_s(),
        "asicCount": cfg.asic_count, "smallCoreCount": 0, "ASICModel": "BM1370",
        "stratumURL": cfg.stratum_url, "stratumPort": cfg.stratum_port, "stratumUser": cfg.stratum_user,
        "version": VERSION, "boardVersion": "t-display-s3", "runningPartition": running_partition(),
        "fanspeed": cfg.fanspeed, "fanrpm": 0, "autofanspeed": cfg.autofanspeed,
        "overheat_mode": 0,
        "resetReason": diag::reset_reason(),
    })
}

fn running_partition() -> String {
    EspOta::new()
        .and_then(|o| o.get_running_slot())
        .map(|s| s.label.to_string())
        .unwrap_or_else(|_| "?".into())
}

/// WarpAxe's own state document.
pub fn v1_state() -> serde_json::Value {
    let n = net::state();
    json!({
        "device": "warpaxe", "version": VERSION,
        "uptime_s": diag::uptime_s(), "reset_reason": diag::reset_reason(),
        "free_heap": diag::free_heap(),
        "net": {
            "usb": { "mounted": n.usb_mounted, "ip": n.usb_ip,
                     "rx": net::usb::RX_FRAMES.load(Ordering::Relaxed),
                     "tx": net::usb::TX_FRAMES.load(Ordering::Relaxed),
                     "dropped": net::usb::TX_DROPPED.load(Ordering::Relaxed) },
            "wifi": { "connected": n.wifi_connected, "ip": n.wifi_ip, "rssi": n.wifi_rssi },
        },
        "http_hits": diag::HTTP_HITS.load(Ordering::Relaxed),
        "last_crash": diag::last_crash(),
        "config": config::get().redacted(),
    })
}

fn read_body(req: &mut Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>, max: usize) -> Result<Vec<u8>, EspIOError> {
    let mut body = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = req.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > max {
            break;
        }
    }
    Ok(body)
}

fn reply_json(req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>, status: u16, v: &serde_json::Value) -> Result<(), EspIOError> {
    let body = v.to_string();
    let mut r = req.into_response(status, None, &[("Content-Type", "application/json"), ("Access-Control-Allow-Origin", "*")])?;
    r.write_all(body.as_bytes())?;
    Ok(())
}

pub fn start() -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&Configuration {
        stack_size: 12 * 1024,
        max_uri_handlers: 16,
        ..Default::default()
    })?;

    server.fn_handler("/", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let cfg = config::get();
        let n = net::state();
        let html = format!(
            "<!doctype html><title>{h}</title><body style=\"font-family:system-ui;background:#0b1020;color:#eee;padding:2em\"><h1>WarpAxe</h1><p>{v} on {h}, up {u} s, last reset: {r}</p><p>USB {usb} &middot; Wi-Fi {wifi}</p><p><a href=/api/system/info style=color:#8cf>/api/system/info</a> &middot; <a href=/api/v1/state style=color:#8cf>/api/v1/state</a></p></body>",
            h = cfg.hostname, v = VERSION, u = diag::uptime_s(), r = diag::reset_reason(),
            usb = n.usb_ip.unwrap_or_else(|| "down".into()), wifi = n.wifi_ip.unwrap_or_else(|| "down".into())
        );
        let mut r = req.into_response(200, None, &[("Content-Type", "text/html")])?;
        r.write_all(html.as_bytes())?;
        Ok::<(), EspIOError>(())
    })?;

    server.fn_handler("/api/system/info", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        reply_json(req, 200, &system_info())
    })?;

    server.fn_handler("/api/v1/state", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        reply_json(req, 200, &v1_state())
    })?;

    server.fn_handler("/api/system", Method::Patch, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let body = read_body(&mut req, 4096)?;
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) => match config::patch(&v) {
                Ok(cfg) => {
                    log::info!("config patched: {:?}", v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()));
                    reply_json(req, 200, &serde_json::to_value(cfg.redacted()).unwrap_or_default())
                }
                Err(e) => reply_json(req, 400, &json!({"error": e.to_string()})),
            },
            Err(e) => reply_json(req, 400, &json!({"error": format!("bad json: {e}")})),
        }
    })?;

    server.fn_handler("/api/system/restart", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let mut r = req.into_ok_response()?;
        r.write_all(b"restarting\n")?;
        r.flush()?;
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            diag::restart();
        });
        Ok::<(), EspIOError>(())
    })?;

    server.fn_handler("/api/system/OTA", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        // Take the whole image into RAM first (PSRAM has room), answer, and
        // flash on a thread of our own: esp_ota_begin erases the target
        // partition, which takes seconds and must not stall the HTTP task.
        let image = match read_body(&mut req, 4 * 1024 * 1024) {
            Ok(b) => b,
            Err(e) => {
                let mut r = req.into_response(500, None, &[])?;
                r.write_all(format!("ota: read failed: {e:?}\n").as_bytes())?;
                return Ok(());
            }
        };
        if image.len() < 4096 || image[0] != 0xE9 {
            let mut r = req.into_response(400, None, &[])?;
            r.write_all(b"ota: not an ESP image (first byte must be 0xE9)\n")?;
            return Ok(());
        }
        let n = image.len();
        log::info!("ota: {n} bytes received, flashing in background");
        let mut r = req.into_ok_response()?;
        r.write_all(format!("ota: {n} bytes accepted, flashing, restarting when done\n").as_bytes())?;
        r.flush()?;
        std::thread::Builder::new()
            .stack_size(16 * 1024)
            .spawn(move || match flash_image(&image) {
                Ok(()) => {
                    log::info!("ota: written and verified, restarting");
                    std::thread::sleep(Duration::from_millis(500));
                    diag::restart();
                }
                Err(e) => log::error!("ota: failed: {e}"),
            })
            .ok();
        Ok::<(), EspIOError>(())
    })?;

    // Diagnostics: swallow a body and report how much arrived and how fast.
    server.fn_handler("/api/v1/sink", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let t0 = std::time::Instant::now();
        let mut total = 0usize;
        let mut buf = [0u8; 1024];
        loop {
            match req.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => total += n,
                Err(e) => {
                    log::warn!("sink: read error after {total} bytes: {e:?}");
                    break;
                }
            }
        }
        let ms = t0.elapsed().as_millis();
        log::info!("sink: {total} bytes in {ms} ms");
        reply_json(req, 200, &json!({"bytes": total, "ms": ms}))
    })?;

    // The flash-back path: into ROM download mode, PHY handed to Serial/JTAG.
    server.fn_handler("/api/v1/bootloader", Method::Post, |req| bootloader(req))?;
    server.fn_handler("/bootloader", Method::Get, |req| bootloader(req))?;

    Ok(server)
}

fn bootloader(req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>) -> Result<(), EspIOError> {
    let mut r = req.into_ok_response()?;
    r.write_all(b"rebooting into ROM download mode; clear RTC_CNTL_OPTION1_REG before the reset that should start the app\n")?;
    r.flush()?;
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(300));
        diag::reboot_to_bootloader();
    });
    Ok(())
}

/// Write an app image to the other OTA slot and make it bootable.
fn flash_image(image: &[u8]) -> anyhow::Result<()> {
    let mut ota = EspOta::new()?;
    log::info!("ota: erasing target slot");
    let mut update = ota.initiate_update()?;
    log::info!("ota: writing {} bytes", image.len());
    for chunk in image.chunks(4096) {
        update.write_all(chunk).map_err(|e| anyhow::anyhow!("write: {e:?}"))?;
    }
    log::info!("ota: verifying");
    update.complete()?;
    Ok(())
}
