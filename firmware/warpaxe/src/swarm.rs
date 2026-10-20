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
//!
//! The network search (`POST /api/v1/swarm/cgminer/scan`, then `GET` for
//! its progress) walks the miner's own /24 for port 4028 on threads of
//! their own (stacks in PSRAM), [`SCAN_PARALLEL`] connections at a time,
//! asking a host that answers for its `version` and nothing else; a hard
//! budget ends it. Only the own private subnet, never an address the
//! browser names; one search at a time; not while an update is written
//! or the internal heap is short; the result is kept for [`SCAN_KEEP_S`]
//! so a second click within that time gets it without another walk.

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
/// The search: connections at a time (lwIP has 16 sockets in all, the
/// HTTP server holds up to five, the pool one, mDNS two), the time to
/// reach a host and for its `version`, and the budget for the whole /24
/// (253 hosts that do not answer ARP cost the full 400 ms each: 253 / 6
/// × 0.4 s ≈ 17 s; refusals come back at once). Worker stacks live in
/// PSRAM (sockets and a 4 KB reply, no flash), so a search costs the
/// internal heap only lwIP's own buffers.
const SCAN_PARALLEL: usize = 6;
const SCAN_CONNECT: Duration = Duration::from_millis(400);
const SCAN_REPLY: Duration = Duration::from_secs(1);
const SCAN_BUDGET: Duration = Duration::from_secs(30);
/// A finished search answers a new request for this long.
const SCAN_KEEP_S: u64 = 30;
/// No search below this much free internal heap: lwIP takes its
/// connection blocks and buffers from there, next to everything else.
const SCAN_MIN_INTERNAL_HEAP: u32 = 20 * 1024;
/// And no further host below this much while a search runs (checked
/// before each one; three refusals skip the host, counted apart).
const SCAN_MIN_HEAP_PER_HOST: u32 = 16 * 1024;
/// Stack of a search worker (PSRAM) and of the coordinator (PSRAM too:
/// it joins the workers and logs).
const SCAN_WORKER_STACK: usize = 10 * 1024;

/// Lock order, everywhere in this file: `SCAN` first, then the scan's
/// `progress`; never the other way round, and never `progress` held
/// while `SCAN` is taken. The coordinator thread therefore copies what
/// it needs out of `progress`, drops that guard, logs, and only then
/// takes `SCAN` to note its end (and only if the entry is still its own
/// generation: a later search may have replaced it).
struct Scan {
    generation: u32,
    progress: std::sync::Arc<Mutex<warpaxe_avalon::Progress>>,
    started: Instant,
    ended: Option<Instant>,
    subnet: String,
}

static SCAN: Mutex<Option<Scan>> = Mutex::new(None);
static SCAN_GENERATION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The search's state as JSON, for both the start and the status reply.
/// Called with `SCAN` held; takes `progress` inside (the lock order).
fn scan_json(s: &Scan) -> Value {
    let p = s.progress.lock().unwrap().clone();
    let seconds = match s.ended {
        Some(e) if p.finished => e.duration_since(s.started).as_secs(),
        _ => s.started.elapsed().as_secs(),
    };
    json!({
        "running": !p.finished, "finished": p.finished, "subnet": s.subnet,
        "done": p.done, "total": p.total,
        "skippedTime": p.skipped_time, "skippedSockets": p.skipped_sockets, "skippedMemory": p.skipped_memory,
        "skipped": p.skipped_time + p.skipped_sockets + p.skipped_memory,
        "openNoAnswer": p.open_no_answer,
        "seconds": seconds,
        "found": p.found.iter().map(|f| json!({
            "host": f.host.to_string(), "product": f.product, "model": f.model,
            "firmware": f.firmware, "api": f.api,
        })).collect::<Vec<_>>(),
    })
}

/// `GET …/scan`: where the search stands; 404 before the first one.
pub fn scan_status() -> (u16, Value) {
    match SCAN.lock().unwrap().as_ref() {
        Some(s) => (200, scan_json(s)),
        None => (404, json!({"error": "no search yet"})),
    }
}

/// An address of the private ranges (RFC 1918): the only networks the
/// search walks. The netmask is taken as /24.
fn is_private(ip: std::net::Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168)
}

/// `POST …/scan`: start a search of the own /24 (202 with the state), or
/// answer with the one under way (202) or the one finished within
/// [`SCAN_KEEP_S`] (200). 400 without a private Wi-Fi address (the USB
/// network is a point-to-point link with nothing to find), 503 while an
/// update is being written or the internal heap is short.
pub fn scan_start() -> (u16, Value) {
    // A search under way or just finished answers first; the guards
    // below gate a new start only.
    let mut guard = SCAN.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        let finished = s.progress.lock().unwrap().finished;
        if !finished {
            return (202, scan_json(s));
        }
        if s.ended
            .is_some_and(|e| e.elapsed() < Duration::from_secs(SCAN_KEEP_S))
        {
            return (200, scan_json(s));
        }
    }
    let Some(own) = crate::net::state()
        .wifi_ip
        .and_then(|ip| ip.parse::<std::net::Ipv4Addr>().ok())
    else {
        return (400, json!({"error": "no Wi-Fi address to search from"}));
    };
    if !is_private(own) {
        return (
            400,
            json!({"error": "the search walks private networks only (10/8, 172.16/12, 192.168/16)"}),
        );
    }
    if crate::display::OTA_STATE.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        return (503, json!({"error": "a firmware update is being written"}));
    }
    let free = crate::diag::free_heap_internal();
    if free < SCAN_MIN_INTERNAL_HEAP {
        return (
            503,
            json!({"error": format!("only {} KB of internal memory free; try again later", free / 1024)}),
        );
    }
    let generation = SCAN_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let hosts = warpaxe_avalon::subnet_hosts(own);
    let progress = std::sync::Arc::new(Mutex::new(warpaxe_avalon::Progress {
        total: hosts.len(),
        ..Default::default()
    }));
    let p2 = std::sync::Arc::clone(&progress);
    let subnet = {
        let o = own.octets();
        format!("{}.{}.{}.0/24", o[0], o[1], o[2])
    };
    log::info!(
        "swarm: cgminer search #{generation} of {subnet} ({} hosts, {} at a time, {} KB internal heap free)",
        hosts.len(),
        SCAN_PARALLEL,
        free / 1024
    );
    let spawned = crate::diag::spawn_psram("cgscan", SCAN_WORKER_STACK, move || {
        let t0 = Instant::now();
        let spawn = |f: Box<dyn FnOnce() + Send>| {
            crate::diag::spawn_psram("cgscan-w", SCAN_WORKER_STACK, f).ok()
        };
        // Before every host: enough internal heap left? Allocations under
        // 16 KB come from there even on a PSRAM stack (replies, buffers),
        // and a failed one would restart the miner.
        warpaxe_avalon::scan_with(
            hosts,
            SCAN_PARALLEL,
            SCAN_BUDGET,
            &p2,
            |h| {
                if crate::diag::free_heap_internal() < SCAN_MIN_HEAP_PER_HOST {
                    return Err(warpaxe_avalon::Error::NoMemory);
                }
                warpaxe_avalon::probe(h, warpaxe_avalon::PORT, SCAN_CONNECT, SCAN_REPLY)
            },
            &spawn,
        );
        // Copy out, drop the progress guard, then log and take SCAN.
        let summary = p2.lock().unwrap().clone();
        log::info!(
            "swarm: cgminer search #{generation} done in {} s: {} found, {} of {} tried, {} skipped for time, {} for sockets, {} for memory, {} open without an answer",
            t0.elapsed().as_secs(),
            summary.found.len(),
            summary.done,
            summary.total,
            summary.skipped_time,
            summary.skipped_sockets,
            summary.skipped_memory,
            summary.open_no_answer
        );
        for f in &summary.found {
            log::info!("swarm: cgminer at {}: {} ({})", f.host, f.product, f.api);
        }
        if let Some(s) = SCAN.lock().unwrap().as_mut() {
            if s.generation == generation {
                s.ended = Some(Instant::now());
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("swarm: cgminer search: no thread: {e}");
        return (
            503,
            json!({"error": format!("no thread for the search: {e}")}),
        );
    }
    let s = Scan {
        generation,
        progress,
        started: Instant::now(),
        ended: None,
        subnet,
    };
    let body = scan_json(&s);
    *guard = Some(s);
    (202, body)
}

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
    // The small buffers of the read (socket, reply) still come from
    // internal RAM, so the same guard as the search's.
    let free = crate::diag::free_heap_internal();
    if free < SCAN_MIN_HEAP_PER_HOST {
        log::warn!(
            "swarm: cgminer {host}: {} KB internal heap free, read later",
            free / 1024
        );
        remember(
            host,
            Err("the miner is short of memory right now, retrying".into()),
        );
        *BUSY.lock().unwrap() = None;
        return;
    }
    // The stack in PSRAM, as the search's workers: the read only opens a
    // socket and parses a few KB of JSON, never flash. 16 KB of internal
    // RAM in one piece was not to be had on a NerdQAxe++ with 33 KB free
    // (fragmented), and every read failed with ENOMEM (2026-10-20). The
    // normal path takes about 4 KB; the parser is kept off deep documents
    // (`warpaxe_avalon::MAX_DEPTH`), and 16 KB leaves room for the eight
    // levels it may see.
    let spawned = crate::diag::spawn_psram("cgminer", 16 * 1024, move || {
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
