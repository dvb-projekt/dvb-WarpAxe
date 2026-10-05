//! Mining on the ESP32-S3 itself: the T-Display-S3 devkit without a
//! BM13xx, behind the `cpu-miner` feature. A toy, never in a release
//! image (tools/release-firmware.sh refuses an image carrying
//! [`SENTINEL`]). Build: `cargo build --release --features cpu-miner`.
//!
//! Three threads replace the chain loop of `miner.rs` when the board
//! profile has no chips: a coordinator that turns Stratum notifies into
//! a job with its SHA-256 midstate and hands out nonce ranges, a worker
//! on core 0 that drives the S3's SHA peripheral (the loop of NerdMiner
//! v2's `minerWorkerHw`, two blocks per nonce, the first from the
//! midstate), and a worker on core 1 hashing in software
//! (`warpaxe_cpuminer::sha256`). Both run at priority 1, above only the
//! idle task, and sleep a tick after every batch so the idle task feeds
//! its watchdog; the peripheral is taken per batch and released between
//! them, so mbedTLS (the online update, a TLS pool) gets its turn.
//!
//! A hash whose top sixteen bits are zero is a candidate: re-hashed in
//! software when it came from the peripheral, scored with
//! `hash_difficulty`, counted in `MinerState`, and submitted when it
//! reaches the pool's share difficulty. Hashrate: counted per worker,
//! 30 s buckets into a 10-minute window as the chain loop does, reported
//! in `MinerState` in GH/s (so a few 1e-4) and in kH/s through
//! [`info`] for the web UI.
//!
//! First start: [`import_nerdminer`] takes the Wi-Fi and the wallet a
//! NerdMiner left in flash (see `warpaxe_cpuminer::nerdminer`).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_svc::nvs::EspNvs;
use esp_idf_svc::sys;
use serde::Serialize;
use warpaxe_cpuminer::nerdminer::{self, ConfigScan};
use warpaxe_cpuminer::sha256;
use warpaxe_stratum::job::hash_difficulty;
use warpaxe_stratum::Job;

use crate::{board, config, diag, miner, stratum};

/// Marks an image built with the feature; the release script greps for
/// it and refuses to publish.
pub const SENTINEL: &str = "WARPAXE-CPU-MINER-BUILD";

/// Nonces per peripheral batch (one acquire/release of the SHA block).
const HW_BATCH: u32 = 4096;
/// Nonces per software batch.
const SW_BATCH: u32 = 512;

/// The job the workers hash: the Stratum job, its constants, and the
/// next unclaimed nonce.
struct CpuJob {
    job: Job,
    /// Notify number the job came from.
    seq: u64,
    midstate: [u32; 8],
    hw_state: [u32; 8],
    tail: [u8; 12],
    tail_words: [u32; 3],
    next_nonce: AtomicU32,
    exhausted: AtomicBool,
}

impl CpuJob {
    fn new(job: Job, seq: u64) -> Self {
        let header = job.header(job.version, 0);
        let midstate = sha256::midstate(&header);
        let tail = sha256::tail(&header);
        CpuJob {
            job,
            seq,
            midstate,
            hw_state: sha256::hw_state(&midstate),
            tail,
            tail_words: sha256::hw_tail_words(&tail),
            next_nonce: AtomicU32::new(0),
            exhausted: AtomicBool::new(false),
        }
    }

    /// The next `n` nonces, or `None` when the space is used up.
    fn take(&self, n: u32) -> Option<u32> {
        if self.exhausted.load(Ordering::Relaxed) {
            return None;
        }
        let start = self.next_nonce.fetch_add(n, Ordering::Relaxed);
        if start.checked_add(n).is_none() {
            self.exhausted.store(true, Ordering::Relaxed);
            return None;
        }
        Some(start)
    }
}

static JOB: Mutex<Option<Arc<CpuJob>>> = Mutex::new(None);
/// Hashes done, wrapping; the coordinator takes the differences.
static HASHES_HW: AtomicU32 = AtomicU32::new(0);
static HASHES_SW: AtomicU32 = AtomicU32::new(0);
static CANDIDATES: AtomicU32 = AtomicU32::new(0);
/// Peripheral hashes the software re-hash did not confirm.
static HW_MISMATCH: AtomicU32 = AtomicU32::new(0);

/// What the web UI shows for the CPU miner.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Info {
    /// Last second, peripheral worker.
    #[serde(rename = "hwKhs")]
    pub hw_khs: f64,
    /// Last second, software worker.
    #[serde(rename = "swKhs")]
    pub sw_khs: f64,
    /// Last 30 s, both.
    #[serde(rename = "khsNow")]
    pub khs_now: f64,
    /// 10-minute window, both.
    #[serde(rename = "khs10m")]
    pub khs_10m: f64,
    /// Hashes with their top sixteen bits zero since boot.
    pub candidates: u32,
    /// Peripheral results the software re-hash rejected (should stay 0).
    #[serde(rename = "hwMismatch")]
    pub hw_mismatch: u32,
    /// Nonces of the current job handed out so far.
    #[serde(rename = "nonceProgress")]
    pub nonce_progress: u32,
}
static INFO: Mutex<Option<Info>> = Mutex::new(None);

pub fn info() -> Info {
    let mut i = INFO.lock().unwrap().clone().unwrap_or_default();
    i.candidates = CANDIDATES.load(Ordering::Relaxed);
    i.hw_mismatch = HW_MISMATCH.load(Ordering::Relaxed);
    i.nonce_progress = JOB
        .lock()
        .unwrap()
        .as_ref()
        .map_or(0, |j| j.next_nonce.load(Ordering::Relaxed));
    i
}

/// Start the coordinator and the two workers.
pub fn start() {
    log::warn!("cpu miner: {SENTINEL}: hashing on the ESP32-S3 itself, a toy, never a release");
    std::thread::Builder::new()
        .name("cpu-miner".into())
        .stack_size(12 * 1024)
        .spawn(run)
        .expect("cpu miner thread");
    spawn_pinned(c"cpu-hw", 0, 6 * 1024, || {
        worker(hw_batch, HW_BATCH, &HASHES_HW)
    });
    spawn_pinned(c"cpu-sw", 1, 6 * 1024, || {
        worker(sw_batch, SW_BATCH, &HASHES_SW)
    });
}

/// A thread pinned to a core at priority 1 (just above idle).
fn spawn_pinned(
    name: &'static std::ffi::CStr,
    core: i32,
    stack: usize,
    f: impl FnOnce() + Send + 'static,
) {
    unsafe {
        let mut cfg = sys::esp_pthread_get_default_config();
        cfg.stack_size = stack;
        cfg.prio = 1;
        cfg.pin_to_core = core;
        cfg.thread_name = name.as_ptr();
        sys::esp_pthread_set_cfg(&cfg);
    }
    let r = std::thread::Builder::new().stack_size(stack).spawn(f);
    unsafe {
        let d = sys::esp_pthread_get_default_config();
        sys::esp_pthread_set_cfg(&d);
    }
    r.expect("cpu worker thread");
}

fn worker(scan: fn(&CpuJob, u32, u32), batch: u32, counter: &AtomicU32) {
    loop {
        let job = JOB.lock().unwrap().clone();
        let Some(job) = job else {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Some(start) = job.take(batch) else {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        scan(&job, start, batch);
        counter.fetch_add(batch, Ordering::Relaxed);
        // One tick for the idle task (its watchdog) and anything else
        // at this priority.
        unsafe { sys::vTaskDelay(1) };
    }
}

fn sw_batch(job: &CpuJob, start: u32, n: u32) {
    for i in 0..n {
        let nonce = start.wrapping_add(i);
        let hash = sha256::sha256d_tail(&job.midstate, &job.tail, nonce);
        if sha256::candidate(&hash) {
            found(job, nonce, hash, false);
        }
    }
}

// The S3's SHA peripheral (soc/hwcrypto_reg.h): a message block goes
// into TEXT as sixteen words, the digest state lives in H as eight.
const SHA_BASE: usize = 0x6003_B000;
const SHA_MODE: usize = SHA_BASE;
const SHA_START: usize = SHA_BASE + 0x10;
const SHA_CONTINUE: usize = SHA_BASE + 0x14;
const SHA_BUSY: usize = SHA_BASE + 0x18;
const SHA_H: usize = SHA_BASE + 0x40;
const SHA_TEXT: usize = SHA_BASE + 0x80;
const SHA2_256: u32 = 2;

extern "C" {
    // mbedTLS port (sha/sha_dma.h): the crypto lock plus the block's clock.
    fn esp_sha_acquire_hardware();
    fn esp_sha_release_hardware();
}

#[inline(always)]
unsafe fn reg_w(addr: usize, v: u32) {
    core::ptr::write_volatile(addr as *mut u32, v);
}

#[inline(always)]
unsafe fn reg_r(addr: usize) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

#[inline(always)]
unsafe fn wait_idle() {
    while reg_r(SHA_BUSY) != 0 {}
}

fn hw_batch(job: &CpuJob, start: u32, n: u32) {
    unsafe {
        esp_sha_acquire_hardware();
        reg_w(SHA_MODE, SHA2_256);
        // TEXT[9..15] are zero in both blocks and never touched by the
        // engine: once per batch.
        for i in 9..15 {
            reg_w(SHA_TEXT + 4 * i, 0);
        }
        for i in 0..n {
            let nonce = start.wrapping_add(i);
            // First hash, second block, from the midstate.
            for (k, w) in job.hw_state.iter().enumerate() {
                reg_w(SHA_H + 4 * k, *w);
            }
            reg_w(SHA_TEXT, job.tail_words[0]);
            reg_w(SHA_TEXT + 4, job.tail_words[1]);
            reg_w(SHA_TEXT + 8, job.tail_words[2]);
            reg_w(SHA_TEXT + 12, nonce);
            reg_w(SHA_TEXT + 16, sha256::HW_PAD);
            reg_w(SHA_TEXT + 20, 0);
            reg_w(SHA_TEXT + 24, 0);
            reg_w(SHA_TEXT + 28, 0);
            reg_w(SHA_TEXT + 32, 0);
            reg_w(SHA_TEXT + 60, sha256::HW_LEN_640);
            reg_w(SHA_CONTINUE, 1);
            wait_idle();
            // Second hash: the digest, padded, as a fresh block.
            for k in 0..8 {
                reg_w(SHA_TEXT + 4 * k, reg_r(SHA_H + 4 * k));
            }
            reg_w(SHA_TEXT + 32, sha256::HW_PAD2);
            reg_w(SHA_TEXT + 60, sha256::HW_LEN_256);
            reg_w(SHA_START, 1);
            wait_idle();
            let h7 = reg_r(SHA_H + 28);
            if sha256::hw_candidate(h7) {
                let mut h = [0u32; 8];
                for (k, w) in h.iter_mut().enumerate() {
                    *w = reg_r(SHA_H + 4 * k);
                }
                let hash = sha256::hw_hash(&h);
                esp_sha_release_hardware();
                found(job, nonce, hash, true);
                esp_sha_acquire_hardware();
                reg_w(SHA_MODE, SHA2_256);
                for k in 9..15 {
                    reg_w(SHA_TEXT + 4 * k, 0);
                }
            }
        }
        esp_sha_release_hardware();
    }
}

/// A candidate from either worker: verify, score, count, submit.
fn found(job: &CpuJob, nonce: u32, hash: [u8; 32], from_hw: bool) {
    if from_hw && sha256::sha256d_tail(&job.midstate, &job.tail, nonce) != hash {
        HW_MISMATCH.fetch_add(1, Ordering::Relaxed);
        log::warn!("cpu miner: peripheral hash for nonce {nonce:08x} failed the software re-hash");
        return;
    }
    CANDIDATES.fetch_add(1, Ordering::Relaxed);
    let diff = hash_difficulty(&hash);
    let st = stratum::state();
    let want = if st.difficulty > 0.0 {
        st.difficulty
    } else {
        1.0
    };
    let header = job.job.header(job.job.version, nonce);
    let hex: String = header.iter().map(|b| format!("{b:02x}")).collect();
    miner::update_state(|s| {
        s.nonces += 1;
        s.last_nonce_uptime_s = diag::uptime_s();
        s.best_session_diff = s.best_session_diff.max(diff as u64);
        s.last_found = format!(
            "{} nonce={nonce:08x} ntime={:08x} diff={diff:.3} want={want:.3} header={hex}",
            if from_hw { "hw" } else { "sw" },
            job.job.ntime
        );
    });
    config::raise_best_diff(diff as u64);
    if st.network_difficulty > 0.0 && diff >= st.network_difficulty {
        log::warn!(
            "cpu miner: BLOCK FOUND at height {} (diff {diff:.0})",
            st.block_height
        );
        miner::update_state(|s| {
            s.blocks_found += 1;
            s.last_block = Some((st.block_height, diag::uptime_s()));
        });
    }
    if diff >= want {
        let age_n = st.jobs_received.saturating_sub(job.seq);
        stratum::submit(&job.job, job.job.ntime, nonce, 0);
        log::info!(
            "cpu miner: share diff {diff:.3} (want {want:.3}), nonce {nonce:08x}, job age {age_n}"
        );
        miner::update_state(|s| {
            s.shares_sent += 1;
            s.submit_age_hist[(age_n as usize).min(3)] += 1;
            s.last_submit_age = format!("{age_n} notifies");
        });
    } else {
        miner::update_state(|s| s.low_diff += 1);
    }
}

/// The coordinator: jobs from Stratum, the hashrate from the counters.
fn run() {
    let epoch = Instant::now();
    let mut last_hw = 0u32;
    let mut last_sw = 0u32;
    let mut last_tick = Instant::now();
    let mut hashes = 0f64;
    let mut hashes_since = Instant::now();
    let mut buckets: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
    miner::update_state(|s| {
        s.phase = "CPU".into();
        s.frequency_hz = 240_000_000;
    });
    loop {
        if board::shutdown() {
            if JOB.lock().unwrap().take().is_some() {
                log::info!("cpu miner: stopped");
            }
            miner::update_state(|s| {
                s.phase = "OFF".into();
                s.hashrate_ghs = 0.0;
                s.hashrate_now_ghs = 0.0;
            });
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let st = stratum::state();
        let need = JOB.lock().unwrap().as_ref().map_or(true, |j| {
            j.seq != st.jobs_received || j.exhausted.load(Ordering::Relaxed)
        });
        if need {
            match stratum::next_job() {
                Some((job, seq, _clean_seq)) => {
                    let want = if st.difficulty > 0.0 {
                        st.difficulty
                    } else {
                        1.0
                    };
                    log::info!(
                        "cpu miner: job {} from notify {seq}, share difficulty {want:.3}",
                        job.job_id
                    );
                    *JOB.lock().unwrap() = Some(Arc::new(CpuJob::new(job, seq)));
                    miner::update_state(|s| {
                        s.jobs_sent += 1;
                        s.phase = "CPU".into();
                        s.difficulty = want.ceil().max(1.0) as u32;
                    });
                }
                None => {
                    if JOB.lock().unwrap().is_none() {
                        miner::update_state(|s| s.phase = "NO JOB".into());
                    }
                }
            }
        }
        let dt = last_tick.elapsed().as_secs_f64();
        if dt >= 1.0 {
            last_tick = Instant::now();
            let hw = HASHES_HW.load(Ordering::Relaxed);
            let sw = HASHES_SW.load(Ordering::Relaxed);
            let d_hw = f64::from(hw.wrapping_sub(last_hw));
            let d_sw = f64::from(sw.wrapping_sub(last_sw));
            last_hw = hw;
            last_sw = sw;
            hashes += d_hw + d_sw;
            {
                let mut g = INFO.lock().unwrap();
                let i = g.get_or_insert_with(Info::default);
                i.hw_khs = d_hw / dt / 1e3;
                i.sw_khs = d_sw / dt / 1e3;
            }
            let elapsed = hashes_since.elapsed().as_secs_f64();
            if elapsed >= 30.0 {
                let now_s = epoch.elapsed().as_secs_f64();
                buckets.push_back((now_s - elapsed, hashes));
                while buckets.front().is_some_and(|(t, _)| now_s - *t > 600.0) {
                    buckets.pop_front();
                }
                let sum: f64 = buckets.iter().map(|(_, h)| *h).sum();
                let span = buckets
                    .front()
                    .map_or(elapsed, |(t, _)| (now_s - *t).max(elapsed));
                let hs_10m = sum / span;
                let hs_now = hashes / elapsed;
                miner::update_state(|s| {
                    s.hashrate_ghs = hs_10m / 1e9;
                    s.hashrate_now_ghs = hs_now / 1e9;
                });
                if let Some(i) = INFO.lock().unwrap().as_mut() {
                    i.khs_now = hs_now / 1e3;
                    i.khs_10m = hs_10m / 1e3;
                }
                log::info!(
                    "cpu miner: {:.1} kH/s (30 s), {:.1} kH/s (10 min), candidates {}, shares {}",
                    hs_now / 1e3,
                    hs_10m / 1e3,
                    CANDIDATES.load(Ordering::Relaxed),
                    miner::state().shares_sent
                );
                hashes = 0.0;
                hashes_since = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// First start on the devkit: the Wi-Fi and the wallet a NerdMiner left
/// in flash, where WarpAxe has nothing yet. Returns what was taken,
/// never a value. The pool is WarpPool, whatever the NerdMiner mined to.
pub fn import_nerdminer() -> Vec<&'static str> {
    let mut cfg = config::get();
    let mut took = Vec::new();
    if cfg.ssid.is_empty() {
        if let Some(part) = config::partition() {
            match EspNvs::new(part, nerdminer::WIFI_NAMESPACE, false) {
                Ok(nvs) => {
                    let mut buf = [0u8; 96];
                    let ssid = nvs
                        .get_raw(nerdminer::WIFI_SSID_KEY, &mut buf)
                        .ok()
                        .flatten()
                        .and_then(nerdminer::wifi_ssid);
                    if let Some(ssid) = ssid {
                        let mut pbuf = [0u8; 96];
                        cfg.wifi_pass = nvs
                            .get_raw(nerdminer::WIFI_PASSWORD_KEY, &mut pbuf)
                            .ok()
                            .flatten()
                            .and_then(nerdminer::wifi_password)
                            .unwrap_or_default();
                        cfg.ssid = ssid;
                        took.push("wifi (nerdminer)");
                    } else {
                        log::info!("cpu miner: no station SSID in the Wi-Fi library's storage");
                    }
                }
                Err(e) => log::info!("cpu miner: no Wi-Fi library namespace in NVS ({e})"),
            }
        }
    }
    if cfg.stratum_user.is_empty() {
        let mut scan = ConfigScan::default();
        let mut buf = vec![0u8; 4096];
        let end = nerdminer::SPIFFS_FLASH_ADDR + nerdminer::SPIFFS_SIZE;
        let mut addr = nerdminer::SPIFFS_FLASH_ADDR;
        while addr < end {
            let rc = unsafe {
                sys::esp_flash_read(
                    core::ptr::null_mut(),
                    buf.as_mut_ptr().cast(),
                    addr,
                    buf.len() as u32,
                )
            };
            if rc != sys::ESP_OK {
                log::warn!("cpu miner: flash read at {addr:#x} failed ({rc})");
                break;
            }
            scan.feed(&buf);
            addr += buf.len() as u32;
        }
        let copies = scan.copies;
        match scan.result().and_then(|c| nerdminer::stratum_user(&c.btc)) {
            Some(user) => {
                cfg.stratum_user = user;
                cfg.stratum_url = nerdminer::POOL_HOST.into();
                cfg.stratum_port = nerdminer::POOL_PORT;
                cfg.stratum_protocol = "SV1".into();
                cfg.stratum_tls = 0;
                // The pool's floor: a diff-1 share takes hours at a few
                // hundred kH/s.
                cfg.suggested_difficulty = 1;
                took.push("wallet (nerdminer)");
                log::info!("cpu miner: wallet taken from NerdMiner's config.json ({copies} copies in flash)");
            }
            None => log::info!(
                "cpu miner: no NerdMiner config.json with a wallet in flash ({copies} copies)"
            ),
        }
    }
    if !took.is_empty() {
        if let Err(e) = config::save(&cfg) {
            log::warn!("cpu miner: saving the imported settings failed: {e}");
        }
    }
    took
}
