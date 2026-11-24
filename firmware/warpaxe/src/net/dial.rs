//! TCP to a pool over IPv6 or IPv4, whichever answers first (Happy
//! Eyeballs, RFC 8305). The order and timing live in
//! `warpaxe_stratum::dial`; this module owns the sockets.
//!
//! lwIP's `getaddrinfo` with `AF_UNSPEC` returns only the A records when
//! a name has both, so the two families are asked for separately. AAAA is
//! only asked for when the setting allows IPv6 and the station holds a
//! global or unique-local IPv6 address: an IPv4-only network pays nothing.
//! A station with only a unique-local address (a router without IPv6
//! upstream) uses IPv6 for unique-local pools only. A name whose AAAA
//! lookup came back empty or slow is not asked again for half an hour, so
//! a resolver that drops AAAA queries costs one wait, not one per
//! reconnect. IPv4 is looked up first and never waits for AAAA.

use std::ffi::CString;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use esp_idf_svc::sys;
use warpaxe_stratum::dial::{
    host_port, interleave, ipv4_first, pool_host, Race, Step, FIRST_IPV6_STAGGER_MS, STAGGER_MS,
};

/// Most addresses kept per family. lwIP's resolver keeps
/// `CONFIG_LWIP_DNS_MAX_HOST_IP` per name (1 in this build), so in
/// practice the race is one IPv6 against one IPv4 address.
const PER_FAMILY: usize = 4;
/// A name whose AAAA lookup came back empty, or took longer than
/// [`AAAA_SLOW`], is not asked for AAAA again for this long.
const AAAA_SKIP: Duration = Duration::from_secs(30 * 60);
const AAAA_SLOW: Duration = Duration::from_secs(2);
/// Names to skip AAAA for, with when that ends (a few entries at most:
/// the main and the fallback pool).
static NO_AAAA: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
/// A session over IPv6 that brought no work (the pool took the
/// connection and said nothing, or closed it before the first job) puts
/// IPv4 first for this pool for this long. Happy Eyeballs only sees the
/// TCP handshake: a silent IPv6 front end won every race, and IPv4 of
/// the same pool was never tried. IPv6 stays in the race behind IPv4, so
/// the hour costs nothing where IPv6 is fine; it outlasts the longest
/// wait between probes of the main pool (`stratum::PRIMARY_PROBE_MAX`).
const V4_FIRST_FOR: Duration = Duration::from_secs(60 * 60);
/// `host:port` of pools that were silent over IPv6, and until when IPv4
/// goes first.
static V4_FIRST: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// The session to `host:port` over `ip` brought no work. Over IPv6: IPv4
/// first for [`V4_FIRST_FOR`] (`candidates`). Over IPv4: IPv6 goes first
/// anyway; the IPv6 verdict is dropped, so the next try races as usual.
pub fn no_work_over(host: &str, port: u16, ip: IpAddr) {
    let key = host_port(pool_host(host), port);
    let mut g = V4_FIRST.lock().unwrap();
    g.retain(|(k, _)| *k != key);
    if ip.is_ipv6() {
        g.push((key, Instant::now() + V4_FIRST_FOR));
    }
}

/// Whether `host` has an IPv4 address (an A record, or is an IPv4
/// literal): only then can "IPv4 first" change anything. lwIP answers
/// a name it resolved a moment ago from its cache.
pub fn has_ipv4(host: &str, port: u16) -> bool {
    !lookup(pool_host(host), port, sys::AF_INET, false).is_empty()
}

/// The pool settings changed: IPv6 first again for every pool.
pub fn forget_no_work() {
    V4_FIRST.lock().unwrap().clear();
}

/// One resolved address, as lwIP wants it for `connect`.
struct Cand {
    addr: SocketAddr,
    raw: [u8; 28],
    len: u32,
    family: i32,
}

/// Right after joining the network the router's prefix is still on its
/// way (about 3 s after SLAAC starts on a FRITZ!Box, 2026-11-17), while the first pool
/// connection follows the IPv4 address within a fraction of a second.
/// That first connection waits up to this long, measured from the start
/// of SLAAC, for a global or unique-local address; later ones never wait.
/// On an IPv4-only network this is the one cost: up to 4 s once per
/// association.
const FIRST_PREFIX_WAIT: Duration = Duration::from_secs(4);

/// The station's IPv6 addresses beyond the link, waiting for them only in
/// the first [`FIRST_PREFIX_WAIT`] of SLAAC.
fn own_ipv6_soon() -> Vec<Ipv6Addr> {
    loop {
        let own = global_ipv6();
        let waiting = super::ip6_started_ago().is_some_and(|t| t < FIRST_PREFIX_WAIT);
        if !own.is_empty() || !waiting {
            return own;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// fc00::/7.
fn is_ula(a: &Ipv6Addr) -> bool {
    (a.segments()[0] & 0xfe00) == 0xfc00
}

/// The station's IPv6 addresses that reach beyond the link: global
/// unicast and unique local (fc00::/7), not link-local.
pub fn global_ipv6() -> Vec<Ipv6Addr> {
    let key = CString::new("WIFI_STA_DEF").unwrap();
    let netif = unsafe { sys::esp_netif_get_handle_from_ifkey(key.as_ptr()) };
    if netif.is_null() {
        return Vec::new();
    }
    // ESP-IDF writes up to LWIP_IPV6_NUM_ADDRESSES entries, unbounded.
    const N: usize = sys::LWIP_IPV6_NUM_ADDRESSES as usize;
    let mut buf: [sys::esp_ip6_addr_t; N] = unsafe { core::mem::zeroed() };
    let n = unsafe { sys::esp_netif_get_all_preferred_ip6(netif, buf.as_mut_ptr()) };
    buf.iter()
        .take(n.clamp(0, N as i32) as usize)
        .map(|a| {
            let mut b = [0u8; 16];
            for (i, w) in a.addr.iter().enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
            }
            Ipv6Addr::from(b)
        })
        .filter(|a| (a.segments()[0] & 0xffc0) != 0xfe80 && !a.is_unspecified())
        .collect()
}

/// Every address of `host` for `family`, at most [`PER_FAMILY`].
/// `numeric`: the host must be a literal (no DNS query).
fn lookup(host: &str, port: u16, family: u32, numeric: bool) -> Vec<Cand> {
    let (Ok(h), Ok(s)) = (CString::new(host), CString::new(port.to_string())) else {
        return Vec::new();
    };
    let mut hints: sys::addrinfo = unsafe { core::mem::zeroed() };
    hints.ai_family = family as i32;
    hints.ai_socktype = sys::SOCK_STREAM as i32;
    hints.ai_protocol = sys::IPPROTO_TCP as i32;
    if numeric {
        hints.ai_flags = sys::AI_NUMERICHOST as i32;
    }
    let mut res: *mut sys::addrinfo = core::ptr::null_mut();
    if unsafe { sys::lwip_getaddrinfo(h.as_ptr(), s.as_ptr(), &hints, &mut res) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut p = res;
    while !p.is_null() && out.len() < PER_FAMILY {
        let ai = unsafe { &*p };
        let len = ai.ai_addrlen as usize;
        if !ai.ai_addr.is_null() && (8..=28).contains(&len) {
            let mut raw = [0u8; 28];
            unsafe {
                core::ptr::copy_nonoverlapping(ai.ai_addr as *const u8, raw.as_mut_ptr(), len)
            };
            if let Some(addr) = decode(&raw) {
                out.push(Cand {
                    addr,
                    raw,
                    len: len as u32,
                    family: ai.ai_family,
                });
            }
        }
        p = ai.ai_next;
    }
    unsafe { sys::lwip_freeaddrinfo(res) };
    out
}

/// An lwIP sockaddr (len, family, port, address) as a `SocketAddr`.
fn decode(raw: &[u8; 28]) -> Option<SocketAddr> {
    let port = u16::from_be_bytes([raw[2], raw[3]]);
    match u32::from(raw[1]) {
        sys::AF_INET => Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(raw[4], raw[5], raw[6], raw[7])),
            port,
        )),
        sys::AF_INET6 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(&raw[8..24]);
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(b)), port))
        }
        _ => None,
    }
}

/// The addresses to try for `host:port`, in order: a literal alone (an
/// IPv6 literal is taken as typed, whatever the setting); a name's IPv6
/// addresses (when usable) and IPv4 addresses alternating.
fn candidates(host: &str, port: u16) -> Vec<Cand> {
    let host = pool_host(host);
    let literal = lookup(host, port, sys::AF_UNSPEC, true);
    if !literal.is_empty() {
        return literal;
    }
    let v4 = lookup(host, port, sys::AF_INET, false);
    let own = if crate::config::get().ipv6 {
        own_ipv6_soon()
    } else {
        Vec::new()
    };
    let now = Instant::now();
    let skip = {
        let mut no = NO_AAAA.lock().unwrap();
        no.retain(|(_, until)| *until > now);
        no.iter().any(|(h, _)| h == host)
    };
    let mut v6 = Vec::new();
    if !own.is_empty() && !skip {
        let t = Instant::now();
        v6 = lookup(host, port, sys::AF_INET6, false);
        let took = t.elapsed();
        if v6.is_empty() || took > AAAA_SLOW {
            log::info!(
                "dial: AAAA for {host}: {} in {} ms, not asked again for {} min",
                v6.len(),
                took.as_millis(),
                AAAA_SKIP.as_secs() / 60
            );
            NO_AAAA
                .lock()
                .unwrap()
                .push((host.to_string(), now + AAAA_SKIP));
        }
        // Only unique-local addresses of our own: a global pool address
        // has no route (a router without IPv6 upstream), so only
        // unique-local pools are tried over IPv6.
        if !own.iter().any(|a| !is_ula(a)) {
            v6.retain(|c| matches!(c.addr.ip(), IpAddr::V6(a) if is_ula(&a)));
        }
    }
    let silent_v6 = {
        let key = host_port(host, port);
        let mut g = V4_FIRST.lock().unwrap();
        g.retain(|(_, until)| *until > now);
        g.iter().any(|(k, _)| *k == key)
    };
    if ipv4_first(silent_v6, !v6.is_empty(), !v4.is_empty()) {
        log::info!("dial: {host} sent no work over IPv6 lately: IPv4 first");
        return interleave(v4, v6);
    }
    interleave(v6, v4)
}

/// A started, still pending connect.
struct Pending {
    fd: i32,
    idx: usize,
}

fn start(c: &Cand) -> io::Result<i32> {
    let fd =
        unsafe { sys::lwip_socket(c.family, sys::SOCK_STREAM as i32, sys::IPPROTO_TCP as i32) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { sys::lwip_fcntl(fd, sys::F_SETFL as i32, sys::O_NONBLOCK as i32) };
    let r = unsafe { sys::lwip_connect(fd, c.raw.as_ptr() as *const sys::sockaddr, c.len) };
    if r != 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(sys::EINPROGRESS as i32) {
            unsafe { sys::lwip_close(fd) };
            return Err(e);
        }
    }
    Ok(fd)
}

/// The socket's pending error after poll said it is done: 0 = connected.
fn so_error(fd: i32) -> i32 {
    let mut err: i32 = 0;
    let mut len = core::mem::size_of::<i32>() as u32;
    let r = unsafe {
        sys::lwip_getsockopt(
            fd,
            sys::SOL_SOCKET as i32,
            sys::SO_ERROR as i32,
            &mut err as *mut i32 as *mut core::ffi::c_void,
            &mut len,
        )
    };
    if r != 0 {
        -1
    } else {
        err
    }
}

/// Connect to `host:port` (a name, an IPv4 or IPv6 literal, brackets
/// allowed) within `budget`: the first address that accepts wins, the
/// rest are closed. The stream comes back blocking, as `TcpStream`
/// expects, with the address it went to.
pub fn dial(host: &str, port: u16, budget: Duration) -> anyhow::Result<(TcpStream, SocketAddr)> {
    race(host, port, &candidates(host, port), budget)
}

/// The address a TLS session should go to: with one address (any
/// IPv4-only network, any literal) that address without a connect; with
/// several, the one that answers first (the race's connection is closed
/// again, esp-tls opens its own). esp-tls gets the literal and the name
/// separately, as SNI.
pub fn pick(host: &str, port: u16, budget: Duration) -> anyhow::Result<SocketAddr> {
    let cands = candidates(host, port);
    match cands.as_slice() {
        [] => anyhow::bail!("dns: no address for {}", pool_host(host)),
        [one] => Ok(one.addr),
        _ => race(host, port, &cands, budget).map(|(_probe, addr)| addr),
    }
}

/// Set by the first race of an association that leads with IPv6;
/// cleared when SLAAC starts on a new association.
static FIRST_IPV6_RACE_DONE: AtomicBool = AtomicBool::new(false);

pub(crate) fn rearm_first_ipv6_race() {
    FIRST_IPV6_RACE_DONE.store(false, Ordering::Relaxed);
}

fn race(
    host: &str,
    port: u16,
    cands: &[Cand],
    budget: Duration,
) -> anyhow::Result<(TcpStream, SocketAddr)> {
    if cands.is_empty() {
        anyhow::bail!("dns: no address for {}", pool_host(host));
    }
    let t0 = Instant::now();
    let ms = |t0: Instant| t0.elapsed().as_millis() as u64;
    // The first race of an association that leads with IPv6 gives it a
    // longer head start ([`FIRST_IPV6_STAGGER_MS`]): the first IPv6
    // packets on a fresh address are slow to get an answer. Taken once
    // per association (rearmed with SLAAC), whatever the outcome.
    let first_gap = if cands.len() > 1
        && cands[0].addr.is_ipv6()
        && !FIRST_IPV6_RACE_DONE.swap(true, Ordering::Relaxed)
    {
        FIRST_IPV6_STAGGER_MS
    } else {
        STAGGER_MS
    };
    let mut race = Race::with_first_gap(cands.len(), 0, budget.as_millis() as u64, first_gap);
    let mut pending: Vec<Pending> = Vec::new();
    let mut last_err: Option<String> = None;
    let close_all = |p: &mut Vec<Pending>| {
        for x in p.drain(..) {
            unsafe { sys::lwip_close(x.fd) };
        }
    };
    loop {
        let wait_ms = match race.step(ms(t0)) {
            Step::Start(i) => {
                match start(&cands[i]) {
                    Ok(fd) => pending.push(Pending { fd, idx: i }),
                    Err(e) => {
                        log::info!("dial: {} failed at once: {e}", cands[i].addr);
                        last_err = Some(format!("{}: {e}", cands[i].addr));
                        race.failed(ms(t0));
                    }
                }
                continue;
            }
            Step::Wait(w) => w,
            Step::GiveUp => {
                close_all(&mut pending);
                anyhow::bail!(
                    "connect to {} failed: {}",
                    warpaxe_stratum::dial::host_port(host, port),
                    last_err.unwrap_or_else(|| "timed out".into())
                );
            }
        };
        let mut fds: Vec<sys::pollfd> = pending
            .iter()
            .map(|p| sys::pollfd {
                fd: p.fd,
                events: sys::POLLOUT as i16,
                revents: 0,
            })
            .collect();
        let n = unsafe { sys::poll(fds.as_mut_ptr(), fds.len() as _, wait_ms.min(1000) as i32) };
        if n < 0 {
            // A select error: not a refusal; do not spin on it.
            log::warn!("dial: poll failed: {}", io::Error::last_os_error());
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if n == 0 {
            continue;
        }
        let mut done: Option<usize> = None;
        let mut failed: Vec<usize> = Vec::new();
        for (k, f) in fds.iter().enumerate() {
            if f.revents == 0 {
                continue;
            }
            let err = so_error(f.fd);
            if err == 0 && (f.revents as u32 & sys::POLLOUT) != 0 {
                done = Some(k);
                break;
            }
            failed.push(k);
            let c = &cands[pending[k].idx];
            let why = if err > 0 {
                io::Error::from_raw_os_error(err).to_string()
            } else {
                "connect failed (no socket error reported)".to_string()
            };
            log::info!("dial: {} refused: {why}", c.addr);
            last_err = Some(format!("{}: {why}", c.addr));
        }
        if let Some(k) = done {
            let win = pending.remove(k);
            close_all(&mut pending);
            unsafe { sys::lwip_fcntl(win.fd, sys::F_SETFL as i32, 0) };
            let addr = cands[win.idx].addr;
            if race.started() > 1 || win.idx > 0 {
                log::info!(
                    "dial: {} answered first ({} of {} tried, {} ms)",
                    addr,
                    race.started(),
                    cands.len(),
                    ms(t0)
                );
            }
            return Ok((unsafe { TcpStream::from_raw_fd(win.fd) }, addr));
        }
        for k in failed.into_iter().rev() {
            let p = pending.remove(k);
            unsafe { sys::lwip_close(p.fd) };
            race.failed(ms(t0));
        }
    }
}
