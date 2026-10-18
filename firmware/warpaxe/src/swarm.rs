//! The swarm page's Avalon: `GET /api/v1/swarm/cgminer?host=<ip>` reads
//! a cgminer over TCP 4028 for the browser, which cannot open such a
//! socket itself, and answers with the AxeOS-like document of
//! `warpaxe_avalon::translate`.
//!
//! Guard rails, in this order:
//! - only a host that stands on the stored swarm list as `cgminer:<host>`
//!   (`config::swarm_has_cgminer`) is ever connected to;
//! - only the three read commands go over the wire (the crate has no
//!   other);
//! - one read at a time, on a short-lived thread of its own with a
//!   bounded reply, never on the HTTP task (which would stall every
//!   other request for the seconds an unreachable host costs) and never
//!   on a mining thread;
//! - the answer is kept for [`CACHE_S`] and served from there, so several
//!   open browsers ask the Avalon no more than one would. A failure is
//!   cached too, so an Avalon that is off is tried every [`CACHE_S`]
//!   seconds, not on every poll.
//!
//! The handler never waits: without a fresh entry it starts a read and
//! says `202 pending`, and the page asks again after about 700 ms. The
//! HTTP server has one task; a second spent here would hold
//! `/api/system/info`, WarpTune, WarpPool and every save with it.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::config;

/// How long an answer (or a failure) is served from the cache.
const CACHE_S: u64 = 10;
/// Connect and per-read timeout towards the Avalon; a whole reply has
/// `warpaxe_avalon::READ_BUDGET` on top.
const TIMEOUT: Duration = Duration::from_millis(2500);
/// Hosts remembered at most; the list itself allows 32 members.
const CACHE_MAX: usize = 8;

struct Entry {
    host: String,
    at: Instant,
    result: Result<Value, String>,
}

static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
/// The host a read is under way for, if any: one at a time.
static BUSY: Mutex<Option<String>> = Mutex::new(None);

/// The reply for the endpoint: `(status, body)`.
pub fn lookup(host: &str) -> (u16, Value) {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return (400, json!({"error": "host= missing"}));
    }
    // The list is the allow-list: the browser can name any address, the
    // socket opens only towards an Avalon the owner put on the list.
    if !config::swarm_has_cgminer(&host) {
        return (
            403,
            json!({"error": "host is not on the swarm list as cgminer:<host>"}),
        );
    }
    if let Some(r) = cached(&host) {
        return r;
    }
    start_read(&host);
    (202, json!({"pending": true, "host": host}))
}

fn cached(host: &str) -> Option<(u16, Value)> {
    let cache = CACHE.lock().unwrap();
    let e = cache
        .iter()
        .find(|e| e.host == host && e.at.elapsed() < Duration::from_secs(CACHE_S))?;
    Some(match &e.result {
        Ok(v) => (200, v.clone()),
        Err(why) => (502, json!({"error": why, "host": host})),
    })
}

fn remember(host: &str, result: Result<Value, String>) {
    let mut cache = CACHE.lock().unwrap();
    cache.retain(|e| e.host != host);
    while cache.len() >= CACHE_MAX {
        cache.remove(0);
    }
    cache.push(Entry {
        host: host.to_string(),
        at: Instant::now(),
        result,
    });
}

/// Start a read for `host` unless one is under way (for it or another
/// host: one socket towards the Avalons at a time).
fn start_read(host: &str) {
    {
        let mut busy = BUSY.lock().unwrap();
        if busy.is_some() {
            return;
        }
        *busy = Some(host.to_string());
    }
    let h = host.to_string();
    // Internal RAM for the stack (the default): sockets and a few KB of
    // JSON, no flash. The normal path takes about 4 KB; the parser is
    // kept off deep documents (`warpaxe_avalon::MAX_DEPTH`), and 16 KB
    // leaves room for the eight levels it may see. If no thread can be
    // had right now the request says so and the page tries again later.
    let spawned = std::thread::Builder::new()
        .name("cgminer".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let t0 = Instant::now();
            let result = warpaxe_avalon::fetch(&h, warpaxe_avalon::PORT, TIMEOUT)
                .map(|raw| warpaxe_avalon::translate(&raw))
                .map_err(|e| e.to_string());
            match &result {
                Ok(v) => log::info!(
                    "swarm: cgminer {h}: {} {} GH/s {} W in {} ms",
                    v["deviceModel"].as_str().unwrap_or("?"),
                    v["hashRate"].as_f64().map_or(0, |g| g.round() as i64),
                    v["power"].as_f64().map_or(0, |w| w.round() as i64),
                    t0.elapsed().as_millis()
                ),
                Err(e) => log::warn!("swarm: cgminer {h}: {e}"),
            }
            remember(&h, result);
            *BUSY.lock().unwrap() = None;
        });
    if let Err(e) = spawned {
        log::warn!("swarm: cgminer {host}: no thread: {e}");
        remember(host, Err(format!("no thread for the read: {e}")));
        *BUSY.lock().unwrap() = None;
    }
}
