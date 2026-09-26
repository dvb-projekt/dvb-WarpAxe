//! HTTP API. Two faces on one server: the AxeOS-compatible routes a tuner
//! already speaks (`/api/system/info`, `PATCH /api/system`, restart, OTA)
//! and WarpAxe's own `/api/v1/*`, plus a small web UI at `/`.

use std::sync::atomic::Ordering;
use std::time::Duration;

use esp_idf_svc::http::server::{Configuration, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::{EspIOError, Read, Write};
use esp_idf_svc::ota::EspOta;
use serde_json::json;

use crate::{board, config, diag, miner, net, stratum};

/// The web UI: one static page on top of the JSON API.
const UI: &str = include_str!("ui/index.html");

pub const VERSION: &str = concat!("warpaxe ", env!("CARGO_PKG_VERSION"));

fn mac_str(m: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

/// The AxeOS `/api/system/info` shape, as far as a tuner reads it.
pub fn system_info() -> serde_json::Value {
    let cfg = config::get();
    let n = net::state();
    let m = miner::state();
    let st = stratum::state();
    let b = board::state();
    let profile = board::profile();
    json!({
        "boardVersion": b.board, "deviceModel": b.board,
        "nominalVoltage": profile.nominal_voltage_v, "maxPower": profile.max_power_w,
        "power": b.power_w.unwrap_or(0.0), "voltage": b.vin_mv.unwrap_or(0.0), "current": b.iout_ma.unwrap_or(0.0),
        "temp": b.temp_c.unwrap_or(0.0), "vrTemp": b.vr_temp_c.unwrap_or(0),
        "overheat_mode": u8::from(b.overheat), "fanrpm": b.fan_rpm,
        "hashRate": m.hashrate_ghs, "hashRate_10m": m.hashrate_ghs, "hashRate_now": m.hashrate_now_ghs,
        "expectedHashrate": f64::from(board::chip().small_cores) * f64::from(m.frequency_hz) / 1e9,
        "bestDiff": "0", "bestSessionDiff": "0",
        "freeHeap": diag::free_heap(),
        "coreVoltage": cfg.core_voltage,
        "coreVoltageActual": b.vout_mv.map(|v| profile.core_mv_for_vout(v / 1000.0)).unwrap_or(0),
        "frequency": cfg.frequency,
        "ssid": cfg.ssid, "macAddr": mac_str(net::usb::base_mac()),
        "hostname": cfg.hostname,
        "wifiStatus": if n.wifi_connected { "Connected!" } else { "Not connected" },
        "wifiRSSI": n.wifi_rssi.unwrap_or(0),
        "usbStatus": if n.usb_mounted { "Connected!" } else { "Not connected" },
        "sharesAccepted": st.shares_accepted, "sharesRejected": st.shares_rejected,
        "staleAfterCleanHist": st.stale_after_clean_hist, "sharesRejectedReasons": st.reject_reasons.iter().map(|(m, c)| json!({"message": m, "count": c})).collect::<Vec<_>>(),
        "uptimeSeconds": diag::uptime_s(),
        "asicCount": m.chips, "smallCoreCount": board::chip().small_cores, "ASICModel": board::chip().name,
        "chainPhase": m.phase, "frequencyActual": m.frequency_hz / 1_000_000,
        "stratumURL": cfg.stratum_url, "stratumPort": cfg.stratum_port, "stratumUser": cfg.stratum_user,
        "isUsingFallbackStratum": false,
        "poolDifficulty": stratum::state().difficulty,
        "stratumConnected": stratum::state().authorized,
        "version": VERSION, "runningPartition": running_partition(),
        "fanspeed": if b.has_thermal { b.fan_pct } else { cfg.fanspeed }, "autofanspeed": cfg.autofanspeed,
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
        "stratum": stratum::state(),
        "miner": miner::state(),
        "board": board::state(),
        "display": json!({"asleep": crate::display::asleep(), "timeout_min": config::get().display_timeout}),
        "config": config::get().redacted(),
    })
}

fn read_body(
    req: &mut Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
    max: usize,
) -> Result<Vec<u8>, EspIOError> {
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

fn reply_json(
    req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
    status: u16,
    v: &serde_json::Value,
) -> Result<(), EspIOError> {
    let body = v.to_string();
    let mut r = req.into_response(
        status,
        None,
        &[
            ("Content-Type", "application/json"),
            ("Access-Control-Allow-Origin", "*"),
        ],
    )?;
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
        let mut r = req.into_response(
            200,
            None,
            &[
                ("Content-Type", "text/html; charset=utf-8"),
                ("Cache-Control", "no-cache"),
            ],
        )?;
        r.write_all(UI.as_bytes())?;
        Ok::<(), EspIOError>(())
    })?;

    server.fn_handler("/api/system/info", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        reply_json(req, 200, &system_info())
    })?;

    // The captured IDF log ring (what the display's log tail shows),
    // oldest first, one line each.
    server.fn_handler("/api/v1/log", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let count = unsafe { esp_idf_svc::sys::tusb::wa_log_count() } as usize;
        let mut out = String::new();
        for i in 0..count {
            let mut buf = [0u8; 160];
            unsafe {
                esp_idf_svc::sys::tusb::wa_log_line(
                    i as i32,
                    buf.as_mut_ptr() as *mut core::ffi::c_char,
                    buf.len(),
                )
            };
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            out.push_str(&String::from_utf8_lossy(&buf[..end]));
            out.push('\n');
        }
        let mut r = req.into_response(200, None, &[("Content-Type", "text/plain")])?;
        r.write_all(out.as_bytes())?;
        Ok::<(), anyhow::Error>(())
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
                    crate::display::touch();
                    log::info!(
                        "config patched: {:?}",
                        v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())
                    );
                    if v.get("frequency").is_some() {
                        miner::request_frequency(cfg.frequency);
                    }
                    if v.get("coreVoltage").is_some() {
                        board::request_core_mv(cfg.core_voltage);
                    }
                    reply_json(
                        req,
                        200,
                        &serde_json::to_value(cfg.redacted()).unwrap_or_default(),
                    )
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
        r.write_all(
            format!("ota: {n} bytes accepted, flashing, restarting when done\n").as_bytes(),
        )?;
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

fn bootloader(
    req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
) -> Result<(), EspIOError> {
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
        update
            .write_all(chunk)
            .map_err(|e| anyhow::anyhow!("write: {e:?}"))?;
    }
    log::info!("ota: verifying");
    update.complete()?;
    Ok(())
}
