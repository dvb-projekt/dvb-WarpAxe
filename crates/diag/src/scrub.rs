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

    /// `text` with every rule applied.
    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (from, to) in self.ordered() {
            if out.contains(from.as_str()) {
                out = out.replace(from.as_str(), to);
            }
        }
        out
    }

    /// Whether any rule's text still appears in `text` (a check after
    /// scrubbing, for tests and for the firmware's own assertion).
    pub fn leaks(&self, text: &str) -> bool {
        self.rules
            .iter()
            .any(|(from, to)| from != to && text.contains(from.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
