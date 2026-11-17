//! Which pool address to try when, without a socket: the host as typed,
//! the order of the addresses DNS gave, and the timing of the attempts
//! (Happy Eyeballs, RFC 8305). The firmware owns the sockets and the
//! clock; it asks [`Race::step`] what to do next and reports back.
//!
//! The rules, and why:
//! - IPv6 first, then the families alternate (RFC 8305 §4). A pool with
//!   only A records loses nothing; one with both is reached over IPv6
//!   when that works.
//! - The next address starts [`STAGGER_MS`] after the previous one, or at
//!   once when the previous one failed. A route that drops packets
//!   silently costs a quarter second, not a whole connect timeout per
//!   address (ESP-Miner PR #2105 waits 5 s per AAAA record, sequentially;
//!   tested 2026-11-17).
//! - The first connection that completes wins; the others are closed.

/// Delay before the next address is tried while earlier ones are still
/// pending (RFC 8305 §5 recommends 250 ms).
pub const STAGGER_MS: u64 = 250;

/// The host part of a pool address as the user may type it: an IPv6
/// literal in brackets (`[2001:db8::1]`, the usual form next to a port)
/// loses them; surrounding spaces go. Anything else is returned as is.
pub fn pool_host(s: &str) -> &str {
    let s = s.trim();
    match s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        Some(inner) => inner,
        None => s,
    }
}

/// `host:port` for a log line or the API, with brackets around an IPv6
/// literal so the port stays readable.
pub fn host_port(host: &str, port: u16) -> String {
    let host = pool_host(host);
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The host back out of a [`host_port`] string, for a display that has
/// no room for the port: `[2001:db8::1]:3333` gives `2001:db8::1`,
/// `pool.example.org:3333` gives `pool.example.org`.
pub fn host_of(host_port: &str) -> &str {
    match host_port.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => host_port.split(':').next().unwrap_or(host_port),
    }
}

/// Whether an IPv6 peer of the web UI counts as local: the link
/// (fe80::/10), unique local (fc00::/7), loopback, an IPv4 address on the
/// dual-stack socket (::ffff:a.b.c.d), or the same /64 as one of `own`
/// (the miner's global addresses). Everything else is the internet.
pub fn ipv6_peer_is_local(peer: core::net::Ipv6Addr, own: &[core::net::Ipv6Addr]) -> bool {
    let seg = peer.segments();
    let o = peer.octets();
    let v4_mapped = o[..10].iter().all(|&b| b == 0) && o[10] == 0xff && o[11] == 0xff;
    v4_mapped
        || peer.is_loopback()
        || (seg[0] & 0xffc0) == 0xfe80
        || (seg[0] & 0xfe00) == 0xfc00
        || own.iter().any(|a| a.segments()[..4] == seg[..4])
}

/// The order of the attempts: IPv6 and IPv4 addresses alternating,
/// IPv6 first (RFC 8305 §4, "first address family count" 1). Either list
/// may be empty.
pub fn interleave<T>(v6: Vec<T>, v4: Vec<T>) -> Vec<T> {
    let mut out = Vec::with_capacity(v6.len() + v4.len());
    let (mut a, mut b) = (v6.into_iter(), v4.into_iter());
    loop {
        match (a.next(), b.next()) {
            (None, None) => return out,
            (x, y) => {
                out.extend(x);
                out.extend(y);
            }
        }
    }
}

/// What the dialer should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Start a connect to address `i` (index into the ordered list).
    Start(usize),
    /// Nothing to start yet: wait for a pending connect at most this many
    /// milliseconds, then ask again.
    Wait(u64),
    /// No address left and none pending, or the deadline passed.
    GiveUp,
}

/// Happy Eyeballs timing for `n` addresses, all times in milliseconds of
/// the caller's clock.
#[derive(Debug, Clone)]
pub struct Race {
    n: usize,
    next: usize,
    pending: usize,
    /// When the next start is due (after a start: start + stagger; after
    /// a failure: now).
    due_ms: u64,
    deadline_ms: u64,
}

impl Race {
    /// `n` addresses, the first one due at `now_ms`, all of it over at
    /// `now_ms + budget_ms`.
    pub fn new(n: usize, now_ms: u64, budget_ms: u64) -> Race {
        Race {
            n,
            next: 0,
            pending: 0,
            due_ms: now_ms,
            deadline_ms: now_ms.saturating_add(budget_ms),
        }
    }

    /// The next action at `now_ms`. A [`Step::Start`] counts the attempt
    /// as pending until [`Race::failed`] (or the caller's success) ends it.
    pub fn step(&mut self, now_ms: u64) -> Step {
        if now_ms >= self.deadline_ms {
            return Step::GiveUp;
        }
        if self.next < self.n && now_ms >= self.due_ms {
            let i = self.next;
            self.next += 1;
            self.pending += 1;
            self.due_ms = now_ms.saturating_add(STAGGER_MS);
            return Step::Start(i);
        }
        if self.pending == 0 && self.next >= self.n {
            return Step::GiveUp;
        }
        let until_deadline = self.deadline_ms - now_ms;
        if self.next < self.n {
            Step::Wait(self.due_ms.saturating_sub(now_ms).min(until_deadline))
        } else {
            Step::Wait(until_deadline)
        }
    }

    /// A pending attempt failed (refused, unreachable, reset): the next
    /// address may start at once.
    pub fn failed(&mut self, now_ms: u64) {
        self.pending = self.pending.saturating_sub(1);
        self.due_ms = self.due_ms.min(now_ms);
    }

    /// Attempts started so far.
    pub fn started(&self) -> usize {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_and_spaces_go_names_stay() {
        assert_eq!(pool_host("[2001:db8::1]"), "2001:db8::1");
        assert_eq!(pool_host("  [fd00::5] "), "fd00::5");
        assert_eq!(pool_host("2001:db8::1"), "2001:db8::1");
        assert_eq!(pool_host("pool.example.org"), "pool.example.org");
        assert_eq!(pool_host("192.168.1.10"), "192.168.1.10");
        // Half a bracket is not a literal: left for DNS to refuse.
        assert_eq!(pool_host("[2001:db8::1"), "[2001:db8::1");
    }

    #[test]
    fn host_port_brackets_only_ipv6() {
        assert_eq!(host_port("2001:db8::1", 3333), "[2001:db8::1]:3333");
        assert_eq!(host_port("[2001:db8::1]", 3333), "[2001:db8::1]:3333");
        assert_eq!(host_port("pool.example.org", 3333), "pool.example.org:3333");
        assert_eq!(host_port("10.0.0.2", 21496), "10.0.0.2:21496");
    }

    #[test]
    fn host_of_undoes_host_port() {
        for h in ["2001:db8::1", "pool.example.org", "10.0.0.2"] {
            assert_eq!(host_of(&host_port(h, 3333)), h);
        }
        assert_eq!(host_of("pool.example.org"), "pool.example.org");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn only_the_local_network_reaches_the_web_ui_over_ipv6() {
        let p = |s: &str| s.parse::<core::net::Ipv6Addr>().unwrap();
        let own = [p("2a02:db8:1:8d60:9a88:e0ff:fe03:55c8"), p("fd00:1::5")];
        assert!(ipv6_peer_is_local(p("fe80::1"), &own));
        assert!(ipv6_peer_is_local(p("fdc8:396b:5f6c::61"), &own));
        assert!(ipv6_peer_is_local(p("::ffff:192.168.178.10"), &own));
        assert!(ipv6_peer_is_local(p("::1"), &own));
        assert!(ipv6_peer_is_local(p("2a02:db8:1:8d60::abcd"), &own));
        // Same provider, another customer's /64; and the wider internet.
        assert!(!ipv6_peer_is_local(p("2a02:db8:1:8d61::abcd"), &own));
        assert!(!ipv6_peer_is_local(p("2600:9000:a612:9253::1"), &own));
        // Without a global address of our own, no global peer is local.
        assert!(!ipv6_peer_is_local(p("2a02:db8:1:8d60::abcd"), &[]));
    }

    #[test]
    fn families_alternate_ipv6_first() {
        assert_eq!(
            interleave(vec!["a6", "b6", "c6"], vec!["a4"]),
            ["a6", "a4", "b6", "c6"]
        );
        assert_eq!(interleave(vec!["a6"], vec!["a4", "b4"]), ["a6", "a4", "b4"]);
        assert_eq!(
            interleave(Vec::<&str>::new(), vec!["a4", "b4"]),
            ["a4", "b4"]
        );
        assert_eq!(interleave(vec!["a6"], Vec::new()), ["a6"]);
        assert!(interleave(Vec::<u8>::new(), Vec::new()).is_empty());
    }

    #[test]
    fn a_silent_first_address_costs_one_stagger() {
        // IPv6 hangs (blackhole), IPv4 answers: IPv4 starts 250 ms in.
        let mut r = Race::new(2, 1000, 10_000);
        assert_eq!(r.step(1000), Step::Start(0));
        assert_eq!(r.step(1000), Step::Wait(250));
        assert_eq!(r.step(1100), Step::Wait(150));
        assert_eq!(r.step(1250), Step::Start(1));
        // Both pending, nothing left to start: wait up to the deadline.
        assert_eq!(r.step(1250), Step::Wait(9750));
    }

    #[test]
    fn a_refused_address_starts_the_next_at_once() {
        let mut r = Race::new(3, 0, 10_000);
        assert_eq!(r.step(0), Step::Start(0));
        r.failed(20);
        assert_eq!(r.step(20), Step::Start(1));
        r.failed(40);
        assert_eq!(r.step(40), Step::Start(2));
        r.failed(60);
        assert_eq!(r.step(60), Step::GiveUp);
        assert_eq!(r.started(), 3);
    }

    #[test]
    fn a_failure_while_others_pend_does_not_delay_the_next() {
        let mut r = Race::new(3, 0, 10_000);
        assert_eq!(r.step(0), Step::Start(0));
        assert_eq!(r.step(250), Step::Start(1));
        r.failed(300); // the second one, refused
        assert_eq!(r.step(300), Step::Start(2));
        assert_eq!(r.step(300), Step::Wait(9700));
    }

    #[test]
    fn the_deadline_ends_it() {
        let mut r = Race::new(5, 0, 600);
        assert_eq!(r.step(0), Step::Start(0));
        assert_eq!(r.step(250), Step::Start(1));
        assert_eq!(r.step(500), Step::Start(2));
        assert_eq!(r.step(550), Step::Wait(50));
        assert_eq!(r.step(600), Step::GiveUp);
    }

    #[test]
    fn no_address_gives_up_at_once() {
        let mut r = Race::new(0, 0, 10_000);
        assert_eq!(r.step(0), Step::GiveUp);
    }
}
