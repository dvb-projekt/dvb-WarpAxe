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

use crate::{board, config, diag, display, events, miner, net, selftest, stratum, swarm};
use warpaxe_displaycfg as dcfg;

/// The web UI's files: fonts (OFL, Latin subsets), the wordmark and the
/// icon, all in the firmware so the page looks the same offline.
fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match name {
        "favicon.svg" => (include_bytes!("ui/assets/favicon.svg"), "image/svg+xml"),
        "wordmark.webp" => (include_bytes!("ui/assets/wordmark.webp"), "image/webp"),
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

/// The web UI: one static page on top of the JSON API, gzip-compressed
/// by build.rs (the source is `ui/index.html`; the image holds only the
/// compressed form, 90 KB instead of 290 KB) and served with
/// `Content-Encoding: gzip` on every path the page comes from: Wi-Fi,
/// the USB network and the setup hotspot's captive portal, whose
/// redirects all end at `/`.
const UI_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.html.gz"));

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

/// NerdOS `GET /api/v2/dashboard` → `performance` (v1.1.1-alpha4,
/// shufps, ESP-Miner-NerdQAxePlus #706): hash rates and per chip the hash
/// rate and the error hash rate, GH/s, from the chips' own counters. Only
/// the `performance` object; `hashRate1d` is null (three hours are kept).
fn dashboard_v2() -> serde_json::Value {
    let m = miner::state();
    let i = system_info();
    let chips: Vec<serde_json::Value> = m
        .hashrate_monitor
        .iter()
        .map(|a| json!({"hashRate": a.total, "errorHashRate": a.error_ghs}))
        .collect();
    let one_min: f32 = m.hashrate_monitor.iter().map(|a| a.total_1m).sum();
    json!({"performance": {
        "hashRateTimestamp": diag::uptime_s() * 1000,
        "hashRate": m.hashrate_now_ghs,
        "hashRate1m": one_min,
        "hashRate10m": m.hashrate_ghs,
        "hashRate1h": crate::history::hour_ghs(),
        "hashRate1d": null,
        "bestDiff": i["bestDiff"], "bestSessionDiff": i["bestSessionDiff"],
        "sharesAccepted": i["sharesAccepted"], "sharesRejected": i["sharesRejected"],
        "frequency": i["frequency"], "asicCount": i["asicCount"], "smallCoreCount": i["smallCoreCount"],
        "chips": chips,
    }})
}

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
    // Delivered against small cores × clock (see yieldPercent below).
    let expected_ghs = f64::from(board::chip().small_cores)
        * f64::from(m.frequency_hz)
        * f64::from(m.chips.max(1))
        / 1e9;
    let yield_percent = (expected_ghs > 0.0 && m.hashrate_ghs > 0.0)
        .then(|| (f64::from(m.hashrate_ghs) / expected_ghs * 100.0).min(999.0));
    let mut v = json!({
        "boardVersion": b.board, "deviceModel": b.board, "boardName": profile.model,
        "boardDetect": board::detect_info(),
        "boardModels": warpaxe_board::profile::ALL.iter().map(|p| json!({"name": p.name, "model": if p.asic_count == 0 { "WarpMiner (T-Display-S3)" } else { p.model }, "experimental": p.asic_count == 0, "tested": warpaxe_board::detect::TESTED.contains(&p.name)})).collect::<Vec<_>>(),
        "nominalVoltage": profile.nominal_voltage_v, "maxPower": profile.max_power_w,
        "power": b.power_w, "powerEstimated": b.power_estimated, "hasFan": profile.has_fan(), "hasVr": b.has_vr, "voltage": b.vin_mv, "current": b.iout_ma,
        "temp": b.temp_c, "vrTemp": b.vr_temp_c.unwrap_or(0),
        "overheat_mode": u8::from(b.overheat), "overheatLatched": b.overheat_latched, "overheatTrips": b.overheat_trips, "overheatCause": b.overheat_cause, "fanWarmup": b.fan_warmup, "faultCause": fault_cause(&b), "coreCurrentA": b.core_current_a, "coreCurrentWarnA": b.core_current_warn_a, "power_fault": b.power_fault, "powerFaultCodes": b.power_fault_codes, "shutdown": b.shutdown, "chainStopped": board::chain_stopped(), "fanrpm": b.fan_rpm,
        "vrOverheatTemp": profile.vr_stop_c.round() as u32, "boardError": b.last_error,
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
        "overheat_temp": profile.chip_limit_c(cfg.overheat_temp).round() as u32, "overheatTempMax": profile.chip_stop_max_c().round() as u32,
        "hashRate": m.hashrate_ghs, "hashRate_10m": m.hashrate_ghs, "hashRate_now": m.hashrate_now_ghs,
        "hashrateMonitor": { "asics": m.hashrate_monitor }, "errorPercentage": m.error_percent, "errorPercentage1m": m.error_percent_1m,
        "expectedHashrate": f64::from(board::chip().small_cores) * f64::from(m.frequency_hz) * f64::from(m.chips.max(1)) / 1e9,
        // What the chips deliver against small cores × clock, from the
        // nonce-weighted ten-minute figure: a chip without an error counter
        // (the BM1397) shows a failing core only as missing nonces, so this
        // is its health figure (the .46 runs 90–95 % on its 1.4 V rail).
        "yieldPercent": yield_percent,
        // The devkit's shares sit below 1: its figures with decimals.
        "bestDiff": if board::devkit() { json!(config::best_diff_f()) } else { json!(config::best_diff()) },
        "bestSessionDiff": if board::devkit() { json!(crate::cpuminer::info().best_diff) } else { json!(m.best_session_diff) },
        "freeHeap": diag::free_heap(), "freeHeapInternal": diag::free_heap_internal(),
        // The largest piece of internal heap in one go (bytes): a task
        // stack or a DMA buffer must fit into it, the total says nothing
        // about that once the heap is fragmented.
        "largestFreeInternal": diag::largest_free_internal(),
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
        "hostname": cfg.hostname, "mdnsHostname": net::mdns::hostname(), "fullHostname": net::mdns::hostname().map(|h| format!("{h}.local")), "usbSubnet": cfg.usb_subnet, "psuWatts": (cfg.psu_watts > 0).then_some(cfg.psu_watts),
        "wifiStatus": if n.wifi_connected { "Connected!" } else { "Not connected" },
        "wifiRSSI": n.wifi_rssi.unwrap_or(0),
        "usbStatus": if n.usb_mounted { "Connected!" } else { "Not connected" },
        "sharesAccepted": st.shares_accepted, "sharesRejected": st.shares_rejected,
        "staleAfterCleanHist": st.stale_after_clean_hist, "sharesRejectedReasons": st.reject_reasons.iter().map(|(m, c)| json!({"message": m, "count": c})).collect::<Vec<_>>(),
        "uptimeSeconds": diag::uptime_s(), "unixTime": crate::clock::unix_now(), "ntpLastSync": crate::clock::last_sync(), "useNTP": u8::from(cfg.use_ntp), "ntpServer": cfg.ntp_server,
        "asicCount": m.chips, "smallCoreCount": board::chip().small_cores, "ASICModel": asic_model(),
        "chainPhase": m.phase, "frequencyActual": m.frequency_hz / 1_000_000, "clockStep": m.clock_step,
        // The ESP32-S3's own die temperature, on every board: how warm the
        // controller next to the chip runs (the NerdNOS shares a case with it).
        "dieTempC": crate::cpuminer::die_temp_c(),
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
        "coinbaseOwnAddress": st.coinbase_own_address, "coinbaseBitcoin": st.coinbase_bitcoin, "coinTicker": cfg.coin_ticker,
        "blockFound": m.blocks_found, "updateAvailable": crate::update::state().available, "updateCheck": cfg.update_check, "timezone": cfg.timezone, "btcPrice": cfg.btc_price, "showHashCounters": cfg.show_hash_counters, "priceCurrency": cfg.price_currency, "displayLanguage": cfg.display_language,
        // The display's choosable parts, as they take effect (the
        // config's empty lists resolved to the board's defaults), and
        // what kind of display the board has, so the web UI shows the
        // LCD or the OLED settings.
        "displayKind": display_kind(profile),
        "displayPages": dcfg::pages(&cfg.display_pages), "displayStartPage": dcfg::pages(&cfg.display_pages)[dcfg::start_index(&dcfg::pages(&cfg.display_pages), &cfg.display_start_page)],
        "cockpitBig": dcfg::big(&cfg.cockpit_big), "cockpitSlots": dcfg::slots(&cfg.cockpit_slots, board::devkit()), "cockpitFooter": dcfg::footer(&cfg.cockpit_footer),
        "oledSlides": dcfg::oled_slides(&cfg.oled_slides), "oledInterval": dcfg::oled_interval_s(cfg.oled_interval),
        "stratumConnected": stratum::state().authorized,
        "version": VERSION, "gitHash": env!("WARPAXE_GIT"), "runningPartition": crate::update::running_partition(), "otaState": crate::update::ota_state(),
        "fanspeed": if b.has_thermal { b.fan_pct } else { cfg.fanspeed }, "autofanspeed": u8::from(cfg.autofanspeed),
        "resetReason": diag::reset_reason(),
        // The last reset was a crash or a watchdog: the web UI shows a
        // banner once per boot (bootId tells one boot from the next).
        "crashed": diag::last_run_crashed(), "bootId": diag::boot_id(), "coreDumpBytes": diag::core_dump().map(|(_, n)| n),
        "selfTest": selftest::brief(),
    });
    if board::devkit() {
        v["cpuMiner"] = json!(crate::cpuminer::info());
    }
    v
}

/// "lcd", "oled" or "none": which display settings apply to this board.
fn display_kind(p: &warpaxe_board::Profile) -> &'static str {
    use warpaxe_board::profile::Display;
    match p.display {
        Display::St7789I80 => "lcd",
        Display::Ssd1306 { .. } => "oled",
        Display::None => "none",
    }
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
    let chip_limit = p.chip_limit_c(cfg.overheat_temp).round() as u32;
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
         "overheatTemp": p.vr_stop_c.round() as u32, "pid": gains(vr_target(cfg)),
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
    expert_refuse(v).or_else(|| display_refuse(v))
}

/// The display's choosable parts, checked by `warpaxe_displaycfg` (host-tested).
fn display_refuse(v: &serde_json::Value) -> Option<String> {
    dcfg::check_patch(v, &config::get().display_pages)
}

/// Expert fields outside what the controllers can use sensibly.
fn expert_refuse(v: &serde_json::Value) -> Option<String> {
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64());
    if let Some(x) = num("overheat_temp") {
        let max = board::profile().chip_stop_max_c();
        if x > f64::from(max) {
            return Some(format!(
                "overheat_temp {x} above this board's ceiling of {max} °C (0 = default)"
            ));
        }
    }
    for (k, lo, hi) in [
        ("pidP", 0.0, 100.0),
        ("pidI", 0.0, 10.0),
        ("pidD", 0.0, 100.0),
        ("manualFanSpeed2", 0.0, 100.0),
        ("fanspeed", 0.0, 100.0),
        ("minFanSpeed", 0.0, 100.0),
        ("usbSubnet", 1.0, 254.0),
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
        // The chip stop: 50 at the lowest, the board's ceiling checked
        // below (90 on the NerdOS boards, the board's own line elsewhere).
        ("overheat_temp", 50.0, 90.0),
        // 10 ms: a BM1397 scans its nonce space in ~100 ms at 250 MHz, so
        // cadence experiments need the range below the old 100 ms floor.
        ("jobInterval", 10.0, 5000.0),
        // Whole watts of the miner's power supply, for WarpTune.
        ("psuWatts", 5.0, 3000.0),
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
    if let Some(c) = v.get("coinTicker").cloned() {
        match c.as_str().map(normalize_ticker) {
            Some(Some(t)) => {
                if let Some(o) = v.as_object_mut() {
                    o.insert("coinTicker".into(), serde_json::Value::String(t));
                }
            }
            _ => return Some("coinTicker: 1 to 8 letters or digits".into()),
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

/// A coin ticker as the web UI shows it: upper case, 1–8 letters or
/// digits; empty means the default, BTC.
fn normalize_ticker(t: &str) -> Option<String> {
    let t = t.trim().to_ascii_uppercase();
    if t.is_empty() {
        return Some("BTC".into());
    }
    (t.len() <= 8 && t.chars().all(|c| c.is_ascii_alphanumeric())).then_some(t)
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
        "config_patches": config::patches(),
        "last_crash": diag::last_crash(),
        "core_dump_bytes": diag::core_dump().map(|(_, n)| n),
        "stratum": stratum::state(),
        "miner": miner::state(),
        "board": board::state(),
        "display": json!({"asleep": crate::display::asleep(), "timeout_min": config::get().display_timeout}),
        "config": config::get().redacted(),
        "events": events::list(),
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
    reply_json_cors(req, status, v, true)
}

/// `reply_json` with the choice of the CORS header: every API route
/// carries `Access-Control-Allow-Origin: *` (the swarm page of another
/// miner reads this one), except where the reply was fetched from a
/// third device for this page only.
fn reply_json_cors(
    req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
    status: u16,
    v: &serde_json::Value,
    cors: bool,
) -> Result<(), EspIOError> {
    let body = v.to_string();
    let json = ("Content-Type", "application/json");
    let mut r = if cors {
        req.into_response(status, None, &[json, ("Access-Control-Allow-Origin", "*")])?
    } else {
        req.into_response(status, None, &[json])?
    };
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
        // Every browser, the captive-portal sheets of phones and laptops
        // included, takes gzip; a client that says in so many words that
        // it does not (an Accept-Encoding without gzip or identity-only)
        // gets a line of plain text instead of the compressed bytes. A
        // missing header means anything goes (RFC 9110), so curl and
        // the like get the gzip, which `curl --compressed` unpacks.
        let accepts_gzip = req
            .header("Accept-Encoding")
            .is_none_or(|v| v.contains("gzip") || v.contains('*'));
        if !accepts_gzip {
            let mut r = req.into_response(
                406,
                None,
                &[("Content-Type", "text/plain; charset=utf-8")],
            )?;
            r.write_all(b"WarpAxe: the web UI is served gzip-compressed; open it in a browser or request it with gzip in Accept-Encoding.\n")?;
            return Ok::<(), EspIOError>(());
        }
        let len = UI_GZ.len().to_string();
        let mut r = req.into_response(
            200,
            None,
            &[
                ("Content-Type", "text/html; charset=utf-8"),
                ("Content-Encoding", "gzip"),
                ("Content-Length", &len),
                ("Vary", "Accept-Encoding"),
                ("Cache-Control", "no-cache"),
            ],
        )?;
        r.write_all(UI_GZ)?;
        Ok::<(), EspIOError>(())
    })?;

    server.fn_handler("/api/v2/dashboard", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        reply_json(req, 200, &dashboard_v2())
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
        let out = log_text();
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
        let who = peer(&mut req);
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
                    let keys: Vec<String> = v
                        .as_object()
                        .map(|o| o.keys().cloned().collect())
                        .unwrap_or_default();
                    // Who changed what, kept across reboots: a worker renamed
                    // in the web UI showed at the pool hours later (2026-10-08)
                    // and nobody could say where the change had come from.
                    log::info!("config patched from {who}: {keys:?}");
                    config::record_patch(&who, &keys);
                    if v.get("frequency").is_some() {
                        miner::request_frequency(cfg.frequency);
                    }
                    // Any pool field (main or fallback, the fallback
                    // switch, the suggested difficulty): the session is
                    // rebuilt from the new config at once.
                    let pool_change = v.as_object().is_some_and(|o| {
                        o.keys().any(|k| {
                            k.starts_with("stratum")
                                || k.starts_with("fallbackStratum")
                                || k == "useFallbackStratum"
                        })
                    });
                    if pool_change {
                        stratum::reconnect();
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
    // Diagnostics on the chipless board: run the SHA peripheral bench
    // again (the peripheral worker does it between two batches, results
    // in cpuMiner.bench of /api/system/info and in the log). Only in a
    // build with the `shabench` feature (docs/06).
    #[cfg(feature = "shabench")]
    server.fn_handler("/api/v1/cpuminer/bench", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        if !board::devkit() {
            return reply_json(req, 404, &json!({"error": "this board has chips"}));
        }
        if crate::cpuminer::request_bench() {
            reply_json(req, 202, &json!({"status": "bench requested"}))
        } else {
            reply_json(req, 409, &json!({"error": "a bench is still pending"}))
        }
    })?;
    // A test build only (`fragtest`, docs/09): take the internal heap
    // apart so no 16 KB piece is left, as hours of mining do, and prove
    // an update still goes through. `?free=1` gives the blocks back.
    #[cfg(feature = "fragtest")]
    server.fn_handler("/api/v1/diag/fragment", Method::Post, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let free = req.uri().contains("free=1");
        let (before_free, before_largest) =
            (diag::free_heap_internal(), diag::largest_free_internal());
        let kept = if free {
            fragtest::release()
        } else {
            fragtest::fragment()
        };
        reply_json(
            req,
            200,
            &json!({
                "blocksKept": kept, "blockBytes": fragtest::BLOCK,
                "before": {"freeHeapInternal": before_free, "largestFreeInternal": before_largest},
                "after": {"freeHeapInternal": diag::free_heap_internal(), "largestFreeInternal": diag::largest_free_internal()},
            }),
        )
    })?;
    // The self-test (docs/10): start, follow, cancel.
    server.fn_handler("/api/v1/selftest", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        log::info!("api: self-test requested from {}", peer(&mut req));
        match selftest::start() {
            Ok(id) => reply_json(req, 202, &json!({"started": true, "id": id})),
            Err(e) => reply_json(req, 409, &json!({"error": e})),
        }
    })?;
    server.fn_handler("/api/v1/selftest", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        events::flush();
        reply_json(req, 200, &selftest::status_json())
    })?;
    server.fn_handler("/api/v1/selftest", Method::Delete, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        log::info!("api: self-test cancel requested from {}", peer(&mut req));
        let was = selftest::abort();
        reply_json(req, 200, &json!({"cancelled": was}))
    })?;
    // The event log that survives restarts, newest first.
    server.fn_handler("/api/v1/events", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        events::flush();
        reply_json(req, 200, &events::json())
    })?;
    // The diagnostics file for support: one JSON document, secrets never,
    // addresses and the network name masked (docs/10).
    server.fn_handler("/api/v1/diagnostics", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        events::flush();
        diagnostics(req)
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
        // On the main task (update::serve_writes): no thread to spawn
        // from a heap that may have no 8 KB piece left.
        crate::update::submit_restart(crate::update::Work::Restart("restart"));
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
    // Saved pools: GET lists them (passwords never leave the miner),
    // POST {"action": "save"|"use"|"delete", "name": …} saves the main
    // pool as it is now, makes a saved one the main pool, or drops one.
    server.fn_handler("/api/v1/pools", Method::Get, |req| {
        let list: Vec<_> = config::pools().iter().map(|p| p.redacted()).collect();
        reply_json(req, 200, &json!({"pools": list, "max": config::POOLS_MAX}))
    })?;
    server.fn_handler("/api/v1/pools", Method::Post, |mut req| {
        let who = peer(&mut req);
        let body = read_body(&mut req, 512)?;
        let v = serde_json::from_slice::<serde_json::Value>(&body).unwrap_or_default();
        let name = v["name"].as_str().unwrap_or("").to_string();
        let done = match v["action"].as_str() {
            Some("save") => config::save_pool(config::PoolProfile::current(&name, &config::get())),
            Some("delete") => config::delete_pool(&name),
            Some("use") => match config::pool(&name) {
                None => Err("no saved pool of that name".into()),
                Some(p) => {
                    let mut patch = p.as_patch();
                    match pool_refuse(&mut patch)
                        .map_or_else(|| config::patch(&patch).map_err(|e| e.to_string()), Err)
                    {
                        Ok(_) => {
                            log::info!("pools: {who} switched to saved pool {:?}", p.name);
                            config::record_patch(&who, &[format!("pool:{}", p.name)]);
                            crate::events::record("pool", format!("saved pool {} used", p.name));
                            crate::events::flush();
                            crate::display::touch();
                            stratum::reconnect();
                            Ok(config::pools())
                        }
                        Err(e) => Err(e),
                    }
                }
            },
            _ => Err("expected {\"action\": \"save\"|\"use\"|\"delete\", \"name\": …}".into()),
        };
        match done {
            Ok(list) => {
                let list: Vec<_> = list.iter().map(|p| p.redacted()).collect();
                reply_json(req, 200, &json!({"pools": list}))
            }
            Err(e) => reply_json(req, 400, &json!({"error": e})),
        }
    })?;

    // An Avalon on the swarm list, read over the cgminer API for the
    // browser (see `swarm.rs`): ?host=<ip>. 200 with the AxeOS-like
    // document, 202 while a read is under way (the page asks again, the
    // HTTP task never waits), 403 for a host not on the list as
    // cgminer:<host>, 502 when the Avalon did not answer.
    server.fn_handler("/api/v1/swarm/cgminer", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let uri = req.uri().to_string();
        let host = uri
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or("")
            .split('&')
            .find_map(|kv| kv.strip_prefix("host="))
            .unwrap_or("");
        // No CORS header: the Avalon's data is for this miner's own page.
        let (status, body) = swarm::lookup(host);
        reply_json_cors(req, status, &body, false)
    })?;
    // The network search for an Avalon: POST starts a walk of the own
    // /24 for port 4028 (or answers with the one under way or just
    // finished), GET says where it stands; neither waits (see swarm.rs).
    server.fn_handler("/api/v1/swarm/cgminer/scan", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        log::info!("swarm: cgminer search requested from {}", peer(&mut req));
        let (status, body) = swarm::scan_start();
        reply_json_cors(req, status, &body, false)
    })?;
    server.fn_handler("/api/v1/swarm/cgminer/scan", Method::Get, |req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
        let (status, body) = swarm::scan_status();
        reply_json_cors(req, status, &body, false)
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
        log::info!("update: check requested from {}", peer(&mut req));
        crate::update::request_check();
        reply_json(req, 200, &json!({"checking": true}))
    })?;
    server.fn_handler("/api/v1/update/install", Method::Post, |mut req| {
        diag::HTTP_HITS.fetch_add(1, Ordering::Relaxed);
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
        // Hand the image to the main task's writer (update::serve_writes):
        // no thread, no 16 KB of internal RAM to find in a fragmented
        // heap. It waits a moment so the answer goes out first. Refused
        // only while another image is queued or being written.
        if let Err(why) = crate::update::submit(crate::update::Job {
            image,
            origin: "ota",
            record: None,
            delay: Duration::from_millis(300),
        }) {
            let mut r = req.into_response(409, None, &[])?;
            r.write_all(format!("ota: {why}, try again later\n").as_bytes())?;
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
    crate::update::submit_restart(crate::update::Work::Bootloader);
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
/// The log ring as one string, one line per entry.
fn log_text() -> String {
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
    out
}

/// The scrubber for the diagnostics file: every secret the firmware
/// knows replaced with `***`, the worker addresses and the Wi-Fi name
/// with their masked forms (`warpaxe_diag::scrub`).
fn scrubber(cfg: &config::Config) -> warpaxe_diag::scrub::Scrubber {
    use warpaxe_diag::scrub::{mask_ssid, mask_worker, Scrubber};
    let mut s = Scrubber::new()
        .secret(&cfg.wifi_pass)
        .secret(&cfg.stratum_pass)
        .secret(&cfg.fallback_pass)
        .replace(&cfg.ssid, &mask_ssid(&cfg.ssid))
        .replace(&cfg.stratum_user, &mask_worker(&cfg.stratum_user))
        .replace(&cfg.fallback_user, &mask_worker(&cfg.fallback_user));
    for p in config::pools() {
        s = s.secret(&p.pass).replace(&p.user, &mask_worker(&p.user));
    }
    if let Some((_, pass)) = net::hotspot() {
        s = s.secret(&pass);
    }
    s
}

/// `GET /api/v1/diagnostics`: the file, section by section so no whole
/// copy sits in RAM. Every section's text goes through the scrubber;
/// one that still contains a secret afterwards is replaced by a marker,
/// never sent.
fn diagnostics(
    req: Request<&mut esp_idf_svc::http::server::EspHttpConnection<'_>>,
) -> Result<(), EspIOError> {
    let cfg = config::get();
    let scrub = scrubber(&cfg);
    let now = crate::clock::unix_now().unwrap_or(0);
    let name = format!(
        "attachment; filename=\"warpaxe-diagnostics-{}-{}.json\"",
        net::mdns::label(&cfg.hostname),
        if now > 0 { now.to_string() } else { format!("up{}", diag::uptime_s()) }
    );
    let mut r = req.into_response(
        200,
        None,
        &[
            ("Content-Type", "application/json; charset=utf-8"),
            ("Content-Disposition", &name),
            ("Cache-Control", "no-store"),
        ],
    )?;
    let mut first = true;
    let mut section = |r: &mut esp_idf_svc::http::server::Response<
        &mut esp_idf_svc::http::server::EspHttpConnection<'_>,
    >,
                       key: &str,
                       mut v: serde_json::Value|
     -> Result<(), EspIOError> {
        scrub.apply_json(&mut v);
        let text = serde_json::to_string_pretty(&v).unwrap_or_else(|_| "null".into());
        let text = if scrub.leaks(&text) {
            log::error!("diagnostics: section {key} still carried a secret after scrubbing; left out");
            "\"left out: a secret would have leaked\"".to_string()
        } else {
            text
        };
        r.write_all(if first { b"{\n" } else { b",\n" })?;
        first = false;
        r.write_all(format!("\"{key}\": ").as_bytes())?;
        r.write_all(text.as_bytes())?;
        Ok(())
    };
    section(&mut r, "file", json!({
        "what": "WarpAxe diagnostics file. No passwords; worker addresses and the Wi-Fi name are masked. Send it to support as it is.",
        "generatedAt": (now > 0).then_some(now), "uptimeSeconds": diag::uptime_s(),
        "resetReason": diag::reset_reason(), "crashed": diag::last_run_crashed(), "lastCrash": diag::last_crash(),
        "coreDumpBytes": diag::core_dump().map(|(_, n)| n),
        "coreDump": diag::core_dump().map(|_| "stored on the miner: GET /api/v1/coredump (ELF)"),
        "installRecord": config::install_record(),
        "memory": {"freeHeap": diag::free_heap(), "freeHeapInternal": diag::free_heap_internal(), "largestFreeInternal": diag::largest_free_internal(), "httpHandlers": handler_count()},
    }))?;
    // Everything /api/system/info says (firmware, board, chips, hash
    // counters, pool, regulator, temperatures, fans), with the worker
    // addresses and the Wi-Fi name masked by the scrubber.
    section(&mut r, "info", system_info())?;
    section(&mut r, "selfTest", selftest::status_json())?;
    section(&mut r, "events", json!(events::list()))?;
    // The chain driver's and the pool client's own state (chain counters,
    // the last nonce, reject reasons, reconnects).
    section(&mut r, "chain", json!(miner::state()))?;
    section(&mut r, "pool", json!(stratum::state()))?;
    let mut c = serde_json::to_value(cfg.redacted()).unwrap_or_default();
    if let Some(o) = c.as_object_mut() {
        o.remove("wifiPass");
        o.remove("stratumPassword");
        o.remove("fallbackStratumPassword");
    }
    section(&mut r, "config", c)?;
    section(&mut r, "savedPools", json!(config::pools().iter().map(|p| json!({"name": p.name, "host": p.url, "port": p.port, "user": p.user, "protocol": p.protocol})).collect::<Vec<_>>()))?;
    section(&mut r, "configPatches", json!(config::patches()))?;
    section(&mut r, "log", json!(log_text().lines().collect::<Vec<_>>()))?;
    r.write_all(b"\n}\n")?;
    Ok(())
}

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

/// The heap-fragmentation test (a build with the `fragtest` feature):
/// the internal heap is carved into [`BLOCK`]-byte pieces until none is
/// left, then every other piece is given back. What remains free is
/// half the heap, in holes of one block each: no allocation above
/// [`BLOCK`] bytes can succeed any more, which is what a fragmented heap
/// after hours of mining looks like to the writer thread of v0.1.31
/// (16 KB wanted, 45 KB free, no piece that size). 8 KB holes leave the
/// 6 KB threads of a pool reconnect their room; the 12 and 16 KB stacks
/// of v0.1.31's writers would not fit.
#[cfg(feature = "fragtest")]
mod fragtest {
    use std::sync::Mutex;

    use esp_idf_svc::sys;

    pub const BLOCK: usize = 8 * 1024;
    /// The pieces kept, as raw heap pointers (freed by [`release`]).
    static KEPT: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    /// Carve the heap and keep every other piece; how many are kept.
    pub fn fragment() -> usize {
        let mut kept = KEPT.lock().unwrap();
        release_all(&mut kept);
        let mut all = Vec::new();
        loop {
            let p = unsafe {
                sys::heap_caps_malloc(BLOCK, sys::MALLOC_CAP_INTERNAL | sys::MALLOC_CAP_8BIT)
            };
            if p.is_null() {
                break;
            }
            all.push(p as usize);
        }
        for (i, p) in all.into_iter().enumerate() {
            if i % 2 == 0 {
                unsafe { sys::heap_caps_free(p as *mut core::ffi::c_void) };
            } else {
                kept.push(p);
            }
        }
        log::warn!(
            "fragtest: {} blocks of {BLOCK} bytes kept, {}",
            kept.len(),
            crate::diag::heap_line()
        );
        kept.len()
    }

    /// Give every kept piece back; how many were kept.
    pub fn release() -> usize {
        let mut kept = KEPT.lock().unwrap();
        let n = release_all(&mut kept);
        log::warn!(
            "fragtest: {n} blocks released, {}",
            crate::diag::heap_line()
        );
        n
    }

    fn release_all(kept: &mut Vec<usize>) -> usize {
        let n = kept.len();
        for p in kept.drain(..) {
            unsafe { sys::heap_caps_free(p as *mut core::ffi::c_void) };
        }
        n
    }
}
