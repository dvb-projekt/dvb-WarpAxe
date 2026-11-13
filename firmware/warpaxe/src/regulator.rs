//! WarpTune's heartbeat (`POST /api/v1/regulator`): an expiry on the
//! uptime clock, in RAM only. The rules are `warpaxe_diag::heartbeat`.
//!
//! Nothing here touches NVS or the display: a tuner renews this every
//! couple of minutes, and a `PATCH /api/system` for that would wear the
//! flash and wake the display each time (rockenowl's Gaia). No event is
//! recorded either. For now the heartbeat is only reported
//! (`externalRegulator` in `/api/system/info`); the firmware's base
//! regulation, a later block, will stand back while it is active.

use std::sync::atomic::{AtomicU32, Ordering};

use warpaxe_diag::heartbeat::{Heartbeat, Status};

use crate::diag;

/// When the heartbeat expires, uptime s; 0 = none (as after a boot).
/// 32 bits (the Xtensa has no 64-bit atomic): 136 years of uptime.
static EXPIRES_AT_S: AtomicU32 = AtomicU32::new(0);

fn load() -> Heartbeat {
    Heartbeat::from_expires_at_s(u64::from(EXPIRES_AT_S.load(Ordering::Relaxed)))
}

/// A heartbeat with the time to live the request gave (`None` for the
/// default; 0 ends it). Returns the status as the reply reports it.
pub fn beat(ttl_s: Option<u32>) -> Status {
    let now = diag::uptime_s();
    let mut h = load();
    h.beat(now, ttl_s);
    EXPIRES_AT_S.store(
        h.expires_at_s().min(u64::from(u32::MAX)) as u32,
        Ordering::Relaxed,
    );
    h.status(now)
}

/// Active, and for how long, now.
pub fn status() -> Status {
    load().status(diag::uptime_s())
}

/// The status as `/api/system/info` and the endpoint report it.
pub fn json() -> serde_json::Value {
    let s = status();
    warpaxe_jsonval::json!({"active": s.active, "expires_in_s": s.expires_in_s})
}
