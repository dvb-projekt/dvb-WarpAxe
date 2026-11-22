//! The pool client: one thread, one connection, reconnects on its own.
//! Stratum V1 (plain or TLS) or Stratum V2 (Noise-encrypted, extended
//! channel) per pool, chosen by the config. Jobs land in [`next_job`] for
//! the chain driver in the same shape either way; everything the API
//! shows is in [`state`].

use std::collections::HashMap;
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_stratum::message::{
    authorize_request, configure_request, parse_subscribe_result, submit_request,
    subscribe_request, suggest_difficulty_request, Request,
};
use warpaxe_stratum::outage::{self, Change, PoolWatch};
use warpaxe_stratum::{coinbase, line, parse_line, payout, Incoming, Job, Prepared, Session};

use crate::link::{KeyRefused, Link, LinkReader};
use crate::{board, clock, config, diag, miner};
use warpaxe_trust::pin::Pin;
use warpaxe_sv2::{Client, ClientConfig, Event, Frame, Handshake, Template, Transport};

const AGENT: &str = concat!("warpaxe/", env!("CARGO_PKG_VERSION"));
/// The devkit toy's user agent: "nerdminer" in it puts the worker into
/// WarpPool's NerdMiner class (share difficulty floor 0.001, start 0.01),
/// the only way the pool lets a few hundred kH/s have a difficulty below
/// its profile floor (Warppool, 2026-10-05). None of the markers checked
/// first (bitaxe, nerdaxe, bm13xx ...) may appear in it.
const AGENT_CPU: &str = concat!(
    "warpaxe-cpu/",
    env!("CARGO_PKG_VERSION"),
    " (nerdminer-class)"
);

/// The firmware name and version (Sv2 `SetupConnection.firmware`, which
/// carries the board in its own field), and the base of the V1 agent.
fn agent() -> &'static str {
    if board::devkit() {
        return AGENT_CPU;
    }
    AGENT
}

/// The user agent for `mining.subscribe`: "warpaxe/0.1.42 (nerdaxe-gaia)",
/// the board's profile name in brackets as in the Sv2 hardware field, so
/// a pool can tell the boards apart. Without a settled board (first start
/// pending, nothing detected) the bare agent; the devkit keeps its own.
/// A pool that sorts by user agent (WarpPool's hardware classes) sees the
/// board's marker ("bitaxe", "nerdaxe", "nerdnos" ...) where it has one.
fn v1_agent() -> String {
    if board::devkit() {
        return agent().to_string();
    }
    match board_id() {
        "" => agent().to_string(),
        id => format!("{AGENT} ({id})"),
    }
}

/// The board as the pools see it (Sv2 hardware field, V1 agent): the
/// profile name, empty while no board is settled (first start pending,
/// nothing detected).
fn board_id() -> &'static str {
    let p = board::profile();
    if board::choice_pending() || p.name == "undetected" {
        ""
    } else {
        p.name
    }
}
const BIP320_MASK: u32 = 0x1fff_e000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct StratumState {
    pub pool: String,
    /// Connected (or trying) to the fallback pool.
    pub using_fallback: bool,
    /// The live session runs over TLS.
    pub tls: bool,
    /// The live session speaks Stratum V2 (always encrypted).
    pub sv2: bool,
    /// Height of the block the current job builds (BIP34), 0 = unknown.
    pub block_height: u64,
    /// Network difficulty of the current job (from nbits).
    pub network_difficulty: f64,
    /// Readable text in the coinbase script (the pool's tag).
    pub scriptsig: String,
    /// Who the current job pays if it finds a block: (satoshis, address)
    /// per coinbase output, their total and what goes to the worker's own
    /// address (AxeOS `coinbaseOutputs`, `coinbaseValue*Satoshis`).
    /// At most [`payout::SHOWN_OUTPUTS`] of them (own first, then the
    /// witness commitment, then the largest); the rest only counted.
    pub coinbase_outputs: Vec<(u64, String)>,
    /// Outputs left out of `coinbase_outputs`, and their sum.
    pub coinbase_outputs_hidden: u32,
    pub coinbase_outputs_hidden_sat: u64,
    pub coinbase_total_sat: u64,
    pub coinbase_user_sat: u64,
    /// The worker name starts with an address of some SHA-256 coin; if
    /// not, it is a pool account and the pool pays, not the coinbase.
    pub coinbase_own_address: bool,
    /// That address is a Bitcoin one (amounts in BTC).
    pub coinbase_bitcoin: bool,
    pub connected: bool,
    /// The interface address the current (or last) session left from:
    /// Ethernet's or Wi-Fi's. `net::eth` moves a session that runs over
    /// Wi-Fi onto the cable once the cable has settled, and ends one
    /// bound to the cable when the cable goes.
    pub local_ip: Option<String>,
    pub subscribed: bool,
    pub authorized: bool,
    /// The running TLS session's certificate was accepted by the pool's
    /// confirmed key, not by the CA bundle.
    pub tls_pinned: bool,
    /// The address the live connection went to (IPv4 or IPv6, from
    /// `net::dial`); empty while none is up.
    pub pool_ip: String,
    pub extranonce1: String,
    pub extranonce2_size: usize,
    pub difficulty: f64,
    pub version_mask: u32,
    pub jobs_received: u64,
    pub last_job_id: String,
    pub last_job_uptime_s: u64,
    pub clean_jobs: u64,
    /// Uptime second of the last notify with clean_jobs.
    pub last_clean_uptime_s: u64,
    /// Stale rejects by seconds since the last clean_jobs: <2, <5, <30, more.
    pub stale_after_clean_hist: [u32; 4],
    pub reconnects: u64,
    pub last_error: String,
    pub shares_sent: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    /// Reject reasons the pool gave, with counts (AxeOS
    /// `sharesRejectedReasons`).
    pub reject_reasons: std::collections::BTreeMap<String, u32>,
    /// Sessions ended because the pool sent a line over
    /// [`line::MAX_LINE`], since boot.
    pub lines_too_long: u32,
    pub last_reject: String,
    /// Neither the main nor (where one is set) the fallback pool
    /// answers: the chips are off until one sends work again.
    pub pools_unavailable: bool,
}

static STATE: Mutex<Option<StratumState>> = Mutex::new(None);
/// The outage rule's memory (RAM only; nothing is stored per attempt).
static WATCH: Mutex<PoolWatch> = Mutex::new(PoolWatch::new());
/// An outage's two events (out, back) are written at most once per
/// [`OUTAGE_EVENT_S`]: a link that flaps by the minute must not wear the
/// flash. When the "out" was not written, its "back" is not either.
const OUTAGE_EVENT_S: u64 = 600;
/// Uptime second of the last recorded "out" event; 0 = none yet.
static OUTAGE_EVENT_AT: AtomicU32 = AtomicU32::new(0);
static OUTAGE_RECORDED: AtomicBool = AtomicBool::new(false);

/// Feed the outage rule and act on what it says: the chips off or back
/// on (the board's flag), the API's `poolsUnavailable`, the event log.
/// Atomics, the state lock and the event queue only, so the Sv2
/// session's thread (stack in PSRAM) may call it.
fn pools(f: impl FnOnce(&mut PoolWatch, u64) -> Option<Change>) {
    let now = diag::uptime_s();
    let change = f(&mut WATCH.lock().unwrap(), now);
    match change {
        Some(Change::Unavailable) => {
            update(|s| s.pools_unavailable = true);
            crate::board::set_no_pool(true);
            log::warn!("stratum: no pool answers, chips off until one does (a board on its first start keeps them on)");
            let last = u64::from(OUTAGE_EVENT_AT.load(Ordering::Relaxed));
            if last == 0 || now.saturating_sub(last) >= OUTAGE_EVENT_S {
                OUTAGE_EVENT_AT.store(now.clamp(1, u64::from(u32::MAX)) as u32, Ordering::Relaxed);
                OUTAGE_RECORDED.store(true, Ordering::Relaxed);
                crate::events::record("pool", warpaxe_diag::events::detail::pools_out());
            }
        }
        Some(Change::Available { off_s }) => {
            update(|s| s.pools_unavailable = false);
            crate::board::set_no_pool(false);
            log::warn!("stratum: a pool answers again after {off_s} s, chips back on");
            // Also for an outage whose start was not written (it began
            // within ten minutes of the last one) once it has been a
            // long one: it must not go unrecorded altogether.
            if OUTAGE_RECORDED.swap(false, Ordering::Relaxed) || off_s >= OUTAGE_EVENT_S {
                crate::events::record("pool", warpaxe_diag::events::detail::pools_back(off_s));
            }
        }
        None => {}
    }
}

/// Neither pool answers right now.
fn pools_out() -> bool {
    WATCH.lock().unwrap().unavailable()
}
/// The newest work from the pool, whichever protocol brought it.
enum Feed {
    /// Decoded once per notify; every job hashes from it.
    V1(Session, Prepared),
    V2(Template),
}

/// A share on its way to the pool.
enum Outgoing {
    V1(Request),
    /// A ready line for the V1 link (a pong), sent by the submit thread
    /// so that the link has one writer.
    Line(String),
    V2 {
        job_id: u32,
        extranonce2: Vec<u8>,
        ntime: u32,
        nonce: u32,
        version: u32,
    },
}

/// The newest work with its notify number and block-change number,
/// published together so the miner's view of both stays in step.
static NOTIFY: Mutex<Option<(Feed, u64, u64)>> = Mutex::new(None);
/// The job ids the miner has taken from this session's work
/// (`next_job`), newest last, at most [`SESSION_JOBS_MAX`]. Emptied when
/// a session ends, with [`NOTIFY`]. A share for a job not in it was found
/// on the work of an earlier session (the chips keep hashing it until the
/// new pool's first job arrives) and would only come back stale: it is
/// dropped and counted instead of sent (device test 2026-11-22: 1 reject
/// after a V2 → V1 switch, 9 during a session without a job yet).
static SESSION_JOBS: Mutex<std::collections::VecDeque<String>> =
    Mutex::new(std::collections::VecDeque::new());
const SESSION_JOBS_MAX: usize = 64;
/// Shares dropped because their job came from an earlier session
/// (`sharesDroppedOldSession` in the API).
pub static OLD_SESSION_SHARES: AtomicU32 = AtomicU32::new(0);
static EXTRANONCE2: Mutex<u32> = Mutex::new(0);
static SUBMIT: Mutex<Option<Sender<Outgoing>>> = Mutex::new(None);
static NEXT_ID: Mutex<u64> = Mutex::new(100);
/// The worker the running V1 session authorized as: `mining.submit`
/// names it. On the fallback that is the fallback's worker, not the main
/// pool's (ckpool rejected every share with "Worker mismatch" when the
/// two differ).
static SESSION_USER: Mutex<String> = Mutex::new(String::new());

/// Keep a job's payout; a job that does not parse clears it rather than
/// leaving the previous pool's split on show.
fn set_payout(st: &mut StratumState, pay: Option<payout::Payout>, user: &str) {
    let p = pay.unwrap_or_default();
    let own = if p.own_address { user.split('.').next().unwrap_or("").trim() } else { "" };
    let shown = payout::shown(p.outputs, own, payout::SHOWN_OUTPUTS);
    st.coinbase_outputs = shown
        .outputs
        .into_iter()
        .map(|o| (o.value, o.address))
        .collect();
    st.coinbase_outputs_hidden = u32::try_from(shown.hidden).unwrap_or(u32::MAX);
    st.coinbase_outputs_hidden_sat = shown.hidden_value;
    st.coinbase_total_sat = p.total;
    st.coinbase_user_sat = p.user;
    st.coinbase_own_address = p.own_address;
    st.coinbase_bitcoin = p.bitcoin;
}

pub fn state() -> StratumState {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

/// Whether the session in use is on the fallback pool, without cloning
/// the whole state (the probe thread asks every 5 s on a 4 KB stack).
fn using_fallback() -> bool {
    STATE.lock().unwrap().as_ref().is_some_and(|s| s.using_fallback)
}

fn update(f: impl FnOnce(&mut StratumState)) {
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(StratumState::default);
    f(s);
}

/// A fresh job from the newest notify: a new extranonce2 every call, so
/// re-submitting work to the chips never repeats a nonce space.
pub fn next_job() -> Option<(Job, u64, u64)> {
    let g = NOTIFY.lock().unwrap();
    let (feed, seq, clean_seq) = g.as_ref()?;
    let size = match feed {
        Feed::V1(session, _) => session.extranonce2_size,
        Feed::V2(t) => t.extranonce2_size,
    };
    let mut c = EXTRANONCE2.lock().unwrap();
    *c = c.wrapping_add(1);
    let mut en2 = vec![0u8; size];
    let be = c.to_be_bytes();
    let take = en2.len().min(4);
    let n = en2.len();
    en2[n - take..].copy_from_slice(&be[4 - take..]);
    let job = match feed {
        Feed::V1(session, prepared) => prepared.build(session, &en2),
        Feed::V2(t) => t.build(&en2),
    }?;
    {
        let mut ids = SESSION_JOBS.lock().unwrap();
        if ids.back() != Some(&job.job_id) && !ids.contains(&job.job_id) {
            if ids.len() >= SESSION_JOBS_MAX {
                ids.pop_front();
            }
            ids.push_back(job.job_id.clone());
        }
    }
    Some((job, *seq, *clean_seq))
}

/// What the newest work builds on: the previous block's hash (header
/// byte order) and the height of the block it would be (0 = not
/// readable). Both from the same notify, so a found block's status is
/// never judged on a hash from one job and a height from another.
pub fn tip() -> Option<([u8; 32], u64)> {
    let g = NOTIFY.lock().unwrap();
    let (feed, _, _) = g.as_ref()?;
    Some(match feed {
        Feed::V1(_, prepared) => (*prepared.prev_hash(), prepared.height()),
        Feed::V2(t) => (t.prev_hash, t.height()),
    })
}

/// Count a notify (and a block change), publish the work with those
/// numbers. The miner compares the numbers it fetched with the work
/// against the counters, so the order of the two writes no longer
/// matters.
fn publish(feed: Feed, clean: bool) {
    let (seq, clean_seq) = {
        let mut g = STATE.lock().unwrap();
        let s = g.get_or_insert_with(StratumState::default);
        s.jobs_received += 1;
        s.last_job_uptime_s = diag::uptime_s();
        if clean {
            s.clean_jobs += 1;
            s.last_clean_uptime_s = diag::uptime_s();
        }
        (s.jobs_received, s.clean_jobs)
    };
    *NOTIFY.lock().unwrap() = Some((feed, seq, clean_seq));
    // Work from a pool: it answers (and ends an outage).
    pools(|w, now| w.answered(now));
}

/// Queue a share. Silently dropped when no session is up.
pub fn submit(job: &Job, ntime: u32, nonce: u32, version_bits: u32) {
    if !SESSION_JOBS.lock().unwrap().contains(&job.job_id) {
        let n = OLD_SESSION_SHARES.fetch_add(1, Ordering::Relaxed) + 1;
        log::info!(
            "stratum: share for job {} from an earlier session dropped, not sent ({n} so far)",
            job.job_id
        );
        return;
    }
    let out = if state().sv2 {
        let Ok(job_id) = job.job_id.parse::<u32>() else {
            return;
        };
        Outgoing::V2 {
            job_id,
            extranonce2: job.extranonce2.clone(),
            ntime,
            nonce,
            // Sv2 wants the whole header version, rolled bits included.
            version: job.version ^ version_bits,
        }
    } else {
        let user = {
            let u = SESSION_USER.lock().unwrap();
            if u.is_empty() { config::get().stratum_user } else { u.clone() }
        };
        let id = {
            let mut g = NEXT_ID.lock().unwrap();
            *g += 1;
            *g
        };
        Outgoing::V1(submit_request(
            id,
            &user,
            &job.job_id,
            &job.extranonce2_hex(),
            ntime,
            nonce,
            version_bits,
        ))
    };
    if let Some(tx) = SUBMIT.lock().unwrap().as_ref() {
        if tx.send(out).is_ok() {
            update(|s| s.shares_sent += 1);
        }
    }
}

/// The V1 session thread and its fallback probe run on PSRAM stacks like
/// the sv2 session (docs/20): neither writes flash. Config is read from
/// RAM, events are only queued (`events::record`; the store is
/// `events::flush` on an internal stack), a confirmed TLS key is written
/// by the HTTP handler, and TLS memory comes from PSRAM anyway. Without
/// PSRAM for the stack, one from internal RAM as before.
pub fn start() {
    const STACK: usize = 12 * 1024;
    diag::spawn_psram("stratum", STACK, run)
        .or_else(|_| std::thread::Builder::new().stack_size(STACK).spawn(run))
        .expect("stratum thread");
}

/// Failures in a row on the main pool before the fallback takes over.
const FAILS_BEFORE_FALLBACK: u32 = 3;
/// A session shorter than this counts as a failure too.
const SHORT_SESSION: Duration = Duration::from_secs(60);
/// While on the fallback, how often the main pool is probed (seconds).
/// The probe is a TCP connect; a pool that takes it and then sends no
/// work sent the miner back for its three failed sessions every time.
/// Now a failed session after a probe goes straight back to the
/// fallback and doubles the wait (5, 10, 20, 40 min, up to
/// [`PRIMARY_PROBE_MAX`]); work from the main pool sets it back. A
/// session silent over IPv6 gets one more try first, with IPv4 first.
const PRIMARY_PROBE: u32 = 300;
const PRIMARY_PROBE_MAX: u32 = 2400;

/// Strikes against a pool that keeps the connection and sends no new job
/// (`warpaxe_stratum::stale`).
static STALE: Mutex<warpaxe_stratum::stale::Watch> =
    Mutex::new(warpaxe_stratum::stale::Watch::new());

/// A session ended because the pool sent no new job for too long.
#[derive(Debug)]
struct StaleJob {
    secs: u64,
    pings: u32,
}

impl std::fmt::Display for StaleJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no new job from the pool for {} s", self.secs)?;
        if self.pings > 0 {
            write!(f, " ({} pings meanwhile)", self.pings)?;
        }
        Ok(())
    }
}

impl std::error::Error for StaleJob {}
static PROBE_EVERY_S: AtomicU32 = AtomicU32::new(PRIMARY_PROBE);

/// The live stream while on the fallback, so the probe thread can end
/// that session once the main pool answers again.
static FALLBACK_STREAM: Mutex<Option<Arc<Link>>> = Mutex::new(None);
/// The link of the running session, whichever pool: `reconnect` closes
/// it so `run` builds the next session from the current config.
static CURRENT_STREAM: Mutex<Option<Arc<Link>>> = Mutex::new(None);
/// Set by `reconnect`: the session that ends next did so on purpose, so
/// `run` neither counts a failure nor waits for the backoff.
static RECONNECT: AtomicBool = AtomicBool::new(false);
/// What the pending `RECONNECT` asks for: `MOVE` from `move_session`
/// (the session only changes the interface it leaves from; `run` keeps
/// the pool, the fallback, the stuck-pool mark and the probe's wait),
/// `FULL` from a settings change. Raised with `fetch_max` before
/// `RECONNECT` is set and taken with `swap` after it is cleared, so a
/// settings change that meets a cable move always gets its full reset.
static RECONNECT_KIND: AtomicU8 = AtomicU8::new(0);
const MOVE: u8 = 1;
const FULL: u8 = 2;

/// Take a `RECONNECT` that is pending: `None` if none is, else whether
/// it asks for the full reset of a settings change.
fn take_reconnect() -> Option<bool> {
    RECONNECT
        .swap(false, Ordering::Relaxed)
        .then(|| RECONNECT_KIND.swap(0, Ordering::Relaxed) != MOVE)
}

/// A pool key the TLS check refused (no public authority, and not the
/// confirmed key), per slot (main, fallback), in RAM only: the Pool page
/// shows it for confirmation. Cleared when that pool's settings change
/// (`clear_tls_pending`) or its certificate is accepted.
static TLS_PENDING: Mutex<[Option<PendingKey>; 2]> = Mutex::new([None, None]);

/// A pool's unconfirmed key, with the pool it was seen at.
#[derive(Clone, Debug, Serialize)]
pub struct PendingKey {
    pub host: String,
    pub port: u16,
    /// SHA-256 of the SubjectPublicKeyInfo, `AB:CD:…:EF`.
    pub key: String,
    /// A key is confirmed for this pool, and this is another one.
    pub changed: bool,
}

/// The unconfirmed key seen at the main (or fallback) pool as it is
/// configured now; none once the pool's address changed.
pub fn tls_pending(fallback: bool) -> Option<PendingKey> {
    let cfg = config::get();
    let (host, port) = if fallback {
        (&cfg.fallback_url, cfg.fallback_port)
    } else {
        (&cfg.stratum_url, cfg.stratum_port)
    };
    TLS_PENDING.lock().unwrap()[usize::from(fallback)]
        .clone()
        .filter(|p| p.host.eq_ignore_ascii_case(host) && p.port == port)
}

/// Forget the unconfirmed key of the main (or fallback) pool: its
/// settings changed, or a key was confirmed or forgotten.
pub fn clear_tls_pending(fallback: bool) {
    TLS_PENDING.lock().unwrap()[usize::from(fallback)] = None;
}

fn set_tls_pending(fallback: bool, host: &str, port: u16, seen: crate::tls_trust::Seen) {
    TLS_PENDING.lock().unwrap()[usize::from(fallback)] = Some(PendingKey {
        host: host.to_string(),
        port,
        key: seen.pin.text().as_str().to_string(),
        changed: seen.changed,
    });
}

/// The confirmed key of a pool as stored, or none (also for a value that
/// does not parse: it is only ever written by `pool_refuse`).
fn pin_of(stored: &str) -> Option<Pin> {
    Pin::parse(stored)
}

/// Pool settings changed over the API: end the running session now, so
/// the new pool, worker, password, TLS or protocol applies within
/// seconds instead of at the next reboot (Oliver renamed a worker on
/// 2026-10-08 and the pool saw it hours later, at an OTA reboot).
pub fn reconnect() {
    reconnect_because("pool settings changed");
}

/// The same for another reason. Not counted in `reconnects`, which
/// counts sessions that ended with an error.
pub fn reconnect_because(why: &str) {
    RECONNECT_KIND.fetch_max(FULL, Ordering::Relaxed);
    RECONNECT.store(true, Ordering::Relaxed);
    if let Some(s) = CURRENT_STREAM.lock().unwrap().as_ref() {
        log::info!("stratum: {why}, reconnecting");
        s.shutdown();
    } else {
        log::info!("stratum: {why}, the next session takes it into account");
    }
}

/// End the running session so the next one leaves over the interface
/// that now carries the default route (`net::eth`: the cable came or
/// went). Unlike `reconnect` the pool choice stays: a miner on the
/// fallback because its main pool is stuck stays on the fallback. A
/// settings change already pending keeps its full reset.
pub fn move_session(why: &str) {
    RECONNECT_KIND.fetch_max(MOVE, Ordering::Relaxed);
    RECONNECT.store(true, Ordering::Relaxed);
    if let Some(s) = CURRENT_STREAM.lock().unwrap().as_ref() {
        log::info!("stratum: {why}, reconnecting to the same pool");
        s.shutdown();
    } else {
        log::info!("stratum: {why}, the next session takes it into account");
    }
}

fn run() {
    let mut backoff = Duration::from_secs(3);
    let mut fails: u32 = 0;
    let mut on_fallback = false;
    // Back on the main pool because the probe reached it, no work yet.
    let mut probing = false;
    // That probe's second try, IPv4 first after silence over IPv6, used.
    let mut probe_retried = false;
    // PSRAM like this thread (see `start`): a TCP connect, no flash.
    if diag::spawn_psram("probe", 4096, probe_primary).is_err() {
        std::thread::Builder::new()
            .stack_size(4096)
            .spawn(probe_primary)
            .ok();
    }
    loop {
        // A change that came while no session was up (connecting, or in
        // the backoff) applies now: this pass reads the new config.
        if take_reconnect() == Some(true) {
            on_fallback = false;
            fails = 0;
            backoff = Duration::from_secs(3);
            probing = false;
            BACK_UNREPORTED.store(false, Ordering::Relaxed);
            PROBE_EVERY_S.store(PRIMARY_PROBE, Ordering::Relaxed);
            crate::net::dial::forget_no_work();
            STALE.lock().unwrap().reset();
        }
        let cfg = config::get();
        if cfg.stratum_url.is_empty() || cfg.stratum_user.is_empty() {
            update(|s| s.last_error = "no pool or user configured".into());
            // Nothing to mine for: counts like a pool that does not
            // answer, so the chips do not idle at full clock.
            pools(|w, now| w.failed(now, false, false));
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        let fallback_ok =
            cfg.use_fallback && !cfg.fallback_url.is_empty() && !cfg.fallback_user.is_empty();
        if !fallback_ok {
            on_fallback = false;
        } else if !on_fallback && fails >= FAILS_BEFORE_FALLBACK && !pools_out() {
            log::warn!("stratum: main pool failed {fails} times, switching to the fallback");
            crate::events::record("pool", warpaxe_diag::events::detail::pool_fallback(fails));
            on_fallback = true;
            fails = 0;
            STALE.lock().unwrap().switched();
        }
        let (host, port, tls, pin, user, pass, v2) = if on_fallback {
            (
                cfg.fallback_url.clone(),
                cfg.fallback_port,
                cfg.fallback_tls > 0,
                pin_of(&cfg.fallback_tls_pin),
                cfg.fallback_user.clone(),
                cfg.fallback_pass.clone(),
                cfg.fallback_sv2().then(|| V2Pool {
                    authority: cfg.fallback_v2_authority_pubkey.clone(),
                    require_auth: cfg.fallback_v2_require_auth,
                }),
            )
        } else {
            (
                cfg.stratum_url.clone(),
                cfg.stratum_port,
                cfg.stratum_tls > 0,
                pin_of(&cfg.stratum_tls_pin),
                cfg.stratum_user.clone(),
                cfg.stratum_pass.clone(),
                cfg.sv2().then(|| V2Pool {
                    authority: cfg.stratum_v2_authority_pubkey.clone(),
                    require_auth: cfg.stratum_v2_require_auth,
                }),
            )
        };
        let pool = warpaxe_stratum::dial::host_port(&host, port);
        let sv2 = v2.is_some();
        *SESSION_USER.lock().unwrap() = user.clone();
        // Without a network a pool cannot answer: such an attempt says
        // nothing about the pool and does not count towards the
        // fallback (a cold boot whose Wi-Fi took a minute switched to it).
        // Wi-Fi or the Ethernet cable with an address: a board on the
        // cable alone has no Wi-Fi at all.
        let net_up = {
            let n = crate::net::state();
            n.wifi_connected || n.eth_ip.is_some()
        };
        update(|s| {
            s.pool = pool.clone();
            s.using_fallback = on_fallback;
            s.tls = tls && !sv2;
            s.tls_pinned = false;
            s.sv2 = sv2;
            s.connected = false;
            s.local_ip = None;
            s.subscribed = false;
            s.authorized = false;
            s.pool_ip.clear();
        });
        let started = Instant::now();
        let jobs_before = state().jobs_received;
        // Only this session's verdict counts (a `continue` above could
        // have left the last one's flag standing).
        CLOCK_UNSURE.store(false, Ordering::Relaxed);
        // The probe's word is for the fallback session it ended, and is
        // taken right after it (below). One left standing (a settings
        // change ended that session first) must not end the next
        // fallback session at once. The probe sets it only while a
        // fallback session's link is registered, which none is here.
        BACK_TO_PRIMARY.store(false, Ordering::Relaxed);
        let result = match v2 {
            Some(v2) => session_v2_on_psram(host.clone(), port, user.clone(), v2, on_fallback),
            None => session(&host, port, tls, pin, &user, &pass, on_fallback),
        };
        *FALLBACK_STREAM.lock().unwrap() = None;
        *CURRENT_STREAM.lock().unwrap() = None;
        // The network as the session ends: one lost during it is not the
        // pool's fault either (`net_up` was read before the connect).
        let net_up = net_up && {
            let n = crate::net::state();
            n.wifi_connected || n.eth_ip.is_some()
        };
        if let Some(full) = take_reconnect() {
            update(|s| s.connected = false);
            if !full {
                // Moved between the cable and Wi-Fi: the same pool again
                // at once, nothing counted, nothing forgotten. A probe
                // that reached the main pool meanwhile still takes the
                // miner back (below), not some later session's end.
                backoff = Duration::from_secs(3);
                if !(on_fallback && BACK_TO_PRIMARY.load(Ordering::Relaxed)) {
                    continue;
                }
            } else {
                // Ended on purpose after a settings change: straight back
                // in with the new settings, main pool first.
                on_fallback = false;
                fails = 0;
                backoff = Duration::from_secs(3);
                probing = false;
                BACK_UNREPORTED.store(false, Ordering::Relaxed);
                PROBE_EVERY_S.store(PRIMARY_PROBE, Ordering::Relaxed);
                crate::net::dial::forget_no_work();
                STALE.lock().unwrap().reset();
                continue;
            }
        }
        if on_fallback && BACK_TO_PRIMARY.swap(false, Ordering::Relaxed) {
            // Reported once the main pool sends work (`session`), so a
            // probe whose session brings none leaves no pair of events.
            BACK_UNREPORTED.store(true, Ordering::Relaxed);
            if STALE.lock().unwrap().main_stuck() {
                // Left for sending no new jobs: reachable says nothing yet.
                // The event comes with its first fresh job (`session`).
                log::warn!("stratum: main pool reachable again, checking whether it sends new jobs");
            } else {
                // Reachable over TCP: the event comes with its first job.
                log::warn!("stratum: main pool answers again, leaving the fallback");
            }
            on_fallback = false;
            fails = 0;
            backoff = Duration::from_secs(3);
            probing = true;
            probe_retried = false;
            // A main pool left for sending no new jobs gets the short
            // limit (`warpaxe_stratum::stale`).
            STALE.lock().unwrap().back_to_main();
            continue;
        }
        // The address this session used (cleared below on an error).
        let used_ip = state().pool_ip.clone();
        // Sessions only end with an error (a closed or broken
        // connection). One that ran for a while was a working pool: start
        // counting again. A connect failure or a session under a minute
        // counts towards the fallback.
        if let Err(e) = &result {
            log::warn!("stratum: {e}");
            update(|s| {
                s.last_error = e.to_string();
                s.connected = false;
                s.pool_ip.clear();
                s.local_ip = None;
                s.reconnects += 1;
            });
        }
        if let Some(stale) = result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<StaleJob>())
        {
            // The pool kept the connection and stopped sending jobs (a
            // ckpool whose node is down pings instead). Reconnect once;
            // if that brings no fresh work either, the other pool.
            use warpaxe_stratum::stale::Verdict;
            let (verdict, next_limit) = {
                let mut w = STALE.lock().unwrap();
                let v = w.stale(fallback_ok, on_fallback);
                (v, w.limit_s())
            };
            match verdict {
                Verdict::Reconnect => {
                    log::warn!(
                        "stratum: {stale}, reconnecting; another {} min without a new job and the miner switches pools",
                        next_limit / 60
                    );
                    crate::events::record("pool", warpaxe_diag::events::detail::pool_stale(stale.secs));
                }
                Verdict::SwitchPool if on_fallback => {
                    log::warn!("stratum: {stale}, the fallback pool sends no new jobs either, back to the main pool");
                    crate::events::record("pool", warpaxe_diag::events::detail::fallback_stale(stale.secs));
                    on_fallback = false;
                    probing = false;
                }
                Verdict::SwitchPool => {
                    // The probe finds a stuck pool reachable over TCP: wait
                    // longer before each next check (10, 20, 40 min), as
                    // for a main pool that does not hold.
                    let next = warpaxe_stratum::dial::next_probe_s(
                        PROBE_EVERY_S.load(Ordering::Relaxed),
                        PRIMARY_PROBE_MAX,
                    );
                    PROBE_EVERY_S.store(next, Ordering::Relaxed);
                    log::warn!(
                        "stratum: {stale}, the main pool sends no new jobs, switching to the fallback; next check in {} min",
                        next / 60
                    );
                    crate::events::record(
                        "pool",
                        warpaxe_diag::events::detail::pool_stale_fallback(stale.secs),
                    );
                    on_fallback = true;
                    probing = false;
                    // Never reported back (no fresh job): nothing to wait for.
                    BACK_UNREPORTED.store(false, Ordering::Relaxed);
                }
            }
            fails = 0;
            backoff = Duration::from_secs(3);
            pause_unless_reconnect(Duration::from_secs(3), fallback_ok);
            continue;
        }
        // A failed attempt: a session under a minute, or one of any
        // length that brought no work at all (a pool that takes the
        // connection and then says nothing until the read times out).
        let no_work = state().jobs_received == jobs_before;
        let used_ip = used_ip.parse::<std::net::IpAddr>().ok();
        if no_work && net_up {
            // Silent over IPv6: IPv4 first next time (`net::dial`).
            if let Some(ip) = used_ip {
                crate::net::dial::no_work_over(&host, port, ip);
            }
        }
        let held = started.elapsed() >= SHORT_SESSION && !no_work;
        if probing && (on_fallback || !net_up) {
            // The probe's session ended without a network, or the pool
            // choice moved on to the fallback since (the outage's rounds,
            // the alternation): neither says anything about the main
            // pool the probe reached. Counted as an ordinary failure.
            probing = false;
            // A return still pending stays pending when only the network
            // went: the miner retries the main pool, and its first job
            // there records the return (review 2026-11-22). Dropped when
            // the choice has moved to the fallback.
            if on_fallback {
                BACK_UNREPORTED.store(false, Ordering::Relaxed);
            }
        }
        if probing
            && fallback_ok
            && net_up
            && !held
            && !probe_retried
            && no_work
            && used_ip.is_some_and(|ip| ip.is_ipv6())
            && crate::net::dial::has_ipv4(&host, port)
        {
            // Silent over IPv6 only, maybe: one more try right away with
            // IPv4 first, before the fallback and the longer wait. Only
            // for a name with an A record: without one the retry would
            // go over IPv6 again.
            probe_retried = true;
            log::warn!("stratum: main pool sent no work over IPv6, trying again with IPv4 first");
            pause_unless_reconnect(Duration::from_secs(3), fallback_ok);
            continue;
        }
        if probing && fallback_ok && net_up && !held {
            // The probe reached the main pool, the session did not hold:
            // back to the fallback that worked, and wait longer before
            // the next probe. Not an outage: the fallback mines.
            probing = false;
            on_fallback = true;
            fails = 0;
            STALE.lock().unwrap().switched();
            backoff = Duration::from_secs(3);
            let next = warpaxe_stratum::dial::next_probe_s(
                PROBE_EVERY_S.load(Ordering::Relaxed),
                PRIMARY_PROBE_MAX,
            );
            PROBE_EVERY_S.store(next, Ordering::Relaxed);
            log::warn!(
                "stratum: main pool took the connection but did not hold, back to the fallback; next check in {} min",
                next / 60
            );
            // Recorded only when the return was: a probe whose session
            // brought no job leaves the event log as it was (the miner
            // never mined on the main pool), not "back" and "fallback".
            if !BACK_UNREPORTED.swap(false, Ordering::Relaxed) {
                crate::events::record("pool", warpaxe_diag::events::detail::pool_fallback(1));
            }
            continue;
        }
        if started.elapsed() < SHORT_SESSION || no_work {
            if CLOCK_UNSURE.swap(false, Ordering::Relaxed) {
                log::warn!("stratum: the pool certificate failed against a clock NTP has not set yet: not counted towards the fallback");
            } else {
                if !on_fallback && net_up {
                    fails += 1;
                } else if !net_up {
                    log::info!("stratum: no network yet: not counted towards the fallback");
                }
                pools(|w, now| w.failed(now, on_fallback, fallback_ok));
            }
            if pools_out() {
                // No pool answers and the chips are off: both pools in
                // turn, a round every half minute, until one sends work
                // (`publish`). The main pool is tried again too, which
                // the fallback's own backoff never did.
                let (next_fallback, pause_s) = outage::next_probe(on_fallback, fallback_ok);
                if next_fallback != on_fallback {
                    STALE.lock().unwrap().switched();
                }
                on_fallback = next_fallback;
                fails = 0;
                if pause_s > 0 {
                    pause_unless_reconnect(Duration::from_secs(pause_s), fallback_ok);
                }
                backoff = Duration::from_secs(3);
            } else {
                // The fallback has stopped answering as well: both
                // pools in turn after every failure, with short pauses,
                // so that within the minute either is found back or both
                // are known to be out (the fallback alone used to be
                // retried for ever). `fails` stays at 0 meanwhile: no
                // switch at the loop's top, no fallback event per turn.
                if fallback_ok && WATCH.lock().unwrap().alternate() {
                    on_fallback = !on_fallback;
                    fails = 0;
                    STALE.lock().unwrap().switched();
                    backoff = backoff.min(Duration::from_secs(12));
                }
                pause_unless_reconnect(backoff, fallback_ok);
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        } else {
            fails = 0;
            backoff = Duration::from_secs(3);
            if !on_fallback {
                probing = false;
                PROBE_EVERY_S.store(PRIMARY_PROBE, Ordering::Relaxed);
            }
        }
    }
}

/// The backoff, cut short by a settings change: a saved pool chosen while
/// the old one keeps failing applies within a second, not a minute.
fn pause_unless_reconnect(d: Duration, has_fallback: bool) {
    let end = Instant::now() + d;
    let was_out = pools_out();
    while Instant::now() < end && !RECONNECT.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
        // The outage's minute may run out in a backoff; the probing
        // then starts at once, not after the rest of the backoff.
        pools(|w, now| w.tick(now, has_fallback));
        if !was_out && pools_out() {
            break;
        }
    }
}

static BACK_TO_PRIMARY: AtomicBool = AtomicBool::new(false);
/// The probe took the miner back to the main pool and `pool_back` is not
/// recorded yet: it comes with that pool's first job (`session`), or,
/// for a main pool left as stuck, with its first fresh job.
static BACK_UNREPORTED: AtomicBool = AtomicBool::new(false);

/// The session on the main pool has its first job: the return the probe
/// started goes into the event log now, unless the pool was left as
/// stuck (then fresh work is the proof, `Watch::fresh`).
fn report_back_with_work(fallback: bool) {
    if !fallback
        && !STALE.lock().unwrap().main_stuck()
        && BACK_UNREPORTED.swap(false, Ordering::Relaxed)
    {
        crate::events::record("pool", warpaxe_diag::events::detail::pool_back());
    }
}

/// The least stack the probe thread (4 KB, `run`) has had to spare since
/// boot, bytes, read after each probe's connect (DNS and the race are its
/// deepest calls); 0 until the first probe. `probeStackFree` in the API.
static PROBE_STACK_FREE: AtomicU32 = AtomicU32::new(0);

/// The least stack the probe thread has had to spare, bytes.
pub fn probe_stack_free() -> u32 {
    PROBE_STACK_FREE.load(Ordering::Relaxed)
}

/// While on the fallback: every few minutes, try a TCP connect to the main
/// pool; when it answers, end the fallback session so `run` goes back.
fn probe_primary() {
    loop {
        // Read again while waiting: the stratum thread doubles the wait
        // after a probe that did not hold, and sets it back on work. The
        // wait counts from the switch to the fallback: while the miner is
        // on the main pool the clock starts over, so a probe that brought
        // it back to a still stuck pool waits the full time again once it
        // is on the fallback (the .42 test of 2026-11-20 saw 15 min
        // instead of 20, counted from the probe before).
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_secs(PROBE_EVERY_S.load(Ordering::Relaxed) as u64) {
            std::thread::sleep(Duration::from_secs(5));
            if !using_fallback() {
                since = Instant::now();
            }
        }
        // Only when the miner went back to the main pool in the last tick.
        if !using_fallback() {
            continue;
        }
        let cfg = config::get();
        // Over IPv6 or IPv4 like the session itself (`net::dial`).
        let reachable =
            crate::net::dial::dial(&cfg.stratum_url, cfg.stratum_port, Duration::from_secs(5))
                .is_ok();
        // The high-water mark only falls, so the newest reading is the least.
        PROBE_STACK_FREE.store(diag::stack_unused(), Ordering::Relaxed);
        if reachable {
            if let Some(s) = FALLBACK_STREAM.lock().unwrap().as_ref() {
                BACK_TO_PRIMARY.store(true, Ordering::Relaxed);
                s.shutdown();
            }
        }
    }
}

fn session(
    host: &str,
    port: u16,
    tls: bool,
    pin: Option<Pin>,
    user: &str,
    pass: &str,
    fallback: bool,
) -> anyhow::Result<()> {
    // No try_clone: ESP-IDF has no dup(). The link is read on this thread
    // and written on a small writer thread (see `link`).
    let stream = match Link::connect(host, port, tls, pin) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            // A certificate refused for its key alone: the Pool page
            // offers the key seen for confirmation (RAM only; the pin
            // is written by the HTTP handler on the click).
            if let Some(KeyRefused(seen)) = e.downcast_ref::<KeyRefused>() {
                // Unless the click came while this connect was under way:
                // the key is confirmed now, and the reconnect the PATCH
                // started uses it.
                let cfg = config::get();
                let stored = if fallback {
                    &cfg.fallback_tls_pin
                } else {
                    &cfg.stratum_tls_pin
                };
                if !seen.pin.matches_text(stored) {
                    set_tls_pending(fallback, host, port, *seen);
                }
            }
            return Err(e);
        }
    };
    if tls {
        // Accepted, by the bundle or by the confirmed key: nothing is
        // pending for this pool any more.
        clear_tls_pending(fallback);
    }
    let pinned = tls && crate::tls_trust::pool_outcome().1;
    *CURRENT_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
    // Until the first job the link waits `link::SETUP_TIMEOUT` for the
    // pool, then the full `link::READ_TIMEOUT`.
    let mut has_work = false;
    let ip = stream
        .peer()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    update(|s| s.pool_ip = ip);
    // Settings changed while this session was connecting (DNS, connect,
    // TLS): `reconnect` found no stream to shut, so end here and let
    // `run` start over with the new config.
    if RECONNECT.load(Ordering::Relaxed) {
        return Ok(());
    }
    if fallback {
        *FALLBACK_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
    }
    let mut reader = BufReader::new(LinkReader(&stream));
    let local_ip = stream.local_ip();
    update(|s| {
        s.connected = true;
        s.tls_pinned = pinned;
        s.local_ip = local_ip;
        s.last_error.clear();
    });
    {
        let mut hello = String::new();
        for r in [
            configure_request(1, BIP320_MASK),
            subscribe_request(2, &v1_agent()),
            authorize_request(3, user, pass),
        ] {
            hello.push_str(&r.to_line());
        }
        // Ask for a share difficulty (AxeOS sends it right after
        // authorizing); the pool may ignore it.
        let suggest = config::get().suggested_difficulty;
        if board::devkit() {
            // The devkit: a fraction, and as the session's first difficulty
            // request (right after configure, subscribe and authorize),
            // which WarpPool takes as the start value even over a stored
            // snapshot (an integer would only raise it).
            {
                let d = crate::cpuminer::SUGGESTED_DIFFICULTY;
                hello.push_str(
                    &warpaxe_stratum::message::suggest_difficulty_fraction_request(4, d).to_line(),
                );
                log::info!("stratum: suggest difficulty {d}");
            }
        } else if suggest > 0 {
            hello.push_str(&suggest_difficulty_request(4, suggest).to_line());
            log::info!("stratum: suggest difficulty {suggest}");
        }
        stream.send(hello.as_bytes())?;
    }
    let pending: Arc<Mutex<HashMap<u64, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = channel::<Outgoing>();
    *SUBMIT.lock().unwrap() = Some(tx);
    {
        let stream = Arc::clone(&stream);
        let pending = Arc::clone(&pending);
        // The submit thread's stack in PSRAM: 6 KB of internal RAM in one
        // piece is not always there on a reconnect (4.8 KB largest block
        // seen on a NerdOctaxe on TLS, 2026-11-13), and without the thread
        // the session hashed on and sent no share, with no word of it.
        // It only sends on the link, never touches flash.
        diag::spawn_psram("submit", 6 * 1024, move || {
                while let Ok(out) = rx.recv() {
                    let req = match out {
                        Outgoing::V1(req) => req,
                        Outgoing::Line(l) => {
                            if stream.send(l.as_bytes()).is_err() {
                                break;
                            }
                            continue;
                        }
                        _ => continue, // a share from before the protocol switch
                    };
                    if let Some(job_id) = req.params.get(1).and_then(|v| v.as_str()) {
                        pending.lock().unwrap().insert(req.id, job_id.to_string());
                    }
                    if stream.send(req.to_line().as_bytes()).is_err() {
                        break;
                    }
                }
        })
        .map_err(|e| {
            // No thread to take them: shares go nowhere until the next session.
            *SUBMIT.lock().unwrap() = None;
            anyhow::anyhow!("submit thread not started: {e}")
        })?;
    }
    let _drop_sender_on_exit = SenderGuard;

    let mut sess: Option<Session> = None;
    let mut line = String::new();
    let started = Instant::now();
    // No new job for this long ends the session (`warpaxe_stratum::stale`);
    // read each pass, since the first refresh from the pool shortens it.
    let mut last_job = Instant::now();
    let mut first_job: Option<Instant> = None;
    let mut fresh_seen = false;
    let mut pings: u32 = 0;
    // A notify that came before the subscribe answer (it needs the
    // extranonce from there), kept and read again right after it.
    let mut early: Option<String> = None;
    let mut replay: Option<String> = None;
    loop {
        let since_job = last_job.elapsed().as_secs();
        let stale_limit = STALE.lock().unwrap().limit_s();
        if warpaxe_stratum::stale::is_stale(has_work, since_job, stale_limit) {
            return Err(StaleJob {
                secs: since_job,
                pings,
            }
            .into());
        }
        // The read timeout alone never ends a session whose pool pings
        // and sends no job at all: a failed attempt after a minute.
        if warpaxe_stratum::stale::no_first_job(has_work, started.elapsed().as_secs()) {
            anyhow::bail!(
                "no job from the pool {} s after connecting ({pings} pings)",
                started.elapsed().as_secs()
            );
        }
        line.clear();
        let n = if let Some(l) = replay.take() {
            line.push_str(&l);
            line.len()
        } else {
            match line::read_line(&mut reader, &mut line, line::MAX_LINE) {
                Ok(n) => n,
                Err(e) if line::is_too_long(&e) => {
                    update(|s| s.lines_too_long += 1);
                    anyhow::bail!("{e}: the pool sent a line too long to keep, reconnecting");
                }
                Err(e) => return Err(e.into()),
            }
        };
        if n == 0 {
            anyhow::bail!("pool closed the connection");
        }
        match parse_line(&line) {
            Incoming::Response(r) => match r.id {
                1 => {
                    if let Some(m) = r
                        .result
                        .get("version-rolling.mask")
                        .and_then(|v| v.as_str())
                    {
                        let mask = u32::from_str_radix(m, 16).unwrap_or(0);
                        update(|s| s.version_mask = mask);
                        if let Some(s) = sess.as_mut() {
                            s.version_mask = mask;
                        }
                        log::info!("stratum: version rolling mask {m}");
                    }
                }
                2 => match parse_subscribe_result(&r.result)
                    .and_then(|(e1, sz)| Session::new(&e1, sz).map(|s| (e1, s)))
                {
                    Some((e1, mut s)) => {
                        s.version_mask = state().version_mask;
                        log::info!(
                            "stratum: subscribed, extranonce1 {e1}, extranonce2 size {}",
                            s.extranonce2_size
                        );
                        update(|st| {
                            st.subscribed = true;
                            st.extranonce1 = e1;
                            st.extranonce2_size = s.extranonce2_size;
                        });
                        sess = Some(s);
                        replay = early.take();
                    }
                    None => anyhow::bail!("subscribe: unexpected result {}", r.result),
                },
                3 => {
                    let ok = r.result.as_bool().unwrap_or(false) && r.error.is_none();
                    update(|s| s.authorized = ok);
                    if ok {
                        log::info!("stratum: authorized as {user}");
                    } else {
                        anyhow::bail!("authorize refused: {:?}", r.error);
                    }
                }
                id if id >= 100 => {
                    let job_id = pending.lock().unwrap().remove(&id).unwrap_or_default();
                    let ok = r.result.as_bool().unwrap_or(false) && r.error.is_none();
                    if ok {
                        update(|s| s.shares_accepted += 1);
                    } else {
                        let why = r
                            .error
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "rejected".into());
                        record_reject(id, &job_id, why);
                    }
                }
                _ => log::info!("stratum: response {} -> {} {:?}", r.id, r.result, r.error),
            },
            Incoming::SetDifficulty(d) => {
                log::info!("stratum: difficulty {}", fmt_difficulty(d));
                update(|s| s.difficulty = d);
                if let Some(s) = sess.as_mut() {
                    s.difficulty = d;
                }
            }
            Incoming::SetVersionMask(m) => {
                update(|s| s.version_mask = m);
                if let Some(s) = sess.as_mut() {
                    s.version_mask = m;
                }
            }
            Incoming::Notify(n) => {
                let Some(s) = sess.as_ref() else {
                    // Work all the same: kept, and taken as the first job
                    // once the subscribe answer brings the extranonce. The
                    // setup limit stays until then, so a pool that sends a
                    // notify and never answers the subscribe is still left
                    // after 30 s (review 2026-11-22).
                    if Prepared::new(n).is_some() {
                        log::info!("stratum: notify before the subscribe answer, kept for it");
                        early = Some(line.clone());
                    } else {
                        log::warn!("stratum: notify before subscribe with a field that is not hex, ignored");
                    }
                    continue;
                };
                let clean = n.clean_jobs;
                let job_id = n.job_id.clone();
                let Some(prepared) = Prepared::new(n) else {
                    log::warn!("stratum: notify {job_id} with a field that is not hex, ignored");
                    continue;
                };
                let (c1, c2) = prepared.coinbase_parts();
                let n = &prepared.notify;
                let height = coinbase::block_height(c1).unwrap_or(0);
                let net_diff = u32::from_str_radix(&n.nbits, 16)
                    .map(coinbase::nbits_difficulty)
                    .unwrap_or(0.0);
                let tag = coinbase::script_text(c1, c2, s.extranonce1.len() + s.extranonce2_size);
                let pay = payout::payout(c1, s.extranonce1.len() + s.extranonce2_size, c2, user);
                if started.elapsed() < Duration::from_secs(5) || clean {
                    log::info!(
                        "stratum: job {} ntime {} clean {}",
                        n.job_id,
                        n.ntime,
                        clean
                    );
                }
                publish(Feed::V1(s.clone(), prepared), clean);
                if !clean
                    && first_job
                        .is_some_and(|t| warpaxe_stratum::stale::is_fresh(t.elapsed().as_secs()))
                {
                    // New work on the same block, not the jobs handed out
                    // right after the connect: this pool refreshes.
                    STALE.lock().unwrap().refresh();
                }
                if !has_work {
                    has_work = true;
                    stream.set_read_timeout(crate::link::READ_TIMEOUT);
                    report_back_with_work(fallback);
                }
                last_job = Instant::now();
                match first_job {
                    None => first_job = Some(last_job),
                    Some(t)
                        if !fresh_seen
                            && warpaxe_stratum::stale::is_fresh(t.elapsed().as_secs()) =>
                    {
                        // Fresh work, not just the jobs a pool hands out
                        // right after the connect.
                        fresh_seen = true;
                        if STALE.lock().unwrap().fresh(fallback) {
                            BACK_UNREPORTED.store(false, Ordering::Relaxed);
                            crate::events::record("pool", warpaxe_diag::events::detail::pool_back());
                        }
                        if !fallback {
                            PROBE_EVERY_S.store(PRIMARY_PROBE, Ordering::Relaxed);
                        }
                    }
                    Some(_) => {}
                }
                update(|st| {
                    st.last_job_id = job_id;
                    st.block_height = height;
                    st.network_difficulty = net_diff;
                    st.scriptsig = tag;
                    set_payout(st, pay, user);
                });
            }
            Incoming::Ping { id } => {
                pings = pings.saturating_add(1);
                if pings == 1 {
                    log::info!("stratum: the pool sends mining.ping, answering with pong");
                }
                if warpaxe_stratum::needs_pong(&id) {
                    if let Some(tx) = SUBMIT.lock().unwrap().as_ref() {
                        let _ = tx.send(Outgoing::Line(warpaxe_stratum::pong_line(&id)));
                    }
                }
            }
            Incoming::Other { method, params } => log::info!("stratum: {method} {params}"),
            Incoming::Unparsed(l) => {
                log::warn!("stratum: unparsed line: {}", &l[..l.len().min(80)])
            }
        }
    }
}

/// Drops the submit sender when a session ends, so the writer thread exits.
struct SenderGuard;

impl Drop for SenderGuard {
    fn drop(&mut self) {
        *SUBMIT.lock().unwrap() = None;
        *NOTIFY.lock().unwrap() = None;
        SESSION_JOBS.lock().unwrap().clear();
    }
}

/// Count a rejected share: reason, the newest job, the stale histogram.
fn record_reject(id: u64, job_id: &str, why: String) {
    log::warn!("stratum: share {id} for {job_id} rejected: {why}");
    let stale = why.contains("stale");
    update(|s| {
        s.shares_rejected += 1;
        *s.reject_reasons.entry(why.clone()).or_insert(0) += 1;
        // Which job the pool refused, against the newest notify: tells a
        // late old-block share from a rebuild of the wrong job.
        s.last_reject = format!(
            "{why} job={job_id} newest={} clean_ago={}s",
            s.last_job_id,
            diag::uptime_s().saturating_sub(s.last_clean_uptime_s)
        );
        if stale {
            let since = diag::uptime_s().saturating_sub(s.last_clean_uptime_s);
            let b = if since < 2 {
                0
            } else if since < 5 {
                1
            } else if since < 30 {
                2
            } else {
                3
            };
            s.stale_after_clean_hist[b] += 1;
        }
    });
}

// ---------------------------------------------------------------------
// Stratum V2
// ---------------------------------------------------------------------

/// What an Sv2 pool needs beyond host, port and worker.
struct V2Pool {
    /// The pool's authority key, Base58Check; empty = none.
    authority: String,
    /// Refuse to connect without a verified certificate.
    require_auth: bool,
}

/// Seconds to wait for SNTP before a certificate check is given up.
const CLOCK_WAIT_S: u32 = 20;
/// For this long after boot a pool certificate that fails while NTP has
/// not set the clock yet is the clock's fault, not the pool's: it does
/// not count towards the fallback. After that the clock is what it is.
const CLOCK_GRACE_S: u64 = 300;
/// Set by a V2 session whose certificate check failed against a clock
/// NTP had not confirmed; `run` takes it and skips the failure count.
static CLOCK_UNSURE: AtomicBool = AtomicBool::new(false);

/// The Sv2 session on a thread whose stack lives in PSRAM: the Noise
/// handshake (secp256k1) wants more stack than the V1 thread carries,
/// and internal RAM is short.
fn session_v2_on_psram(
    host: String,
    port: u16,
    user: String,
    pool: V2Pool,
    fallback: bool,
) -> anyhow::Result<()> {
    // The error itself, not its text: `run` tells a stuck pool
    // (`StaleJob`) from other ends by its type.
    let (tx, rx) = channel::<anyhow::Result<()>>();
    let handle = diag::spawn_psram("sv2", 32 * 1024, move || {
        let r = session_v2(&host, port, &user, &pool, fallback);
        let _ = tx.send(r);
    })
    .map_err(|e| anyhow::anyhow!("sv2 thread: {e}"))?;
    let result = rx.recv();
    let _ = handle.join();
    match result {
        Ok(r) => r,
        Err(_) => anyhow::bail!("sv2 thread ended without a result"),
    }
}

/// Read exactly `buf.len()` bytes from the link.
fn read_exact(stream: &Link, buf: &mut [u8]) -> anyhow::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        let n = stream.recv(&mut buf[got..])?;
        if n == 0 {
            anyhow::bail!("pool closed the connection during the handshake");
        }
        got += n;
    }
    Ok(())
}

fn send_frame(stream: &Link, transport: &Mutex<Transport>, frame: &Frame) -> anyhow::Result<()> {
    let bytes = transport
        .lock()
        .unwrap()
        .seal(frame)
        .map_err(|e| anyhow::anyhow!("sv2: {e}"))?;
    stream.send(&bytes)?;
    Ok(())
}

/// One Stratum V2 session: handshake, setup, an extended channel, jobs
/// into the feed, shares out through the sender thread. Ends with an
/// error, like the V1 session.
fn session_v2(
    host: &str,
    port: u16,
    user: &str,
    pool: &V2Pool,
    fallback: bool,
) -> anyhow::Result<()> {
    let key = pool.authority.trim();
    let mut authority = if key.is_empty() {
        if pool.require_auth {
            anyhow::bail!("sv2: pool authentication required, but no authority key is set");
        }
        None
    } else {
        Some(warpaxe_sv2::authority_key_from_b58(key).map_err(|e| anyhow::anyhow!("sv2: {e}"))?)
    };
    // The certificate carries a validity window; without a clock it
    // cannot be checked, and a clock carried across the reset in RTC
    // memory is not trusted before NTP has confirmed it this boot. That
    // second wait is for the boot's first minutes only: with NTP
    // unreachable for good, every reconnect would otherwise pay it.
    let early = || diag::uptime_s() < CLOCK_GRACE_S;
    let mut now = clock::unix_now();
    let mut waited = 0;
    while authority.is_some()
        && (now.is_none() || (clock::ntp_pending() && early()))
        && waited < CLOCK_WAIT_S
    {
        std::thread::sleep(Duration::from_secs(1));
        waited += 1;
        now = clock::unix_now();
    }
    // Read once, with `now`: a sync between here and the handshake would
    // otherwise count a failure the stale `now` caused.
    let clock_unsure = authority.is_some() && now.is_some() && clock::ntp_pending() && early();
    if clock_unsure {
        log::warn!("stratum: sv2: NTP has not confirmed the clock yet, checking the pool certificate against it anyway");
    }
    if authority.is_some() && now.is_none() {
        if pool.require_auth {
            anyhow::bail!("sv2: no clock yet (NTP) to check the pool certificate");
        }
        log::warn!("stratum: sv2: no clock yet (NTP), the pool certificate is not checked");
        authority = None;
    }
    let now = now.unwrap_or(0) as u32;

    let stream = Arc::new(Link::connect(host, port, false, None)?);

    *CURRENT_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
    // Until the first job the link waits `link::SETUP_TIMEOUT` for the
    // pool, then the full `link::READ_TIMEOUT`.
    let mut has_work = false;
    let jobs_at_start = state().jobs_received;
    let ip = stream
        .peer()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    update(|s| s.pool_ip = ip);
    // Settings changed while this session was connecting (DNS, connect,
    // TLS): `reconnect` found no stream to shut, so end here and let
    // `run` start over with the new config.
    if RECONNECT.load(Ordering::Relaxed) {
        return Ok(());
    }
    if fallback {
        *FALLBACK_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
    }
    let (hs, act1) = Handshake::start(authority).map_err(|e| anyhow::anyhow!("sv2: {e}"))?;
    stream.send(&act1)?;
    let mut act2 = [0u8; warpaxe_sv2::ACT_2_LEN];
    read_exact(&stream, &mut act2)?;
    let transport = hs.finish(act2, now).map_err(|e| {
        // A certificate that fails against a clock NTP has not confirmed
        // is the clock's doing until proven otherwise: `run` does not
        // count it towards the fallback.
        if clock_unsure && matches!(e, warpaxe_sv2::Error::Certificate(_)) {
            CLOCK_UNSURE.store(true, Ordering::Relaxed);
        }
        anyhow::anyhow!("sv2: {e}")
    })?;
    log::info!(
        "stratum: sv2 handshake done{}",
        if authority.is_some() {
            ", pool certificate verified"
        } else {
            ", pool not verified"
        }
    );
    let transport = Arc::new(Mutex::new(transport));
    let local_ip = stream.local_ip();
    update(|s| {
        s.connected = true;
        s.local_ip = local_ip;
        s.last_error.clear();
    });

    let m = miner::state();
    let hz = if m.frequency_hz > 0 {
        m.frequency_hz
    } else {
        config::get().frequency.saturating_mul(1_000_000)
    };
    #[allow(unused_mut)]
    let mut nominal =
        f64::from(board::chip().small_cores) * f64::from(hz) * f64::from(m.chips.max(1));
    // The devkit: cores × clock would claim 215 GH/s and the pool would set
    // a target the ESP never meets; its measured rate instead.
    if board::devkit() {
        nominal = crate::cpuminer::nominal_hash_rate();
    }
    let client = Arc::new(Mutex::new(Client::new(ClientConfig {
        user: user.into(),
        vendor: "dvb-projekt".into(),
        hardware_version: board_id().into(),
        firmware: agent().into(),
        device_id: String::new(),
        endpoint_host: host.into(),
        endpoint_port: port,
        nominal_hash_rate: nominal as f32,
        min_extranonce_size: 4,
    })));
    let hello = client.lock().unwrap().hello();
    send_frame(&stream, &transport, &hello)?;

    let (tx, rx) = channel::<Outgoing>();
    *SUBMIT.lock().unwrap() = Some(tx);
    {
        let stream = Arc::clone(&stream);
        let transport = Arc::clone(&transport);
        let client = Arc::clone(&client);
        // The submit thread's stack in PSRAM: 6 KB of internal RAM in one
        // piece is not always there on a reconnect (4.8 KB largest block
        // seen on a NerdOctaxe on TLS, 2026-11-13), and without the thread
        // the session hashed on and sent no share, with no word of it.
        // It only sends on the link, never touches flash.
        diag::spawn_psram("submit", 6 * 1024, move || {
                while let Ok(out) = rx.recv() {
                    let Outgoing::V2 {
                        job_id,
                        extranonce2,
                        ntime,
                        nonce,
                        version,
                    } = out
                    else {
                        continue; // a share from before the protocol switch
                    };
                    let frame =
                        client
                            .lock()
                            .unwrap()
                            .submit(job_id, &extranonce2, ntime, nonce, version);
                    match frame {
                        Ok(f) => {
                            if send_frame(&stream, &transport, &f).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::warn!("stratum: sv2 share dropped: {e}"),
                    }
                }
        })
        .map_err(|e| {
            // No thread to take them: shares go nowhere until the next session.
            *SUBMIT.lock().unwrap() = None;
            anyhow::anyhow!("submit thread not started: {e}")
        })?;
    }
    let _drop_sender_on_exit = SenderGuard;
    let started = Instant::now();
    let mut buf = vec![0u8; 1024];
    // No new job for this long ends the session, as on V1
    // (`warpaxe_stratum::stale`). Sv2 pools refresh the job every 30 s to
    // two minutes even without a new block (measured 2026-11-20 on
    // Braiins and Blitzpool over two hours, a 77 min block gap included),
    // so the V1 limits hold. A stuck pool keeps answering shares; the
    // check runs whenever anything arrives, and a pool that sends nothing
    // at all ends the session by the read timeout.
    let stale_limit = {
        let mut w = STALE.lock().unwrap();
        w.refresh();
        w.limit_s()
    };
    let mut last_job = Instant::now();
    let mut first_job: Option<Instant> = None;
    let mut fresh_seen = false;
    loop {
        let since_job = last_job.elapsed().as_secs();
        if warpaxe_stratum::stale::is_stale(has_work, since_job, stale_limit) {
            return Err(StaleJob {
                secs: since_job,
                pings: 0,
            }
            .into());
        }
        if warpaxe_stratum::stale::no_first_job(has_work, started.elapsed().as_secs()) {
            anyhow::bail!(
                "sv2: no job from the pool {} s after the channel opened",
                started.elapsed().as_secs()
            );
        }
        let n = stream.recv(&mut buf)?;
        if n == 0 {
            anyhow::bail!("pool closed the connection");
        }
        let frames = {
            let mut t = transport.lock().unwrap();
            t.feed(&buf[..n]);
            let mut v = Vec::new();
            while let Some(f) = t.next_frame().map_err(|e| anyhow::anyhow!("sv2: {e}"))? {
                v.push(f);
            }
            v
        };
        for frame in frames {
            let mut out = Vec::new();
            let events = client
                .lock()
                .unwrap()
                .handle(&frame, &mut out)
                .map_err(|e| anyhow::anyhow!("sv2: {e}"))?;
            for f in &out {
                send_frame(&stream, &transport, f)?;
            }
            for ev in events {
                let job = matches!(ev, Event::Job(_));
                on_v2_event(ev, user, &started)?;
                if !has_work && state().jobs_received != jobs_at_start {
                    has_work = true;
                    stream.set_read_timeout(crate::link::READ_TIMEOUT);
                    report_back_with_work(fallback);
                }
                if job {
                    last_job = Instant::now();
                    match first_job {
                        None => first_job = Some(last_job),
                        Some(t)
                            if !fresh_seen
                                && warpaxe_stratum::stale::is_fresh(t.elapsed().as_secs()) =>
                        {
                            // Fresh work, not just the jobs right after the
                            // channel opened.
                            fresh_seen = true;
                            if STALE.lock().unwrap().fresh(fallback) {
                                BACK_UNREPORTED.store(false, Ordering::Relaxed);
                                crate::events::record(
                                    "pool",
                                    warpaxe_diag::events::detail::pool_back(),
                                );
                            }
                            if !fallback {
                                PROBE_EVERY_S.store(PRIMARY_PROBE, Ordering::Relaxed);
                            }
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
}

fn on_v2_event(ev: Event, user: &str, started: &Instant) -> anyhow::Result<()> {
    match ev {
        Event::SetupDone {
            used_version,
            flags,
        } => {
            log::info!("stratum: sv2 setup ok, protocol version {used_version}, flags {flags:#x}");
            update(|s| s.subscribed = true);
        }
        Event::ChannelOpen {
            channel_id,
            extranonce2_size,
            extranonce_prefix,
        } => {
            let prefix = hex::encode(&extranonce_prefix);
            log::info!(
                "stratum: sv2 channel {channel_id} open as {user}, extranonce prefix {prefix}, extranonce2 size {extranonce2_size}"
            );
            update(|s| {
                s.authorized = true;
                s.extranonce1 = prefix;
                s.extranonce2_size = extranonce2_size;
            });
        }
        Event::Difficulty(d) => {
            log::info!("stratum: difficulty {}", fmt_difficulty(d));
            update(|s| s.difficulty = d);
        }
        Event::Job(t) => {
            let clean = t.clean;
            let job_id = t.job_id.to_string();
            let height = coinbase::block_height(&t.coinbase_prefix).unwrap_or(0);
            let net_diff = coinbase::nbits_difficulty(t.nbits);
            let tag = coinbase::script_text(
                &t.coinbase_prefix,
                &t.coinbase_suffix,
                t.extranonce_prefix.len() + t.extranonce2_size,
            );
            let pay = payout::payout(
                &t.coinbase_prefix,
                t.extranonce_prefix.len() + t.extranonce2_size,
                &t.coinbase_suffix,
                user,
            );
            let mask = t.version_mask;
            if started.elapsed() < Duration::from_secs(5) || clean {
                log::info!("stratum: sv2 job {job_id} ntime {} clean {clean}", t.ntime);
            }
            publish(Feed::V2(t), clean);
            update(|st| {
                st.version_mask = mask;
                st.last_job_id = job_id;
                st.block_height = height;
                st.network_difficulty = net_diff;
                st.scriptsig = tag;
                set_payout(st, pay, user);
            });
        }
        Event::ExtranoncePrefix(p) => {
            let prefix = hex::encode(&p);
            log::info!("stratum: sv2 extranonce prefix now {prefix}");
            update(|s| s.extranonce1 = prefix);
        }
        Event::Accepted { count, .. } => update(|s| s.shares_accepted += u64::from(count)),
        Event::Rejected {
            sequence,
            job_id,
            code,
        } => {
            let job = job_id.map(|j| j.to_string()).unwrap_or_default();
            record_reject(u64::from(sequence), &job, code);
        }
        Event::Reconnect { host, port } => {
            anyhow::bail!("sv2: the pool asks for a reconnect to {host}:{port}")
        }
        Event::Ignored(ty) => log::debug!("stratum: sv2 message {ty:#04x} ignored"),
        Event::Warn(w) => log::warn!("stratum: sv2: {w}"),
    }
    Ok(())
}

/// A share difficulty for the log: pools send it as a float, and values
/// like 29793.999999999996 (float arithmetic on the pool's side, or a Sv2
/// target turned back into a difficulty) read as a different number. At
/// most three decimals, trailing zeros cut, a whole number without any.
fn fmt_difficulty(d: f64) -> String {
    let s = format!("{d:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}
