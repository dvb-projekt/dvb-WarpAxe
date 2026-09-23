//! The Stratum V1 client: one thread, one TCP connection, reconnects on
//! its own. Jobs land in [`current_job`] for the chain driver (not yet
//! attached); everything the API shows is in [`state`].

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_stratum::message::{authorize_request, configure_request, parse_subscribe_result, subscribe_request};
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
}

static STATE: Mutex<Option<StratumState>> = Mutex::new(None);
static JOB: Mutex<Option<Job>> = Mutex::new(None);

pub fn state() -> StratumState {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

fn update(f: impl FnOnce(&mut StratumState)) {
    let mut g = STATE.lock().unwrap();
    let s = g.get_or_insert_with(StratumState::default);
    f(s);
}

/// The latest job built from the newest notify.
pub fn current_job() -> Option<Job> {
    JOB.lock().unwrap().clone()
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
    // No try_clone: ESP-IDF has no dup(). `&TcpStream` reads and writes.
    let mut writer = &stream;
    let mut reader = BufReader::new(&stream);
    update(|s| {
        s.connected = true;
        s.last_error.clear();
    });

    let mut send = |line: String| -> anyhow::Result<()> {
        writer.write_all(line.as_bytes())?;
        writer.flush()?;
        Ok(())
    };
    send(configure_request(1, BIP320_MASK).to_line())?;
    send(subscribe_request(2, AGENT).to_line())?;
    send(authorize_request(3, user, pass).to_line())?;

    let mut sess: Option<Session> = None;
    let mut extranonce2: u32 = 0;
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
                extranonce2 = extranonce2.wrapping_add(1);
                let mut en2 = vec![0u8; s.extranonce2_size];
                let be = extranonce2.to_be_bytes();
                let take = en2.len().min(4);
                en2[s.extranonce2_size - take..].copy_from_slice(&be[4 - take..]);
                match Job::build(s, &n, &en2) {
                    Some(job) => {
                        let clean = job.clean_jobs;
                        update(|st| {
                            st.jobs_received += 1;
                            st.last_job_id = job.job_id.clone();
                            st.last_job_uptime_s = diag::uptime_s();
                            if clean {
                                st.clean_jobs += 1;
                            }
                        });
                        if started.elapsed() < Duration::from_secs(5) || clean {
                            log::info!("stratum: job {} ntime {:08x} clean {}", job.job_id, job.ntime, clean);
                        }
                        *JOB.lock().unwrap() = Some(job);
                    }
                    None => log::warn!("stratum: notify {} did not parse", n.job_id),
                }
            }
            Incoming::Other { method, params } => log::info!("stratum: {method} {params}"),
            Incoming::Unparsed(l) => log::warn!("stratum: unparsed line: {}", &l[..l.len().min(80)]),
        }
    }
}
