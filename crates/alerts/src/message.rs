//! What a message says.
//!
//! One line in English, starting with an emoji and the miner's hostname
//! so a channel that several miners post to stays readable:
//!
//! ```text
//! ⛏️ bitaxe602: new best difficulty 14.8M (network 102.3T)
//! 🎉 bitaxe602: BLOCK FOUND at height 978480, difficulty 128.1T (network 102.3T)
//! ↪️ bitaxe602: main pool failed 3 times, mining on the fallback pool
//! ↩️ bitaxe602: back on the main pool
//! ⚠️ bitaxe602: restarted after a crash (int-wdt)
//! ✅ bitaxe602: test message from WarpAxe
//! ```
//!
//! Nothing else goes out: no IP or MAC address, no worker name, no pool
//! address. The body asks Discord to ping no one (`allowed_mentions`),
//! whatever the hostname says.

use serde_json::json;

/// Which switch a message belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The test button: always sent when a webhook is set.
    Test,
    /// `alertBlockFound`.
    Block,
    /// `alertBestDiff`.
    BestDiff,
    /// `alertPoolSwitch`.
    Pool,
    /// `alertCrash`.
    Crash,
}

impl Kind {
    /// The name in `discordLastResult.what`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Test => "test",
            Kind::Block => "block",
            Kind::BestDiff => "bestDiff",
            Kind::Pool => "pool",
            Kind::Crash => "crash",
        }
    }
}

/// One message to send.
#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    /// The test button.
    Test,
    /// A new all-time best difficulty.
    BestDiff {
        /// The new best.
        diff: f64,
    },
    /// A block found (counted once, as the event log has it).
    Block {
        /// Its height.
        height: u64,
        /// The hash's difficulty.
        diff: f64,
        /// The coin's ticker; said only when not BTC.
        coin: String,
    },
    /// The main pool failed `fails` times: on the fallback.
    Fallback {
        /// Failed attempts.
        fails: u32,
    },
    /// The main pool sent no new job for `secs` seconds: on the fallback.
    StaleFallback {
        /// Seconds without a job.
        secs: u64,
    },
    /// The main pool answers again: back from the fallback.
    Back,
    /// The fallback sent no new job for `secs` seconds either: back to
    /// the main pool.
    FallbackStale {
        /// Seconds without a job.
        secs: u64,
    },
    /// The miner restarted after a crash or a watchdog; `reason` is the
    /// reset reason (`panic`, `int-wdt`, `task-wdt`, `wdt`).
    Crash {
        /// The reset reason.
        reason: String,
    },
}

impl Alert {
    /// Its switch.
    pub fn kind(&self) -> Kind {
        match self {
            Alert::Test => Kind::Test,
            Alert::BestDiff { .. } => Kind::BestDiff,
            Alert::Block { .. } => Kind::Block,
            Alert::Fallback { .. }
            | Alert::StaleFallback { .. }
            | Alert::Back
            | Alert::FallbackStale { .. } => Kind::Pool,
            Alert::Crash { .. } => Kind::Crash,
        }
    }

    /// The message: `host` the miner's hostname, `network` the network
    /// difficulty of the current job (0 = unknown, then left out),
    /// `skipped` how many messages of the same kind were folded into
    /// this one (coalescing, [`crate::pace::Coalesce`]).
    pub fn text(&self, host: &str, network: f64, skipped: u32) -> String {
        let h = hostname(host);
        let net = if network.is_finite() && network > 0.0 {
            format!(" (network {})", si(network))
        } else {
            String::new()
        };
        let mut s = match self {
            Alert::Test => format!("✅ {h}: test message from WarpAxe"),
            Alert::BestDiff { diff } => {
                format!("⛏️ {h}: new best difficulty {}{net}", si(*diff))
            }
            Alert::Block { height, diff, coin } => {
                let coin = if coin.is_empty() || coin.eq_ignore_ascii_case("BTC") {
                    String::new()
                } else {
                    format!(" ({})", plain(coin, 12))
                };
                format!(
                    "🎉 {h}: BLOCK FOUND at height {height}{coin}, difficulty {}{net}",
                    si(*diff)
                )
            }
            Alert::Fallback { fails } => match fails {
                0 | 1 => format!("↪️ {h}: main pool failed, mining on the fallback pool"),
                n => format!("↪️ {h}: main pool failed {n} times, mining on the fallback pool"),
            },
            Alert::StaleFallback { secs } => format!(
                "↪️ {h}: main pool sent no new job for {secs} s, mining on the fallback pool"
            ),
            Alert::Back => format!("↩️ {h}: back on the main pool"),
            Alert::FallbackStale { secs } => format!(
                "↩️ {h}: fallback pool sent no new job for {secs} s either, back to the main pool"
            ),
            Alert::Crash { reason } => {
                format!("⚠️ {h}: restarted after a crash ({})", plain(reason, 16))
            }
        };
        if skipped > 0 && self.kind() == Kind::Pool {
            let what = if skipped == 1 { "switch" } else { "switches" };
            s.push_str(&format!(" ({skipped} earlier pool {what} not sent)"));
        }
        s
    }
}

/// The detail codes of the event log (`warpaxe_diag::events::detail`)
/// that are a pool switch, as a message; `value` reads the event's
/// numbers by name (`n` failures, `s` seconds).
pub fn from_event(code: &str, value: impl Fn(&str) -> Option<u64>) -> Option<Alert> {
    Some(match code {
        "pool_fallback" => Alert::Fallback {
            fails: value("n").map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        },
        "pool_stale_fallback" => Alert::StaleFallback {
            secs: value("s").unwrap_or(0),
        },
        "pool_back" => Alert::Back,
        "fallback_stale" => Alert::FallbackStale {
            secs: value("s").unwrap_or(0),
        },
        _ => return None,
    })
}

/// A difficulty as the display writes it, in English: `14.8M`, `102.3T`,
/// `4.12K`, `523`; below 100 with up to two decimals (the devkit's
/// shares sit below 1).
pub fn si(v: f64) -> String {
    const UNITS: [&str; 7] = ["", "K", "M", "G", "T", "P", "E"];
    let mut v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 {
        if v >= 100.0 {
            return format!("{v:.0}");
        }
        let s = format!("{v:.2}");
        return s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    // 999.96K would print as 1000.0K: the next unit instead.
    let d = if v >= 10.0 { 1 } else { 2 };
    let s = format!("{v:.d$}");
    if s.starts_with("1000") && i < UNITS.len() - 1 {
        return format!("1.00{}", UNITS[i + 1]);
    }
    format!("{s}{}", UNITS[i])
}

/// Text from the device without control characters, cut to `max`
/// characters.
fn plain(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// The hostname as it goes into a message: at most 32 characters, no
/// control characters, Discord's markdown characters escaped (a host
/// called `rig_1_` must not turn the rest into italics); `warpaxe` when
/// empty.
pub fn hostname(host: &str) -> String {
    let host = plain(host.trim(), 32);
    if host.is_empty() {
        return "warpaxe".into();
    }
    let mut out = String::with_capacity(host.len() + 4);
    for c in host.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '<' | ':' | '#' | '[' | ']'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Discord's limit for `content`.
pub const MAX_CONTENT: usize = 2000;

/// The JSON body of the webhook POST: the text (escaped by serde_json,
/// cut to [`MAX_CONTENT`] characters) and no mentions.
pub fn body(text: &str) -> String {
    let content: String = text.chars().take(MAX_CONTENT).collect();
    json!({ "content": content, "allowed_mentions": { "parse": [] } }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_reads_as_documented() {
        let h = "bitaxe602";
        let cases = [
            (Alert::Test, 0.0, "✅ bitaxe602: test message from WarpAxe"),
            (
                Alert::BestDiff { diff: 14_812_345.0 },
                102.3e12,
                "⛏️ bitaxe602: new best difficulty 14.8M (network 102.3T)",
            ),
            (
                Alert::BestDiff { diff: 4_123.0 },
                0.0,
                "⛏️ bitaxe602: new best difficulty 4.12K",
            ),
            (
                Alert::Block {
                    height: 978_480,
                    diff: 128.1e12,
                    coin: "BTC".into(),
                },
                102.3e12,
                "🎉 bitaxe602: BLOCK FOUND at height 978480, difficulty 128.1T (network 102.3T)",
            ),
            (
                Alert::Block {
                    height: 54_110,
                    diff: 2.5e6,
                    coin: "FIX".into(),
                },
                0.0,
                "🎉 bitaxe602: BLOCK FOUND at height 54110 (FIX), difficulty 2.50M",
            ),
            (
                Alert::Fallback { fails: 3 },
                0.0,
                "↪️ bitaxe602: main pool failed 3 times, mining on the fallback pool",
            ),
            (
                Alert::Fallback { fails: 1 },
                0.0,
                "↪️ bitaxe602: main pool failed, mining on the fallback pool",
            ),
            (
                Alert::StaleFallback { secs: 300 },
                0.0,
                "↪️ bitaxe602: main pool sent no new job for 300 s, mining on the fallback pool",
            ),
            (Alert::Back, 0.0, "↩️ bitaxe602: back on the main pool"),
            (
                Alert::FallbackStale { secs: 600 },
                0.0,
                "↩️ bitaxe602: fallback pool sent no new job for 600 s either, back to the main pool",
            ),
            (
                Alert::Crash {
                    reason: "int-wdt".into(),
                },
                0.0,
                "⚠️ bitaxe602: restarted after a crash (int-wdt)",
            ),
        ];
        for (a, net, want) in cases {
            assert_eq!(a.text(h, net, 0), want);
        }
    }

    #[test]
    fn folded_pool_switches_are_counted_in_the_text() {
        assert_eq!(
            Alert::Back.text("rig", 0.0, 1),
            "↩️ rig: back on the main pool (1 earlier pool switch not sent)"
        );
        assert_eq!(
            Alert::Back.text("rig", 0.0, 3),
            "↩️ rig: back on the main pool (3 earlier pool switches not sent)"
        );
        // A folded best difficulty says only the newest.
        assert_eq!(
            Alert::BestDiff { diff: 2e6 }.text("rig", 0.0, 5),
            "⛏️ rig: new best difficulty 2.00M"
        );
    }

    #[test]
    fn kinds_follow_the_switches() {
        assert_eq!(Alert::Test.kind(), Kind::Test);
        assert_eq!(Alert::BestDiff { diff: 1.0 }.kind(), Kind::BestDiff);
        assert_eq!(Alert::Back.kind(), Kind::Pool);
        assert_eq!(Alert::FallbackStale { secs: 1 }.kind(), Kind::Pool);
        assert_eq!(
            Alert::Crash {
                reason: "panic".into()
            }
            .kind(),
            Kind::Crash
        );
        assert_eq!(Kind::BestDiff.name(), "bestDiff");
    }

    #[test]
    fn pool_events_become_messages() {
        let v = |k: &str| match k {
            "n" => Some(3),
            "s" => Some(300),
            _ => None,
        };
        assert_eq!(
            from_event("pool_fallback", v),
            Some(Alert::Fallback { fails: 3 })
        );
        assert_eq!(
            from_event("pool_stale_fallback", v),
            Some(Alert::StaleFallback { secs: 300 })
        );
        assert_eq!(from_event("pool_back", v), Some(Alert::Back));
        assert_eq!(
            from_event("fallback_stale", v),
            Some(Alert::FallbackStale { secs: 300 })
        );
        // Reconnects to the same pool and the pools-out stop are not a
        // switch between main and fallback.
        assert_eq!(from_event("pool_stale", v), None);
        assert_eq!(from_event("pools_out", v), None);
        assert_eq!(from_event("block_found", v), None);
        assert_eq!(
            from_event("pool_fallback", |_| None),
            Some(Alert::Fallback { fails: 0 })
        );
    }

    #[test]
    fn si_prefixes() {
        for (v, want) in [
            (0.0, "0"),
            (0.0123, "0.01"),
            (1.5, "1.5"),
            (42.0, "42"),
            (523.7, "524"),
            (999.0, "999"),
            (1000.0, "1.00K"),
            (4123.0, "4.12K"),
            (14_812_345.0, "14.8M"),
            (999_960.0, "1.00M"),
            (102.3e12, "102.3T"),
            (3.2e21, "3200.0E"),
            (-5.0, "0"),
            (f64::NAN, "0"),
        ] {
            assert_eq!(si(v), want, "{v}");
        }
    }

    #[test]
    fn hostnames_cannot_format_or_break_the_message() {
        assert_eq!(hostname("bitaxe602"), "bitaxe602");
        assert_eq!(hostname("rig_1_"), "rig\\_1\\_");
        assert_eq!(hostname("**big**"), "\\*\\*big\\*\\*");
        assert_eq!(hostname(":tada:"), "\\:tada\\:");
        assert_eq!(hostname("  "), "warpaxe");
        assert_eq!(hostname("a\nb\u{7}"), "ab");
        assert_eq!(hostname(&"x".repeat(40)).len(), 32);
    }

    #[test]
    fn the_body_is_escaped_json_without_mentions() {
        let b = body("⛏️ \"quoted\" back\\slash\nnew line\ttab \u{1} @everyone");
        let v: serde_json::Value = serde_json::from_str(&b).expect("valid JSON");
        assert_eq!(
            v["content"],
            "⛏️ \"quoted\" back\\slash\nnew line\ttab \u{1} @everyone"
        );
        assert_eq!(v["allowed_mentions"]["parse"], json!([]));
        assert!(b.contains("\\\"quoted\\\""));
        assert!(b.contains("\\n"));
        assert!(b.contains("\\u0001"));
        let long = body(&"é".repeat(MAX_CONTENT + 10));
        let v: serde_json::Value = serde_json::from_str(&long).unwrap();
        assert_eq!(v["content"].as_str().unwrap().chars().count(), MAX_CONTENT);
    }
}
