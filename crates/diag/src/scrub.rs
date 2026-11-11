//! The diagnostics file's privacy rules.

/// A worker name with its address masked: `bc1qabcd…wxyz.miner1`. The
/// part before the first dot is the address (or a pool account); its
/// first four and last four characters stay so support can tell two
/// miners apart, the middle goes. Short names (up to twelve characters)
/// keep their first two characters only. The part after the dot is the
/// worker's own name and stays.
pub fn mask_worker(user: &str) -> String {
    let (addr, rest) = match user.split_once('.') {
        Some((a, r)) => (a, Some(r)),
        None => (user, None),
    };
    let masked = mask_middle(addr);
    match rest {
        Some(r) => format!("{masked}.{r}"),
        None => masked,
    }
}

/// `abcd…wxyz` for a string longer than twelve characters, `ab…` for a
/// shorter one, empty stays empty.
fn mask_middle(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    if chars.len() > 12 {
        let head: String = chars[..4].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    } else {
        let head: String = chars.iter().take(2).collect();
        format!("{head}…")
    }
}

/// A Wi-Fi name masked to its first character and its length: `F…8`.
/// Enough to see that the miner is on the network it should be, not
/// enough to name the network.
pub fn mask_ssid(ssid: &str) -> String {
    let mut it = ssid.chars();
    match it.next() {
        None => String::new(),
        Some(c) => format!("{c}…{}", ssid.chars().count()),
    }
}

/// Every MAC address in `text` (six hex pairs joined by `:`) with its
/// last three pairs masked: `58:e6:c5:**:**:**`. The first three name
/// the maker (Espressif, the router's vendor), which support may need;
/// the rest is the device's own and goes. Covers the miner's own MAC and
/// the access point's BSSID in the Wi-Fi log lines alike.
pub fn mask_macs(text: &str) -> String {
    const LEN: usize = 17;
    let b = text.as_bytes();
    let is_mac = |w: &[u8]| {
        w.len() == LEN
            && w.iter().enumerate().all(|(i, c)| {
                if i % 3 == 2 {
                    *c == b':'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
    };
    let edge = |c: Option<&u8>| c.is_none_or(|c| !(c.is_ascii_hexdigit() || *c == b':'));
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    while i + LEN <= b.len() {
        if is_mac(&b[i..i + LEN])
            && edge(i.checked_sub(1).and_then(|j| b.get(j)))
            && edge(b.get(i + LEN))
        {
            out.push_str(&text[copied..i + 9]);
            out.push_str("**:**:**");
            i += LEN;
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Replaces every secret with `***` and every other rule's text with its
/// replacement, in the order added. Secrets shorter than four characters
/// are not searched for: the firmware never prints a password, and a
/// one-letter secret (the pool default "x") would eat the text.
#[derive(Debug, Default, Clone)]
pub struct Scrubber {
    rules: Vec<(String, String)>,
}

/// What a secret is replaced with.
pub const MASK: &str = "***";
/// Secrets shorter than this are not searched for.
pub const MIN_SECRET_LEN: usize = 4;

impl Scrubber {
    /// No rules yet.
    pub fn new() -> Self {
        Scrubber::default()
    }

    /// A secret to replace with [`MASK`] wherever it appears.
    pub fn secret(mut self, s: &str) -> Self {
        if s.chars().count() >= MIN_SECRET_LEN {
            self.rules.push((s.to_string(), MASK.to_string()));
        }
        self
    }

    /// A text to replace with another (an address with its masked form).
    pub fn replace(mut self, from: &str, to: &str) -> Self {
        if !from.is_empty() && from != to {
            self.rules.push((from.to_string(), to.to_string()));
        }
        self
    }

    /// Longest rule first, so a secret that contains another is masked
    /// whole.
    fn ordered(&self) -> Vec<&(String, String)> {
        let mut r: Vec<&(String, String)> = self.rules.iter().collect();
        r.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        r
    }

    /// `text` with every rule applied, and every MAC address masked
    /// ([`mask_macs`]).
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (from, to) in self.ordered() {
            if out.contains(from.as_str()) {
                out = out.replace(from.as_str(), to);
            }
        }
        if out.contains(':') {
            out = mask_macs(&out);
        }
        out
    }

    /// Every string value of a JSON document scrubbed, keys left alone:
    /// a network name that happens to be an English word ("pool",
    /// "temp") must not rewrite the keys the file is read by.
    pub fn apply_json(&self, v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) => {
                let out = self.apply(s);
                if out != *s {
                    *s = out;
                }
            }
            serde_json::Value::Array(a) => a.iter_mut().for_each(|x| self.apply_json(x)),
            serde_json::Value::Object(o) => o.values_mut().for_each(|x| self.apply_json(x)),
            _ => {}
        }
    }

    /// Whether a secret (a password, not a masked name) still appears in
    /// `text`: the check after scrubbing, for tests and for the
    /// firmware's own assertion before a section goes out.
    pub fn leaks(&self, text: &str) -> bool {
        self.rules
            .iter()
            .any(|(from, to)| to == MASK && text.contains(from.as_str()))
    }

    /// [`Self::leaks`] over the string values of a JSON document only,
    /// the same values [`Self::apply_json`] scrubs. Numbers and keys are
    /// the firmware's own: a pool password that is a number ("1000", a
    /// difficulty hint) must not drop a whole section because a reading
    /// or a setting happens to have the same digits.
    pub fn leaks_json(&self, v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::String(s) => self.leaks(s),
            serde_json::Value::Array(a) => a.iter().any(|x| self.leaks_json(x)),
            serde_json::Value::Object(o) => o.values().any(|x| self.leaks_json(x)),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numeric_password_does_not_count_numbers_as_a_leak() {
        let s = Scrubber::new().secret("1000");
        let mut v =
            serde_json::json!({"difficulty": 1000, "power": 10000.5, "log": ["difficulty 10000"]});
        s.apply_json(&mut v);
        assert!(!s.leaks_json(&v));
        assert_eq!(v["log"][0], "difficulty ***0");
        assert_eq!(v["difficulty"], 1000);
        // A string that still carries it after scrubbing is a leak.
        assert!(s.leaks_json(&serde_json::json!({"x": "pass 1000"})));
    }

    #[test]
    fn a_worker_keeps_its_name_and_loses_the_middle_of_its_address() {
        assert_eq!(
            mask_worker("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.gamma"),
            "bc1q…mdq.gamma".replace("…mdq", "…5mdq")
        );
        assert_eq!(
            mask_worker("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq"),
            "bc1q…5mdq"
        );
        assert_eq!(mask_worker("myaccount.worker1"), "my….worker1");
        assert_eq!(mask_worker("x"), "x…");
        assert_eq!(mask_worker(""), "");
        // Thirteen characters is long, twelve is short.
        assert_eq!(mask_worker("abcdefghijklm"), "abcd…jklm");
        assert_eq!(mask_worker("abcdefghijkl"), "ab…");
    }

    #[test]
    fn mac_addresses_keep_the_maker_and_lose_the_device() {
        assert_eq!(mask_macs("58:E6:C5:6A:48:04"), "58:E6:C5:**:**:**");
        assert_eq!(
            mask_macs("I (11305) wifi:mode : sta (58:e6:c5:6a:48:04)"),
            "I (11305) wifi:mode : sta (58:e6:c5:**:**:**)"
        );
        assert_eq!(
            mask_macs("aid = 21, bssid = c8:7f:54:27:96:10, x 00:11:22:33:44:55"),
            "aid = 21, bssid = c8:7f:54:**:**:**, x 00:11:22:**:**:**"
        );
        // Not a MAC: times, version masks, longer colon runs, non-hex.
        for t in [
            "12:30:45",
            "aa:bb:cc:dd:ee:ff:00",
            "gg:hh:ii:jj:kk:ll",
            "58:e6:c5:6a:48",
            "",
            "ü 12:34",
        ] {
            assert_eq!(mask_macs(t), t);
        }
        // Through the scrubber, in JSON string values.
        let mut v = serde_json::json!({"macAddr": "58:E6:C5:6A:48:04", "n": 5});
        Scrubber::new().apply_json(&mut v);
        assert_eq!(v["macAddr"], "58:E6:C5:**:**:**");
    }

    #[test]
    fn an_ssid_is_a_first_letter_and_a_length() {
        assert_eq!(mask_ssid("FRITZ!Box 7590"), "F…14");
        assert_eq!(mask_ssid("Ü"), "Ü…1");
        assert_eq!(mask_ssid(""), "");
    }

    #[test]
    fn every_secret_goes_including_inside_a_log_line() {
        let s = Scrubber::new()
            .secret("hunter2hunter2")
            .secret("Tr0ub4dor&3")
            .secret("x")
            .replace("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", "bc1q…5mdq")
            .replace("FRITZ!Box 7590", "F…14");
        let log = "I (123) wifi: joined FRITZ!Box 7590 with hunter2hunter2\nI (200) stratum: authorized as bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.gamma pass Tr0ub4dor&3 extra x\n";
        let out = s.apply(log);
        assert_eq!(
            out,
            "I (123) wifi: joined F…14 with ***\nI (200) stratum: authorized as bc1q…5mdq.gamma pass *** extra x\n"
        );
        assert!(!s.leaks(&out));
        assert!(s.leaks(log));
        assert!(s.leaks("Tr0ub4dor&3 in a line"));
    }

    #[test]
    fn json_values_are_scrubbed_and_keys_stay() {
        let s = Scrubber::new()
            .secret("hunter2hunter2")
            .replace("pool", "p…4")
            .replace("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq", "bc1q…5mdq");
        let mut v = serde_json::json!({
            "pool": "stratum.pool.example:3333",
            "ssid": "pool",
            "stratumUser": "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq.gamma",
            "log": ["joined pool with hunter2hunter2", "temp 61"],
            "n": 5
        });
        s.apply_json(&mut v);
        assert_eq!(v["pool"], "stratum.p…4.example:3333");
        assert_eq!(v["ssid"], "p…4");
        assert_eq!(v["stratumUser"], "bc1q…5mdq.gamma");
        assert_eq!(v["log"][0], "joined p…4 with ***");
        assert_eq!(v["log"][1], "temp 61");
        assert_eq!(v["n"], 5);
        let text = v.to_string();
        assert!(text.contains("\"pool\":"), "the key survives: {text}");
        assert!(!s.leaks(&text));
        // Only a secret counts as a leak: a masked name that is also an
        // English word may well appear elsewhere.
        assert!(!s.leaks("the pool page"));
        assert!(s.leaks("pass hunter2hunter2"));
    }

    #[test]
    fn a_secret_inside_a_longer_one_is_masked_whole() {
        let s = Scrubber::new().secret("pass").secret("password123");
        assert_eq!(
            s.apply("the password123 and the pass"),
            "the *** and the ***"
        );
    }

    #[test]
    fn short_secrets_and_empty_rules_are_ignored() {
        let s = Scrubber::new()
            .secret("abc")
            .secret("")
            .replace("", "x")
            .replace("same", "same");
        assert_eq!(s.apply("abc same"), "abc same");
        assert!(!s.leaks("abc same"));
    }
}
