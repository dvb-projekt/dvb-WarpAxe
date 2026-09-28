//! First-time network setup: the Wi-Fi hotspot a miner opens when it has
//! no network to join, and the captive-portal DNS answer that sends a
//! phone joining it to the web UI.
//!
//! AxeOS opens an open hotspot for this. WarpAxe's is WPA2 with a password
//! made at boot and shown only on the device display: whoever stands at
//! the miner gets in, the neighbours do not.

/// Seconds after boot without a station link before the hotspot opens.
pub const SETUP_AFTER_S: u32 = 180;
/// Seconds the station must stay up before the hotspot closes.
pub const CLOSE_AFTER_S: u32 = 60;
/// Where the hotspot serves the web UI.
pub const AP_IP: [u8; 4] = [192, 168, 4, 1];

/// Password characters: no 0/O, 1/I/L, easy to read off a small display.
const ALPHABET: &[u8; 31] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// The hotspot name: `WarpAxe-` and the last two bytes of the MAC.
pub fn ssid(mac: [u8; 6]) -> [u8; 12] {
    let mut s = *b"WarpAxe-0000";
    let hex = b"0123456789ABCDEF";
    for (i, b) in [mac[4], mac[5]].iter().enumerate() {
        s[8 + i * 2] = hex[usize::from(b >> 4)];
        s[9 + i * 2] = hex[usize::from(b & 15)];
    }
    s
}

/// A hotspot password `ABCD-EFGH` from two random words: eight
/// characters of 31, about 40 bits.
pub fn password(r0: u32, r1: u32) -> [u8; 9] {
    let mut p = [b'-'; 9];
    let mut n = (u64::from(r0) << 32) | u64::from(r1);
    for slot in [0, 1, 2, 3, 5, 6, 7, 8] {
        p[slot] = ALPHABET[(n % 31) as usize];
        n /= 31;
    }
    p
}

/// When the hotspot should be up. Sampled by the Wi-Fi thread.
#[derive(Clone, Debug, Default)]
pub struct Hotspot {
    connected_once: bool,
    connected_since: Option<u32>,
    open: bool,
}

/// What the Wi-Fi thread should do with the hotspot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the hotspot.
    Open,
    /// Stop it.
    Close,
    /// Leave it as it is.
    Keep,
}

impl Hotspot {
    /// No link yet, hotspot closed.
    pub const fn new() -> Self {
        Self {
            connected_once: false,
            connected_since: None,
            open: false,
        }
    }

    /// Whether the hotspot is up.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// One sample: `ssid_set` is a network in the config, `now` seconds
    /// since boot, `connected` the station link.
    pub fn tick(&mut self, ssid_set: bool, now: u32, connected: bool) -> Action {
        if connected {
            self.connected_once = true;
            self.connected_since.get_or_insert(now);
        } else {
            self.connected_since = None;
        }
        let want = if !ssid_set {
            true
        } else if self.open {
            // Stays up until the station has held for a while.
            !self
                .connected_since
                .is_some_and(|t| now.saturating_sub(t) >= CLOSE_AFTER_S)
        } else {
            // Only a boot that never got on the network; a later outage of
            // the router does not open it.
            !self.connected_once && now >= SETUP_AFTER_S
        };
        match (self.open, want) {
            (false, true) => {
                self.open = true;
                Action::Open
            }
            (true, false) => {
                self.open = false;
                Action::Close
            }
            _ => Action::Keep,
        }
    }
}

/// The captive-portal DNS answer: every A query resolves to `ip`, any
/// other type gets an empty answer. Writes the reply into `out` and
/// returns its length; `None` for anything that is not a plain query.
pub fn dns_reply(query: &[u8], ip: [u8; 4], out: &mut [u8]) -> Option<usize> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    // A query (QR 0), standard opcode, one question.
    if flags & 0x8000 != 0 || (flags >> 11) & 0xf != 0 || qdcount != 1 {
        return None;
    }
    let mut i = 12;
    loop {
        let len = usize::from(*query.get(i)?);
        if len == 0 {
            i += 1;
            break;
        }
        if len > 63 {
            return None; // compression is not valid in a question
        }
        i += 1 + len;
        if i > 12 + 255 {
            return None;
        }
    }
    let qend = i + 4;
    if query.len() < qend {
        return None;
    }
    let qtype = u16::from_be_bytes([query[i], query[i + 1]]);
    let qclass = u16::from_be_bytes([query[i + 2], query[i + 3]]);
    let answer = qtype == 1 && qclass == 1;
    let total = qend + if answer { 16 } else { 0 };
    if out.len() < total {
        return None;
    }
    out[..qend].copy_from_slice(&query[..qend]);
    // QR, AA, RD copied; RA 0, RCODE 0.
    let rflags = 0x8000 | 0x0400 | (flags & 0x0100);
    out[2..4].copy_from_slice(&rflags.to_be_bytes());
    out[6..8].copy_from_slice(&u16::from(answer).to_be_bytes());
    out[8..12].fill(0);
    if answer {
        let a = &mut out[qend..total];
        a[0..2].copy_from_slice(&[0xc0, 0x0c]); // name: pointer to the question
        a[2..4].copy_from_slice(&1u16.to_be_bytes()); // A
        a[4..6].copy_from_slice(&1u16.to_be_bytes()); // IN
        a[6..10].copy_from_slice(&60u32.to_be_bytes()); // TTL
        a[10..12].copy_from_slice(&4u16.to_be_bytes());
        a[12..16].copy_from_slice(&ip);
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16, extra: &[u8]) -> Vec<u8> {
        let mut q = vec![
            0x12,
            0x34,
            0x01,
            0x00,
            0,
            1,
            0,
            0,
            0,
            0,
            0,
            u8::from(!extra.is_empty()),
        ];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q.extend_from_slice(extra);
        q
    }

    #[test]
    fn a_query_resolves_to_the_hotspot() {
        let q = query("connectivitycheck.gstatic.com", 1, &[]);
        let mut out = [0u8; 512];
        let n = dns_reply(&q, AP_IP, &mut out).unwrap();
        let r = &out[..n];
        assert_eq!(&r[0..2], &[0x12, 0x34], "same id");
        assert_eq!(r[2] & 0x80, 0x80, "response");
        assert_eq!(r[2] & 0x01, 0x01, "RD copied");
        assert_eq!(&r[6..8], &[0, 1], "one answer");
        assert_eq!(&r[n - 4..], &AP_IP);
        assert_eq!(&r[n - 16..n - 14], &[0xc0, 0x0c]);
    }

    #[test]
    fn edns_record_is_dropped_and_other_types_get_no_answer() {
        let opt = [0, 0, 41, 0x10, 0, 0, 0, 0, 0, 0, 0];
        let q = query("captive.apple.com", 28, &opt); // AAAA
        let mut out = [0u8; 512];
        let n = dns_reply(&q, AP_IP, &mut out).unwrap();
        assert_eq!(n, q.len() - opt.len(), "question only");
        assert_eq!(&out[6..12], &[0; 6]);
    }

    #[test]
    fn junk_is_ignored() {
        let mut out = [0u8; 512];
        assert!(dns_reply(&[0; 5], AP_IP, &mut out).is_none(), "short");
        let mut resp = query("a.b", 1, &[]);
        resp[2] |= 0x80;
        assert!(dns_reply(&resp, AP_IP, &mut out).is_none(), "a response");
        let mut ptr = query("a.b", 1, &[]);
        ptr[12] = 0xc0;
        assert!(
            dns_reply(&ptr, AP_IP, &mut out).is_none(),
            "pointer in question"
        );
        let q = query("a.b", 1, &[]);
        assert!(
            dns_reply(&q[..q.len() - 2], AP_IP, &mut out).is_none(),
            "cut off"
        );
        assert!(dns_reply(&q, AP_IP, &mut [0u8; 10]).is_none(), "no room");
        let mut long = vec![0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        long.extend(std::iter::repeat_n([3u8, b'a', b'b', b'c'], 80).flatten());
        assert!(
            dns_reply(&long, AP_IP, &mut out).is_none(),
            "name too long, no terminator"
        );
    }

    #[test]
    fn ssid_and_password_shape() {
        assert_eq!(&ssid([0x80, 0xb5, 0x4e, 0xc4, 0x6c, 0x98]), b"WarpAxe-6C98");
        let p = password(0xdead_beef, 0x0123_4567);
        assert_eq!(p[4], b'-');
        assert!(p
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || ALPHABET.contains(c)));
        assert_ne!(password(1, 2), password(2, 1));
    }

    #[test]
    fn no_network_configured_opens_at_once_and_stays() {
        let mut h = Hotspot::new();
        assert_eq!(h.tick(false, 0, false), Action::Open);
        assert_eq!(h.tick(false, 5000, false), Action::Keep);
        assert!(h.is_open());
    }

    #[test]
    fn opens_after_three_minutes_without_a_link_then_closes_once_it_holds() {
        let mut h = Hotspot::new();
        assert_eq!(h.tick(true, SETUP_AFTER_S - 1, false), Action::Keep);
        assert_eq!(h.tick(true, SETUP_AFTER_S, false), Action::Open);
        assert_eq!(h.tick(true, 300, true), Action::Keep);
        assert_eq!(h.tick(true, 300 + CLOSE_AFTER_S - 1, true), Action::Keep);
        assert_eq!(h.tick(true, 300 + CLOSE_AFTER_S, true), Action::Close);
        assert!(!h.is_open());
    }

    #[test]
    fn a_short_link_does_not_close_it() {
        let mut h = Hotspot::new();
        h.tick(true, SETUP_AFTER_S, false);
        h.tick(true, 200, true);
        h.tick(true, 230, false);
        assert_eq!(
            h.tick(true, 290, true),
            Action::Keep,
            "clock restarts after the drop"
        );
        assert_eq!(h.tick(true, 290 + CLOSE_AFTER_S, true), Action::Close);
    }

    #[test]
    fn a_router_outage_later_does_not_open_it() {
        let mut h = Hotspot::new();
        h.tick(true, 20, true);
        assert_eq!(h.tick(true, 10_000, false), Action::Keep);
        assert!(!h.is_open());
    }
}
