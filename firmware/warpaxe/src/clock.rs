//! Wall-clock time by SNTP (optional, `useNTP`), and the Wi-Fi scan the
//! settings page lists networks from.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use esp_idf_svc::sntp::{EspSntp, SntpConf};
use esp_idf_svc::sys;
use serde_json::json;

use crate::config;

static SNTP: Mutex<Option<EspSntp<'static>>> = Mutex::new(None);

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
    match EspSntp::new(&conf) {
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
