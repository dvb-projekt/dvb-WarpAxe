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
//! headers went out, so a Discord message is never sent twice. A GET whose
//! IPv6 answer is 404 or a server error (an IPv6 front end not bound to
//! the site) is asked once more over IPv4. A host that failed over IPv6
//! goes over IPv4 for half an hour ([`NO_V6_FOR`]), so a broken IPv6 path
//! costs one failed attempt, not one per request.

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use esp_idf_svc::http::client::{Configuration, EspHttpConnection, FollowRedirectsPolicy};
use esp_idf_svc::http::Method;
use esp_idf_svc::sys::EspError;
use warpaxe_stratum::url::{ipv6_answer_suspect, parse, resolve_location, try_ipv6};

/// At most this long for the address race (one TCP handshake per family;
/// with one address there is no race at all).
const PICK_BUDGET: Duration = Duration::from_secs(5);
/// Redirects followed over IPv6 (over IPv4 the client follows them).
const MAX_REDIRECTS: usize = 5;

/// After an IPv6 failure, the host goes over IPv4 for this long.
const NO_V6_FOR: Duration = Duration::from_secs(30 * 60);
/// Hosts that failed over IPv6, and until when they go over IPv4 (a
/// handful at most: the update, its download host, mempool.space).
static NO_V6: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());

/// `host` failed over IPv6 (no connection, no TLS, a suspect answer or a
/// body that broke off): IPv4 only for [`NO_V6_FOR`].
pub fn ipv6_failed(host: &str) {
    log::info!(
        "http: {host}: IPv4 only for {} min",
        NO_V6_FOR.as_secs() / 60
    );
    let mut g = NO_V6.lock().unwrap();
    g.retain(|(h, _)| h != host);
    g.push((host.to_string(), Instant::now() + NO_V6_FOR));
}

fn ipv6_skipped(host: &str) -> bool {
    let now = Instant::now();
    let mut g = NO_V6.lock().unwrap();
    g.retain(|(_, until)| *until > now);
    g.iter().any(|(h, _)| h == host)
}

/// Which families a request may use.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// IPv6 when the rule allows it, else IPv4.
    Any,
    /// IPv4 only, as before IPv6 (a last attempt).
    Ipv4Only,
}

/// How an answer from [`get`] came: over IPv6 from `host`, or over IPv4.
pub struct Via(Option<String>);

impl Via {
    /// The body of this answer broke off: if it came over IPv6, that host
    /// goes over IPv4 for a while, so the caller's retry takes IPv4.
    pub fn body_failed(&self) {
        if let Some(h) = &self.0 {
            ipv6_failed(h);
        }
    }
}

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
    ) || ipv6_skipped(u.host)
    {
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
            Ok(c) => {
                log::info!("http: {} open over IPv6", t.name);
                return Ok(c);
            }
            Err(e) => {
                log::info!("http: {} over IPv6 failed ({e}): over IPv4", t.name);
                ipv6_failed(&t.name);
            }
        }
    }
    open_v4(url, conf, method, headers)
}

/// A GET to `url` with the response headers read: over IPv6 when the
/// rule allows it and `family` is [`Family::Any`], redirects followed
/// (each hop decides anew), and once more over IPv4 when the IPv6 attempt
/// brings no answer or a suspect one (404, 5xx). The status is the
/// caller's to check, as with the client itself; [`Via`] says how the
/// answer came, for a body that breaks off.
pub fn get(
    url: &str,
    conf: &Configuration,
    headers: &[(&str, &str)],
    family: Family,
) -> Result<(EspHttpConnection, Via), EspError> {
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if family == Family::Ipv4Only {
            break;
        }
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
                ipv6_failed(&t.name);
                break;
            }
        };
        let status = c.status();
        if ipv6_answer_suspect(status) {
            log::info!("http: {} answered {status} over IPv6: over IPv4", t.name);
            drop(c);
            ipv6_failed(&t.name);
            break;
        }
        if !(300..400).contains(&status) || status == 304 {
            log::info!("http: {} answered {status} over IPv6", t.name);
            return Ok((c, Via(Some(t.name))));
        }
        match c.header("Location").and_then(|l| resolve_location(&url, l)) {
            Some(next) => url = next,
            // A redirect without a usable Location: the caller sees it.
            None => return Ok((c, Via(Some(t.name)))),
        }
    }
    let mut c = open_v4(&url, conf, Method::Get, headers)?;
    c.initiate_response()?;
    Ok((c, Via(None)))
}
