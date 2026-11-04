//! The pool client: one thread, one connection, reconnects on its own.
//! Stratum V1 (plain or TLS) or Stratum V2 (Noise-encrypted, extended
//! channel) per pool, chosen by the config. Jobs land in [`next_job`] for
//! the chain driver in the same shape either way; everything the API
//! shows is in [`state`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_stratum::message::{
    authorize_request, configure_request, parse_subscribe_result, submit_request,
    subscribe_request, suggest_difficulty_request, Notify, Request,
};
use warpaxe_stratum::{coinbase, parse_line, payout, Incoming, Job, Session};

use crate::link::{Link, LinkReader};
use crate::{board, clock, config, diag, miner};
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

/// The user agent for `mining.subscribe`.
fn agent() -> &'static str {
    if board::devkit() {
        return AGENT_CPU;
    }
    AGENT
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
    pub coinbase_outputs: Vec<(u64, String)>,
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
    pub last_reject: String,
}

static STATE: Mutex<Option<StratumState>> = Mutex::new(None);
/// The newest work from the pool, whichever protocol brought it.
enum Feed {
    V1(Session, Notify),
    V2(Template),
}

/// A share on its way to the pool.
enum Outgoing {
    V1(Request),
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
static EXTRANONCE2: Mutex<u32> = Mutex::new(0);
static SUBMIT: Mutex<Option<Sender<Outgoing>>> = Mutex::new(None);
static NEXT_ID: Mutex<u64> = Mutex::new(100);

/// Keep a job's payout; a job that does not parse clears it rather than
/// leaving the previous pool's split on show.
fn set_payout(st: &mut StratumState, pay: Option<payout::Payout>) {
    let p = pay.unwrap_or_default();
    st.coinbase_outputs = p
        .outputs
        .into_iter()
        .map(|o| (o.value, o.address))
        .collect();
    st.coinbase_total_sat = p.total;
    st.coinbase_user_sat = p.user;
    st.coinbase_own_address = p.own_address;
    st.coinbase_bitcoin = p.bitcoin;
}

pub fn state() -> StratumState {
    STATE.lock().unwrap().clone().unwrap_or_default()
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
        Feed::V1(session, notify) => Job::build(session, notify, &en2),
        Feed::V2(t) => t.build(&en2),
    }?;
    Some((job, *seq, *clean_seq))
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
}

/// Queue a share. Silently dropped when no session is up.
pub fn submit(job: &Job, ntime: u32, nonce: u32, version_bits: u32) {
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
        let user = config::get().stratum_user;
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

pub fn start() {
    std::thread::Builder::new()
        .stack_size(12 * 1024)
        .spawn(run)
        .expect("stratum thread");
}

/// Failures in a row on the main pool before the fallback takes over.
const FAILS_BEFORE_FALLBACK: u32 = 3;
/// A session shorter than this counts as a failure too.
const SHORT_SESSION: Duration = Duration::from_secs(60);
/// While on the fallback, how often the main pool is probed.
const PRIMARY_PROBE: Duration = Duration::from_secs(300);

/// The live stream while on the fallback, so the probe thread can end
/// that session once the main pool answers again.
static FALLBACK_STREAM: Mutex<Option<Arc<Link>>> = Mutex::new(None);
/// The link of the running session, whichever pool: `reconnect` closes
/// it so `run` builds the next session from the current config.
static CURRENT_STREAM: Mutex<Option<Arc<Link>>> = Mutex::new(None);
/// Set by `reconnect`: the session that ends next did so on purpose, so
/// `run` neither counts a failure nor waits for the backoff.
static RECONNECT: AtomicBool = AtomicBool::new(false);

/// Pool settings changed over the API: end the running session now, so
/// the new pool, worker, password, TLS or protocol applies within
/// seconds instead of at the next reboot (Oliver renamed a worker on
/// 2026-10-08 and the pool saw it hours later, at an OTA reboot).
pub fn reconnect() {
    reconnect_because("pool settings changed");
}

/// The same for another reason (`net::eth` moves the session between the
/// cable and Wi-Fi). Not counted in `reconnects`, which counts sessions
/// that ended with an error.
pub fn reconnect_because(why: &str) {
    RECONNECT.store(true, Ordering::Relaxed);
    if let Some(s) = CURRENT_STREAM.lock().unwrap().as_ref() {
        log::info!("stratum: {why}, reconnecting");
        s.shutdown();
    } else {
        log::info!("stratum: {why}, the next session takes it into account");
    }
}

fn run() {
    let mut backoff = Duration::from_secs(3);
    let mut fails: u32 = 0;
    let mut on_fallback = false;
    std::thread::Builder::new()
        .stack_size(4096)
        .spawn(probe_primary)
        .ok();
    loop {
        // A change that came while no session was up (connecting, or in
        // the backoff) applies now: this pass reads the new config.
        if RECONNECT.swap(false, Ordering::Relaxed) {
            on_fallback = false;
            fails = 0;
            backoff = Duration::from_secs(3);
        }
        let cfg = config::get();
        if cfg.stratum_url.is_empty() || cfg.stratum_user.is_empty() {
            update(|s| s.last_error = "no pool or user configured".into());
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        let fallback_ok =
            cfg.use_fallback && !cfg.fallback_url.is_empty() && !cfg.fallback_user.is_empty();
        if !fallback_ok {
            on_fallback = false;
        } else if !on_fallback && fails >= FAILS_BEFORE_FALLBACK {
            log::warn!("stratum: main pool failed {fails} times, switching to the fallback");
            crate::events::record("pool", warpaxe_diag::events::detail::pool_fallback(fails));
            on_fallback = true;
            fails = 0;
        }
        let (host, port, tls, user, pass, v2) = if on_fallback {
            (
                cfg.fallback_url.clone(),
                cfg.fallback_port,
                cfg.fallback_tls > 0,
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
                cfg.stratum_user.clone(),
                cfg.stratum_pass.clone(),
                cfg.sv2().then(|| V2Pool {
                    authority: cfg.stratum_v2_authority_pubkey.clone(),
                    require_auth: cfg.stratum_v2_require_auth,
                }),
            )
        };
        let pool = format!("{host}:{port}");
        let sv2 = v2.is_some();
        update(|s| {
            s.pool = pool.clone();
            s.using_fallback = on_fallback;
            s.tls = tls && !sv2;
            s.sv2 = sv2;
            s.connected = false;
            s.local_ip = None;
            s.subscribed = false;
            s.authorized = false;
        });
        let started = Instant::now();
        // Only this session's verdict counts (a `continue` above could
        // have left the last one's flag standing).
        CLOCK_UNSURE.store(false, Ordering::Relaxed);
        let result = match v2 {
            Some(v2) => session_v2_on_psram(host.clone(), port, user.clone(), v2, on_fallback),
            None => session(&host, port, tls, &user, &pass, on_fallback),
        };
        *FALLBACK_STREAM.lock().unwrap() = None;
        *CURRENT_STREAM.lock().unwrap() = None;
        if RECONNECT.swap(false, Ordering::Relaxed) {
            // Ended on purpose after a settings change: straight back in
            // with the new settings, main pool first.
            update(|s| s.connected = false);
            on_fallback = false;
            fails = 0;
            backoff = Duration::from_secs(3);
            continue;
        }
        if on_fallback && BACK_TO_PRIMARY.swap(false, Ordering::Relaxed) {
            log::warn!("stratum: main pool answers again, leaving the fallback");
            crate::events::record("pool", warpaxe_diag::events::detail::pool_back());
            on_fallback = false;
            fails = 0;
            backoff = Duration::from_secs(3);
            continue;
        }
        // Sessions only end with an error (a closed or broken
        // connection). One that ran for a while was a working pool: start
        // counting again. A connect failure or a session under a minute
        // counts towards the fallback.
        if let Err(e) = &result {
            log::warn!("stratum: {e}");
            update(|s| {
                s.last_error = e.to_string();
                s.connected = false;
                s.local_ip = None;
                s.reconnects += 1;
            });
        }
        if started.elapsed() < SHORT_SESSION {
            if CLOCK_UNSURE.swap(false, Ordering::Relaxed) {
                log::warn!("stratum: the pool certificate failed against a clock NTP has not set yet: not counted towards the fallback");
            } else if !on_fallback {
                fails += 1;
            }
            pause_unless_reconnect(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(60));
        } else {
            fails = 0;
            backoff = Duration::from_secs(3);
        }
    }
}

/// The backoff, cut short by a settings change: a saved pool chosen while
/// the old one keeps failing applies within a second, not a minute.
fn pause_unless_reconnect(d: Duration) {
    let end = Instant::now() + d;
    while Instant::now() < end && !RECONNECT.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(250));
    }
}

static BACK_TO_PRIMARY: AtomicBool = AtomicBool::new(false);

/// While on the fallback: every few minutes, try a TCP connect to the main
/// pool; when it answers, end the fallback session so `run` goes back.
fn probe_primary() {
    loop {
        std::thread::sleep(PRIMARY_PROBE);
        if !state().using_fallback {
            continue;
        }
        let cfg = config::get();
        let target = format!("{}:{}", cfg.stratum_url, cfg.stratum_port);
        let reachable = target
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .is_some_and(|a| TcpStream::connect_timeout(&a, Duration::from_secs(5)).is_ok());
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
    user: &str,
    pass: &str,
    fallback: bool,
) -> anyhow::Result<()> {
    // No try_clone: ESP-IDF has no dup(). The link is read on this thread
    // and written on a small writer thread (see `link`).
    let stream = Arc::new(Link::connect(host, port, tls)?);
    *CURRENT_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
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
        s.local_ip = local_ip;
        s.last_error.clear();
    });
    {
        let mut hello = String::new();
        for r in [
            configure_request(1, BIP320_MASK),
            subscribe_request(2, agent()),
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
        std::thread::Builder::new()
            .stack_size(6 * 1024)
            .spawn(move || {
                while let Ok(out) = rx.recv() {
                    let Outgoing::V1(req) = out else {
                        continue; // a share from before the protocol switch
                    };
                    if let Some(job_id) = req.params.get(1).and_then(|v| v.as_str()) {
                        pending.lock().unwrap().insert(req.id, job_id.to_string());
                    }
                    if stream.send(req.to_line().as_bytes()).is_err() {
                        break;
                    }
                }
            })
            .ok();
    }
    let _drop_sender_on_exit = SenderGuard;

    let mut sess: Option<Session> = None;
    let mut line = String::new();
    let started = Instant::now();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
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
                    log::warn!("stratum: notify before subscribe, ignored");
                    continue;
                };
                let clean = n.clean_jobs;
                let job_id = n.job_id.clone();
                let c1 = hex::decode(&n.coinb1).unwrap_or_default();
                let c2 = hex::decode(&n.coinb2).unwrap_or_default();
                let height = coinbase::block_height(&c1).unwrap_or(0);
                let net_diff = u32::from_str_radix(&n.nbits, 16)
                    .map(coinbase::nbits_difficulty)
                    .unwrap_or(0.0);
                let tag = coinbase::script_text(&c1, &c2, s.extranonce1.len() + s.extranonce2_size);
                let pay = payout::payout(&c1, s.extranonce1.len() + s.extranonce2_size, &c2, user);
                if started.elapsed() < Duration::from_secs(5) || clean {
                    log::info!(
                        "stratum: job {} ntime {} clean {}",
                        n.job_id,
                        n.ntime,
                        clean
                    );
                }
                publish(Feed::V1(s.clone(), n), clean);
                update(|st| {
                    st.last_job_id = job_id;
                    st.block_height = height;
                    st.network_difficulty = net_diff;
                    st.scriptsig = tag;
                    set_payout(st, pay);
                });
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
    let (tx, rx) = channel::<Result<(), String>>();
    let handle = diag::spawn_psram("sv2", 32 * 1024, move || {
        let r = session_v2(&host, port, &user, &pool, fallback);
        let _ = tx.send(r.map_err(|e| e.to_string()));
    })
    .map_err(|e| anyhow::anyhow!("sv2 thread: {e}"))?;
    let result = rx.recv();
    let _ = handle.join();
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => anyhow::bail!("{e}"),
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

    let stream = Arc::new(Link::connect(host, port, false)?);

    *CURRENT_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
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
        hardware_version: board::profile().name.into(),
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
        std::thread::Builder::new()
            .stack_size(6 * 1024)
            .spawn(move || {
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
            .ok();
    }
    let _drop_sender_on_exit = SenderGuard;
    let started = Instant::now();
    let mut buf = vec![0u8; 1024];
    loop {
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
                on_v2_event(ev, user, &started)?;
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
                set_payout(st, pay);
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
