//! An unencrypted pool on a public host, and when the miner keeps away
//! from one (docs/25).
//!
//! Plain Stratum V1 over the internet can be rerouted: an attacker who
//! announces a more specific route to the pool's address answers in its
//! place, takes any worker name and hands out another pool's jobs, so the
//! shares pay the attacker (Braiins reported such BGP hijacks in November
//! 2026). TLS with a checked certificate, or Stratum V2 with the pool's
//! authority key, stops that; a plain fallback pool behind them opens it
//! again. On the local network plain V1 is fine.

use core::net::IpAddr;

/// Whether `host` (a pool address as configured: a name, an IPv4 or an
/// IPv6 literal, with or without brackets) is on the miner's own
/// network: a private, loopback or link-local address, or a name that
/// only resolves there. The same list as WarpPool's connect dialog
/// (`ui/src/lib/localHost.ts`), plus the IPv4-mapped IPv6 form.
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let h = h.strip_prefix('[').unwrap_or(&h);
    let h = h.strip_suffix(']').unwrap_or(h);
    let h = h.strip_suffix('.').unwrap_or(h);
    if h.is_empty() || h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    if [".local", ".lan", ".home.arpa", ".internal"]
        .iter()
        .any(|s| h.ends_with(s))
    {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => local_v4(v4),
        Ok(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => local_v4(v4),
            None => {
                let s = v6.segments()[0];
                v6.is_loopback() || (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80
            }
        },
        // A single label (`umbrel`, `nas`) only resolves on the local
        // network; a dotted name is taken as public.
        Err(_) => !h.contains('.') && !h.contains(':'),
    }
}

fn local_v4(a: core::net::Ipv4Addr) -> bool {
    let [a, b, ..] = a.octets();
    a == 10
        || a == 127
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 169 && b == 254)
        || (a == 100 && (64..=127).contains(&b))
}

/// A pool reached over plain Stratum V1 (neither TLS nor Stratum V2) on a
/// public host: the connection a reroute can take over.
pub fn plain_public(tls: bool, sv2: bool, host: &str) -> bool {
    !tls && !sv2 && !is_local_host(host)
}

/// Whether a main pool whose certificate or key was just refused stays
/// the miner's pool instead of the fallback. A refused certificate on a
/// protected main pool is what a reroute looks like; moving on to a plain
/// fallback on a public host would hand the attacker the work there. The
/// miner keeps trying the main pool (the chips idle meanwhile, as for any
/// pool that sends no work) until it is accepted again or the settings
/// change. A fallback with TLS, Stratum V2 or on the local network is
/// switched to as before.
pub fn hold_main(main_protected: bool, fallback_plain_public: bool, refused: bool) -> bool {
    main_protected && fallback_plain_public && refused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_and_addresses() {
        for h in [
            "",
            "localhost",
            "pool.localhost",
            "umbrel",
            "NAS",
            "umbrel.local",
            "umbrel.local.",
            "pool.lan",
            "pool.home.arpa",
            "pool.internal",
            "10.0.0.5",
            "127.0.0.1",
            "172.16.0.1",
            "172.31.255.1",
            "192.168.1.40",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.1.1",
            "::1",
            "[::1]",
            "fd12:3456::1",
            "fc00::1",
            "fe80::1",
            "febf::1",
            "::ffff:192.168.1.2",
            " 192.168.1.40 ",
        ] {
            assert!(is_local_host(h), "{h:?} should be local");
        }
    }

    #[test]
    fn public_names_and_addresses() {
        for h in [
            "public-pool.io",
            "eu.stratum.braiins.com",
            "pool.example.org.",
            "8.8.8.8",
            "172.15.0.1",
            "172.32.0.1",
            "100.63.0.1",
            "100.128.0.1",
            "192.169.0.1",
            "2001:db8::1",
            "[2a01:4f8::1]",
            "fec0::1",
            "::ffff:8.8.8.8",
            "999.1.1.1",
        ] {
            assert!(!is_local_host(h), "{h:?} should be public");
        }
    }

    #[test]
    fn plain_public_needs_all_three() {
        assert!(plain_public(false, false, "public-pool.io"));
        assert!(!plain_public(true, false, "public-pool.io"));
        assert!(!plain_public(false, true, "public-pool.io"));
        assert!(!plain_public(false, false, "192.168.1.10"));
    }

    #[test]
    fn hold_only_for_a_refused_protected_main_and_a_plain_public_fallback() {
        assert!(hold_main(true, true, true));
        assert!(!hold_main(false, true, true));
        assert!(!hold_main(true, false, true));
        assert!(!hold_main(true, true, false));
    }
}
