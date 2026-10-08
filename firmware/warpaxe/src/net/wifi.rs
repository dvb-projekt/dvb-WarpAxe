//! Wi-Fi: the station when the config has a network, and the setup
//! hotspot when it has none or the network cannot be reached after boot
//! (`warpaxe_board::setup`). Runs on its own thread; the rest of the
//! firmware sees the station as one more interface in `net::state()`.
//!
//! The hotspot is WPA2 with a password made at boot and shown only on the
//! device display. While it is up, a small DNS responder sends every name
//! to 192.168.4.1 so a phone opens the web UI as a captive portal.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::ipv4::{self, Ipv4Addr};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi, WifiDriver,
};
use warpaxe_board::setup::{self, Action, Hotspot, AP_IP};

/// Between station attempts while the hotspot is up: each attempt moves
/// the hotspot to the network's channel and can drop the phone on it.
const RETRY_WITH_AP: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(10);

pub fn start(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: String,
    pass: String,
) {
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || run(modem, sysloop, nvs, ssid, pass))
        .expect("wifi thread");
}

fn run(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: String,
    pass: String,
) {
    // The hotspot on 192.168.4.1, handing out itself as DNS server:
    // esp-idf-svc's default router netif is 192.168.71.1 with 8.8.8.8 as
    // DNS, which a phone on the hotspot cannot reach, so it reports "no
    // internet" and never asks the captive-portal DNS.
    let ap_netif_conf = NetifConfiguration {
        ip_configuration: Some(ipv4::Configuration::Router(ipv4::RouterConfiguration {
            subnet: ipv4::Subnet {
                gateway: Ipv4Addr::from(AP_IP),
                mask: ipv4::Mask(24),
            },
            dhcp_enabled: true,
            dns: Some(Ipv4Addr::from(AP_IP)),
            secondary_dns: None,
        })),
        ..NetifConfiguration::wifi_default_router()
    };
    let wifi = WifiDriver::new(modem, sysloop.clone(), Some(nvs)).and_then(|driver| {
        EspWifi::wrap_all(
            driver,
            EspNetif::new(NetifStack::Sta)?,
            EspNetif::new_with_conf(&ap_netif_conf)?,
        )
    });
    let mut wifi = match wifi {
        Ok(w) => w,
        Err(e) => {
            log::error!("wifi: init failed: {e}");
            return;
        }
    };
    let has_ssid = !ssid.is_empty();
    let client = ClientConfiguration {
        ssid: heapless::String::try_from(ssid.as_str()).unwrap_or_default(),
        password: heapless::String::try_from(pass.as_str()).unwrap_or_default(),
        auth_method: if pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    };
    let (ap_ssid, ap_pass) = hotspot_credentials();
    let ap = AccessPointConfiguration {
        ssid: heapless::String::try_from(ap_ssid.as_str()).unwrap_or_default(),
        password: heapless::String::try_from(ap_pass.as_str()).unwrap_or_default(),
        auth_method: AuthMethod::WPA2Personal,
        channel: 1,
        max_connections: 4,
        ..Default::default()
    };
    if let Err(e) = wifi.set_configuration(&Configuration::Client(client.clone())) {
        log::error!("wifi: config failed: {e}");
        return;
    }
    let mut wifi = match BlockingWifi::wrap(&mut wifi, sysloop) {
        Ok(w) => w,
        Err(e) => {
            log::error!("wifi: wrap failed: {e}");
            return;
        }
    };
    if let Err(e) = wifi.start() {
        log::error!("wifi: start failed: {e}");
        return;
    }
    no_modem_sleep();

    let t0 = Instant::now();
    let mut hotspot = Hotspot::new();
    let mut dns_started = false;
    let mut was_connected = false;
    let mut next_try = Instant::now();
    let mut ip: Option<String> = None;
    loop {
        let connected = wifi.is_connected().unwrap_or(false);
        if was_connected && !connected {
            log::warn!("wifi: link lost");
            super::set_wifi(false, None, None);
            ip = None;
        }
        was_connected = connected;

        match hotspot.tick(has_ssid, t0.elapsed().as_secs() as u32, connected) {
            Action::Open => {
                let _ = wifi.stop();
                match wifi
                    .set_configuration(&Configuration::Mixed(client.clone(), ap.clone()))
                    .and_then(|_| wifi.start())
                {
                    Ok(()) => {
                        no_modem_sleep();
                        log::warn!(
                            "wifi: setup hotspot {ap_ssid} open (password on the device display), web UI at 192.168.4.1"
                        );
                        super::set_hotspot(Some((ap_ssid.clone(), ap_pass.clone())));
                        if !dns_started {
                            dns_started = start_dns();
                        }
                    }
                    Err(e) => log::error!("wifi: hotspot failed: {e}"),
                }
                was_connected = false;
            }
            Action::Close => {
                let _ = wifi.stop();
                if let Err(e) = wifi
                    .set_configuration(&Configuration::Client(client.clone()))
                    .and_then(|_| wifi.start())
                {
                    log::error!("wifi: back to station only failed: {e}");
                }
                no_modem_sleep();
                super::set_hotspot(None);
                log::info!("wifi: setup hotspot closed, station holds");
                was_connected = false;
                next_try = Instant::now();
            }
            Action::Keep => {}
        }

        if connected {
            if ip.is_none() {
                ip = wifi
                    .wifi()
                    .sta_netif()
                    .get_ip_info()
                    .map(|i| i.ip.to_string())
                    .ok();
                log::info!("wifi: connected, ip {ip:?}");
            }
            super::set_wifi(true, ip.clone(), rssi());
        } else if has_ssid && Instant::now() >= next_try && !(hotspot.is_open() && ap_clients() > 0)
        {
            // Not while someone is on the hotspot: an attempt scans every
            // channel for ~20 s and drops the phone that is setting it up.
            match wifi.connect().and_then(|_| wifi.wait_netif_up()) {
                Ok(()) => continue, // picked up at the top of the loop
                Err(e) => {
                    log::warn!("wifi: connect failed: {e}");
                    super::set_wifi(false, None, None);
                    let _ = wifi.disconnect();
                    next_try = Instant::now()
                        + if hotspot.is_open() {
                            RETRY_WITH_AP
                        } else {
                            RETRY
                        };
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// `WarpAxe-XXXX` from the soft-AP MAC and a password from the hardware
/// RNG, new at every boot.
fn hotspot_credentials() -> (String, String) {
    let mut mac = [0u8; 6];
    // SAFETY: 6-byte out-buffer as the call requires.
    unsafe {
        esp_idf_svc::sys::esp_read_mac(
            mac.as_mut_ptr(),
            esp_idf_svc::sys::esp_mac_type_t_ESP_MAC_WIFI_SOFTAP,
        );
    }
    // SAFETY: plain read of the hardware RNG (RF is on once Wi-Fi exists).
    let (r0, r1) = unsafe {
        (
            esp_idf_svc::sys::esp_random(),
            esp_idf_svc::sys::esp_random(),
        )
    };
    (
        String::from_utf8_lossy(&setup::ssid(mac)).into_owned(),
        String::from_utf8_lossy(&setup::password(r0, r1)).into_owned(),
    )
}

/// The captive-portal DNS on the hotspot address; stays for the uptime,
/// silent while the hotspot is down.
/// Keep the radio awake. IDF's default modem sleep wakes only at the
/// access point's beacons (~100 ms), so jobs from the pool, API answers
/// and pings wait for the next one; a miner on mains power gains nothing
/// from the few tenths of a watt it saves.
///
/// Except a board without a fan to drive (the NerdNOS): its T-Display
/// sits in the same closed case as the chip, and those tenths of a watt
/// showed up as 3 °C on the chip's thermistor against the stock firmware
/// (2026-10-08, 300 MHz, same hashrate: 74 °C against 71). A job 100 ms
/// late costs a 170 GH/s chip nothing, so the radio sleeps there. The
/// devkit keeps the radio awake: its CPU miner submits shares every few
/// seconds and feels the beacon wait.
fn no_modem_sleep() {
    if !crate::board::profile().has_fan() && !crate::board::devkit() {
        log::info!("wifi: modem sleep stays on (no fan to drive: the radio's heat reaches the chip)");
        return;
    }
    let r = unsafe { esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE) };
    if r != 0 {
        log::warn!("wifi: power save off failed: {r}");
    }
}

fn start_dns() -> bool {
    let spawned = std::thread::Builder::new().stack_size(4096).spawn(|| {
        let addr = std::net::SocketAddr::from((AP_IP, 53));
        let sock = loop {
            match UdpSocket::bind(addr) {
                Ok(s) => break s,
                Err(e) => {
                    log::debug!("dns: bind {addr}: {e}, retrying");
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        };
        log::info!("dns: captive portal on {addr}");
        let mut q = [0u8; 512];
        let mut r = [0u8; 512];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut q) else {
                continue;
            };
            if let Some(len) = setup::dns_reply(&q[..n], AP_IP, &mut r) {
                let _ = sock.send_to(&r[..len], from);
            }
        }
    });
    match spawned {
        Ok(_) => true,
        Err(e) => {
            log::error!("dns: thread failed: {e}");
            false
        }
    }
}

/// Stations on the setup hotspot.
fn ap_clients() -> usize {
    let mut list = esp_idf_svc::sys::wifi_sta_list_t::default();
    // SAFETY: plain out-parameter; fails harmlessly when the AP is down.
    let err = unsafe { esp_idf_svc::sys::esp_wifi_ap_get_sta_list(&mut list) };
    if err == esp_idf_svc::sys::ESP_OK {
        list.num.max(0) as usize
    } else {
        0
    }
}

/// Signal strength of the associated AP in dBm, as AxeOS reports `wifiRSSI`.
fn rssi() -> Option<i32> {
    let mut info = esp_idf_svc::sys::wifi_ap_record_t::default();
    // SAFETY: plain out-parameter; only called while the station is connected.
    let err = unsafe { esp_idf_svc::sys::esp_wifi_sta_get_ap_info(&mut info) };
    (err == esp_idf_svc::sys::ESP_OK).then_some(info.rssi as i32)
}
