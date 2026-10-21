//! The event log that survives restarts: the last few dozen things that
//! happened to the miner (restarts, overheat stops, regulator faults,
//! pool switches, updates and rollbacks, silent chips, self-test
//! results), each with the time and the uptime, as one NVS string like
//! the config patches. The ring's rules are `warpaxe_diag::events`.
//!
//! Writing goes to flash, so two rules apply: an event is written when it
//! happens and never on a timer (the ring drops a repeat within a
//! minute, and the chain module rations its own silent-chip events), and
//! the write happens on a thread whose stack sits in internal RAM only.
//! `record` can be called from anywhere: it queues the event and logs
//! it; `flush` stores what is queued and is called from the board and
//! miner loops and from the HTTP handlers, all on internal stacks. The
//! main task stores the boot event itself before any thread starts.

use std::sync::Mutex;

use warpaxe_diag::events::{Event, Ring};

use crate::{clock, config, diag};

const KEY: &str = "events";
/// Room for the stored string plus its terminator.
const MAX_READ: usize = warpaxe_diag::events::MAX_BYTES + 64;

static RING: Mutex<Option<Ring>> = Mutex::new(None);
static PENDING: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// Read the stored ring. On the main task at boot (a flash read).
pub fn init() {
    let ring = config::nvs_get_str(KEY, MAX_READ)
        .map(|s| Ring::from_json(&s))
        .unwrap_or_default();
    log::info!("events: {} stored", ring.events().len());
    *RING.lock().unwrap() = Some(ring);
}

/// Note an event: queued for the next `flush`, and in the log at once.
/// `kind` is one of boot, overheat, power, pool, update, rollback, chain,
/// selftest; `detail` one short line.
pub fn record(kind: &'static str, detail: impl Into<String>) {
    let detail = detail.into();
    log::info!("event: {kind}: {detail}");
    PENDING.lock().unwrap().push(Event {
        t: clock::unix_now().unwrap_or(0),
        u: diag::uptime_s(),
        k: kind.into(),
        d: detail,
    });
}

/// Store the queued events. From a thread with an internal stack only.
/// Nothing is written when nothing is queued or every queued event was a
/// repeat.
pub fn flush() {
    let pending: Vec<Event> = std::mem::take(&mut *PENDING.lock().unwrap());
    if pending.is_empty() {
        return;
    }
    let mut g = RING.lock().unwrap();
    let ring = g.get_or_insert_with(Ring::new);
    let mut changed = false;
    for e in pending {
        changed |= ring.push(e);
    }
    if !changed {
        return;
    }
    let json = ring.to_json();
    drop(g);
    if let Err(e) = config::nvs_set_str(KEY, &json) {
        log::warn!("events: storing the log failed: {e}");
    }
}

/// The events, oldest first: the stored ring plus what is still queued.
pub fn list() -> Vec<Event> {
    let mut v: Vec<Event> = RING
        .lock()
        .unwrap()
        .as_ref()
        .map(|r| r.events().to_vec())
        .unwrap_or_default();
    v.extend(PENDING.lock().unwrap().iter().cloned());
    v
}

/// The events for the API, newest first.
pub fn json() -> serde_json::Value {
    let mut v = list();
    v.reverse();
    serde_json::json!({ "events": v, "capacity": warpaxe_diag::events::CAPACITY })
}
