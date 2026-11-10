//! The event log that survives restarts.
//!
//! One JSON string in NVS, like the config patches: a list of the last
//! [`CAPACITY`] events, oldest first, at most [`MAX_BYTES`] long so it
//! fits an NVS string entry with room to spare. Written only when an
//! event is pushed, never on a timer; a push identical to the previous
//! event within [`COALESCE_S`] seconds of uptime changes nothing and is
//! not written (a chip that goes silent every few seconds is one event,
//! not a flash write per occurrence). The firmware writes from threads
//! whose stack is in internal RAM only (`firmware/warpaxe/src/events.rs`).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use warpaxe_board::power_fault::Condition;
use warpaxe_board::profile::Profile;
use warpaxe_board::selftest::{Check, Report, Status};

/// Events kept.
pub const CAPACITY: usize = 32;
/// The stored string at most this long (an NVS string entry holds 4000
/// bytes; the oldest events go first when it would not fit).
pub const MAX_BYTES: usize = 3200;
/// A push identical to the last event within this many seconds of
/// uptime is dropped.
pub const COALESCE_S: u64 = 60;
/// An event's detail is cut to this many characters.
pub const MAX_DETAIL: usize = 48;
/// A coded event is trimmed to at most this many stored bytes where it
/// can be (a list shortened to one entry, long names cut). A plain
/// detail cut to [`MAX_DETAIL`] stores up to ~105 bytes; most coded
/// events store 55–95, so a ring of everyday events keeps all
/// [`CAPACITY`], and one made only of the longest (failed self-tests
/// with three failed checks, say) still keeps 26 or more within
/// [`MAX_BYTES`].
pub const MAX_EVENT_BYTES: usize = 120;
/// A name among a coded event's values (board, pool) is cut to this
/// many characters.
pub const MAX_NAME: usize = 32;
/// A failed self-test keeps at most this many failed checks in its
/// event; the self-test card has the whole report.
pub const MAX_FAILS: usize = 2;

/// One event. Short keys: the string is stored as is.
///
/// A coded event carries its detail as a code `c` and the values `p`
/// the text is made of, so the web UI can say it in its own language;
/// its English text is made from them ([`Event::text`]). Stored, a coded
/// event's `d` is empty (an older image reads the ring all the same,
/// with an empty detail); in the API `d` is the English text, as it
/// always was ([`Event::for_api`]). Events stored before the codes have
/// a `d` only.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Unix seconds when it happened; 0 before NTP set the clock.
    pub t: u64,
    /// Uptime seconds when it happened.
    pub u: u64,
    /// The kind: `boot`, `overheat`, `power`, `pool`, `update`,
    /// `rollback`, `chain`, `selftest`, `board`.
    pub k: String,
    /// What exactly, one short line in English.
    pub d: String,
    /// The detail's code (see [`detail`]), empty for a plain detail.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub c: String,
    /// The detail's values, by the names its texts use.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub p: Map<String, Value>,
}

impl Event {
    /// An event of `kind` with `detail`, at `t` (unix seconds) and `u`
    /// (uptime seconds).
    pub fn new(t: u64, u: u64, kind: &str, detail: Detail) -> Self {
        Event {
            t,
            u,
            k: kind.into(),
            d: detail.d,
            c: detail.c.into(),
            p: detail.p,
        }
    }

    /// The detail in English: made from the code and values when the
    /// event has them, else the stored text.
    pub fn text(&self) -> String {
        if self.c.is_empty() {
            return self.d.clone();
        }
        english(&self.c, &self.p).unwrap_or_else(|| {
            if self.d.is_empty() {
                self.c.clone()
            } else {
                self.d.clone()
            }
        })
    }

    /// The event as the API shows it: `d` the English text, the code and
    /// values beside it.
    pub fn for_api(&self) -> Event {
        Event {
            d: self.text(),
            ..self.clone()
        }
    }

    fn stored_len(&self) -> usize {
        serde_json::to_string(self).map_or(0, |s| s.len())
    }

    /// Bring a coded event towards [`MAX_EVENT_BYTES`]: a list loses its
    /// last entries (one stays), then the longest name is cut.
    fn fit(&mut self) {
        while self.stored_len() > MAX_EVENT_BYTES {
            let len = match self.p.get("f") {
                Some(Value::Array(a)) => a.len(),
                _ => 0,
            };
            if len > 1 {
                // The full count stays, so the text says something is
                // left out.
                self.p.entry("n").or_insert(Value::from(len));
                if let Some(Value::Array(a)) = self.p.get_mut("f") {
                    a.pop();
                }
                continue;
            }
            // Only names are cut: a core dump's tag must stay whole (a
            // later boot looks for it), codes must stay codes.
            let longest = self
                .p
                .iter()
                .filter(|(k, _)| matches!(k.as_str(), "m" | "p"))
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.chars().count())))
                .max_by_key(|(_, n)| *n);
            match longest {
                Some((k, n)) if n > 12 => {
                    let s = self.p[&k].as_str().unwrap_or_default();
                    let cut = cut(s, n - 4);
                    self.p.insert(k, Value::String(cut));
                }
                _ => return,
            }
        }
    }
}

/// What an event says: a code with its values, or (from older call
/// sites and for anything without a code) one line of text.
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    c: &'static str,
    p: Map<String, Value>,
    d: String,
}

impl Detail {
    /// The detail in English, as the API and the log show it.
    pub fn text(&self) -> String {
        if self.c.is_empty() {
            return self.d.clone();
        }
        english(self.c, &self.p).unwrap_or_else(|| {
            if self.d.is_empty() {
                self.c.into()
            } else {
                self.d.clone()
            }
        })
    }

    /// The code, empty for a plain text.
    pub fn code(&self) -> &'static str {
        self.c
    }
}

impl From<String> for Detail {
    fn from(d: String) -> Self {
        Detail {
            c: "",
            p: Map::new(),
            d,
        }
    }
}

impl From<&str> for Detail {
    fn from(d: &str) -> Self {
        Detail::from(d.to_string())
    }
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.into()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn coded(c: &'static str, p: &[(&str, Value)]) -> Detail {
    let p = p.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    Detail {
        c,
        p,
        d: String::new(),
    }
}

/// A name among the values, cut to [`MAX_NAME`].
fn name(s: &str) -> Value {
    Value::String(cut(s, MAX_NAME))
}

/// A measured figure, to two decimals (an f32 widened as is would store
/// 4.849999904632568 for 4.85).
/// A figure as the English text always printed it (`{:.0}`, `{:.2}`:
/// half to even, `-0`, `NaN`): a number where that reads back as one,
/// else the printed string, so the text stays word for word.
fn fig(v: f32, decimals: usize) -> Value {
    let s = format!("{v:.decimals$}");
    match s.parse::<f64>() {
        Ok(f) if f.is_finite() && !(f == 0.0 && s.starts_with('-')) => {
            if decimals == 0 {
                Value::from(f as i64)
            } else {
                Value::from(f)
            }
        }
        _ => Value::String(s),
    }
}

/// The details the firmware records, one constructor each. The code is
/// what the web UI translates (`evd_<code>`); the value names are the
/// placeholders of its texts.
pub mod detail {
    use super::*;

    /// A chip stopped answering.
    pub fn chip_silent(chip: u8) -> Detail {
        coded("chip_silent", &[("c", Value::from(chip))])
    }

    /// One chip silent for ten minutes while the others hashed: the
    /// chain was restarted for it.
    pub fn chip_restart(chip: u8, mv: u16) -> Detail {
        coded(
            "chip_restart",
            &[("c", Value::from(chip)), ("mv", Value::from(mv))],
        )
    }

    /// Restarts did not keep the chip running: no more for it until a
    /// new clock or voltage, or the next boot.
    pub fn chip_given_up(chip: u8) -> Detail {
        coded("chip_given_up", &[("c", Value::from(chip))])
    }

    /// One hash domain of a chip at `percent` of the chip's other
    /// domains for ten minutes while the chip itself kept answering:
    /// the chips were switched off and on for it.
    pub fn domain_latch(chip: u8, domain: u8, percent: u8) -> Detail {
        coded(
            "domain_latch",
            &[
                ("c", Value::from(chip)),
                ("d", Value::from(domain)),
                ("pct", Value::from(percent)),
            ],
        )
    }

    /// The same for a domain dead from the chain's start, found at the
    /// early look after three minutes: the same code, with the minutes
    /// (`min`) the text names instead of its ten.
    pub fn domain_latch_early(chip: u8, domain: u8, percent: u8) -> Detail {
        coded(
            "domain_latch",
            &[
                ("c", Value::from(chip)),
                ("d", Value::from(domain)),
                ("pct", Value::from(percent)),
                ("min", Value::from(3)),
            ],
        )
    }

    /// Resets did not keep the domain running: no more for it until a
    /// new clock or voltage, or the next boot.
    pub fn domain_given_up(chip: u8, domain: u8) -> Detail {
        coded(
            "domain_given_up",
            &[("c", Value::from(chip)), ("d", Value::from(domain))],
        )
    }

    /// All chips silent for a minute: the chain was restarted.
    pub fn chain_restart(chips: u8, mv: u16) -> Detail {
        coded(
            "chain_restart",
            &[("n", Value::from(chips)), ("mv", Value::from(mv))],
        )
    }

    /// A share met the network difficulty: a block at this height of
    /// this coin.
    pub fn block_found(height: u64, coin: &str) -> Detail {
        coded(
            "block_found",
            &[("h", Value::from(height)), ("coin", name(coin))],
        )
    }

    /// The pool's next work builds on the block found at this height.
    pub fn block_confirmed(height: u64) -> Detail {
        coded("block_confirmed", &[("h", Value::from(height))])
    }

    /// Work for the next height builds on another block: the one found
    /// at this height did not make it into the chain.
    pub fn block_orphaned(height: u64) -> Detail {
        coded("block_orphaned", &[("h", Value::from(height))])
    }

    /// The block found at this height was still pending when work for a
    /// height two or more further on came: what became of it can no
    /// longer be told from the pool's work.
    pub fn block_unknown(height: u64) -> Detail {
        coded("block_unknown", &[("h", Value::from(height))])
    }

    /// The main pool failed `fails` times: on the fallback.
    pub fn pool_fallback(fails: u32) -> Detail {
        coded("pool_fallback", &[("n", Value::from(fails))])
    }

    /// The main pool answers again.
    pub fn pool_back() -> Detail {
        coded("pool_back", &[])
    }

    /// Neither the main nor the fallback pool answers: the chips are
    /// off until one does.
    pub fn pools_out() -> Detail {
        coded("pools_out", &[])
    }

    /// A pool sent work again after `off_s` seconds: the chips are back
    /// on.
    pub fn pools_back(off_s: u64) -> Detail {
        coded("pools_back", &[("s", Value::from(off_s))])
    }

    /// A saved pool was made the main pool.
    pub fn pool_saved(pool: &str) -> Detail {
        coded("pool_saved", &[("p", name(pool))])
    }

    /// A restart, with the reset reason (`power-on`, `software`, `panic`,
    /// `int-wdt`, …) and, after a crash, the stored core dump's tag and
    /// whether it is this crash's (`current`) or an older one.
    ///
    /// Stored as the code plus the English text, no values: an older
    /// image that rewrites the ring keeps `d` (and drops `c`), and a later
    /// boot finds the dump's tag in `d` either way (diag::check_core_dump
    /// looks for it there). The reason is the text's first word, the tag
    /// the `dump:…` in it; the text is never cut.
    pub fn boot(reason: &str, dump: Option<(&str, bool)>) -> Detail {
        let (c, d) = match dump {
            None => ("boot", reason.to_string()),
            Some((tag, true)) => ("boot_dump", format!("{reason} {tag}")),
            Some((tag, false)) => ("boot_old_dump", format!("{reason} no dump ({tag} old)")),
        };
        Detail {
            c,
            p: Map::new(),
            d,
        }
    }

    /// The last update did not boot: back on the previous image.
    pub fn rollback() -> Detail {
        coded("rollback", &[])
    }

    /// The restart the image `from` began (for `why`: "update",
    /// "upload", "restart", "download mode") ended in a crash at
    /// `step` (`crate::restart`): the next boot found the marker and a
    /// crash reset.
    pub fn restart_crash(why: &str, from: &str, step: crate::restart::Step) -> Detail {
        let c = match step {
            crate::restart::Step::Chips => "rs_crash_chips",
            crate::restart::Step::Reset => "rs_crash_reset",
        };
        coded(c, &[("w", name(why)), ("v", name(from))])
    }

    /// An online update installed: `record` is the install record
    /// (`<tag> <signature>`, the signature `verified` or `missing`), or
    /// empty after an install without one, then `version` names it.
    pub fn installed(version: &str, record: &str) -> Detail {
        match record.split_once(' ') {
            Some((tag, sig)) if !tag.is_empty() && !sig.is_empty() => {
                coded("install_rec", &[("v", name(tag)), ("s", name(sig))])
            }
            _ if record.is_empty() => coded("installed", &[("v", name(version))]),
            _ => Detail::from(record),
        }
    }

    /// The board passed its first start with `chips` chips, `expected`
    /// in its profile.
    pub fn verified(board: &str, chips: u8, expected: u8) -> Detail {
        if chips < expected {
            coded(
                "verified_short",
                &[
                    ("m", name(board)),
                    ("n", Value::from(chips)),
                    ("e", Value::from(expected)),
                ],
            )
        } else {
            coded("verified", &[("m", name(board)), ("n", Value::from(chips))])
        }
    }

    /// Refused at the first start: `found` chips answered, the board
    /// has `expected` (its model, for the text, from its profile).
    pub fn refused_count(board: &str, found: u8, expected: u8) -> Detail {
        coded(
            "mm_count",
            &[
                ("m", name(board)),
                ("f", Value::from(found)),
                ("e", Value::from(expected)),
            ],
        )
    }

    /// Refused at the first start: a chip answered with another ID.
    pub fn refused_chip(board: &str, chip_id: u16, chip: &str) -> Detail {
        coded(
            "mm_chip",
            &[
                ("m", name(board)),
                ("id", Value::from(format!("{chip_id:#06x}"))),
                ("c", Value::from(chip)),
            ],
        )
    }

    /// Refused at the first start: no chip answered.
    pub fn refused_none(board: &str) -> Detail {
        coded("mm_none", &[("m", name(board))])
    }

    /// Refused before power: the bus says otherwise.
    pub fn refused(board: &str, r: warpaxe_board::choice::Reason) -> Detail {
        use warpaxe_board::choice::Reason;
        let m = name(board);
        match r {
            Reason::Missing(p) => coded("r_missing", &[("m", m), ("p", Value::from(p.name()))]),
            Reason::Unexpected(p) => {
                coded("r_unexpected", &[("m", m), ("p", Value::from(p.name()))])
            }
            Reason::InputVoltage { read_dv, nominal_v } => coded(
                "r_vin",
                &[
                    ("m", m),
                    ("v", Value::from(f64::from(read_dv) / 10.0)),
                    ("n", Value::from(nominal_v)),
                ],
            ),
            Reason::PartsAnswered => coded("r_parts", &[("m", m)]),
            Reason::NoThermistor => coded("r_ntc", &[("m", m)]),
        }
    }

    /// The board fits the bus: its first start, at `mv` (0: the
    /// profile's own voltage).
    pub fn first_start(board: &str, mv: u16) -> Detail {
        if mv > 0 {
            coded("first_mv", &[("m", name(board)), ("mv", Value::from(mv))])
        } else {
            coded("first", &[("m", name(board))])
        }
    }

    /// A board chosen in the web UI (`reverify`: the verified one, to be
    /// checked again).
    pub fn chosen(board: &str, reverify: bool) -> Detail {
        coded(
            if reverify { "reverify" } else { "by_hand" },
            &[("m", name(board))],
        )
    }

    /// No chip temperature for `s` seconds: chips off.
    pub fn blind(s: u32) -> Detail {
        coded("blind", &[("s", Value::from(s))])
    }

    /// An overheat stop: chip and regulator temperature (`None`: no
    /// reading) and their limits.
    pub fn hot(chip: Option<f32>, chip_limit: f32, vr: Option<i32>, vr_limit: f32) -> Detail {
        coded(
            "hot",
            &[
                ("t", chip.map_or(Value::Null, |t| fig(t, 0))),
                ("l", fig(chip_limit, 0)),
                ("v", Value::from(vr)),
                ("vl", fig(vr_limit, 0)),
            ],
        )
    }

    /// A regulator fault, the conditions by their codes
    /// ([`Condition::code`]).
    pub fn power(codes: &[&str]) -> Detail {
        coded("power", &[("f", Value::from(codes))])
    }

    /// A self-test's verdict: the checks judged when it passed, else the
    /// failed ones (at most [`MAX_FAILS`], with their count).
    pub fn selftest(r: &Report) -> Detail {
        if r.passed() {
            return coded("st_pass", &[("n", Value::from(r.judged()))]);
        }
        let fails: Vec<Value> = r
            .outcomes
            .iter()
            .filter(|o| o.status == Status::Fail)
            .take(MAX_FAILS)
            .map(|o| {
                // [check, value, chip, domain, note], trailing blanks
                // left out; the value to the precision the text shows.
                let value = match (o.check, o.value) {
                    (Check::InputVoltage, Some(v)) => fig(v, 2),
                    (_, Some(v)) => fig(v, 0),
                    _ => Value::Null,
                };
                let mut a = vec![
                    json!(o.check.id()),
                    value,
                    o.chip.map_or(Value::Null, |c| json!(c)),
                    o.domain.map_or(Value::Null, |d| json!(d)),
                    json!(o.note.id()),
                ];
                while a.len() > 1
                    && a.last()
                        .is_some_and(|v| v.is_null() || v.as_str() == Some(""))
                {
                    a.pop();
                }
                Value::Array(a)
            })
            .collect();
        coded(
            "st_fail",
            &[("n", Value::from(r.failed())), ("f", Value::from(fails))],
        )
    }
}

/// A value as the English text shows it.
fn show(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => "–".into(),
    }
}

/// One failed check (`[check, value, chip, domain, note]`) as the
/// English text shows it: `hashrate chip 3 62 % (noReply)`.
fn english_fail(a: &[Value]) -> String {
    let id = a.first().and_then(Value::as_str).unwrap_or_default();
    let mut s = id.to_string();
    if let Some(c) = a.get(2).filter(|v| !v.is_null()) {
        s.push_str(&format!(" chip {}", show(Some(c))));
    }
    if let Some(d) = a.get(3).filter(|v| !v.is_null()) {
        s.push_str(&format!(" domain {}", show(Some(d))));
    }
    if let Some(v) = a.get(1).filter(|v| !v.is_null()) {
        // A number printed at the check's precision; a string (`-0`,
        // `NaN`) as it was printed.
        let num = |d: usize| match v.as_f64() {
            Some(f) => format!("{f:.d$}"),
            None => show(Some(v)),
        };
        let check = [
            Check::Hashrate,
            Check::Domains,
            Check::Errors,
            Check::Fan,
            Check::CoreVoltage,
            Check::InputVoltage,
            Check::Temperature,
            Check::Chips,
        ]
        .into_iter()
        .find(|c| c.id() == id);
        match check {
            Some(Check::Hashrate | Check::Domains | Check::Errors) => {
                s.push_str(&format!(" {} %", num(0)))
            }
            Some(Check::Fan) => s.push_str(&format!(" {} rpm", num(0))),
            Some(Check::CoreVoltage) => s.push_str(&format!(" {} mV", num(0))),
            Some(Check::InputVoltage) => s.push_str(&format!(" {} V", num(2))),
            Some(Check::Temperature) => s.push_str(&format!(" {} °C", num(0))),
            Some(Check::Chips) => s.push_str(&format!(" {} found", num(0))),
            _ => {}
        }
    }
    if let Some(n) = a.get(4).and_then(Value::as_str).filter(|n| !n.is_empty()) {
        s.push_str(&format!(" ({n})"));
    }
    s
}

/// The English text of a coded detail; `None` for a code this image
/// does not know (written by a newer one).
pub fn english(c: &str, p: &Map<String, Value>) -> Option<String> {
    let g = |k: &str| show(p.get(k));
    Some(match c {
        "chip_silent" => format!("chip {} silent", g("c")),
        "chip_restart" => format!(
            "chip {} silent for 10 min at {} mV, chain restarted",
            g("c"),
            g("mv")
        ),
        "chip_given_up" => format!(
            "chip {} still silent after 3 restarts, no more until a new clock, a new voltage or the next boot",
            g("c")
        ),
        "domain_latch" => format!(
            "chip {} domain {} at {} % of the chip's other domains for {} min, chips switched off and on",
            g("c"),
            g("d"),
            g("pct"),
            // Ten minutes unless the event says otherwise (the early
            // look after a chain start: three).
            p.get("min").map_or_else(|| "10".into(), |m| show(Some(m)))
        ),
        "domain_given_up" => format!(
            "chip {} domain {} still stuck after 3 resets, no more until a new clock, a new voltage or the next boot",
            g("c"),
            g("d")
        ),
        "chain_restart" => format!(
            "all {} chips silent for 60 s at {} mV, chain restarted",
            g("n"),
            g("mv")
        ),
        "block_found" => format!("block {} found ({})", g("h"), g("coin")),
        "block_confirmed" => format!("block {} confirmed: the pool's next work builds on it", g("h")),
        "block_unknown" => format!(
            "block {}: status unknown, the miner was offline in between and it can no longer be checked",
            g("h")
        ),
        "block_orphaned" => format!(
            "block {} orphaned: the work for the next height builds on another block",
            g("h")
        ),
        "pool_fallback" => format!("main pool failed {} times, on the fallback", g("n")),
        "pool_back" => "main pool answers again, back from the fallback".into(),
        "pools_out" => "no pool answers, chips off until one does".into(),
        "pools_back" => format!("a pool answers again after {} s, chips back on", g("s")),
        "pool_saved" => format!("saved pool {} used", g("p")),
        // The text is stored with the code (detail::boot).
        "boot" | "boot_dump" | "boot_old_dump" => return None,
        "rollback" => "the last update did not boot: back on the previous image".into(),
        "installed" => format!("{} installed", g("v")),
        "install_rec" => format!("{} {}", g("v"), g("s")),
        "rs_crash_chips" => format!(
            "{} crashed while restarting ({}), switching the chips off",
            g("v"),
            g("w")
        ),
        "rs_crash_reset" => format!(
            "{} crashed while restarting ({}), in the system reset",
            g("v"),
            g("w")
        ),
        "verified" => format!("{} verified, {} chips", g("m"), g("n")),
        "verified_short" => format!("{} verified, {} of {} chips", g("m"), g("n"), g("e")),
        "mm_count" => {
            let m = g("m");
            let model = Profile::by_name(&m).map_or(m.as_str(), |p| p.model);
            format!(
                "{m} refused: {} chip(s) answered, a {model} has {}",
                g("f"),
                g("e")
            )
        }
        "mm_chip" => format!(
            "{} refused: a chip answered as {}, not a {}",
            g("m"),
            g("id"),
            g("c")
        ),
        "mm_none" => format!("{} refused: no chip answered in three attempts", g("m")),
        "r_missing" => format!("{} refused: the {} did not answer", g("m"), g("p")),
        "r_unexpected" => format!(
            "{} refused: a {} answered, which it does not have",
            g("m"),
            g("p")
        ),
        "r_vin" => format!(
            "{} refused: the regulator reads {:.1} V in, the board runs on {} V",
            g("m"),
            p.get("v").and_then(Value::as_f64).unwrap_or(0.0),
            g("n")
        ),
        "r_parts" => format!(
            "{} refused: parts answered on the I²C bus, which it has none of",
            g("m")
        ),
        "r_ntc" => format!("{} refused: no thermistor divider on GPIO2", g("m")),
        "first" => format!("{} first start", g("m")),
        "first_mv" => format!("{} first start at {} mV", g("m"), g("mv")),
        "reverify" => format!("{} to be verified again", g("m")),
        "by_hand" => format!("{} chosen by hand", g("m")),
        "blind" => format!("no chip temperature for {} s, chips off", g("s")),
        "hot" => format!(
            "chip {} °C (limit {}), regulator {} °C (limit {}), chips off",
            g("t"),
            g("l"),
            g("v"),
            g("vl")
        ),
        "power" => {
            let what: Vec<&str> = p
                .get("f")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|c| match Condition::from_code(c) {
                            Some(cond) => cond.describe(),
                            None => c,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let more = p
                .get("n")
                .and_then(Value::as_u64)
                .is_some_and(|n| n as usize > what.len());
            format!(
                "regulator fault: {}{}",
                what.join("; "),
                if more { "; …" } else { "" }
            )
        }
        "st_pass" => format!("PASS, {} checks", g("n")),
        "st_fail" => {
            let fails: Vec<String> = p
                .get("f")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_array)
                        .map(|a| english_fail(a))
                        .collect()
                })
                .unwrap_or_default();
            let more = p
                .get("n")
                .and_then(Value::as_u64)
                .is_some_and(|n| n as usize > fails.len());
            format!(
                "FAIL: {}{}",
                fails.join(", "),
                if more { ", …" } else { "" }
            )
        }
        _ => return None,
    })
}

/// The ring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ring {
    events: Vec<Event>,
}

impl Ring {
    /// Empty.
    pub fn new() -> Self {
        Ring::default()
    }

    /// From the stored string; anything unreadable (damaged, written by
    /// another version) is an empty ring, not an error, so one bad entry
    /// never blocks the log.
    pub fn from_json(s: &str) -> Self {
        let s = s.trim_end_matches('\0').trim();
        if s.is_empty() {
            return Ring::new();
        }
        let mut events: Vec<Event> = serde_json::from_str(s).unwrap_or_default();
        if events.len() > CAPACITY {
            events.drain(..events.len() - CAPACITY);
        }
        Ring { events }
    }

    /// The events, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The last event, if any.
    pub fn last(&self) -> Option<&Event> {
        self.events.last()
    }

    /// Add an event; `false` when it was coalesced with the previous one
    /// (nothing to store then). A plain detail is cut to [`MAX_DETAIL`],
    /// a coded one trimmed towards [`MAX_EVENT_BYTES`].
    pub fn push(&mut self, mut e: Event) -> bool {
        if e.c.is_empty() {
            if e.d.chars().count() > MAX_DETAIL {
                e.d = cut(&e.d, MAX_DETAIL);
            }
        } else {
            // A coded event's text (a restart's, with the dump tag) is
            // never cut.
            e.fit();
        }
        if let Some(last) = self.events.last() {
            if last.k == e.k
                && last.d == e.d
                && last.c == e.c
                && last.p == e.p
                && e.u.saturating_sub(last.u) < COALESCE_S
            {
                return false;
            }
        }
        self.events.push(e);
        while self.events.len() > CAPACITY {
            self.events.remove(0);
        }
        true
    }

    /// The string to store: the events as JSON, the oldest dropped until
    /// it is at most [`MAX_BYTES`].
    pub fn to_json(&self) -> String {
        let mut from = 0;
        loop {
            let s = serde_json::to_string(&self.events[from..]).unwrap_or_else(|_| "[]".into());
            if s.len() <= MAX_BYTES || from >= self.events.len() {
                return s;
            }
            from += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(u: u64, k: &str, d: &str) -> Event {
        Event::new(1_760_000_000 + u, u, k, Detail::from(d))
    }

    #[test]
    fn events_round_trip_through_the_stored_string_oldest_first() {
        let mut r = Ring::new();
        assert!(r.push(ev(5, "boot", "power-on")));
        assert!(r.push(ev(900, "overheat", "chip 76 °C")));
        let s = r.to_json();
        assert!(
            s.starts_with(r#"[{"t":1760000005,"u":5,"k":"boot","d":"power-on"}"#),
            "{s}"
        );
        let back = Ring::from_json(&s);
        assert_eq!(back, r);
        assert_eq!(back.last().map(|e| e.k.as_str()), Some("overheat"));
    }

    #[test]
    fn the_same_event_within_a_minute_is_one_event_and_no_write() {
        let mut r = Ring::new();
        assert!(r.push(ev(100, "chain", "chip 3 silent")));
        assert!(!r.push(ev(130, "chain", "chip 3 silent")));
        assert!(!r.push(ev(159, "chain", "chip 3 silent")));
        assert_eq!(r.events().len(), 1);
        // A different detail, or a minute later: a new event.
        assert!(r.push(ev(160, "chain", "chip 4 silent")));
        assert!(r.push(ev(220, "chain", "chip 4 silent")));
        assert_eq!(r.events().len(), 3);
    }

    #[test]
    fn the_ring_keeps_the_newest_and_fits_the_nvs_entry() {
        let mut r = Ring::new();
        for i in 0..100u64 {
            r.push(ev(i * 100, "pool", &format!("fallback {i}")));
        }
        assert_eq!(r.events().len(), CAPACITY);
        assert_eq!(r.events()[0].d, format!("fallback {}", 100 - CAPACITY));
        let s = r.to_json();
        assert!(s.len() <= MAX_BYTES, "{}", s.len());
        // Long details are cut, and a ring of them still fits.
        let mut r = Ring::new();
        for i in 0..CAPACITY as u64 {
            r.push(ev(i * 100, "selftest", &"x".repeat(200)));
        }
        assert_eq!(r.last().unwrap().d.chars().count(), MAX_DETAIL);
        assert!(r.to_json().len() <= MAX_BYTES);
        assert_eq!(
            Ring::from_json(&r.to_json()).events().len(),
            r.events().len()
        );
        // Details at the cap plus ASCII-only kinds: a full ring of the
        // longest events still stores whole.
        assert!(r.to_json().len() <= MAX_BYTES);
    }

    #[test]
    fn a_crash_in_a_restart_says_what_was_restarted_and_how_far_it_got() {
        use crate::restart::Step;
        let d = detail::restart_crash("update", "warpaxe 0.1.34", Step::Chips);
        assert_eq!(d.code(), "rs_crash_chips");
        assert_eq!(
            d.text(),
            "warpaxe 0.1.34 crashed while restarting (update), switching the chips off"
        );
        let d = detail::restart_crash("download mode", "warpaxe 0.1.36", Step::Reset);
        assert_eq!(
            d.text(),
            "warpaxe 0.1.36 crashed while restarting (download mode), in the system reset"
        );
        // Stored and read back, the text is the same.
        let mut r = Ring::new();
        r.push(Event::new(1, 1, "boot", d.clone()));
        let back = Ring::from_json(&r.to_json());
        assert_eq!(back.last().map(|e| e.text()), Some(d.text()));
    }

    fn st(check: Check, status: Status, value: Option<f32>) -> warpaxe_board::selftest::Outcome {
        warpaxe_board::selftest::Outcome {
            check,
            status,
            value,
            limit: None,
            value2: None,
            limit2: None,
            chip: None,
            domain: None,
            note: warpaxe_board::selftest::Note::None,
        }
    }

    /// A failed check: which, its value, chip, domain and note.
    type Fail = (
        Check,
        Option<f32>,
        Option<u8>,
        Option<u8>,
        warpaxe_board::selftest::Note,
    );

    fn report(fails: &[Fail]) -> Report {
        let outcomes =
            warpaxe_board::selftest::CHECKS.map(|c| match fails.iter().find(|f| f.0 == c) {
                Some(&(_, v, chip, domain, note)) => warpaxe_board::selftest::Outcome {
                    chip,
                    domain,
                    note,
                    ..st(c, Status::Fail, v)
                },
                None if c == Check::Regulator => st(c, Status::Skip, None),
                None => st(c, Status::Pass, Some(1.0)),
            });
        Report { outcomes }
    }

    #[test]
    fn every_coded_detail_reads_in_english_as_the_text_it_replaces() {
        use warpaxe_board::choice::{Part, Reason};
        use warpaxe_board::selftest::Note;
        let cases: Vec<(Detail, &str)> =
            vec![
            (detail::chip_silent(3), "chip 3 silent"),
            (detail::block_found(54110, "FIX"), "block 54110 found (FIX)"),
            (
                detail::block_confirmed(54110),
                "block 54110 confirmed: the pool's next work builds on it",
            ),
            (
                detail::block_unknown(54110),
                "block 54110: status unknown, the miner was offline in between and it can no longer be checked",
            ),
            (
                detail::block_orphaned(54110),
                "block 54110 orphaned: the work for the next height builds on another block",
            ),
            (
                detail::chain_restart(4, 1150),
                "all 4 chips silent for 60 s at 1150 mV, chain restarted",
            ),
            (
                detail::chip_restart(4, 1210),
                "chip 4 silent for 10 min at 1210 mV, chain restarted",
            ),
            (
                detail::chip_given_up(4),
                "chip 4 still silent after 3 restarts, no more until a new clock, a new voltage or the next boot",
            ),
            (
                detail::domain_latch_early(4, 2, 0),
                "chip 4 domain 2 at 0 % of the chip's other domains for 3 min, chips switched off and on",
            ),
            (
                detail::domain_latch(4, 2, 3),
                "chip 4 domain 2 at 3 % of the chip's other domains for 10 min, chips switched off and on",
            ),
            (
                detail::domain_given_up(4, 2),
                "chip 4 domain 2 still stuck after 3 resets, no more until a new clock, a new voltage or the next boot",
            ),
            (detail::pool_fallback(3), "main pool failed 3 times, on the fallback"),
            (detail::pool_back(), "main pool answers again, back from the fallback"),
            (detail::pools_out(), "no pool answers, chips off until one does"),
            (
                detail::pools_back(125),
                "a pool answers again after 125 s, chips back on",
            ),
            (detail::pool_saved("Braiins"), "saved pool Braiins used"),
            (detail::boot("power-on", None), "power-on"),
            (
                detail::boot("panic", Some(("dump:3f2a9c1b0@4037a1b2", true))),
                "panic dump:3f2a9c1b0@4037a1b2",
            ),
            (
                detail::boot("int-wdt", Some(("dump:3f2a9c1b0@4037a1b2", false))),
                "int-wdt no dump (dump:3f2a9c1b0@4037a1b2 old)",
            ),
            (
                detail::rollback(),
                "the last update did not boot: back on the previous image",
            ),
            (detail::installed("v0.1.35", ""), "v0.1.35 installed"),
            (detail::installed("v0.1.35", "v0.1.35 verified"), "v0.1.35 verified"),
            (detail::installed("v0.1.35", "odd"), "odd"),
            (detail::verified("nerdaxe-gaia", 1, 1), "nerdaxe-gaia verified, 1 chips"),
            (
                detail::verified("nerdqaxe++", 3, 4),
                "nerdqaxe++ verified, 3 of 4 chips",
            ),
            (
                detail::refused_count("nerdqaxe-plusplus", 2, 4),
                "nerdqaxe-plusplus refused: 2 chip(s) answered, a NerdQAxe++ has 4",
            ),
            (
                detail::refused_chip("gamma", 0x1368, "BM1370"),
                "gamma refused: a chip answered as 0x1368, not a BM1370",
            ),
            (
                detail::refused_none("gamma"),
                "gamma refused: no chip answered in three attempts",
            ),
            (
                detail::refused("gamma", Reason::Missing(Part::Tps546)),
                "gamma refused: the TPS546 did not answer",
            ),
            (
                detail::refused("gamma", Reason::Unexpected(Part::Emc2302)),
                "gamma refused: a EMC2302 answered, which it does not have",
            ),
            (
                detail::refused("gamma", Reason::InputVoltage { read_dv: 51, nominal_v: 12 }),
                "gamma refused: the regulator reads 5.1 V in, the board runs on 12 V",
            ),
            (
                detail::refused("gamma", Reason::InputVoltage { read_dv: 120, nominal_v: 5 }),
                "gamma refused: the regulator reads 12.0 V in, the board runs on 5 V",
            ),
            (
                detail::refused("nano", Reason::PartsAnswered),
                "nano refused: parts answered on the I²C bus, which it has none of",
            ),
            (
                detail::refused("nano", Reason::NoThermistor),
                "nano refused: no thermistor divider on GPIO2",
            ),
            (detail::first_start("nerdaxe-gaia", 1000), "nerdaxe-gaia first start at 1000 mV"),
            (detail::first_start("devkit", 0), "devkit first start"),
            (detail::chosen("gamma", true), "gamma to be verified again"),
            (detail::chosen("gamma", false), "gamma chosen by hand"),
            (detail::blind(30), "no chip temperature for 30 s, chips off"),
            (
                detail::hot(Some(76.4), 75.0, Some(60), 105.0),
                "chip 76 °C (limit 75), regulator 60 °C (limit 105), chips off",
            ),
            (
                detail::hot(None, 75.0, None, 105.0),
                "chip – °C (limit 75), regulator – °C (limit 105), chips off",
            ),
            (
                detail::power(&["vout_ov", "temp"]),
                "regulator fault: An output overvoltage fault has occurred; Overtemperature fault",
            ),
            (detail::selftest(&report(&[])), "PASS, 8 checks"),
            (
                detail::selftest(&report(&[
                    (Check::Hashrate, Some(62.4), Some(3), None, Note::None),
                    (Check::InputVoltage, Some(4.85), None, None, Note::None),
                ])),
                "FAIL: hashrate chip 3 62 %, inputVoltage 4.85 V",
            ),
            (
                detail::selftest(&report(&[
                    (Check::Chips, Some(3.0), None, None, Note::None),
                    (Check::Domains, Some(40.0), Some(2), Some(1), Note::NoReply),
                    (Check::Fan, None, None, None, Note::NoReading),
                    (Check::Temperature, Some(91.0), None, None, Note::Overheat),
                ])),
                "FAIL: chips 3 found, domains chip 2 domain 1 40 % (noReply), …",
            ),
        ];
        for (i, (d, want)) in cases.into_iter().enumerate() {
            assert_eq!(d.text(), want, "case {i}");
            // Stored and read back, the event says the same.
            let mut r = Ring::new();
            r.push(Event::new(1, i as u64 * 100, "x", d.clone()));
            let back = Ring::from_json(&r.to_json());
            assert_eq!(back.events()[0].text(), want, "case {i} stored");
            assert_eq!(back.events()[0].for_api().d, want, "case {i} api");
        }
    }

    #[test]
    fn a_coded_event_stores_no_text_and_an_older_image_still_reads_it() {
        let mut r = Ring::new();
        r.push(ev(5, "boot", "power-on"));
        r.push(Event::new(9, 9, "chain", detail::chip_silent(3)));
        let s = r.to_json();
        assert!(
            s.ends_with(r#"{"t":9,"u":9,"k":"chain","d":"","c":"chip_silent","p":{"c":3}}]"#),
            "{s}"
        );
        // An image from before the codes: the four fields only.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Old {
            t: u64,
            u: u64,
            k: String,
            d: String,
        }
        let old: Vec<Old> = serde_json::from_str(&s).unwrap();
        assert_eq!(old.len(), 2);
        assert_eq!(old[0].d, "power-on");
        // An event stored before the codes reads as it was.
        let back = Ring::from_json(&s);
        assert_eq!(back.events()[0].c, "");
        assert_eq!(back.events()[0].text(), "power-on");
        let api = serde_json::to_value(back.events()[0].for_api()).unwrap();
        assert_eq!(
            api,
            json!({"t": 1_760_000_005u64, "u": 5, "k": "boot", "d": "power-on"})
        );
        let api = serde_json::to_value(back.events()[1].for_api()).unwrap();
        assert_eq!(
            api,
            json!({"t": 9, "u": 9, "k": "chain", "d": "chip 3 silent", "c": "chip_silent", "p": {"c": 3}})
        );
        // A code from a newer image: its stored text, else the code.
        let e = Event {
            c: "later".into(),
            ..ev(1, "boot", "")
        };
        assert_eq!(e.text(), "later");
    }

    #[test]
    fn coded_repeats_coalesce_and_other_values_do_not() {
        let mut r = Ring::new();
        assert!(r.push(Event::new(0, 100, "chain", detail::chip_silent(3))));
        assert!(!r.push(Event::new(0, 130, "chain", detail::chip_silent(3))));
        assert!(r.push(Event::new(0, 131, "chain", detail::chip_silent(4))));
        assert_eq!(r.events().len(), 2);
        // The same domain again within the minute: one event; another
        // domain of the same chip is another event.
        assert!(r.push(Event::new(0, 200, "chain", detail::domain_latch(4, 2, 3))));
        assert!(!r.push(Event::new(0, 230, "chain", detail::domain_latch(4, 2, 3))));
        assert!(r.push(Event::new(0, 231, "chain", detail::domain_latch(4, 3, 3))));
        assert_eq!(r.events().len(), 4);
    }

    #[test]
    fn a_ring_of_the_longest_coded_events_fits_the_nvs_entry() {
        use warpaxe_board::choice::{Part, Reason};
        use warpaxe_board::selftest::Note;
        // The longest board name, a pool name past the cut.
        let (b, pool) = ("nerdqaxe-plusplus-rev7", "x".repeat(80));
        let worst: Vec<(&str, Detail)> = vec![
            (
                "overheat",
                detail::hot(Some(105.0), 105.0, Some(125), 145.0),
            ),
            ("board", detail::refused_count(b, 12, 12)),
            (
                "board",
                detail::refused(b, Reason::Unexpected(Part::Tps53667)),
            ),
            (
                "board",
                detail::refused(
                    b,
                    Reason::InputVoltage {
                        read_dv: 255,
                        nominal_v: 12,
                    },
                ),
            ),
            ("board", detail::verified(b, 11, 12)),
            ("board", detail::chosen(b, true)),
            ("pool", detail::pool_saved(&pool)),
            (
                "boot",
                detail::boot("task-wdt", Some(("dump:3f2a9c1b0@4037a1b2", false))),
            ),
            (
                "power",
                detail::power(&["off", "vout_ov", "iout_oc", "vin_uv", "temp", "temp_warn"]),
            ),
            ("block", detail::block_confirmed(99_999_999)),
            ("block", detail::block_orphaned(99_999_999)),
            ("block", detail::block_unknown(99_999_999)),
            ("pool", detail::pools_back(99_999_999)),
            ("chain", detail::chain_restart(12, 1350)),
            ("chain", detail::chip_restart(7, 1210)),
            ("chain", detail::chip_given_up(7)),
            ("chain", detail::domain_latch(15, 7, 24)),
            ("chain", detail::domain_latch_early(15, 7, 4)),
            ("chain", detail::domain_given_up(15, 7)),
            (
                "update",
                detail::installed("v0.1.35", "v0.1.35-rc.12 verified"),
            ),
            (
                "selftest",
                detail::selftest(&report(&[
                    (Check::Hashrate, Some(62.4), Some(11), None, Note::NoReply),
                    (Check::Domains, Some(40.0), Some(11), Some(3), Note::NoReply),
                    (Check::InputVoltage, Some(4.85), None, None, Note::NoReading),
                ])),
            ),
        ];
        // Each at most MAX_EVENT_BYTES, uptimes of months included.
        for (k, d) in &worst {
            let mut r = Ring::new();
            r.push(Event::new(1_790_000_000, 99_999_999, k, d.clone()));
            let n = r.to_json().len() - 2;
            assert!(n <= MAX_EVENT_BYTES, "{k} {}: {n}", d.code());
        }
        // A ring of them all, in turn: most of it kept.
        let mut r = Ring::new();
        for i in 0..CAPACITY {
            let (k, d) = &worst[i % worst.len()];
            r.push(Event::new(
                1_790_000_000 + i as u64,
                10_000_000 + i as u64 * 100,
                k,
                d.clone(),
            ));
        }
        let s = r.to_json();
        assert!(s.len() <= MAX_BYTES, "{}", s.len());
        assert!(Ring::from_json(&s).events().len() >= 26);
        // Everyday events, a month up: all kept.
        let everyday = [
            ("boot", detail::boot("power-on", None)),
            ("chain", detail::chip_silent(11)),
            ("pool", detail::pool_fallback(3)),
            ("pool", detail::pool_back()),
            ("board", detail::first_start(b, 1150)),
            ("board", detail::verified(b, 4, 4)),
            ("selftest", detail::selftest(&report(&[]))),
            ("overheat", detail::hot(Some(76.0), 75.0, Some(60), 105.0)),
            ("update", detail::installed("v0.1.35", "v0.1.35 verified")),
        ];
        let mut r = Ring::new();
        for i in 0..CAPACITY {
            let (k, d) = &everyday[i % everyday.len()];
            r.push(Event::new(
                1_790_000_000 + i as u64,
                2_600_000 + i as u64 * 100,
                k,
                d.clone(),
            ));
        }
        let s = r.to_json();
        assert!(s.len() <= MAX_BYTES, "{}", s.len());
        assert_eq!(Ring::from_json(&s).events().len(), CAPACITY);
        // A ring of only the longest kind: still most of it.
        for (k, d) in worst {
            let mut r = Ring::new();
            for i in 0..CAPACITY as u64 {
                r.push(Event::new(
                    1_790_000_000 + i,
                    10_000_000 + i * 100,
                    k,
                    d.clone(),
                ));
            }
            let s = r.to_json();
            assert!(s.len() <= MAX_BYTES, "{k} {}: {}", d.code(), s.len());
            let back = Ring::from_json(&s);
            assert!(
                back.events().len() >= 26,
                "{k} {}: {}",
                d.code(),
                back.events().len()
            );
            // Trimmed, it still reads.
            assert!(!back.events()[0].text().is_empty());
        }
    }

    #[test]
    fn a_restart_keeps_its_dump_tag_in_d_through_an_older_image() {
        let tag = "dump:3f2a9c1b0@4037a1b2";
        let mut r = Ring::new();
        r.push(Event::new(
            1_790_000_000,
            5,
            "boot",
            detail::boot("panic", Some((tag, true))),
        ));
        r.push(Event::new(
            1_790_000_100,
            5,
            "boot",
            detail::boot("task-wdt", Some((tag, false))),
        ));
        r.push(Event::new(
            1_790_000_200,
            6,
            "boot",
            detail::boot("deep-sleep", Some((tag, false))),
        ));
        // As this image reads it (main.rs hands the boot events' `d` to
        // diag::check_core_dump): the tag whole, the code beside it.
        let found = |ring: &Ring| {
            ring.events()
                .iter()
                .filter(|e| e.k == "boot")
                .map(|e| e.for_api().d)
                .filter(|d| d.contains(tag))
                .count()
        };
        assert_eq!(found(&r), 3);
        assert_eq!(r.events()[2].c, "boot_old_dump");
        assert_eq!(r.events()[2].d, format!("deep-sleep no dump ({tag} old)"));
        // An older image reads the ring with four fields and writes it
        // back so (a rollback); this image reads that again.
        #[derive(Serialize, Deserialize)]
        struct Old {
            t: u64,
            u: u64,
            k: String,
            d: String,
        }
        let old: Vec<Old> = serde_json::from_str(&r.to_json()).unwrap();
        let back = Ring::from_json(&serde_json::to_string(&old).unwrap());
        assert_eq!(back.events()[0].c, "");
        assert_eq!(found(&back), 3);
        assert_eq!(
            back.events()[1].text(),
            format!("task-wdt no dump ({tag} old)")
        );
    }

    #[test]
    fn figures_print_as_the_old_format_did_halves_and_minus_zero_included() {
        for t in [72.5f32, 73.5, -0.4, 0.5, 104.49, f32::NAN] {
            assert_eq!(
                detail::hot(Some(t), 75.5, Some(60), 104.5).text(),
                format!(
                    "chip {t:.0} °C (limit {:.0}), regulator 60 °C (limit {:.0}), chips off",
                    75.5f32, 104.5f32
                ),
                "{t}"
            );
        }
        for (check, v) in [
            (Check::Hashrate, 62.5f32),
            (Check::Temperature, -0.4),
            (Check::InputVoltage, 4.125),
            (Check::InputVoltage, 4.85),
            (Check::Fan, 2500.5),
        ] {
            let want = match check {
                Check::InputVoltage => format!("{} {v:.2} V", check.id()),
                Check::Hashrate => format!("{} {v:.0} %", check.id()),
                Check::Temperature => format!("{} {v:.0} °C", check.id()),
                _ => format!("{} {v:.0} rpm", check.id()),
            };
            let r = report(&[(
                check,
                Some(v),
                None,
                None,
                warpaxe_board::selftest::Note::None,
            )]);
            assert_eq!(detail::selftest(&r).text(), format!("FAIL: {want}"), "{v}");
        }
    }

    #[test]
    fn a_shortened_list_says_so() {
        let codes = ["off", "vout_ov", "iout_oc", "vin_uv", "temp", "temp_warn"].repeat(3);
        let mut r = Ring::new();
        r.push(Event::new(
            1_790_000_000,
            99_999_999,
            "power",
            detail::power(&codes),
        ));
        let e = &r.events()[0];
        assert!(e.p["f"].as_array().unwrap().len() < codes.len());
        assert_eq!(e.p["n"], json!(codes.len()));
        assert!(e.text().ends_with("; …"), "{}", e.text());
        assert!(!detail::power(&["off"]).text().ends_with('…'));
    }

    #[test]
    fn a_damaged_or_empty_string_is_an_empty_ring() {
        assert_eq!(Ring::from_json("").events().len(), 0);
        assert_eq!(Ring::from_json("\0\0").events().len(), 0);
        assert_eq!(Ring::from_json("{not json").events().len(), 0);
        assert_eq!(Ring::from_json("[{\"t\":1}]").events().len(), 0);
    }
}
