//! HTTP API. Two faces on one server: the AxeOS-compatible routes a tuner
//! already speaks (`/api/system/info`, `PATCH /api/system`, restart, OTA)
//! and WarpAxe's own `/api/v1/*`, plus a small web UI at `/`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use esp_idf_svc::http::server::{Configuration, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::{EspIOError, Write};
use esp_idf_svc::ota::EspOta;
use serde_json::json;

use crate::{board, config, diag, display, miner, net, stratum};

/// The web UI's files: fonts (OFL, Latin subsets), the wordmark and the
/// icon, all in the firmware so the page looks the same offline.
fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match name {
        "favicon.svg" => (include_bytes!("ui/assets/favicon.svg"), "image/svg+xml"),
        "wordmark.webp" => (include_bytes!("ui/assets/wordmark.webp"), "image/webp"),
        "bitcoin.pdf" => (include_bytes!("ui/assets/bitcoin.pdf"), "application/pdf"),
        "inter.woff2" => (include_bytes!("ui/assets/inter.woff2"), "font/woff2"),
        "space-grotesk-700.woff2" => (
            include_bytes!("ui/assets/space-grotesk-700.woff2"),
            "font/woff2",
        ),
        "jetbrains-mono-400.woff2" => (
            include_bytes!("ui/assets/jetbrains-mono-400.woff2"),
            "font/woff2",
        ),
        _ => return None,
    })
}

/// The web UI: one static page on top of the JSON API.
const UI: &str = include_str!("ui/index.html");

pub const VERSION: &str = concat!("warpaxe ", env!("CARGO_PKG_VERSION"));

/// The chip name the API reports. The devkit (a profile without chips)
/// names what actually hashes, so an AxeOS-style scanner lists it as
/// what it is and not as a BM1370 (Warppool, Oliver, 2026-10-05).
fn asic_model() -> &'static str {
    if board::devkit() {
        return "ESP32-S3 SHA";
    }
    board::chip().name
}

/// The answer of every update endpoint on the T-Display-S3 (see
/// update::request_install): it is installed over USB only.
const DEVKIT_NO_UPDATE: &str =
    "the T-Display-S3 is installed with the web flasher only (no updates over the air)";

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
    #[allow(unused_mut)]
    let mut v = json!({
        "boardVersion": b.board, "deviceModel": b.board, "boardName": profile.model,
        "boardDetect": board::detect_info(),
        "boardModels": warpaxe_board::profile::ALL.iter().map(|p| json!({"name": p.name, "model": if p.asic_count == 0 { "WarpMiner (T-Display-S3)" } else { p.model }, "experimental": p.asic_count == 0, "tested": warpaxe_board::detect::TESTED.contains(&p.name)})).collect::<Vec<_>>(),
        "nominalVoltage": profile.nominal_voltage_v, "maxPower": profile.max_power_w,
        "power": b.power_w, "voltage": b.vin_mv, "current": b.iout_ma,
        "temp": b.temp_c, "vrTemp": b.vr_temp_c.unwrap_or(0),
        "overheat_mode": u8::from(b.overheat), "overheatLatched": b.overheat_latched, "overheatTrips": b.overheat_trips, "overheatCause": b.overheat_cause, "fanWarmup": b.fan_warmup, "faultCause": fault_cause(&b), "coreCurrentA": b.core_current_a, "coreCurrentWarnA": b.core_current_warn_a, "power_fault": b.power_fault, "powerFaultCodes": b.power_fault_codes, "shutdown": b.shutdown, "chainStopped": board::chain_stopped(), "fanrpm": b.fan_rpm,
        "vrOverheatTemp": profile.vr_max_c.round() as u32, "boardError": b.last_error,
        "fanrpm2": b.fan2_rpm, "fanspeed2": b.fan2_pct, "vrPart": b.vr_part,
        "fans": nerd_fans(&cfg, profile, &b),
        "invertfanpolarity": u8::from(cfg.invertfanpolarity), "minFanSpeed": cfg.min_fan_speed,
        "pidTargetTemp": if cfg.pid_target_temp > 0 { u32::from(cfg.pid_target_temp) } else { profile.fan_target_c.round() as u32 },
        // ESP-Miner's name for the fan target; tuners clamp their own target to it.
        "temptarget": if cfg.pid_target_temp > 0 { u32::from(cfg.pid_target_temp) } else { profile.fan_target_c.round() as u32 },
        "pidP": cfg.pid_p, "pidI": cfg.pid_i, "pidD": cfg.pid_d,
        "autofanspeed2": cfg.autofanspeed2, "manualFanSpeed2": cfg.manual_fan_speed2, "manualFanSpeed": cfg.fanspeed,
        "vrTargetTemp": vr_target(&cfg), "linkFans": cfg.link_fans,
        "stratumSuggestedDifficulty": cfg.suggested_difficulty, "jobInterval": cfg.job_interval_ms,
        "overheat_temp": if cfg.overheat_temp > 0 { u32::from(cfg.overheat_temp) } else { profile.max_temp_c.round() as u32 },
        "hashRate": m.hashrate_ghs, "hashRate_10m": m.hashrate_ghs, "hashRate_now": m.hashrate_now_ghs,
        "hashrateMonitor": { "asics": m.hashrate_monitor }, "errorPercentage": m.error_percent, "errorPercentage1m": m.error_percent_1m,
        "expectedHashrate": f64::from(board::chip().small_cores) * f64::from(m.frequency_hz) * f64::from(m.chips.max(1)) / 1e9,
        "bestDiff": config::best_diff(), "bestSessionDiff": m.best_session_diff,
        "freeHeap": diag::free_heap(), "freeHeapInternal": diag::free_heap_internal(),
        "coreVoltage": cfg.core_voltage,
        "coreVoltageActual": b.vout_mv.map(|v| profile.core_mv_for_vout(v / 1000.0)).unwrap_or(0),
        // The lowest core voltage the regulator holds cleanly, mV; a
        // PATCH below it is refused (400). WarpTune's floor for every
        // voltage search.
        "regulatorMinMv": profile.regulator_min_core_mv(),
        // A requested core voltage the regulator refused (mV), null
        // while the last request went through; `coreVoltage` still
        // shows the configured value, `coreVoltageActual` the real one.
        "coreVoltageRefused": b.core_mv_refused,
        // What the regulator answered at boot, before init wrote it:
        // after a power cycle its window is what the straps set.
        "vrProbe": b.vr_probe,
        "frequency": cfg.frequency,
        "ssid": cfg.ssid, "macAddr": mac_str(net::usb::base_mac()),
        "hostname": cfg.hostname,
        "wifiStatus": if n.wifi_connected { "Connected!" } else { "Not connected" },
        "wifiRSSI": n.wifi_rssi.unwrap_or(0),
        "usbStatus": if n.usb_mounted { "Connected!" } else { "Not connected" },
        "sharesAccepted": st.shares_accepted, "sharesRejected": st.shares_rejected,
        "staleAfterCleanHist": st.stale_after_clean_hist, "sharesRejectedReasons": st.reject_reasons.iter().map(|(m, c)| json!({"message": m, "count": c})).collect::<Vec<_>>(),
        "uptimeSeconds": diag::uptime_s(), "unixTime": crate::clock::unix_now(), "useNTP": u8::from(cfg.use_ntp), "ntpServer": cfg.ntp_server,
        "asicCount": m.chips, "smallCoreCount": board::chip().small_cores, "ASICModel": asic_model(),
        "chainPhase": m.phase, "frequencyActual": m.frequency_hz / 1_000_000,
        // Chips flagged silent right now, and the chain restarts after a
        // silent minute: in all, and in a row at the current core
        // voltage (two or more = the chips do not run at this voltage).
        "silentChips": m.silent_chips, "chainSilentRestarts": m.chain_restarts,
        "chainSilentRestartsSameMv": m.silent_restarts_same_mv,
        "stratumURL": cfg.stratum_url, "stratumPort": cfg.stratum_port, "stratumUser": cfg.stratum_user, "stratumTLS": cfg.stratum_tls, "fallbackStratumTLS": cfg.fallback_tls, "stratumTLSActive": st.tls,
        "stratumProtocol": cfg.stratum_protocol, "stratumV2AuthorityPubkey": cfg.stratum_v2_authority_pubkey, "stratumV2ChannelType": cfg.stratum_v2_channel_type, "stratumV2RequireAuth": cfg.stratum_v2_require_auth,
        "fallbackStratumProtocol": cfg.fallback_protocol, "fallbackStratumV2AuthorityPubkey": cfg.fallback_v2_authority_pubkey, "fallbackStratumV2ChannelType": cfg.fallback_v2_channel_type, "fallbackStratumV2RequireAuth": cfg.fallback_v2_require_auth,
        "stratumProtocolActive": if st.sv2 { "SV2" } else { "SV1" },
        "isUsingFallbackStratum": u8::from(st.using_fallback), "activePool": st.pool,
        // NerdOS keeps the fallback flag in a nested `stratum` object;
        // mirror it so scripts written for NerdOS read the same path.
        "stratum": {
            "usingFallback": st.using_fallback,
            "pools": [{
                "connected": stratum::state().authorized,
                "poolDifficulty": st.difficulty, "networkDifficulty": st.network_difficulty,
                "accepted": st.shares_accepted, "rejected": st.shares_rejected,
            }],
        },
        "useFallbackStratum": u8::from(cfg.use_fallback), "fallbackStratumURL": cfg.fallback_url, "fallbackStratumPort": cfg.fallback_port, "fallbackStratumUser": cfg.fallback_user,
        "poolDifficulty": st.difficulty,
        "blockHeight": st.block_height, "networkDifficulty": st.network_difficulty, "scriptsig": st.scriptsig,
        "coinbaseOutputs": st.coinbase_outputs.iter().map(|(v, a)| json!({"value": v, "address": a})).collect::<Vec<_>>(),
        "coinbaseValueTotalSatoshis": st.coinbase_total_sat, "coinbaseValueUserSatoshis": st.coinbase_user_sat,
        "blockFound": m.blocks_found, "updateAvailable": crate::update::state().available, "updateCheck": cfg.update_check, "timezone": cfg.timezone, "btcPrice": cfg.btc_price, "showHashCounters": cfg.show_hash_counters, "priceCurrency": cfg.price_currency, "displayLanguage": cfg.display_language,
        "stratumConnected": stratum::state().authorized,
        "version": VERSION, "gitHash": env!("WARPAXE_GIT"), "runningPartition": running_partition(),
        "fanspeed": if b.has_thermal { b.fan_pct } else { cfg.fanspeed }, "autofanspeed": u8::from(cfg.autofanspeed),
        "resetReason": diag::reset_reason(),
    });
    if board::devkit() {
        v["cpuMiner"] = json!(crate::cpuminer::info());
    }
    v
}

/// NerdOS's `fans` array on the two-channel Nerd boards, the place a
/// tuner reads the shutdown and fan-target temperatures from: channel 0
/// on the chips, channel 1 on the regulator. Other boards leave it out,
/// so a tuner keeps its stock-firmware defaults for them.
fn nerd_fans(
    cfg: &config::Config,
    p: &warpaxe_board::Profile,
    b: &board::BoardState,
) -> serde_json::Value {
    if p.thermal != warpaxe_board::Thermal::Tmp1075Emc2302 {
        return json!([]);
    }
    let chip_limit = if cfg.overheat_temp > 0 {
        u32::from(cfg.overheat_temp)
    } else {
        p.max_temp_c.round() as u32
    };
    let target = if cfg.pid_target_temp > 0 {
        u32::from(cfg.pid_target_temp)
    } else {
        p.fan_target_c.round() as u32
    };
    let gains = |t: u32| json!({"targetTemp": t, "p": cfg.pid_p, "i": cfg.pid_i, "d": cfg.pid_d});
    json!([
        {"label": "M2", "mode": if cfg.autofanspeed { 2 } else { 0 }, "manualSpeed": cfg.fanspeed,
         "overheatTemp": chip_limit, "pid": gains(target),
         "rpm": b.fan_rpm, "speedPerc": b.fan_pct},
        {"label": "M1", "mode": if cfg.autofanspeed2 { 3 } else { 0 }, "manualSpeed": cfg.manual_fan_speed2,
         "overheatTemp": p.vr_max_c.round() as u32, "pid": gains(vr_target(cfg)),
         "rpm": b.fan2_rpm.unwrap_or(0), "speedPerc": b.fan2_pct.unwrap_or(0)},
    ])
}

/// The regulator temperature the fans hold, °C (the second fan output on
/// the Nerd boards, the one fan with the chip on the NerdAxe boards).
fn vr_target(cfg: &config::Config) -> u32 {
    if cfg.vr_target_temp > 0 {
        u32::from(cfg.vr_target_temp)
    } else {
        board::vr_fan_target_c(board::profile()).round() as u32
    }
}

/// NerdOS `GET /api/v2/settings`: the board's own clock and voltage
/// choices, where a tuner looks for them.
/// The board's published choices for the regulator found at start.
fn choices() -> warpaxe_board::profile::Choices {
    board::profile().choices(board::state().vr_part.starts_with("Tps53667"))
}

fn settings_v2() -> Option<serde_json::Value> {
    let p = board::profile();
    let c = choices();
    if c.frequency_options.is_empty() {
        return None;
    }
    Some(json!({
        "deviceModel": p.name,
        "defaultFrequency": p.default_frequency_mhz, "defaultCoreVoltage": p.default_core_mv,
        "absMinCoreVoltage": c.abs_min_core_mv, "absMaxCoreVoltage": c.abs_max_core_mv,
        "frequencyOptions": c.frequency_options, "voltageOptions": c.voltage_options,
    }))
}

/// ESP-Miner's clock and voltage choices per chip (main/device_config.h,
/// v2.15), which AxeOS serves under `/api/system/asic`.
fn axeos_options(chip: &str) -> Option<(&'static [u32], &'static [u32], u32)> {
    match chip {
        "BM1370" => Some((
            &[400, 490, 525, 550, 600, 625, 690],
            &[1000, 1060, 1100, 1150, 1200, 1250],
            4,
        )),
        _ => None,
    }
}

/// `/api/system/asic`: NerdOS's shape (with the board's absolute limits)
/// for boards whose stock firmware publishes its own choices, AxeOS's
/// shape for Bitaxe boards. `/api/v2/settings` stays NerdOS-only: WarpTune
/// takes a device ladder from there and keeps its own table on a Bitaxe.
fn asic_options() -> Option<serde_json::Value> {
    let p = board::profile();
    let c = choices();
    if c.frequency_options.is_empty() {
        let chip = board::chip().name;
        let (f, v, domains) = axeos_options(chip).filter(|_| p.name.starts_with("bitaxe"))?;
        return Some(json!({
            "ASICModel": chip, "deviceModel": p.name, "swarmColor": "green",
            "asicCount": p.asic_count, "hashDomains": domains,
            "defaultFrequency": p.default_frequency_mhz, "frequencyOptions": f,
            "defaultVoltage": p.default_core_mv, "voltageOptions": v,
        }));
    }
    Some(json!({
        "ASICModel": asic_model(), "deviceModel": p.name, "asicCount": p.asic_count,
        "defaultFrequency": p.default_frequency_mhz, "defaultVoltage": p.default_core_mv,
        "absMaxFrequency": c.abs_max_frequency_mhz,
        "absMinVoltage": c.abs_min_core_mv, "absMaxVoltage": c.abs_max_core_mv,
        "ecoFrequency": 0, "ecoVoltage": 0,
        "frequencyOptions": c.frequency_options, "voltageOptions": c.voltage_options,
    }))
}

/// A PATCH outside the board's absolute limits is refused whole, as
/// NerdOS `validateVoltage` / `absMaxAsicFrequency` do.
fn limits_refuse(v: &serde_json::Value) -> Option<String> {
    let p = choices();
    if let Some(f) = v.get("frequency").and_then(|f| f.as_u64()) {
        if p.abs_max_frequency_mhz > 0 && f > u64::from(p.abs_max_frequency_mhz) {
            return Some(format!(
                "frequency {f} MHz above the board's {} MHz",
                p.abs_max_frequency_mhz
            ));
        }
    }
    if let Some(mv) = v.get("coreVoltage").and_then(|m| m.as_u64()) {
        // Never a core voltage below the regulator's clean range,
        // whatever the board's own choices say (`regulatorMinMv`).
        let floor = u64::from(board::profile().regulator_min_core_mv());
        if floor > 0 && mv < floor {
            return Some(format!(
                "coreVoltage {mv} mV below the regulator's {floor} mV floor"
            ));
        }
        let lo = u64::from(p.abs_min_core_mv);
        let hi = u64::from(p.abs_max_core_mv);
        if (lo > 0 && mv < lo) || (hi > 0 && mv > hi) {
            return Some(format!(
                "coreVoltage {mv} mV outside the board's {lo}–{hi} mV"
            ));
        }
    }
    expert_refuse(v)
}

/// Expert fields outside what the controllers can use sensibly.
fn expert_refuse(v: &serde_json::Value) -> Option<String> {
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64());
    for (k, lo, hi) in [
        ("pidP", 0.0, 100.0),
        ("pidI", 0.0, 10.0),
        ("pidD", 0.0, 100.0),
        ("manualFanSpeed2", 0.0, 100.0),
        ("fanspeed", 0.0, 100.0),
        ("minFanSpeed", 0.0, 100.0),
    ] {
        if let Some(x) = num(k) {
            if !(lo..=hi).contains(&x) {
                return Some(format!("{k} {x} outside {lo}–{hi}"));
            }
        }
    }
    for (k, lo, hi) in [
        ("stratumTLS", 0.0, 1.0),
        ("fallbackStratumTLS", 0.0, 1.0),
        ("vrTargetTemp", 40.0, 90.0),
        ("pidTargetTemp", 35.0, 85.0),
        ("jobInterval", 100.0, 5000.0),
    ] {
        // 0 = the default.
        if let Some(x) = num(k) {
            if x != 0.0 && !(lo..=hi).contains(&x) {
                return Some(format!("{k} {x} outside {lo}–{hi} (0 = default)"));
            }
        }
    }
    None
}

/// The app slot this boot runs from; it cannot change until the next
/// boot, so it is read once (each `EspOta` also logs its drop).
/// The pool protocol fields as tools send them: AxeOS strings ("SV1",
/// "SV2") or NerdOS numbers (0, 1). Numbers become strings, anything
/// else is refused; an authority key must parse.
fn pool_refuse(v: &mut serde_json::Value) -> Option<String> {
    for k in ["stratumProtocol", "fallbackStratumProtocol"] {
        let Some(p) = v.get(k).cloned() else {
            continue;
        };
        let norm = match &p {
            serde_json::Value::Number(n) => match n.as_u64() {
                Some(0) => "SV1",
                Some(1) => "SV2",
                _ => return Some(format!("{k}: 0 (V1) or 1 (V2)")),
            },
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("sv1") => "SV1",
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("sv2") => "SV2",
            _ => return Some(format!("{k}: SV1 or SV2")),
        };
        if let Some(o) = v.as_object_mut() {
            o.insert(k.into(), serde_json::Value::String(norm.into()));
        }
    }
    for k in [
        "stratumV2AuthorityPubkey",
        "fallbackStratumV2AuthorityPubkey",
    ] {
        if let Some(key) = v.get(k).and_then(|x| x.as_str()) {
            if !key.trim().is_empty() && warpaxe_sv2::authority_key_from_b58(key).is_err() {
                return Some(format!("{k}: not a valid Sv2 authority key (Base58Check)"));
            }
        }
    }
    for k in ["stratumV2ChannelType", "fallbackStratumV2ChannelType"] {
        if let Some(t) = v.get(k).and_then(|x| x.as_str()) {
            if t != "extended" {
                log::warn!("config: {k} {t:?} noted; WarpAxe opens extended channels");
            }
        }
    }
    None
}

fn running_partition() -> String {
    static SLOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| {
        EspOta::new()
            .and_then(|o| o.get_running_slot())
            .map(|s| s.label.to_string())
            .unwrap_or_else(|_| "?".into())
    })
    .clone()
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
        "core_dump_bytes": diag::core_dump().map(|(_, n)| n),
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

/// `key=` from a request's query string: hex with `0x`, else decimal.
fn query_num(uri: &str, key: &str) -> Option<u32> {
    let q = uri.split_once('?')?.1;
    let v = q
        .split('&')
        .find_map(|kv| kv.strip_prefix(key).and_then(|r| r.strip_prefix('=')))?;
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
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

/// Handler slots the HTTP server is given. Every `fn_handler` below
/// takes one, the captive-portal loop seven; past the limit the
/// registration fails, `start` returns the error and the device runs
/// without a web server (and without OTA: only USB brings it back).
/// Seen 2026-10-02 on a NerdQAxe++ with 41 handlers at a limit of 40.
/// Keep a margin; [`handler_count`] says how many are in use.
const MAX_URI_HANDLERS: usize = 64;
/// Handlers registered by [`start`], for the log and the check.
static HANDLERS: AtomicUsize = AtomicUsize::new(0);

/// How many handler slots [`start`] used, of [`MAX_URI_HANDLERS`].
pub fn handler_count() -> (usize, usize) {
    (HANDLERS.load(Ordering::Relaxed), MAX_URI_HANDLERS)
}

pub fn start() -> anyhow::Result<EspHttpServer<'static>> {
    let mut http = EspHttpServer::new(&Configuration {
        stack_size: 12 * 1024,
        max_uri_handlers: MAX_URI_HANDLERS,
        uri_match_wildcard: true,
        ..Default::default()
    })?;
    let mut counted = Counted(&mut http);
    let server = &mut counted;

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

    server.fn_handler("/api/v2/settings", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        match settings_v2() {
            Some(v) => reply_json(req, 200, &v),
            None => reply_json(req, 404, &json!({"error": "no options for this board"})),
        }
    })?;
    server.fn_handler("/api/system/asic", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        match asic_options() {
            Some(v) => reply_json(req, 200, &v),
            None => reply_json(req, 404, &json!({"error": "no options for this board"})),
        }
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

    // Diagnostics, not in the UI: the raw registers of one chip, read
    // between the work. ?chip=0&from=0x80&to=0x9f (hex or decimal, at
    // most 64 registers); a register the chip did not answer is null.
    server.fn_handler("/api/v1/chip/regs", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let uri = req.uri().to_string();
        let chip = query_num(&uri, "chip").unwrap_or(0);
        let from = query_num(&uri, "from").unwrap_or(0x80);
        let to = query_num(&uri, "to").unwrap_or(0x9f);
        if chip > 255 || from > 255 || to > 255 || to < from || to - from >= 64 {
            return reply_json(
                req,
                400,
                &json!({"error": "chip, from, to: 0..=255, from <= to, at most 64 registers"}),
            );
        }
        if !miner::request_reg_scan(chip as u8, from as u8, to as u8) {
            return reply_json(req, 409, &json!({"error": "a scan is still running"}));
        }
        let mut scan = None;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(100));
            scan = miner::reg_scan();
            if scan.as_ref().is_some_and(|s| s.done) {
                break;
            }
        }
        let Some(s) = scan else {
            return reply_json(req, 500, &json!({"error": "scan lost"}));
        };
        let regs: serde_json::Map<String, serde_json::Value> = s
            .values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                (
                    format!("0x{:02x}", usize::from(s.from) + i),
                    v.map_or(serde_json::Value::Null, |v| json!(format!("0x{v:08x}"))),
                )
            })
            .collect();
        let answered = s.values.iter().filter(|v| v.is_some()).count();
        reply_json(
            req,
            200,
            &json!({"chip": s.chip, "from": format!("0x{:02x}", s.from), "to": format!("0x{:02x}", s.to),
                "done": s.done, "answered": answered, "uptime": diag::uptime_s(), "regs": regs}),
        )
    })?;

    // Diagnostics, not in the UI: which cores the nonces came from since
    // the chain started, per chip (?chip=N for one).
    server.fn_handler("/api/v1/chip/cores", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let uri = req.uri().to_string();
        let all = miner::core_hist();
        let body = match query_num(&uri, "chip") {
            Some(c) => match all.get(c as usize) {
                Some(h) => json!({"chip": c, "uptime": diag::uptime_s(), "nonces": h.nonces, "cores": h.cores, "smallCores": h.small_cores}),
                None => return reply_json(req, 404, &json!({"error": "no such chip"})),
            },
            None => json!({"uptime": diag::uptime_s(), "chips": all}),
        };
        reply_json(req, 200, &body)
    })?;

    server.fn_handler("/api/system", Method::Patch, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let body = read_body(&mut req, 4096)?;
        match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(v) if limits_refuse(&v).is_some() => {
                let why = limits_refuse(&v).unwrap_or_default();
                reply_json(req, 400, &json!({ "error": why }))
            }
            Ok(mut v) => match pool_refuse(&mut v)
                .map_or_else(|| config::patch(&v), |why| Err(anyhow::anyhow!(why)))
            {
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

    // Captive-portal probes of phones and laptops that joined the setup
    // hotspot (its DNS sends every name here): a redirect to the web UI
    // makes them open it as a sign-in page.
    for probe in [
        "/generate_204",
        "/gen_204",
        "/hotspot-detect.html",
        "/library/test/success.html",
        "/ncsi.txt",
        "/connecttest.txt",
        "/redirect",
    ] {
        server.fn_handler(probe, Method::Get, |req| {
            req.into_response(302, None, &[("Location", "http://192.168.4.1/")])?;
            Ok::<(), EspIOError>(())
        })?;
    }

    server.fn_handler("/assets/*", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let name = req
            .uri()
            .trim_start_matches("/assets/")
            .split('?')
            .next()
            .unwrap_or("")
            .to_string();
        match asset(&name) {
            Some((body, kind)) => {
                let mut r = req.into_response(
                    200,
                    None,
                    &[
                        ("Content-Type", kind),
                        ("Cache-Control", "public, max-age=604800"),
                    ],
                )?;
                r.write_all(body)?;
            }
            None => {
                req.into_status_response(404)?.write_all(b"not found")?;
            }
        }
        Ok::<(), EspIOError>(())
    })?;
    server.fn_handler("/favicon.ico", Method::Get, |req| {
        let mut r = req.into_response(200, None, &[("Content-Type", "image/svg+xml")])?;
        r.write_all(include_bytes!("ui/assets/favicon.svg"))?;
        Ok::<(), EspIOError>(())
    })?;
    server.fn_handler("/api/system/wifi/scan", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        match crate::clock::wifi_scan() {
            Ok(v) => reply_json(req, 200, &v),
            Err(e) => reply_json(req, 409, &json!({ "error": e })),
        }
    })?;
    server.fn_handler("/api/v1/history", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let span = req
            .uri()
            .split_once("span=")
            .and_then(|(_, v)| v.split('&').next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3600);
        reply_json(req, 200, &crate::history::json(span))
    })?;
    server.fn_handler("/api/v1/overheat/reset", Method::Post, |mut req| {
        log::warn!("api: overheat reset requested from {}", peer(&mut req));
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        board::reset_overheat();
        reply_json(req, 200, &json!({"overheat": board::overheat()}))
    })?;
    server.fn_handler("/api/v1/chips/off", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        board::set_shutdown(true);
        reply_json(req, 200, &json!({"shutdown": true}))
    })?;
    server.fn_handler("/api/v1/chips/on", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        board::set_shutdown(false);
        reply_json(req, 200, &json!({"shutdown": false}))
    })?;
    server.fn_handler("/api/v1/bestdiff/reset", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        config::reset_best_diff();
        miner::reset_best_session();
        reply_json(req, 200, &json!({"bestDiff": 0}))
    })?;

    server.fn_handler("/api/system/restart", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        log::warn!("api: restart requested from {}", peer(&mut req));
        let mut r = req.into_ok_response()?;
        r.write_all(b"restarting\n")?;
        r.flush()?;
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            board::prepare_reset("restart");
            diag::restart();
        });
        Ok::<(), EspIOError>(())
    })?;

    // The display's next frame, raw: RGB565 big-endian 320×170 (LCD) or
    // SSD1306 pages 128×32 (OLED); the size tells which.
    server.fn_handler("/api/v1/display/frame", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        match display::capture() {
            Some(f) => {
                let mut r =
                    req.into_response(200, None, &[("Content-Type", "application/octet-stream")])?;
                r.write_all(&f)?;
                Ok::<(), EspIOError>(())
            }
            None => reply_json(req, 503, &json!({"error": "display off or not drawing"})),
        }
    })?;

    // The last crash's core dump (ELF) for espcoredump.py, read straight
    // from flash in pieces; it stays until erased here or overwritten by
    // the next crash. Runs on the HTTP task (internal stack), never on a
    // PSRAM thread: flash reads switch the cache off.
    server.fn_handler("/api/v1/coredump", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let Some((addr, size)) = diag::core_dump() else {
            return reply_json(req, 404, &json!({"error": "no core dump stored"}));
        };
        let len = size.to_string();
        let mut r = req.into_response(
            200,
            None,
            &[
                ("Content-Type", "application/octet-stream"),
                ("Content-Length", &len),
                (
                    "Content-Disposition",
                    "attachment; filename=\"warpaxe-coredump.elf\"",
                ),
            ],
        )?;
        let mut buf = vec![0u8; 4096];
        let mut off = 0u32;
        while off < size {
            let n = (size - off).min(buf.len() as u32) as usize;
            if !diag::read_core_dump(addr + off, &mut buf[..n]) {
                log::warn!("coredump: flash read failed at {off}");
                break;
            }
            r.write_all(&buf[..n])?;
            off += n as u32;
        }
        Ok::<(), EspIOError>(())
    })?;
    server.fn_handler("/api/v1/coredump", Method::Delete, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        diag::erase_core_dump();
        log::info!("coredump: erased");
        reply_json(req, 200, &json!({"erased": true}))
    })?;

    // Online update from the project's releases.
    // The swarm page's list of other miners (addresses only).
    server.fn_handler("/api/v1/swarm", Method::Get, |req| {
        reply_json(
            req,
            200,
            &json!({"members": config::swarm(), "max": config::SWARM_MAX}),
        )
    })?;
    server.fn_handler("/api/v1/swarm", Method::Put, |mut req| {
        let mut body = [0u8; 2048];
        let mut n = 0;
        while n < body.len() {
            match req.read(&mut body[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => break,
            }
        }
        let members = serde_json::from_slice::<serde_json::Value>(&body[..n])
            .ok()
            .and_then(|v| serde_json::from_value::<Vec<String>>(v["members"].clone()).ok());
        match members.map(|m| config::set_swarm(&m)) {
            Some(Ok(m)) => {
                log::info!("api: swarm list saved ({} miners)", m.len());
                reply_json(req, 200, &json!({"members": m}))
            }
            Some(Err(e)) => reply_json(req, 400, &json!({"error": e})),
            None => reply_json(
                req,
                400,
                &json!({"error": "expected {\"members\": [addresses]}"}),
            ),
        }
    })?;
    server.fn_handler("/api/v1/update", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let mut v = serde_json::to_value(crate::update::state()).unwrap_or_default();
        let ota = display::OTA_STATE.load(Ordering::Relaxed);
        v["flashProgress"] = if ota >= 2 {
            json!(ota - 2)
        } else {
            json!(null)
        };
        reply_json(req, 200, &v)
    })?;
    server.fn_handler("/api/v1/update/check", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        if board::devkit() {
            return reply_json(req, 403, &json!({"error": DEVKIT_NO_UPDATE}));
        }
        log::info!("update: check requested from {}", peer(&mut req));
        crate::update::request_check();
        reply_json(req, 200, &json!({"checking": true}))
    })?;
    server.fn_handler("/api/v1/update/install", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        if board::devkit() {
            return reply_json(req, 403, &json!({"error": DEVKIT_NO_UPDATE}));
        }
        let who = peer(&mut req);
        if crate::update::request_install() {
            log::warn!(
                "update: install of {} requested from {who}",
                crate::update::state().latest.map_or("?".into(), |r| r.tag)
            );
            reply_json(req, 200, &json!({"installing": true}))
        } else {
            reply_json(
                req,
                409,
                &json!({"error": "no release known yet: check first"}),
            )
        }
    })?;

    // Press a display key remotely: ?k=next|home.
    server.fn_handler("/api/v1/display/key", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let k = if req.uri().contains("k=home") { 2 } else { 1 };
        display::REMOTE_KEY.store(k, Ordering::Relaxed);
        display::touch();
        reply_json(
            req,
            200,
            &json!({"key": if k == 1 { "next" } else { "home" }}),
        )
    })?;

    // Show a special screen on the device for 20 s:
    // ?event=block|overheat|off|ota.
    server.fn_handler("/api/v1/display/demo", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let ev = req
            .uri()
            .split_once("event=")
            .map(|(_, v)| v.split('&').next().unwrap_or("").to_string())
            .unwrap_or_default();
        if !["block", "overheat", "off", "ota"].contains(&ev.as_str()) {
            return reply_json(
                req,
                400,
                &json!({"error": "event must be block, overheat, off or ota"}),
            );
        }
        display::demo(&ev, 20);
        reply_json(req, 200, &json!({"demo": ev, "seconds": 20}))
    })?;

    server.fn_handler("/api/system/OTA", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        if board::devkit() {
            // The T-Display-S3 takes a new image over USB with the web
            // flasher only (Oliver, 2026-10-06); see update::request_install.
            log::warn!("ota: refused from {}, the T-Display-S3 is installed with the web flasher only", peer(&mut req));
            let mut r = req.into_response(403, None, &[])?;
            r.write_all(b"ota: the T-Display-S3 is installed with the web flasher only (no updates over the air)\n")?;
            return Ok(());
        }
        log::warn!("ota: upload started from {}", peer(&mut req));
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
        // Start the writer before answering: if its stack does not fit in
        // internal RAM, say so instead of accepting an update that never
        // happens. It waits a moment so the answer goes out first.
        let spawned = std::thread::Builder::new()
            .stack_size(16 * 1024)
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                match flash_image(&image) {
                Ok(()) => {
                    log::info!("ota: written and verified, restarting");
                    std::thread::sleep(Duration::from_millis(500));
                    board::prepare_reset("update");
                    diag::restart();
                }
                Err(e) => {
                    display::OTA_STATE.store(0, Ordering::Relaxed);
                    log::error!("ota: failed: {e}")
                }
                }
            });
        if let Err(e) = spawned {
            log::error!("ota: cannot start the writer: {e}");
            let mut r = req.into_response(503, None, &[])?;
            r.write_all(
                format!("ota: not enough internal memory to start the update ({} bytes free), try again\n", diag::free_heap_internal()).as_bytes(),
            )?;
            return Ok(());
        }
        let mut r = req.into_ok_response()?;
        r.write_all(
            format!("ota: {n} bytes accepted, flashing, restarting when done\n").as_bytes(),
        )?;
        r.flush()?;
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

    let (n, max) = handler_count();
    log::info!("api: {n} of {max} handler slots used");
    if n > max {
        anyhow::bail!("api: {n} handlers, the server has {max} slots");
    }
    Ok(http)
}

/// `EspHttpServer` with a count of the handlers registered through it.
struct Counted<'a>(&'a mut EspHttpServer<'static>);

impl Counted<'_> {
    fn fn_handler<'r, F, E>(
        &mut self,
        uri: &str,
        method: Method,
        f: F,
    ) -> Result<&mut Self, esp_idf_svc::sys::EspError>
    where
        F: for<'q> Fn(
                Request<&mut esp_idf_svc::http::server::EspHttpConnection<'q>>,
            ) -> Result<(), E>
            + Send
            + 'static,
        E: std::fmt::Debug,
    {
        HANDLERS.fetch_add(1, Ordering::Relaxed);
        self.0.fn_handler(uri, method, f)?;
        Ok(self)
    }
}

fn bootloader(
    mut req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
) -> Result<(), EspIOError> {
    log::warn!("api: download mode requested from {}", peer(&mut req));
    let mut r = req.into_ok_response()?;
    r.write_all(b"rebooting into ROM download mode; clear RTC_CNTL_OPTION1_REG before the reset that should start the app\n")?;
    r.flush()?;
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(300));
        // Nothing runs in ROM download mode: the chips must not sit
        // powered at their last clock meanwhile.
        board::prepare_reset("download mode");
        diag::reboot_to_bootloader();
    });
    Ok(())
}

/// Write an app image to the other OTA slot and make it bootable.
pub(crate) fn flash_image(image: &[u8]) -> anyhow::Result<()> {
    let mut ota = EspOta::new()?;
    log::info!("ota: erasing target slot");
    display::OTA_STATE.store(1, Ordering::Relaxed);
    display::touch();
    let mut update = ota.initiate_update()?;
    log::info!("ota: writing {} bytes", image.len());
    let total = image.len().max(1);
    for (i, chunk) in image.chunks(4096).enumerate() {
        update
            .write_all(chunk)
            .map_err(|e| anyhow::anyhow!("write: {e:?}"))?;
        let pct = ((i + 1) * 4096).min(total) * 100 / total;
        display::OTA_STATE.store(2 + pct as u8, Ordering::Relaxed);
    }
    log::info!("ota: verifying");
    update.complete()?;
    Ok(())
}

/// The address a request came from ("192.168.178.20"), for the log of
/// actions that change the firmware; "?" if the socket does not say.
fn peer(req: &mut Request<&mut esp_idf_svc::http::server::EspHttpConnection>) -> String {
    use esp_idf_svc::handle::RawHandle;
    use esp_idf_svc::sys;
    let Ok(raw) = req.connection().raw_connection() else {
        return "?".into();
    };
    let fd = unsafe { sys::httpd_req_to_sockfd(raw.handle()) };
    // lwIP sockaddr: len, family, port, then the address (IPv4 at 4,
    // IPv6 at 8; the server socket is dual-stack, so IPv4 clients arrive
    // as ::ffff:a.b.c.d).
    let mut buf = [0u8; 32];
    let mut len = buf.len() as sys::socklen_t;
    let rc = unsafe { sys::lwip_getpeername(fd, buf.as_mut_ptr() as *mut sys::sockaddr, &mut len) };
    if rc != 0 {
        return "?".into();
    }
    match buf[1] as u32 {
        f if f == sys::AF_INET => format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]),
        f if f == sys::AF_INET6 => {
            let a = &buf[8..24];
            if a[..10].iter().all(|&b| b == 0) && a[10] == 0xff && a[11] == 0xff {
                format!("{}.{}.{}.{}", a[12], a[13], a[14], a[15])
            } else {
                a.chunks(2)
                    .map(|c| format!("{:x}", u16::from_be_bytes([c[0], c[1]])))
                    .collect::<Vec<_>>()
                    .join(":")
            }
        }
        _ => "?".into(),
    }
}

/// Why the regulator stopped the core, while it is stopped for that
/// (`overheatCause` "power"): "overcurrent", "overvoltage",
/// "undervoltage" or "overtemp"; null otherwise.
fn fault_cause(b: &board::BoardState) -> Option<&'static str> {
    if b.overheat_cause != Some("power") {
        return None;
    }
    b.power_fault_codes.iter().find_map(|c| match *c {
        "iout_oc" => Some("overcurrent"),
        "vout_ov" => Some("overvoltage"),
        "vin_uv" => Some("undervoltage"),
        "temp" => Some("overtemp"),
        _ => None,
    })
}
