//! The Stratum V1 client: one thread, one TCP connection, reconnects on
//! its own. Jobs land in [`current_job`] for the chain driver (not yet
//! attached); everything the API shows is in [`state`].

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_stratum::message::{authorize_request, configure_request, parse_subscribe_result, submit_request, subscribe_request, Notify, Request};
use warpaxe_stratum::{parse_line, Incoming, Job, Session};

use crate::{config, diag};

const AGENT: &str = concat!("warpaxe/", env!("CARGO_PKG_VERSION"));
const BIP320_MASK: u32 = 0x1fff_e000;

#[derive(Clone, Debug, Default, Serialize)]
pub struct StratumState {
    pub pool: String,
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
    pub reconnects: u64,
    pub last_error: String,
    pub shares_sent: u64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
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
    let req = submit_request(id, &user, &job.job_id, &job.extranonce2_hex(), ntime, nonce, version_bits);
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

fn run() {
    let mut backoff = Duration::from_secs(3);
    loop {
        let cfg = config::get();
        if cfg.stratum_url.is_empty() || cfg.stratum_user.is_empty() {
            update(|s| s.last_error = "no pool or user configured".into());
            std::thread::sleep(Duration::from_secs(10));
            continue;
        }
        let pool = format!("{}:{}", cfg.stratum_url, cfg.stratum_port);
        update(|s| {
            s.pool = pool.clone();
            s.connected = false;
            s.subscribed = false;
            s.authorized = false;
        });
        match session(&pool, &cfg.stratum_user, &cfg.stratum_pass) {
            Ok(()) => backoff = Duration::from_secs(3),
            Err(e) => {
                log::warn!("stratum: {e}");
                update(|s| {
                    s.last_error = e.to_string();
                    s.connected = false;
                    s.reconnects += 1;
                });
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }
}

fn session(pool: &str, user: &str, pass: &str) -> anyhow::Result<()> {
    let addr = pool
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow::anyhow!("dns: no address for {pool}"))?;
    log::info!("stratum: connecting to {pool} ({addr})");
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))?;
    stream.set_read_timeout(Some(Duration::from_secs(180)))?;
    if let Err(e) = stream.set_nodelay(true) {
        log::warn!("stratum: nodelay not set: {e}");
    }
    // No try_clone: ESP-IDF has no dup(). A shared `TcpStream` is read on
    // this thread and written on a small writer thread (sockets are
    // full duplex; `&TcpStream` implements both).
    let stream = Arc::new(stream);
    let mut reader = BufReader::new(&*stream);
    update(|s| {
        s.connected = true;
        s.last_error.clear();
    });
    {
        let mut w = &*stream;
        for r in [configure_request(1, BIP320_MASK), subscribe_request(2, AGENT), authorize_request(3, user, pass)] {
            w.write_all(r.to_line().as_bytes())?;
        }
        w.flush()?;
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
                let mut w = &*stream;
                while let Ok(req) = rx.recv() {
                    if let Some(job_id) = req.params.get(1).and_then(|v| v.as_str()) {
                        pending.lock().unwrap().insert(req.id, job_id.to_string());
                    }
                    if w.write_all(req.to_line().as_bytes()).and_then(|_| w.flush()).is_err() {
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
                    if let Some(m) = r.result.get("version-rolling.mask").and_then(|v| v.as_str()) {
                        let mask = u32::from_str_radix(m, 16).unwrap_or(0);
                        update(|s| s.version_mask = mask);
                        if let Some(s) = sess.as_mut() {
                            s.version_mask = mask;
                        }
                        log::info!("stratum: version rolling mask {m}");
                    }
                }
                2 => match parse_subscribe_result(&r.result).and_then(|(e1, sz)| Session::new(&e1, sz).map(|s| (e1, s))) {
                    Some((e1, mut s)) => {
                        s.version_mask = state().version_mask;
                        log::info!("stratum: subscribed, extranonce1 {e1}, extranonce2 size {}", s.extranonce2_size);
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
                        let why = r.error.map(|e| e.to_string()).unwrap_or_else(|| "rejected".into());
                        log::warn!("stratum: share {id} for {job_id} rejected: {why}");
                        update(|s| {
                            s.shares_rejected += 1;
                            s.last_reject = why;
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
                update(|st| {
                    st.jobs_received += 1;
                    st.last_job_id = n.job_id.clone();
                    st.last_job_uptime_s = diag::uptime_s();
                    if clean {
                        st.clean_jobs += 1;
                    }
                });
                if started.elapsed() < Duration::from_secs(5) || clean {
                    log::info!("stratum: job {} ntime {} clean {}", n.job_id, n.ntime, clean);
                }
                *NOTIFY.lock().unwrap() = Some((s.clone(), n));
            }
            Incoming::Other { method, params } => log::info!("stratum: {method} {params}"),
            Incoming::Unparsed(l) => log::warn!("stratum: unparsed line: {}", &l[..l.len().min(80)]),
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
