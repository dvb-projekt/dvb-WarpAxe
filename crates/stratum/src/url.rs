//! The HTTP side of the IPv6 rule (docs/17): the online update, market
//! data and Discord alerts go over IPv6 the way the pool does. The ESP-IDF
//! HTTP client resolves names itself with `AF_UNSPEC`, which in lwIP gives
//! the A record only, so the firmware picks the address with the pool's
//! resolver and Happy Eyeballs, and hands the client a URL with the
//! `[IPv6]` literal. The name stays in the `Host` header and is what the
//! certificate is checked against (and sent as SNI). This module is the
//! string work around that, without a socket.

use core::net::{IpAddr, Ipv6Addr};

/// An `http://` or `https://` URL taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url<'a> {
    /// `https://` rather than `http://`.
    pub https: bool,
    /// The host, without the brackets of an IPv6 literal.
    pub host: &'a str,
    /// The port, the scheme's default when the URL names none.
    pub port: u16,
    /// Whether the URL names the port.
    pub explicit_port: bool,
    /// Everything from the first `/`, `?` or `#` after the host on
    /// (may be empty).
    pub rest: &'a str,
}

/// `url` taken apart; `None` for anything but `http://` or `https://`
/// with a host, or user info (`user@host`, never used here).
pub fn parse(url: &str) -> Option<Url<'_>> {
    let (https, after) = if let Some(r) = strip_prefix_ci(url, "https://") {
        (true, r)
    } else {
        (false, strip_prefix_ci(url, "http://")?)
    };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (auth, rest) = after.split_at(end);
    if auth.contains('@') {
        return None;
    }
    let default = if https { 443 } else { 80 };
    let (host, port) = if let Some(r) = auth.strip_prefix('[') {
        let close = r.find(']')?;
        let host = &r[..close];
        let tail = &r[close + 1..];
        let port = match tail.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().ok()?),
            None if tail.is_empty() => None,
            None => return None,
        };
        (host, port)
    } else {
        match auth.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, Some(p.parse::<u16>().ok()?)),
            Some(_) => return None,
            None => (auth, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(Url {
        https,
        host,
        port: port.unwrap_or(default),
        explicit_port: port.is_some(),
        rest,
    })
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

impl Url<'_> {
    /// Whether the host is an address literal: nothing to resolve, the
    /// client takes it as it is.
    pub fn host_is_literal(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }

    /// The value of the `Host` header (RFC 9110 §7.2): the name, with the
    /// port when the URL names one.
    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.to_string()
        };
        if self.explicit_port {
            format!("{host}:{}", self.port)
        } else {
            host
        }
    }

    /// The same URL with `addr` in place of the host.
    pub fn with_ipv6(&self, addr: Ipv6Addr) -> String {
        let scheme = if self.https { "https" } else { "http" };
        if self.explicit_port {
            format!("{scheme}://[{addr}]:{}{}", self.port, self.rest)
        } else {
            format!("{scheme}://[{addr}]{}", self.rest)
        }
    }
}

/// Where a redirect from `base` goes: `location` itself when it is a
/// whole URL, else `location` as an absolute path on `base`'s scheme and
/// host (relative paths are not used by the servers asked here). `None`
/// when neither works.
pub fn resolve_location(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if parse(location).is_some() {
        return Some(location.to_string());
    }
    if !location.starts_with('/') || location.starts_with("//") {
        return None;
    }
    let b = parse(base)?;
    let origin = &base[..base.len() - b.rest.len()];
    Some(format!("{origin}{location}"))
}

/// Whether an HTTP request may look for an IPv6 address of its host at
/// all: the `ipv6` setting on, the station holding a global or
/// unique-local address, and a name to look up (a literal goes as typed).
/// Anything else is today's IPv4 path, without an AAAA query.
pub fn try_ipv6(setting: bool, own_beyond_link: bool, host_is_literal: bool) -> bool {
    setting && own_beyond_link && !host_is_literal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_ports_and_literals() {
        let u = parse("https://api.github.com/repos/x/releases/latest").unwrap();
        assert!(u.https);
        assert_eq!(u.host, "api.github.com");
        assert_eq!(u.port, 443);
        assert!(!u.explicit_port);
        assert_eq!(u.rest, "/repos/x/releases/latest");

        let u = parse("http://example.org:8080?x=1").unwrap();
        assert!(!u.https);
        assert_eq!(
            (u.host, u.port, u.explicit_port),
            ("example.org", 8080, true)
        );
        assert_eq!(u.rest, "?x=1");

        let u = parse("HTTPS://[2001:db8::1]:8443/a").unwrap();
        assert_eq!(
            (u.host, u.port, u.explicit_port),
            ("2001:db8::1", 8443, true)
        );
        assert!(u.host_is_literal());

        let u = parse("https://mempool.space").unwrap();
        assert_eq!(u.rest, "");
        assert!(!u.host_is_literal());
        assert!(parse("https://192.0.2.1/x").unwrap().host_is_literal());
    }

    #[test]
    fn refuses_what_it_does_not_know() {
        assert!(parse("ftp://example.org/").is_none());
        assert!(parse("https://").is_none());
        assert!(parse("https://user@example.org/").is_none());
        assert!(parse("https://example.org:99999/").is_none());
        assert!(parse("https://2001:db8::1/").is_none());
        assert!(parse("https://[2001:db8::1/").is_none());
        assert!(parse("https://[2001:db8::1]x/").is_none());
    }

    #[test]
    fn literal_url_keeps_path_and_port_and_host_header_keeps_the_name() {
        let a: Ipv6Addr = "2606:4700::6810:84e5".parse().unwrap();
        let u = parse("https://discord.com/api/webhooks/1/abc?wait=true").unwrap();
        assert_eq!(
            u.with_ipv6(a),
            "https://[2606:4700::6810:84e5]/api/webhooks/1/abc?wait=true"
        );
        assert_eq!(u.host_header(), "discord.com");

        let u = parse("http://pool.example:8080/x").unwrap();
        assert_eq!(u.with_ipv6(a), "http://[2606:4700::6810:84e5]:8080/x");
        assert_eq!(u.host_header(), "pool.example:8080");
        assert_eq!(
            parse("http://[fd00::1]:81/").unwrap().host_header(),
            "[fd00::1]:81"
        );
    }

    #[test]
    fn redirects() {
        let base = "https://github.com/o/r/releases/download/v1/a.bin";
        assert_eq!(
            resolve_location(base, "https://objects.example.com/x?sig=1").as_deref(),
            Some("https://objects.example.com/x?sig=1")
        );
        assert_eq!(
            resolve_location(base, "/o/r/other").as_deref(),
            Some("https://github.com/o/r/other")
        );
        assert_eq!(
            resolve_location("http://h:81/a?b", "/c").as_deref(),
            Some("http://h:81/c")
        );
        assert_eq!(resolve_location(base, "other"), None);
        assert_eq!(resolve_location(base, "//evil.example/x"), None);
    }

    #[test]
    fn ipv6_only_with_the_switch_an_address_and_a_name() {
        assert!(try_ipv6(true, true, false));
        assert!(!try_ipv6(false, true, false));
        assert!(!try_ipv6(true, false, false));
        assert!(!try_ipv6(true, true, true));
    }
}
