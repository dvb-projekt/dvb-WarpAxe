//! The optional static IPv4 address (`staticIp` in the config) on the
//! LAN interface: the Ethernet HAT when a W5500 answered at boot, else
//! the Wi-Fi station. The other interface, the setup hotspot and the USB
//! link keep their own addressing; IPv6 (SLAAC) is not touched.
//!
//! The netifs are made as DHCP clients (`wifi.rs`, `eth.rs`); the static
//! address replaces DHCP by stopping the client and setting the address
//! while the link is down, before each association or driver start: an
//! `esp_netif_stop` (the hotspot opening or closing, the Ethernet driver
//! restarted) puts the client back to DHCP, so the address is set again
//! each time. On the link-up event ESP-IDF then raises "got IP" for the
//! static address itself (`esp_netif_action_connected`).
//!
//! The lock-out guard (`warpaxe_board::static_ip::Watch`): with the link
//! up, the gateway is probed with a TCP connect to port 80 every few
//! seconds (an answer of any kind, a refusal included, proves it is
//! there); a pool session over the static address counts as an answer
//! too. Nothing for `GIVE_UP_S` of link and the interface goes back to
//! DHCP until the next restart, with an event and a dashboard banner.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::EspNetif;
use esp_idf_svc::sys;
use warpaxe_board::static_ip::{Settings, Verdict, Watch};

/// How the LAN interface got its IPv4 address, for `ipMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Dhcp,
    Static,
    /// Static was set and nothing answered on it: DHCP until a restart.
    Fallback,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Dhcp => "dhcp",
            Mode::Static => "static",
            Mode::Fallback => "static-fallback",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Status {
    mode: Mode,
    /// "wifi" or "ethernet" while the static address is (or was) in use.
    iface: Option<&'static str>,
    /// The static address answered (gateway or pool) at least once.
    confirmed: bool,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    mode: Mode::Dhcp,
    iface: None,
    confirmed: false,
});

/// The mode, the interface the static address is meant for, and whether
/// it has answered, for the API.
pub fn status() -> (Mode, Option<&'static str>, bool) {
    let s = *STATUS.lock().unwrap();
    (s.mode, s.iface, s.confirmed)
}

/// The checked settings when the switch is on. A config that no longer
/// checks (it was written by hand, or a later rule is stricter) is
/// ignored with a log line: DHCP rather than an address that cannot work.
pub fn planned() -> Option<Settings> {
    let cfg = crate::config::get();
    if !cfg.static_ip {
        return None;
    }
    match cfg.static_settings() {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("static ip: settings refused ({}), using DHCP", e.text());
            None
        }
    }
}

fn addr_t(a: Ipv4Addr) -> sys::esp_ip4_addr_t {
    // lwIP keeps IPv4 addresses in network order in memory.
    sys::esp_ip4_addr_t {
        addr: u32::from_ne_bytes(a.octets()),
    }
}

fn set_dns(handle: *mut sys::esp_netif_t, which: sys::esp_netif_dns_type_t, a: Ipv4Addr) {
    let mut info: sys::esp_netif_dns_info_t = Default::default();
    info.ip.u_addr.ip4 = addr_t(a);
    info.ip.type_ = sys::ESP_IPADDR_TYPE_V4 as u8;
    // SAFETY: a valid netif handle and an initialised info struct.
    let r = unsafe { sys::esp_netif_set_dns_info(handle, which, &mut info) };
    if r != sys::ESP_OK {
        log::warn!("static ip: DNS {a} not set: {r}");
    }
}

/// Stop the DHCP client of `netif` and give it the static address. Call
/// while the link is down (before the association or the driver start):
/// the address then comes up with the link.
pub fn apply(netif: &EspNetif, s: &Settings, iface: &'static str) -> bool {
    let h = netif.handle();
    // SAFETY: a valid netif handle; plain in-parameters.
    let stop = unsafe { sys::esp_netif_dhcpc_stop(h) };
    if stop != sys::ESP_OK && stop != sys::ESP_ERR_ESP_NETIF_DHCP_ALREADY_STOPPED as i32 {
        log::error!("static ip: stopping DHCP on {iface} failed: {stop}");
        return false;
    }
    let info = sys::esp_netif_ip_info_t {
        ip: addr_t(s.addr),
        netmask: addr_t(s.mask()),
        gw: addr_t(s.gateway),
    };
    // SAFETY: as above.
    let r = unsafe { sys::esp_netif_set_ip_info(h, &info) };
    if r != sys::ESP_OK {
        log::error!("static ip: setting {} on {iface} failed: {r}", s.addr);
        return false;
    }
    // After the address: setting it clears the DNS servers.
    set_dns(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN, s.dns);
    if let Some(d2) = s.dns2 {
        set_dns(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP, d2);
    }
    let mut st = STATUS.lock().unwrap();
    if st.mode != Mode::Static || st.iface != Some(iface) {
        log::info!(
            "static ip: {iface} uses {}/{} via {}, DNS {}{}",
            s.addr,
            s.prefix,
            s.gateway,
            s.dns,
            s.dns2.map(|d| format!(", {d}")).unwrap_or_default()
        );
    }
    st.mode = Mode::Static;
    st.iface = Some(iface);
    true
}

/// Put `netif` back on DHCP. With the link down (Wi-Fi after a
/// disconnect, Ethernet after a driver stop) the client starts with the
/// next link-up; with it up, at once. Starting the client also clears the
/// static address from the netif.
pub fn back_to_dhcp(netif: &EspNetif) {
    // SAFETY: a valid netif handle.
    let r = unsafe { sys::esp_netif_dhcpc_start(netif.handle()) };
    if r != sys::ESP_OK && r != sys::ESP_ERR_ESP_NETIF_DHCP_ALREADY_STARTED as i32 {
        log::warn!("static ip: starting DHCP failed: {r}");
    }
}

/// The gateway answers on the LAN: a TCP connect to its port 80 that
/// either opens or is refused. A refusal is an answer (the host is
/// there); a timeout or "host unreachable" (no ARP reply) is not.
fn gateway_answers(gw: Ipv4Addr) -> bool {
    let to = SocketAddr::new(IpAddr::V4(gw), 80);
    match TcpStream::connect_timeout(&to, Duration::from_secs(2)) {
        Ok(_) => true,
        Err(e) => matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
        ),
    }
}

/// The lock-out guard of one interface, driven from its own thread.
pub struct Guard {
    s: Settings,
    iface: &'static str,
    watch: Watch,
    t0: Instant,
}

/// What the interface's thread must do after [`Guard::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Keep,
    /// Go back to DHCP for this boot (and reconnect, for Wi-Fi).
    FallBack,
}

impl Guard {
    pub fn new(s: Settings, iface: &'static str) -> Guard {
        Guard {
            s,
            iface,
            watch: Watch::new(),
            t0: Instant::now(),
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.s
    }

    /// The static address is still the one to set (no fallback yet).
    pub fn active(&self) -> bool {
        !self.watch.fell_back()
    }

    /// One look, every second or two: `link` = associated / cable in
    /// with the static address on the netif. Probes block for up to two
    /// seconds, only while nothing has answered yet.
    pub fn tick(&mut self, link: bool) -> Step {
        if !self.active() || self.watch.confirmed() {
            return Step::Keep;
        }
        let now = self.t0.elapsed().as_secs() as u32;
        let addr = self.s.addr.to_string();
        let st = crate::stratum::state();
        let pool = link && st.connected && st.local_ip.as_deref() == Some(addr.as_str());
        let mut answered = pool;
        if link && !answered && self.watch.probe_due(now, link) {
            self.watch.probed(now);
            answered = gateway_answers(self.s.gateway);
        }
        match self.watch.tick(now, link, answered) {
            Verdict::Wait => Step::Keep,
            Verdict::Confirmed => {
                // A pool on the LAN itself answers without the gateway: the
                // address works for the local network, which is what the
                // guard is for (reaching the miner).
                if pool {
                    log::info!(
                        "static ip: the pool session runs over {addr} on {} ({} s of link)",
                        self.iface,
                        self.watch.waited_s()
                    );
                } else {
                    log::info!(
                        "static ip: the gateway {} answers on {} ({} s of link)",
                        self.s.gateway,
                        self.iface,
                        self.watch.waited_s()
                    );
                }
                STATUS.lock().unwrap().confirmed = true;
                Step::Keep
            }
            Verdict::FallBack => {
                let gw = self.s.gateway.to_string();
                log::warn!(
                    "static ip: no answer from the gateway {gw} or the pool on {} for {} s: back to DHCP until the next restart",
                    self.iface,
                    self.watch.waited_s()
                );
                crate::events::record(
                    "net",
                    warpaxe_diag::events::detail::static_ip_fallback(
                        &addr,
                        &gw,
                        self.iface,
                        self.watch.waited_s(),
                    ),
                );
                STATUS.lock().unwrap().mode = Mode::Fallback;
                // A session that is up on the address dies with it.
                if st.connected && st.local_ip.as_deref() == Some(addr.as_str()) {
                    crate::stratum::move_session("the static IPv4 address is given up for DHCP");
                }
                Step::FallBack
            }
        }
    }
}
