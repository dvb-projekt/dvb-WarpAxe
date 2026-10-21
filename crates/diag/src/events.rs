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

/// One event. Short keys: the string is stored as is.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Unix seconds when it happened; 0 before NTP set the clock.
    pub t: u64,
    /// Uptime seconds when it happened.
    pub u: u64,
    /// The kind: `boot`, `overheat`, `power`, `pool`, `update`,
    /// `rollback`, `chain`, `selftest`.
    pub k: String,
    /// What exactly, one short line.
    pub d: String,
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
    /// (nothing to store then). The detail is cut to [`MAX_DETAIL`].
    pub fn push(&mut self, mut e: Event) -> bool {
        if e.d.chars().count() > MAX_DETAIL {
            e.d = e.d.chars().take(MAX_DETAIL - 1).collect::<String>() + "…";
        }
        if let Some(last) = self.events.last() {
            if last.k == e.k && last.d == e.d && e.u.saturating_sub(last.u) < COALESCE_S {
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
        Event {
            t: 1_760_000_000 + u,
            u,
            k: k.into(),
            d: d.into(),
        }
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
    fn a_damaged_or_empty_string_is_an_empty_ring() {
        assert_eq!(Ring::from_json("").events().len(), 0);
        assert_eq!(Ring::from_json("\0\0").events().len(), 0);
        assert_eq!(Ring::from_json("{not json").events().len(), 0);
        assert_eq!(Ring::from_json("[{\"t\":1}]").events().len(), 0);
    }
}
