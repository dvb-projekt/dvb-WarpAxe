//! The Stratum V1 client: one thread, one TCP connection, reconnects on
//! its own. Jobs land in [`current_job`] for the chain driver (not yet
//! attached); everything the API shows is in [`state`].

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
use warpaxe_stratum::{coinbase, parse_line, Incoming, Job, Session};

use crate::link::{Link, LinkReader};
use crate::{config, diag};

const AGENT: &str = concat!("warpaxe/", env!("CARGO_PKG_VERSION"));
const BIP320_MASK: u32 = 0x1fff_e000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct StratumState {
    pub pool: String,
    /// Connected (or trying) to the fallback pool.
    pub using_fallback: bool,
    /// The live session runs over TLS.
    pub tls: bool,
    /// Height of the block the current job builds (BIP34), 0 = unknown.
    pub block_height: u64,
    /// Network difficulty of the current job (from nbits).
    pub network_difficulty: f64,
    /// Readable text in the coinbase script (the pool's tag).
    pub scriptsig: String,
    pub connected: bool,
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
static NOTIFY: Mutex<Option<(Session, Notify)>> = Mutex::new(None);
static EXTRANONCE2: Mutex<u32> = Mutex::new(0);
static SUBMIT: Mutex<Option<Sender<Request>>> = Mutex::new(None);
static NEXT_ID: Mutex<u64> = Mutex::new(100);

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
pub fn next_job() -> Option<Job> {
    let g = NOTIFY.lock().unwrap();
    let (session, notify) = g.as_ref()?;
    let mut c = EXTRANONCE2.lock().unwrap();
    *c = c.wrapping_add(1);
    let mut en2 = vec![0u8; session.extranonce2_size];
    let be = c.to_be_bytes();
    let take = en2.len().min(4);
    let n = en2.len();
    en2[n - take..].copy_from_slice(&be[4 - take..]);
    Job::build(session, notify, &en2)
}

/// Queue a share. Silently dropped when no session is up.
pub fn submit(job: &Job, ntime: u32, nonce: u32, version_bits: u32) {
    let user = config::get().stratum_user;
    let id = {
        let mut g = NEXT_ID.lock().unwrap();
        *g += 1;
        *g
    };
    let req = submit_request(
        id,
        &user,
        &job.job_id,
        &job.extranonce2_hex(),
        ntime,
        nonce,
        version_bits,
    );
    if let Some(tx) = SUBMIT.lock().unwrap().as_ref() {
        if tx.send(req).is_ok() {
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

fn run() {
    let mut backoff = Duration::from_secs(3);
    let mut fails: u32 = 0;
    let mut on_fallback = false;
    std::thread::Builder::new()
        .stack_size(4096)
        .spawn(probe_primary)
        .ok();
    loop {
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
            on_fallback = true;
            fails = 0;
        }
        let (host, port, tls, user, pass) = if on_fallback {
            (
                cfg.fallback_url.clone(),
                cfg.fallback_port,
                cfg.fallback_tls > 0,
                cfg.fallback_user.clone(),
                cfg.fallback_pass.clone(),
            )
        } else {
            (
                cfg.stratum_url.clone(),
                cfg.stratum_port,
                cfg.stratum_tls > 0,
                cfg.stratum_user.clone(),
                cfg.stratum_pass.clone(),
            )
        };
        let pool = format!("{host}:{port}");
        update(|s| {
            s.pool = pool.clone();
            s.using_fallback = on_fallback;
            s.tls = tls;
            s.connected = false;
            s.subscribed = false;
            s.authorized = false;
        });
        let started = Instant::now();
        let result = session(&host, port, tls, &user, &pass, on_fallback);
        *FALLBACK_STREAM.lock().unwrap() = None;
        if on_fallback && BACK_TO_PRIMARY.swap(false, Ordering::Relaxed) {
            log::warn!("stratum: main pool answers again, leaving the fallback");
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
                s.reconnects += 1;
            });
        }
        if started.elapsed() < SHORT_SESSION {
            if !on_fallback {
                fails += 1;
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(60));
        } else {
            fails = 0;
            backoff = Duration::from_secs(3);
        }
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
    if fallback {
        *FALLBACK_STREAM.lock().unwrap() = Some(Arc::clone(&stream));
    }
    let mut reader = BufReader::new(LinkReader(&stream));
    update(|s| {
        s.connected = true;
        s.last_error.clear();
    });
    {
        let mut hello = String::new();
        for r in [
            configure_request(1, BIP320_MASK),
            subscribe_request(2, AGENT),
            authorize_request(3, user, pass),
        ] {
            hello.push_str(&r.to_line());
        }
        // Ask for a share difficulty (AxeOS sends it right after
        // authorizing); the pool may ignore it.
        let suggest = config::get().suggested_difficulty;
        if suggest > 0 {
            hello.push_str(&suggest_difficulty_request(4, suggest).to_line());
            log::info!("stratum: suggest difficulty {suggest}");
        }
        stream.send(hello.as_bytes())?;
    }
    let pending: Arc<Mutex<HashMap<u64, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = channel::<Request>();
    *SUBMIT.lock().unwrap() = Some(tx);
    {
        let stream = Arc::clone(&stream);
        let pending = Arc::clone(&pending);
        std::thread::Builder::new()
            .stack_size(6 * 1024)
            .spawn(move || {
                while let Ok(req) = rx.recv() {
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
                        log::warn!("stratum: share {id} for {job_id} rejected: {why}");
                        let stale = why.contains("stale");
                        update(|s| {
                            s.shares_rejected += 1;
                            *s.reject_reasons.entry(why.clone()).or_insert(0) += 1;
                            // Which job the pool refused, against the newest
                            // notify: tells a late old-block share from a
                            // rebuild of the wrong job.
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
                }
                _ => log::info!("stratum: response {} -> {} {:?}", r.id, r.result, r.error),
            },
            Incoming::SetDifficulty(d) => {
                log::info!("stratum: difficulty {d}");
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
                if started.elapsed() < Duration::from_secs(5) || clean {
                    log::info!(
                        "stratum: job {} ntime {} clean {}",
                        n.job_id,
                        n.ntime,
                        clean
                    );
                }
                // Publish the job BEFORE the counters: the miner treats a
                // counter change as "new work is there" and builds from
                // NOTIFY at once; the other order let it rebuild the old
                // block's job and mark the new notify as seen (stale shares
                // 5-30 s after every clean).
                *NOTIFY.lock().unwrap() = Some((s.clone(), n));
                update(|st| {
                    st.jobs_received += 1;
                    st.last_job_id = job_id;
                    st.block_height = height;
                    st.network_difficulty = net_diff;
                    st.scriptsig = tag;
                    st.last_job_uptime_s = diag::uptime_s();
                    if clean {
                        st.clean_jobs += 1;
                        st.last_clean_uptime_s = diag::uptime_s();
                    }
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
