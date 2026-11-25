//! Wi-Fi: the station when the config has a network, and the setup
//! hotspot when it has none or the network cannot be reached after boot
//! (`warpaxe_board::setup`) and no Ethernet link stands in (`eth.rs`).
//! Runs on its own thread; the rest of the firmware sees the station as
//! one more interface in `net::state()`.
//!
//! The hotspot is WPA2 with a password made at boot and shown only on the
//! device display. While it is up, a small DNS responder sends every name
//! to 192.168.4.1 so a phone opens the web UI as a captive portal.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::ipv4::{self, Ipv4Addr};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration};
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{
    AccessPointConfiguration, AuthMethod, BlockingWifi, ClientConfiguration, Configuration,
    EspWifi, WifiDriver,
};
use warpaxe_board::setup::{self, Action, Hotspot, AP_IP};

use super::static_ip::{self, Guard, Step};

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
    hostname: String,
) {
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || run(modem, sysloop, nvs, ssid, pass, hostname))
        .expect("wifi thread");
}

fn run(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    ssid: String,
    pass: String,
    hostname: String,
) {
    // The station asks the router's DHCP for an address under the
    // miner's own name (esp-idf's default is "espressif").
    let sta_netif_conf = NetifConfiguration {
        ip_configuration: Some(ipv4::Configuration::Client(
            ipv4::ClientConfiguration::DHCP(ipv4::DHCPClientSettings {
                hostname: heapless::String::try_from(hostname.as_str()).ok(),
            }),
        )),
        ..NetifConfiguration::wifi_default_client()
    };
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
            EspNetif::new_with_conf(&sta_netif_conf)?,
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
    let mut guard = static_guard();

    let t0 = Instant::now();
    let mut hotspot = Hotspot::new();
    let mut dns_started = false;
    let mut was_connected = false;
    let mut next_try = Instant::now();
    let mut ip: Option<String> = None;
    let mut ip6_started = false;
    let mut ip6_logged = false;
    loop {
        let mut connected = wifi.is_connected().unwrap_or(false);
        if was_connected && !connected {
            log::warn!("wifi: link lost");
            super::set_wifi(false, None, None);
            ip = None;
            ip6_started = false;
            ip6_logged = false;
            super::set_ip6_started(None);
        }
        was_connected = connected;

        // Ethernet counts as the connection the hotspot would stand in
        // for: with a cable the hotspot never opens, and one that is open
        // closes once the cable has held for a minute, with or without a
        // network in the config.
        let n = super::state();
        let eth = n.eth_connected;
        // While the HAT's probe and link are still coming (the first
        // seconds of a boot), a board without a network in the settings
        // waits rather than opening the hotspot for a minute and closing
        // it again once the cable has an address. A fitted HAT counts as
        // a configured network from then on: with no cable the hotspot
        // opens after the usual three minutes, not at once. A cable on
        // which no address comes (no DHCP server) stops holding it back
        // after `eth::NO_ADDRESS`.
        let eth_fitted = n.eth_fitted == Some(true);
        let action = if !has_ssid && (n.eth_probing || (eth_fitted && n.eth_waiting)) {
            Action::Keep
        } else {
            hotspot.tick(
                has_ssid || eth || eth_fitted,
                t0.elapsed().as_secs() as u32,
                connected || eth,
            )
        };
        match action {
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
                // The stop dropped the link without the "link lost" branch
                // above: the next association starts over (address, IPv6),
                // and the rest of this pass does not see the old link.
                was_connected = false;
                connected = false;
                ip = None;
                super::set_wifi(false, None, None);
                ip6_started = false;
                ip6_logged = false;
                super::set_ip6_started(None);
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
                connected = false;
                ip = None;
                super::set_wifi(false, None, None);
                ip6_started = false;
                ip6_logged = false;
                super::set_ip6_started(None);
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
                log::info!(
                    "wifi: connected, ip {ip:?} (stack {} B never used)",
                    crate::diag::stack_unused()
                );
                super::set_ip4(false, super::Ip4::of(wifi.wifi().sta_netif()));
            }
            super::set_wifi(true, ip.clone(), rssi());
            // IPv6 (`docs/17`): the link-local address starts SLAAC
            // (LWIP_IPV6_AUTOCONFIG); the router's advertisement then adds
            // the global and unique-local ones a few seconds later. Once
            // per association, retried while the netif is not up yet
            // (ESP_FAIL), and also when the setting is switched on later.
            if !ip6_started {
                ip6_started = start_ip6(wifi.wifi().sta_netif());
            }
            if ip6_started && !ip6_logged {
                if let Some(t) = super::ip6_started_ago() {
                    if !super::dial::global_ipv6().is_empty() {
                        log::info!(
                            "wifi: IPv6 address from the router after {} ms",
                            t.as_millis()
                        );
                        ip6_logged = true;
                    }
                }
            }
        } else if has_ssid && Instant::now() >= next_try && !(hotspot.is_open() && ap_clients() > 0)
        {
            // The static address (`net::static_ip`), set again before each
            // attempt: a stop of the station (the hotspot opening or
            // closing) puts its netif back on DHCP.
            // Then the guard ticks without the link: the attempt below can
            // take 20 s of scanning, and none of it counts as link time
            // ([`Guard::tick`]). After the address, whose DNS servers the
            // tick would otherwise find missing.
            if guard.as_ref().is_some_and(|g| {
                g.active() && !static_ip::apply(wifi.wifi().sta_netif(), g.settings(), "wifi")
            }) {
                guard = None;
            }
            if let Some(g) = guard.as_mut() {
                g.tick(wifi.wifi().sta_netif(), false);
            }
            // Not while someone is on the hotspot: an attempt scans every
            // channel for ~20 s and drops the phone that is setting it up.
            // SLAAC as soon as the station is associated, not after DHCP:
            // the router's prefix then arrives about when the IPv4 address
            // does, before the first pool connection (`net::dial`).
            // With the switch off there is nothing to wait for.
            let want_ip6 = crate::config::get().ipv6;
            match wifi.connect().and_then(|_| {
                for _ in 0..if want_ip6 { 20 } else { 0 } {
                    if ip6_started || start_ip6(wifi.wifi().sta_netif()) {
                        ip6_started = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                wifi.wait_netif_up()
            }) {
                Ok(()) => continue, // picked up at the top of the loop
                Err(e) => {
                    log::warn!("wifi: connect failed: {e}");
                    super::set_wifi(false, None, None);
                    ip6_started = false;
                    ip6_logged = false;
                    super::set_ip6_started(None);
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
        // The lock-out guard: no answer on the static address for a few
        // minutes of link and the station goes back to DHCP for the boot.
        if let Some(g) = guard.as_mut() {
            if g.tick(wifi.wifi().sta_netif(), connected) == Step::FallBack {
                let _ = wifi.disconnect();
                static_ip::back_to_dhcp(wifi.wifi().sta_netif());
                was_connected = false;
                ip = None;
                super::set_wifi(false, None, None);
                ip6_started = false;
                ip6_logged = false;
                super::set_ip6_started(None);
                next_try = Instant::now();
                continue;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// The static IPv4 address for the station, when the setting is on and
/// no Ethernet HAT takes it (`net::static_ip`). Waits for the HAT's
/// probe first (a fraction of a second; at most ten), since its verdict
/// decides which interface carries the address.
fn static_guard() -> Option<Guard> {
    let s = static_ip::planned()?;
    let t = Instant::now();
    while super::state().eth_probing && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let n = super::state();
    if n.eth_fitted == Some(true) && !n.eth_failed {
        log::info!(
            "wifi: the static IPv4 address goes to the Ethernet HAT, the station keeps DHCP"
        );
        return None;
    }
    Some(Guard::new(s, "wifi"))
}

/// IPv6 (`docs/17`): create the station's link-local address, which
/// starts SLAAC (LWIP_IPV6_AUTOCONFIG); the router's advertisement then
/// adds the global and unique-local addresses. Off with the setting;
/// false while the netif is not up yet (ESP_FAIL), so the caller retries.
fn start_ip6(netif: &EspNetif) -> bool {
    if !crate::config::get().ipv6 {
        return false;
    }
    let handle = esp_idf_svc::handle::RawHandle::handle(netif);
    if unsafe { esp_idf_svc::sys::esp_netif_create_ip6_linklocal(handle) } != 0 {
        return false;
    }
    super::set_ip6_started(Some(Instant::now()));
    log::info!("wifi: IPv6 link-local up, waiting for the router's prefix");
    true
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
        log::info!(
            "wifi: modem sleep stays on (no fan to drive: the radio's heat reaches the chip)"
        );
        return;
    }
    let r =
        unsafe { esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_NONE) };
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
