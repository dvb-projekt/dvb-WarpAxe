//! Wall-clock time by SNTP (optional, `useNTP`), and the Wi-Fi scan the
//! settings page lists networks from.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use esp_idf_svc::sntp::{EspSntp, SntpConf};
use esp_idf_svc::sys;
use serde_json::json;

use crate::config;

static SNTP: Mutex<Option<EspSntp<'static>>> = Mutex::new(None);
/// When SNTP last set the clock, seconds since 1970 (32 bits: the Xtensa
/// has no 64-bit atomics, and u32 seconds last until 2106); 0 = not yet
/// this boot.
static LAST_SYNC: AtomicU32 = AtomicU32::new(0);

/// Start SNTP with the configured server; it syncs once a network is up
/// and keeps the clock set from then on.
pub fn start() {
    let cfg = config::get();
    if !cfg.use_ntp || cfg.ntp_server.is_empty() {
        return;
    }
    // SNTP keeps the server name for its lifetime; the config value lives
    // until the next reboot anyway.
    let server: &'static str = Box::leak(cfg.ntp_server.into_boxed_str());
    let mut conf = SntpConf::default();
    conf.servers[0] = server;
    // The callback runs on the SNTP task after every successful sync
    // (about once an hour): the web UI shows whether and when the clock
    // was last set, since TLS and an authenticated Stratum V2 pool wait
    // for it.
    match EspSntp::new_with_callback(&conf, |_| {
        if let Some(s) = unix_now() {
            LAST_SYNC.store(u32::try_from(s).unwrap_or(u32::MAX), Ordering::Relaxed);
        }
    }) {
        Ok(s) => {
            log::info!("clock: sntp via {server}");
            *SNTP.lock().unwrap() = Some(s);
        }
        Err(e) => log::warn!("clock: sntp failed: {e}"),
    }
}

/// Seconds since 1970, once the clock has been set (anything before 2024
/// is the unset clock counting from boot).
pub fn unix_now() -> Option<u64> {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    (s > 1_704_067_200).then_some(s)
}

/// NTP is configured but has not set the clock yet this boot. The time
/// then comes out of RTC memory across the reset: usually right, but not
/// to be trusted for a certificate's validity window (the .45 came back
/// from an esptool session on 2026-10-24 with a clock that read as set
/// and failed both pools' certificates until the first NTP sync).
pub fn ntp_pending() -> bool {
    let cfg = config::get();
    cfg.use_ntp && !cfg.ntp_server.is_empty() && last_sync().is_none()
}

/// The clock is one to check a TLS certificate's dates by: NTP set it
/// this boot. With NTP off, never (`warpaxe_trust::clock_checked`).
/// Reads an atomic only, so the TLS verify callbacks may call it.
pub fn trusted() -> bool {
    last_sync().is_some()
}

/// In the boot's first minutes, wait up to `max_s` seconds for NTP to set
/// the clock, so a TLS certificate's dates can be checked. Later, or with
/// NTP off, returns at once: with NTP unreachable for good every
/// reconnect would otherwise pay the wait.
pub fn wait_for_ntp(max_s: u32) {
    if crate::diag::uptime_s() >= 300 {
        return;
    }
    let mut waited = 0;
    while ntp_pending() && waited < max_s {
        std::thread::sleep(std::time::Duration::from_secs(1));
        waited += 1;
    }
}

/// When SNTP last set the clock this boot, seconds since 1970.
pub fn last_sync() -> Option<u64> {
    match LAST_SYNC.load(Ordering::Relaxed) {
        0 => None,
        s => Some(u64::from(s)),
    }
}

fn auth_name(m: sys::wifi_auth_mode_t) -> &'static str {
    match m {
        sys::wifi_auth_mode_t_WIFI_AUTH_OPEN => "open",
        sys::wifi_auth_mode_t_WIFI_AUTH_WEP => "WEP",
        sys::wifi_auth_mode_t_WIFI_AUTH_WPA_PSK => "WPA",
        sys::wifi_auth_mode_t_WIFI_AUTH_WPA2_PSK => "WPA2",
        sys::wifi_auth_mode_t_WIFI_AUTH_WPA_WPA2_PSK => "WPA/WPA2",
        sys::wifi_auth_mode_t_WIFI_AUTH_WPA3_PSK => "WPA3",
        sys::wifi_auth_mode_t_WIFI_AUTH_WPA2_WPA3_PSK => "WPA2/WPA3",
        sys::wifi_auth_mode_t_WIFI_AUTH_WPA2_ENTERPRISE => "WPA2-Enterprise",
        _ => "other",
    }
}

/// Scan for access points (blocking, about two seconds; the station stays
/// connected). Strongest entry per SSID, strongest first.
pub fn wifi_scan() -> Result<serde_json::Value, String> {
    let mut found: Vec<(String, i8, &'static str)> = Vec::new();
    unsafe {
        let conf = sys::wifi_scan_config_t::default();
        let rc = sys::esp_wifi_scan_start(&conf, true);
        if rc != sys::ESP_OK {
            return Err(format!(
                "wifi scan: {}",
                sys::EspError::from(rc).map_or("?".into(), |e| e.to_string())
            ));
        }
        let mut n: u16 = 0;
        sys::esp_wifi_scan_get_ap_num(&mut n);
        let mut recs = vec![sys::wifi_ap_record_t::default(); usize::from(n.min(32))];
        let mut got = recs.len() as u16;
        if got > 0 {
            sys::esp_wifi_scan_get_ap_records(&mut got, recs.as_mut_ptr());
        }
        for r in recs.iter().take(usize::from(got)) {
            let end = r.ssid.iter().position(|&b| b == 0).unwrap_or(r.ssid.len());
            let ssid = String::from_utf8_lossy(&r.ssid[..end]).into_owned();
            if ssid.is_empty() {
                continue;
            }
            match found.iter_mut().find(|f| f.0 == ssid) {
                Some(f) if f.1 >= r.rssi => {}
                Some(f) => *f = (ssid, r.rssi, auth_name(r.authmode)),
                None => found.push((ssid, r.rssi, auth_name(r.authmode))),
            }
        }
    }
    found.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(json!({
        "networks": found.iter().map(|(s, r, a)| json!({"ssid": s, "rssi": r, "authmode": a})).collect::<Vec<_>>()
    }))
}

static TZ_APPLIED: Mutex<String> = Mutex::new(String::new());

/// Local wall-clock hours and minutes by the configured time zone, once
/// the clock is set. A changed `timezone` applies on the next call.
pub fn local_hm() -> Option<(u8, u8)> {
    let now = unix_now()?;
    let tz = config::get().timezone;
    {
        let mut applied = TZ_APPLIED.lock().unwrap();
        if *applied != tz {
            if let Ok(c) = std::ffi::CString::new(tz.as_str()) {
                unsafe {
                    sys::setenv(c"TZ".as_ptr(), c.as_ptr(), 1);
                    sys::tzset();
                }
            }
            *applied = tz;
        }
    }
    let t = now as sys::time_t;
    let mut tm: sys::tm = unsafe { core::mem::zeroed() };
    unsafe { sys::localtime_r(&t, &mut tm) };
    Some((tm.tm_hour as u8, tm.tm_min as u8))
}
