//! An optional static IPv4 address for the miner's LAN interface, in
//! place of the router's DHCP: the settings check for `PATCH
//! /api/system` and the lock-out guard that gives the address up for the
//! boot when nothing answers on it.
//!
//! The rules, so that a typo cannot cut the miner off its own network:
//! the address, the gateway and the DNS servers are dotted IPv4 unicast
//! addresses; the mask is contiguous (dotted, or a prefix length such as
//! `24`) between /8 and /30; the address and the gateway lie in the same
//! subnet and are neither its network nor its broadcast address. And
//! after the link is up with the static address, something on the
//! network must answer within [`GIVE_UP_S`] seconds (the gateway, or the
//! pool through it); otherwise the interface falls back to DHCP until the
//! next restart ([`Watch`]).

use core::net::Ipv4Addr;

/// Seconds of link with the static address and no answer from the
/// gateway or the pool before the interface goes back to DHCP for this
/// boot: long enough for a router that is still starting after a power
/// cut, short enough that a wrong address costs a few minutes.
pub const GIVE_UP_S: u32 = 180;
/// Seconds between two gateway probes while nothing has answered yet.
pub const PROBE_EVERY_S: u32 = 10;

/// A checked static configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The miner's address.
    pub addr: Ipv4Addr,
    /// Prefix length, 8 to 30.
    pub prefix: u8,
    /// The router.
    pub gateway: Ipv4Addr,
    /// The first DNS server: the one typed, else the gateway.
    pub dns: Ipv4Addr,
    /// The second DNS server, if one was typed.
    pub dns2: Option<Ipv4Addr>,
}

impl Settings {
    /// The mask as an address (`255.255.255.0` for /24).
    pub fn mask(&self) -> Ipv4Addr {
        Ipv4Addr::from(mask_bits(self.prefix))
    }
}

/// Why a static configuration was refused; [`Error::text`] says it in
/// English for the API's error answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The address is not a dotted IPv4 address.
    BadAddr,
    /// The address is not one a host can have (0.x, 127.x, multicast,
    /// 255.255.255.255, …).
    AddrNotUnicast,
    /// The mask is neither dotted nor a prefix length, or not contiguous.
    BadMask,
    /// The mask is valid but leaves no room for a host and a gateway
    /// (/31, /32) or is wider than /8.
    MaskRange,
    /// The gateway is not a dotted IPv4 address.
    BadGateway,
    /// The gateway is not one a host can have.
    GatewayNotUnicast,
    /// The gateway lies outside the address's subnet.
    GatewayOutsideSubnet,
    /// The gateway is the miner's own address.
    GatewayIsAddr,
    /// The address is the subnet's network or broadcast address.
    AddrIsNetworkOrBroadcast,
    /// The gateway is the subnet's network or broadcast address.
    GatewayIsNetworkOrBroadcast,
    /// The first DNS server is not a unicast IPv4 address.
    BadDns,
    /// The second DNS server is not a unicast IPv4 address.
    BadDns2,
}

impl Error {
    /// The reason in English, naming the field.
    pub fn text(self) -> &'static str {
        match self {
            Error::BadAddr => "IPv4 address invalid",
            Error::AddrNotUnicast => "IPv4 address not usable for a device",
            Error::BadMask => "subnet mask invalid",
            Error::MaskRange => "subnet mask must be /8 to /30",
            Error::BadGateway => "gateway invalid",
            Error::GatewayNotUnicast => "gateway not usable for a device",
            Error::GatewayOutsideSubnet => "gateway not in the subnet",
            Error::GatewayIsAddr => "gateway is the miner's own address",
            Error::AddrIsNetworkOrBroadcast => "IPv4 address is the subnet's network or broadcast",
            Error::GatewayIsNetworkOrBroadcast => "gateway is the subnet's network or broadcast",
            Error::BadDns => "DNS server invalid",
            Error::BadDns2 => "second DNS server invalid",
        }
    }
}

fn mask_bits(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    }
}

/// A mask as dotted quad (`255.255.255.0`) or prefix length (`24`, `/24`):
/// its prefix length, if the bits are contiguous. Spaces are taken off by
/// the caller.
pub fn parse_mask(s: &str) -> Option<u8> {
    let s = s.strip_prefix('/').unwrap_or(s);
    let bits = if s.len() <= 2 {
        // A prefix length; mask_bits(33..) is never reached.
        let n: u8 = s.parse().ok().filter(|n| *n <= 32)?;
        return Some(n);
    } else {
        u32::from(s.parse::<Ipv4Addr>().ok()?)
    };
    let prefix = bits.leading_ones() as u8;
    (mask_bits(prefix) == bits).then_some(prefix)
}

/// A dotted address a host on a LAN can carry or talk to as its gateway
/// (no 0.x, 127.x, multicast, 240+ or broadcast; no leading zeros:
/// `010.1.1.1` would read as octal elsewhere): `bad` when it is not an
/// address, `not_host` when it is one no host can have.
fn host_addr(s: &str, bad: Error, not_host: Error) -> Result<Ipv4Addr, Error> {
    let a: Ipv4Addr = s.parse().map_err(|_| bad)?;
    let o = a.octets()[0];
    if o == 0 || o == 127 || o >= 224 {
        return Err(not_host);
    }
    Ok(a)
}

/// The five fields as the config keeps them, in one string:
/// `address,mask,gateway,dns,dns2` (spaces taken off, empty slots
/// allowed, missing trailing ones count as empty). `dns` empty = the
/// gateway answers DNS (what most home routers do); `dns2` empty = none.
pub fn validate_packed(s: &str) -> Result<Settings, Error> {
    let mut f = s.split(',').map(str::trim);
    let mut next = || f.next().unwrap_or("");
    let a = host_addr(next(), Error::BadAddr, Error::AddrNotUnicast)?;
    let prefix = parse_mask(next()).ok_or(Error::BadMask)?;
    if !(8..=30).contains(&prefix) {
        return Err(Error::MaskRange);
    }
    let g = host_addr(next(), Error::BadGateway, Error::GatewayNotUnicast)?;
    let m = mask_bits(prefix);
    let (ab, gb) = (u32::from(a), u32::from(g));
    if ab & m != gb & m {
        return Err(Error::GatewayOutsideSubnet);
    }
    if ab == gb {
        return Err(Error::GatewayIsAddr);
    }
    let edge = |x: u32| x & !m == 0 || x & !m == !m;
    if edge(ab) {
        return Err(Error::AddrIsNetworkOrBroadcast);
    }
    if edge(gb) {
        return Err(Error::GatewayIsNetworkOrBroadcast);
    }
    let d = match next() {
        "" => g,
        x => host_addr(x, Error::BadDns, Error::BadDns)?,
    };
    let d2 = match next() {
        "" => None,
        x => Some(host_addr(x, Error::BadDns2, Error::BadDns2)?),
    };
    if f.next().is_some() {
        return Err(Error::BadDns2);
    }
    Ok(Settings {
        addr: a,
        prefix,
        gateway: g,
        dns: d,
        dns2: d2,
    })
}

/// What the interface's thread does after a [`Watch::tick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing to do yet; probe again when [`Watch::probe_due`] says so.
    Wait,
    /// Something answered on the static address for the first time: it
    /// is kept for the boot, no more probes.
    Confirmed,
    /// [`GIVE_UP_S`] of link and no answer: go back to DHCP for the boot.
    FallBack,
}

/// The lock-out guard for one interface. Counts only the seconds the link
/// is up (a cable out or a router that is off costs nothing), so a static
/// address survives a power cut that takes the router down with the
/// miner, as long as the router comes back within [`GIVE_UP_S`] of the
/// link.
#[derive(Clone, Debug, Default)]
pub struct Watch {
    last: Option<u32>,
    link_s: u32,
    last_probe: Option<u32>,
    confirmed: bool,
    fell_back: bool,
}

impl Watch {
    /// No link seen yet, nothing decided.
    pub fn new() -> Self {
        Self::default()
    }

    /// The static address answered once and stays for the boot.
    pub fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// The interface went back to DHCP for the boot.
    pub fn fell_back(&self) -> bool {
        self.fell_back
    }

    /// Seconds of link without an answer so far.
    pub fn waited_s(&self) -> u32 {
        self.link_s
    }

    /// Whether to probe the gateway now (`now_s` monotonic seconds): with
    /// the link up, not decided yet, and [`PROBE_EVERY_S`] after the last.
    pub fn probe_due(&self, now_s: u32, link: bool) -> bool {
        link && !self.confirmed
            && !self.fell_back
            && self
                .last_probe
                .is_none_or(|t| now_s.saturating_sub(t) >= PROBE_EVERY_S)
    }

    /// A probe was sent at `now_s` (answered or not).
    pub fn probed(&mut self, now_s: u32) {
        self.last_probe = Some(now_s);
    }

    /// One look: `link` = the interface is associated / has its cable;
    /// `answered` = the gateway answered a probe or the pool session runs
    /// over the static address. Each verdict other than `Wait` comes once.
    pub fn tick(&mut self, now_s: u32, link: bool, answered: bool) -> Verdict {
        let dt = self.last.map_or(0, |t| now_s.saturating_sub(t));
        self.last = Some(now_s);
        if self.confirmed || self.fell_back {
            return Verdict::Wait;
        }
        if link && answered {
            self.confirmed = true;
            return Verdict::Confirmed;
        }
        if link {
            self.link_s = self.link_s.saturating_add(dt);
            if self.link_s >= GIVE_UP_S {
                self.fell_back = true;
                return Verdict::FallBack;
            }
        }
        Verdict::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five fields one by one, as the tests write them.
    fn validate(a: &str, m: &str, g: &str, d: &str, d2: &str) -> Result<Settings, Error> {
        validate_packed(&[a, m, g, d, d2].join(","))
    }

    fn ok(a: &str, m: &str, g: &str) -> Settings {
        validate(a, m, g, "", "").unwrap()
    }

    #[test]
    fn a_home_network_passes_with_the_gateway_as_dns() {
        let s = ok("192.168.1.50", "255.255.255.0", "192.168.1.1");
        assert_eq!(s.prefix, 24);
        assert_eq!(s.mask(), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(s.dns, Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(s.dns2, None);
        let s = validate(" 10.0.5.7 ", "/16", "10.0.0.1", "1.1.1.1", "9.9.9.9").unwrap();
        assert_eq!(s.prefix, 16);
        assert_eq!(s.dns, Ipv4Addr::new(1, 1, 1, 1));
        assert_eq!(s.dns2, Some(Ipv4Addr::new(9, 9, 9, 9)));
    }

    #[test]
    fn masks_dotted_or_as_a_prefix_and_contiguous_only() {
        assert_eq!(parse_mask("255.255.255.0"), Some(24));
        assert_eq!(parse_mask("24"), Some(24));
        assert_eq!(parse_mask("/22"), Some(22));
        assert_eq!(parse_mask("255.255.252.0"), Some(22));
        assert_eq!(parse_mask("255.0.255.0"), None);
        assert_eq!(parse_mask("255.255.255.1"), None);
        assert_eq!(parse_mask("33"), None);
        assert_eq!(parse_mask(""), None);
        assert_eq!(
            validate("192.168.1.5", "255.255.255.255", "192.168.1.1", "", ""),
            Err(Error::MaskRange)
        );
        assert_eq!(
            validate("192.168.1.5", "255.255.255.254", "192.168.1.1", "", ""),
            Err(Error::MaskRange)
        );
        assert_eq!(
            validate("10.1.1.5", "7", "10.1.1.1", "", ""),
            Err(Error::MaskRange)
        );
        assert!(validate("192.168.1.1", "30", "192.168.1.2", "", "").is_ok());
    }

    #[test]
    fn typos_are_refused_with_the_field_named() {
        assert_eq!(
            validate("192.168.1", "24", "192.168.1.1", "", ""),
            Err(Error::BadAddr)
        );
        assert_eq!(
            validate("192.168.1.256", "24", "192.168.1.1", "", ""),
            Err(Error::BadAddr)
        );
        assert_eq!(
            validate("192.168.01.5", "24", "192.168.1.1", "", ""),
            Err(Error::BadAddr)
        );
        assert_eq!(
            validate("192.168.1.5", "abc", "192.168.1.1", "", ""),
            Err(Error::BadMask)
        );
        assert_eq!(
            validate("192.168.1.5", "24", "", "", ""),
            Err(Error::BadGateway)
        );
        assert_eq!(
            validate("192.168.1.5", "24", "192.168.1.1", "dns", ""),
            Err(Error::BadDns)
        );
        assert_eq!(
            validate("192.168.1.5", "24", "192.168.1.1", "", "x"),
            Err(Error::BadDns2)
        );
        assert_eq!(
            validate("192.168.1.5", "24", "192.168.1.1", "224.0.0.1", ""),
            Err(Error::BadDns)
        );
        assert!(Error::GatewayOutsideSubnet.text().contains("gateway"));
    }

    #[test]
    fn no_address_a_host_cannot_have() {
        for a in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "224.0.0.5",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert_eq!(
                validate(a, "8", "10.0.0.1", "", ""),
                Err(Error::AddrNotUnicast),
                "{a}"
            );
        }
        assert_eq!(
            validate("192.168.1.5", "24", "127.0.0.1", "", ""),
            Err(Error::GatewayNotUnicast)
        );
    }

    #[test]
    fn the_gateway_must_be_on_the_subnet_and_not_the_miner() {
        assert_eq!(
            validate("192.168.1.50", "24", "192.168.2.1", "", ""),
            Err(Error::GatewayOutsideSubnet)
        );
        // The same pair passes with a wider mask.
        assert!(validate("192.168.1.50", "22", "192.168.2.1", "", "").is_ok());
        assert_eq!(
            validate("192.168.1.50", "24", "192.168.1.50", "", ""),
            Err(Error::GatewayIsAddr)
        );
    }

    #[test]
    fn neither_network_nor_broadcast() {
        assert_eq!(
            validate("192.168.1.0", "24", "192.168.1.1", "", ""),
            Err(Error::AddrIsNetworkOrBroadcast)
        );
        assert_eq!(
            validate("192.168.1.255", "24", "192.168.1.1", "", ""),
            Err(Error::AddrIsNetworkOrBroadcast)
        );
        assert_eq!(
            validate("192.168.1.5", "24", "192.168.1.255", "", ""),
            Err(Error::GatewayIsNetworkOrBroadcast)
        );
        // .255 is a host in a /23.
        assert!(validate("192.168.0.255", "23", "192.168.1.1", "", "").is_ok());
    }

    #[test]
    fn the_packed_form_is_the_five_fields_in_order() {
        let s = validate_packed("192.168.1.50,24,192.168.1.1,,9.9.9.9").unwrap();
        assert_eq!(s.dns, Ipv4Addr::new(192, 168, 1, 1));
        assert_eq!(s.dns2, Some(Ipv4Addr::new(9, 9, 9, 9)));
        assert!(validate_packed("192.168.1.50,24,192.168.1.1").is_ok());
        assert_eq!(validate_packed(""), Err(Error::BadAddr));
        assert_eq!(validate_packed("192.168.1.50,24"), Err(Error::BadGateway));
        assert_eq!(
            validate_packed("192.168.1.50,24,192.168.1.1,,,x"),
            Err(Error::BadDns2)
        );
    }

    #[test]
    fn the_watch_confirms_on_the_first_answer() {
        let mut w = Watch::new();
        assert!(w.probe_due(0, true));
        w.probed(0);
        assert_eq!(w.tick(0, true, false), Verdict::Wait);
        assert!(!w.probe_due(5, true));
        assert!(w.probe_due(10, true));
        assert_eq!(w.tick(10, true, true), Verdict::Confirmed);
        assert!(w.confirmed());
        assert!(!w.probe_due(30, true));
        // Never a fallback afterwards, however long the silence.
        assert_eq!(w.tick(10_000, true, false), Verdict::Wait);
        assert!(!w.fell_back());
    }

    #[test]
    fn the_watch_falls_back_after_the_link_time_without_answer() {
        let mut w = Watch::new();
        let mut t = 0;
        let mut out = Verdict::Wait;
        while t <= GIVE_UP_S + 10 && out == Verdict::Wait {
            out = w.tick(t, true, false);
            t += 2;
        }
        assert_eq!(out, Verdict::FallBack);
        assert!(w.fell_back());
        assert!(w.waited_s() >= GIVE_UP_S && w.waited_s() < GIVE_UP_S + 2);
        // Once only, and no probes after it.
        assert_eq!(w.tick(t, true, true), Verdict::Wait);
        assert!(!w.probe_due(t + 100, true));
    }

    #[test]
    fn time_without_link_does_not_count() {
        let mut w = Watch::new();
        assert_eq!(w.tick(0, true, false), Verdict::Wait);
        assert_eq!(w.tick(100, true, false), Verdict::Wait);
        // An hour with the cable out or the router off.
        assert_eq!(w.tick(3700, false, false), Verdict::Wait);
        assert_eq!(w.waited_s(), 100);
        assert!(!w.probe_due(3700, false));
        // The link comes back and the router answers in time.
        assert_eq!(w.tick(3750, true, false), Verdict::Wait);
        assert_eq!(w.waited_s(), 150);
        assert_eq!(w.tick(3760, true, true), Verdict::Confirmed);
        // An answer without the link (a stale pool flag) confirms nothing.
        let mut w = Watch::new();
        assert_eq!(w.tick(0, false, true), Verdict::Wait);
        assert!(!w.confirmed());
    }
}
