//! TCP to a pool over IPv6 or IPv4, whichever answers first (Happy
//! Eyeballs, RFC 8305). The order and timing live in
//! `warpaxe_stratum::dial`; this module owns the sockets.
//!
//! lwIP's `getaddrinfo` with `AF_UNSPEC` returns only the A records when
//! a name has both, so the two families are asked for separately. AAAA is
//! only asked for when the setting allows IPv6 and the station holds a
//! global or unique-local IPv6 address: an IPv4-only network pays nothing.

use std::ffi::CString;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::os::fd::FromRawFd;
use std::time::{Duration, Instant};

use esp_idf_svc::sys;
use warpaxe_stratum::dial::{interleave, pool_host, Race, Step};

/// Most addresses tried per family (lwIP keeps at most this many per
/// lookup with the default `DNS_MAX_HOST_IP` anyway).
const PER_FAMILY: usize = 4;

/// One resolved address, as lwIP wants it for `connect`.
struct Cand {
    addr: SocketAddr,
    raw: [u8; 28],
    len: u32,
    family: i32,
}

/// Whether pool connections may use IPv6 now: the setting on and the
/// station holding a global or unique-local IPv6 address.
pub fn ipv6_usable() -> bool {
    crate::config::get().ipv6 && !global_ipv6().is_empty()
}

/// The station's IPv6 addresses that reach beyond the link: global
/// unicast and unique local (fc00::/7), not link-local.
pub fn global_ipv6() -> Vec<Ipv6Addr> {
    let key = CString::new("WIFI_STA_DEF").unwrap();
    let netif = unsafe { sys::esp_netif_get_handle_from_ifkey(key.as_ptr()) };
    if netif.is_null() {
        return Vec::new();
    }
    let mut buf: [sys::esp_ip6_addr_t; 3] = unsafe { core::mem::zeroed() };
    let n = unsafe { sys::esp_netif_get_all_preferred_ip6(netif, buf.as_mut_ptr()) };
    buf.iter()
        .take(n.clamp(0, 3) as usize)
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

/// The addresses to try for `host:port`, in order: a literal alone; a
/// name's IPv6 addresses (when usable) and IPv4 addresses alternating.
fn candidates(host: &str, port: u16) -> Vec<Cand> {
    let host = pool_host(host);
    let literal = lookup(host, port, sys::AF_UNSPEC, true);
    if !literal.is_empty() {
        return literal;
    }
    let v6 = if ipv6_usable() {
        lookup(host, port, sys::AF_INET6, false)
    } else {
        Vec::new()
    };
    interleave(v6, lookup(host, port, sys::AF_INET, false))
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
    let cands = candidates(host, port);
    if cands.is_empty() {
        anyhow::bail!("dns: no address for {}", pool_host(host));
    }
    let t0 = Instant::now();
    let ms = |t0: Instant| t0.elapsed().as_millis() as u64;
    let mut race = Race::new(cands.len(), 0, budget.as_millis() as u64);
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
                        log::info!("stratum: {} failed at once: {e}", cands[i].addr);
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
        if n <= 0 {
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
            let e = io::Error::from_raw_os_error(err.max(0));
            log::info!("stratum: {} refused: {e}", c.addr);
            last_err = Some(format!("{}: {e}", c.addr));
        }
        if let Some(k) = done {
            let win = pending.remove(k);
            close_all(&mut pending);
            unsafe { sys::lwip_fcntl(win.fd, sys::F_SETFL as i32, 0) };
            let addr = cands[win.idx].addr;
            if race.started() > 1 || win.idx > 0 {
                log::info!(
                    "stratum: {} answered first ({} of {} tried, {} ms)",
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
