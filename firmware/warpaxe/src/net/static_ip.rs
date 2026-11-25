//! The optional static IPv4 address (`staticIpv4` in the config) on the
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

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::time::Instant;

use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::netif::EspNetif;
use esp_idf_svc::sys;
use warpaxe_board::static_ip::{Settings, Verdict, Watch};

/// `ipMode`, by index: no static address, static and nothing has
/// answered yet, static and answered, given up for DHCP until a restart,
/// switched on but the stored settings do not check (DHCP).
const MODES: [&str; 5] = [
    "dhcp",
    "static-wait",
    "static",
    "static-fallback",
    "static-invalid",
];
const WAIT: u8 = 1;
const CONFIRMED: u8 = 2;
const FALLBACK: u8 = 3;
const INVALID: u8 = 4;
static MODE: AtomicU8 = AtomicU8::new(0);
/// The static address went to the Ethernet HAT (else to the station).
static ON_ETH: AtomicBool = AtomicBool::new(false);

/// The static address while it waits for an answer (network-order bytes
/// as a word, 0 = none), and whether a connection came in on it.
static ADDR: AtomicU32 = AtomicU32::new(0);
static INBOUND: AtomicBool = AtomicBool::new(false);

/// The HTTP server's open hook, for every accepted connection: one whose
/// local address is the static address, while that waits for an answer,
/// confirms it at the guard's next tick. An accepted connection means the
/// handshake went both ways: a client on the network reached the address
/// and got its replies. (An ARP reply of the gateway did not prove that:
/// a mistyped subnet that contains the real gateway gets one too, with
/// the replies to the address lost.)
pub fn note_inbound(fd: i32) {
    let want = ADDR.load(Ordering::Relaxed);
    if want == 0 || MODE.load(Ordering::Relaxed) != WAIT {
        return;
    }
    let mut buf = [0u8; 32];
    let mut len = buf.len() as sys::socklen_t;
    // SAFETY: an open socket of the HTTP server and a buffer of `len`.
    if unsafe { sys::lwip_getsockname(fd, buf.as_mut_ptr() as *mut sys::sockaddr, &mut len) } != 0 {
        return;
    }
    let Some(v4) = warpaxe_board::static_ip::lwip_sockaddr_v4(&buf) else {
        return;
    };
    if u32::from_ne_bytes(v4) == want {
        INBOUND.store(true, Ordering::Relaxed);
    }
}

/// The other interface's DNS servers (main, backup; network-order bytes
/// as a word, 0 = none), seen in lwIP's one list before the typed ones
/// replaced them: with the address on the HAT, the station's from its
/// lease. Put back when the HAT gives the list up again.
static OTHER_DNS: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Put the other interface's DNS servers back ([`OTHER_DNS`]), if any were
/// seen: after the HAT's fallback (its DHCP client start cleared the list)
/// and when the HAT's link goes after the address was confirmed (the
/// typed servers may not be reachable from the station's network). The
/// next lease of either interface writes the list anyway.
pub fn restore_other_dns(netif: &EspNetif) {
    let main = OTHER_DNS[0].load(Ordering::Relaxed).to_ne_bytes();
    if main == [0; 4] {
        return;
    }
    let backup = match OTHER_DNS[1].load(Ordering::Relaxed).to_ne_bytes() {
        [0, 0, 0, 0] => main,
        b => b,
    };
    let h = netif.handle();
    set_dns(
        h,
        sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
        Ipv4Addr::from(main),
    );
    set_dns(
        h,
        sys::esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP,
        Ipv4Addr::from(backup),
    );
    log::info!(
        "static ip: DNS {} of the other interface back",
        Ipv4Addr::from(main)
    );
}

/// The interface (`eth` = the HAT, else the station) carries a static
/// address that nothing has answered yet: its link does not count as
/// online for the setup hotspot (`setup::counts_online`), so a static
/// address that never works, followed by a DHCP that fails too, still
/// gets the hotspot after its three minutes.
pub fn unconfirmed(eth: bool) -> bool {
    MODE.load(Ordering::Relaxed) == WAIT && ON_ETH.load(Ordering::Relaxed) == eth
}

/// `ipMode` for the API.
pub fn mode() -> &'static str {
    MODES
        .get(usize::from(MODE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or(MODES[0])
}

/// The checked settings when the switch is on. A config that no longer
/// checks (it was written by hand, or a later rule is stricter) is
/// ignored with a log line: DHCP rather than an address that cannot work.
pub fn planned() -> Option<Settings> {
    let cfg = crate::config::get();
    if !cfg.static_ip_on() {
        return None;
    }
    match cfg.static_settings() {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("static ip: {}, using DHCP", e.text());
            MODE.store(INVALID, Ordering::Relaxed);
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

/// Both DNS servers of `s`; with no second one typed the first stands in
/// for it (so a lease's backup server does not linger in that slot).
fn set_dns_both(h: *mut sys::esp_netif_t, s: &Settings) {
    set_dns(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN, s.dns);
    set_dns(
        h,
        sys::esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP,
        s.dns2.unwrap_or(s.dns),
    );
}

/// A DNS server as lwIP has it now (one list for all interfaces:
/// CONFIG_ESP_NETIF_SET_DNS_PER_DEFAULT_NETIF is off), 0.0.0.0 for none
/// or an IPv6 one.
pub(super) fn dns_now(h: *mut sys::esp_netif_t, which: sys::esp_netif_dns_type_t) -> [u8; 4] {
    let mut d = sys::esp_netif_dns_info_t::default();
    // SAFETY: a valid netif handle and a plain out-parameter.
    let ok = unsafe { sys::esp_netif_get_dns_info(h, which, &mut d) } == sys::ESP_OK;
    if ok && d.ip.type_ == sys::ESP_IPADDR_TYPE_V4 as u8 {
        // SAFETY: the IPv4 member of the union, as the type says.
        unsafe { d.ip.u_addr.ip4.addr }.to_ne_bytes()
    } else {
        [0; 4]
    }
}

/// Stop the DHCP client of `netif` and give it the static address. Call
/// while the link is down (before the association or the driver start):
/// the address then comes up with the link. False when the address could
/// not be set: the netif is back on DHCP then (a stopped client and no
/// address would leave it with neither, and no guard would ever see a
/// link to fall back from), and the caller drops its guard for the boot.
pub fn apply(netif: &EspNetif, s: &Settings, iface: &'static str) -> bool {
    let h = netif.handle();
    // SAFETY: a valid netif handle; plain in-parameters.
    let mut r = unsafe { sys::esp_netif_dhcpc_stop(h) };
    if r == sys::ESP_OK || r == sys::ESP_ERR_ESP_NETIF_DHCP_ALREADY_STOPPED {
        let info = sys::esp_netif_ip_info_t {
            ip: addr_t(s.addr),
            netmask: addr_t(s.mask()),
            gw: addr_t(s.gateway),
        };
        // SAFETY: as above.
        r = unsafe { sys::esp_netif_set_ip_info(h, &info) };
    }
    if r != sys::ESP_OK {
        log::error!("static ip: not set on {iface}: {r}, DHCP until the next restart");
        back_to_dhcp(netif);
        MODE.store(0, Ordering::Relaxed);
        return false;
    }
    // After the address: setting it clears the DNS servers.
    set_dns_both(h, s);
    ADDR.store(u32::from_ne_bytes(s.addr.octets()), Ordering::Relaxed);
    // The first time only (the address is set again before each
    // association): log it and start in "waiting for an answer".
    if MODE
        .compare_exchange(0, WAIT, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        ON_ETH.store(iface == "ethernet", Ordering::Relaxed);
        log::info!(
            "static ip: {iface} {}/{} gw {}",
            s.addr,
            s.prefix,
            s.gateway
        );
    }
    true
}

/// Put `netif` back on DHCP. With the link down (Wi-Fi after a
/// disconnect, Ethernet after a driver stop) the client starts with the
/// next link-up; with it up, at once. Starting the client also clears the
/// static address from the netif.
pub fn back_to_dhcp(netif: &EspNetif) {
    // SAFETY: a valid netif handle.
    let r = unsafe { sys::esp_netif_dhcpc_start(netif.handle()) };
    if r != sys::ESP_OK && r != sys::ESP_ERR_ESP_NETIF_DHCP_ALREADY_STARTED {
        log::warn!("static ip: starting DHCP failed: {r}");
    }
}

/// The gateway answers on the LAN, asked from the static address `from`
/// (bound, so the answer cannot come over the other interface on the
/// same subnet): a TCP connect to its port 80 that either opens or is
/// refused within two seconds. A refusal is an answer (the host is
/// there); a timeout or "host unreachable" (no ARP reply) is not.
fn gateway_answers(from: Ipv4Addr, gw: Ipv4Addr) -> bool {
    let sin = |a: Ipv4Addr, port: u16| sys::sockaddr_in {
        sin_len: core::mem::size_of::<sys::sockaddr_in>() as u8,
        sin_family: sys::AF_INET as _,
        sin_port: port.to_be(),
        sin_addr: sys::in_addr {
            s_addr: u32::from_ne_bytes(a.octets()),
        },
        sin_zero: [0; 8],
    };
    let (me, to) = (sin(from, 0), sin(gw, 80));
    let len = core::mem::size_of::<sys::sockaddr_in>() as u32;
    let answer = |e: i32| e == 0 || e == sys::ECONNREFUSED as i32 || e == sys::ECONNRESET as i32;
    // SAFETY: plain lwIP socket calls on a socket of our own, closed on
    // every path; the addresses outlive the calls.
    unsafe {
        let fd = sys::lwip_socket(
            sys::AF_INET as i32,
            sys::SOCK_STREAM as i32,
            sys::IPPROTO_TCP as i32,
        );
        if fd < 0 {
            return false;
        }
        let mut ok = false;
        if sys::lwip_bind(fd, &me as *const _ as *const sys::sockaddr, len) == 0 {
            sys::lwip_fcntl(fd, sys::F_SETFL as i32, sys::O_NONBLOCK as i32);
            if sys::lwip_connect(fd, &to as *const _ as *const sys::sockaddr, len) == 0 {
                ok = true;
            } else {
                let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
                if e == sys::EINPROGRESS as i32 {
                    let mut p = sys::pollfd {
                        fd,
                        events: sys::POLLOUT as i16,
                        revents: 0,
                    };
                    ok = sys::poll(&mut p, 1, 2000) > 0 && answer(super::dial::so_error(fd));
                } else {
                    ok = answer(e);
                }
            }
        }
        sys::lwip_close(fd);
        ok
    }
}

/// The lock-out guard of one interface, driven from its own thread.
pub struct Guard {
    s: Settings,
    iface: &'static str,
    watch: Watch,
    t0: Instant,
    /// The last tick had the link.
    had_link: bool,
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
            had_link: false,
        }
    }

    /// The typed DNS servers stay the ones in use while the address
    /// does (confirmed, with the link up). lwIP keeps one list for all
    /// interfaces, and every DHCP client start and every lease of the
    /// other interface (the station beside a static HAT) writes it: set
    /// them again when they were replaced.
    fn keep_dns(&self, netif: &EspNetif) {
        let h = netif.handle();
        let main = dns_now(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN);
        let backup = dns_now(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP);
        let d = self.s.dns.octets();
        let d2 = self.s.dns2.map_or(d, |x| x.octets());
        if main == d && (backup == d2 || (self.s.dns2.is_none() && backup == [0; 4])) {
            return;
        }
        self.remember_other_dns(h);
        set_dns_both(h, &self.s);
        log::info!("static ip: DNS {} set again", self.s.dns);
        if let Some(i) = super::Ip4::of(netif) {
            super::set_ip4(self.iface == "ethernet", Some(i));
        }
    }

    /// What lwIP's list holds now, when it is not the typed servers: the
    /// other interface's ([`OTHER_DNS`]).
    fn remember_other_dns(&self, h: *mut sys::esp_netif_t) {
        let main = dns_now(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN);
        if main == [0; 4] || main == self.s.dns.octets() {
            return;
        }
        let backup = dns_now(h, sys::esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP);
        OTHER_DNS[0].store(u32::from_ne_bytes(main), Ordering::Relaxed);
        OTHER_DNS[1].store(u32::from_ne_bytes(backup), Ordering::Relaxed);
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
    pub fn tick(&mut self, netif: &EspNetif, link: bool) -> Step {
        if !self.active() {
            return Step::Keep;
        }
        if self.watch.holds_dns(link) {
            self.keep_dns(netif);
        }
        // The HAT's cable out after the address was confirmed: the
        // station's DNS again, the typed servers may sit behind the cable.
        let lost = self.had_link && !link;
        self.had_link = link;
        if self.watch.confirmed() {
            if lost && self.iface == "ethernet" {
                restore_other_dns(netif);
            }
            return Step::Keep;
        }
        let now = self.t0.elapsed().as_secs() as u32;
        let addr = self.s.addr.to_string();
        let pool = link && crate::stratum::session_from(&addr);
        let mut answered = pool;
        // A connection to the web UI on the static address: the handshake
        // went both ways (`note_inbound`).
        let inbound = link && INBOUND.swap(false, Ordering::Relaxed);
        let mut by = if pool { "pool" } else { "web UI connection" };
        answered |= inbound;
        if link && !answered && self.watch.probe_due(now, link) {
            self.watch.probed(now);
            answered = gateway_answers(self.s.addr, self.s.gateway);
            by = "gateway";
        }
        match self.watch.tick(now, link, answered) {
            Verdict::Wait => Step::Keep,
            Verdict::Confirmed => {
                // A pool on the LAN itself answers without the gateway: the
                // address works for the local network, which is what the
                // guard is for (reaching the miner).
                log::info!(
                    "static ip: {} confirmed by the {} after {} s (stack {} B never used)",
                    self.iface,
                    by,
                    self.watch.waited_s(),
                    crate::diag::stack_unused()
                );
                MODE.store(CONFIRMED, Ordering::Relaxed);
                Step::Keep
            }
            Verdict::FallBack => {
                // The list as it is now, before the DHCP client start
                // clears it (`restore_other_dns` puts it back).
                self.remember_other_dns(netif.handle());
                // The event's own log line says what happened. No pool
                // session can be up on the address here: one would have
                // confirmed it.
                let gw = self.s.gateway.to_string();
                crate::events::record(
                    "net",
                    warpaxe_diag::events::detail::static_ip_fallback(
                        &addr,
                        &gw,
                        self.iface,
                        self.watch.waited_s(),
                    ),
                );
                MODE.store(FALLBACK, Ordering::Relaxed);
                log::info!(
                    "static ip: {} stack {} B never used",
                    self.iface,
                    crate::diag::stack_unused()
                );
                Step::FallBack
            }
        }
    }
}
