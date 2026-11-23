//! HTTP(S) requests to the internet (the online update, market data,
//! Discord alerts) over IPv6 when the pool's rule allows it (docs/17), else
//! exactly as before.
//!
//! The ESP-IDF client resolves names itself with `AF_UNSPEC`, which in
//! lwIP gives the A record only. So with the `ipv6` setting on and a global
//! or unique-local address on the station, the address is picked by the
//! pool's resolver and Happy Eyeballs (`dial::pick`: AAAA cache, IPv6
//! first, 250 ms stagger), and an IPv6 winner goes to the client as a
//! `[literal]` URL. The name stays in the `Host` header and is what the
//! certificate is checked against and what goes out as SNI
//! (`new_with_common_name`, vendor patch 5), never the literal. An IPv4
//! winner, the setting off or no address beyond the link: the URL goes to
//! the client unchanged, as before, without an AAAA query.
//!
//! A request whose IPv6 attempt fails before an answer (connect, TLS) is
//! made once more over IPv4 at once; a GET also when the answer does not
//! come (a path that drops large packets). A POST is not repeated once its
//! headers went out, so a Discord message is never sent twice.

use std::net::SocketAddr;
use std::time::Duration;

use esp_idf_svc::http::client::{Configuration, EspHttpConnection, FollowRedirectsPolicy};
use esp_idf_svc::http::Method;
use esp_idf_svc::sys::EspError;
use warpaxe_stratum::url::{parse, resolve_location, try_ipv6};

/// At most this long for the address race (one TCP handshake per family;
/// with one address there is no race at all).
const PICK_BUDGET: Duration = Duration::from_secs(5);
/// Redirects followed over IPv6 (over IPv4 the client follows them).
const MAX_REDIRECTS: usize = 5;

/// An IPv6 way to `url`: the literal URL, the `Host` header and the name.
struct V6 {
    url: String,
    host_header: String,
    name: String,
}

fn ipv6_target(url: &str) -> Option<V6> {
    let u = parse(url)?;
    if !try_ipv6(
        crate::config::get().ipv6,
        !super::dial::global_ipv6().is_empty(),
        u.host_is_literal(),
    ) {
        return None;
    }
    match super::dial::pick(u.host, u.port, PICK_BUDGET) {
        Ok(SocketAddr::V6(a)) => Some(V6 {
            url: u.with_ipv6(*a.ip()),
            host_header: u.host_header(),
            name: u.host.to_string(),
        }),
        Ok(SocketAddr::V4(_)) => None,
        Err(e) => {
            log::info!("http: {}: {e}", u.host);
            None
        }
    }
}

fn open_v6(
    t: &V6,
    conf: &Configuration,
    method: Method,
    headers: &[(&str, &str)],
) -> Result<EspHttpConnection, EspError> {
    // Redirects are followed by `get`, each hop with its own name; the
    // client would keep this hop's name for the certificate check.
    let mut conf = *conf;
    conf.follow_redirects_policy = FollowRedirectsPolicy::FollowNone;
    let mut h: Vec<(&str, &str)> = headers
        .iter()
        .copied()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("Host"))
        .collect();
    h.push(("Host", &t.host_header));
    let mut c = EspHttpConnection::new_with_common_name(&conf, Some(&t.name))?;
    c.initiate_request(method, &t.url, &h)?;
    Ok(c)
}

fn open_v4(
    url: &str,
    conf: &Configuration,
    method: Method,
    headers: &[(&str, &str)],
) -> Result<EspHttpConnection, EspError> {
    let mut c = EspHttpConnection::new(conf)?;
    c.initiate_request(method, url, headers)?;
    Ok(c)
}

/// The connection to `url` with the request line and headers sent; the
/// body, if any, and `initiate_response` are the caller's. Over IPv6 when
/// the rule allows it and that opens, else over IPv4 as before.
pub fn open(
    url: &str,
    conf: &Configuration,
    method: Method,
    headers: &[(&str, &str)],
) -> Result<EspHttpConnection, EspError> {
    if let Some(t) = ipv6_target(url) {
        match open_v6(&t, conf, method, headers) {
            Ok(c) => return Ok(c),
            Err(e) => log::info!("http: {} over IPv6 failed ({e}): over IPv4", t.name),
        }
    }
    open_v4(url, conf, method, headers)
}

/// A GET to `url` with the response headers read: over IPv6 when the
/// rule allows it, redirects followed (each hop decides anew), and once
/// more over IPv4 when the IPv6 attempt brings no answer. The status is
/// the caller's to check, as with the client itself.
pub fn get(
    url: &str,
    conf: &Configuration,
    headers: &[(&str, &str)],
) -> Result<EspHttpConnection, EspError> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let Some(t) = ipv6_target(&url) else {
            break;
        };
        let answer = open_v6(&t, conf, Method::Get, headers).and_then(|mut c| {
            c.initiate_response()?;
            Ok(c)
        });
        let c = match answer {
            Ok(c) => c,
            Err(e) => {
                log::info!("http: {} over IPv6 failed ({e}): over IPv4", t.name);
                break;
            }
        };
        let status = c.status();
        if !(300..400).contains(&status) || status == 304 {
            return Ok(c);
        }
        match c.header("Location").and_then(|l| resolve_location(&url, l)) {
            Some(next) => url = next,
            // A redirect without a usable Location: the caller sees it.
            None => return Ok(c),
        }
    }
    let mut c = open_v4(&url, conf, Method::Get, headers)?;
    c.initiate_response()?;
    Ok(c)
}
