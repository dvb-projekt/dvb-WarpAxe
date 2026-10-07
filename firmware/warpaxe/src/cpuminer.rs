//! Mining on the ESP32-S3 itself: the T-Display-S3 without a BM13xx (the
//! NerdMiner devkit, profile `t-display-s3`). Runs instead of the chain
//! loop when the board has no chips (`board::devkit`), never beside it.
//!
//! Three threads: a coordinator that turns Stratum notifies into a job
//! with its SHA-256 midstate and hands out nonce ranges, a worker on
//! core 1 that drives the S3's SHA peripheral (the loop of NerdMiner
//! v2's `minerWorkerHw`, two blocks per nonce, the first from the
//! midstate), and a worker on core 0 hashing in C
//! (`components/warpaxe_glue/warpaxe_cpuhash.c`). Both run at priority
//! 1, above only the idle task, and yield a tick every 200 ms so the
//! idle task feeds its watchdog; the peripheral is taken per batch and
//! released between them, so mbedTLS (the online update, a TLS pool)
//! gets its turn. About 235 kH/s on USB power.
//!
//! A hash whose top sixteen bits are zero is a candidate: re-hashed in
//! software when it came from the peripheral, scored with
//! `hash_difficulty`, counted in `MinerState`, and submitted when it
//! reaches the pool's share difficulty. Hashrate: counted per worker,
//! 30 s buckets into a 10-minute window as the chain loop does, reported
//! in `MinerState` in GH/s (so a few 1e-4) and in kH/s through [`info`]
//! for the web UI. The ESP's own die temperature stands in for the chip
//! temperature; at the profile's `max_temp_c` the workers stop, 10 °C
//! below they resume.
//!
//! First start: [`import_nerdminer`] takes the Wi-Fi and the wallet a
//! NerdMiner left in flash (see `warpaxe_cpuminer::nerdminer`); finding
//! its config.json is what makes a bare board the devkit (board.rs).

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

/// Share difficulty the devkit asks the pool for: what a NerdMiner gets
/// (WarpPool's NerdMiner band is 0.001 to 4, start 0.01). An integer
/// `stratumSuggestedDifficulty` would only ever raise it, so a chipless
/// profile sends this fraction instead (stratum.rs).
pub const SUGGESTED_DIFFICULTY: f64 = 0.005;

/// Nonces per peripheral batch (one acquire/release of the SHA block).
const HW_BATCH: u32 = 4096;
/// Nonces per software batch.
const SW_BATCH: u32 = 512;
/// How long a worker runs batch after batch before it yields a tick to
/// the idle task (its watchdog allows 5 s) and anything else at its
/// priority. A tick after every 18 ms batch cost the peripheral loop
/// 5 %; the SHA lock is held this long at most, which mbedTLS (a TLS
/// pool, the online update) can wait out.
const YIELD_EVERY_US: i64 = 200_000;

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
    /// A start-up probe job: candidates are not reported.
    probe: bool,
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
            probe: false,
        }
    }

    /// A job for the start-up probe: fixed fields, nothing submitted.
    fn probe() -> Self {
        let mut j = CpuJob::new(
            Job {
                job_id: "probe".into(),
                extranonce2: Vec::new(),
                coinbase: Vec::new(),
                merkle_root: [0x5a; 32],
                prev_hash: [0xa5; 32],
                version: 0x2000_0000,
                nbits: 0x1700_0000,
                ntime: 0x6ac3_0000,
                clean_jobs: false,
            },
            0,
        );
        j.probe = true;
        j
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
/// Best candidate difficulty since boot, with its decimals (the shared
/// `best_session_diff` is an integer and reads 0 for this board's shares).
static BEST_DIFF: Mutex<f64> = Mutex::new(0.0);
/// Diagnostics per worker (index 0 = peripheral, 1 = software): the core
/// it runs on, the start-up probe's rate at high priority (kH/s × 10),
/// microseconds spent inside the batches (wrapping; the coordinator takes
/// the difference), and the last batch's microseconds per hash × 100.
static WORKER_CORE: [AtomicU32; 2] = [AtomicU32::new(9), AtomicU32::new(9)];
static PROBE_KHS10: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
static BUSY_US: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
static US_PER_HASH100: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

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
    /// Core of the peripheral worker and of the software worker.
    pub cores: [u32; 2],
    /// Start-up probe at high priority, kH/s: the uncontended ceiling.
    #[serde(rename = "probeKhs")]
    pub probe_khs: [f64; 2],
    /// Share of the last second each worker spent inside its batches.
    #[serde(rename = "busyPct")]
    pub busy_pct: [f64; 2],
    /// Microseconds per hash inside a batch (preemption included).
    #[serde(rename = "usPerHash")]
    pub us_per_hash: [f64; 2],
    /// The ESP32-S3's own die temperature (the devkit has no other sensor).
    #[serde(rename = "dieTempC")]
    pub die_temp_c: Option<f32>,
    /// Best share difficulty since boot, decimals kept.
    #[serde(rename = "bestDiff")]
    pub best_diff: f64,
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
    i.die_temp_c = die_temp_c();
    i.best_diff = *BEST_DIFF.lock().unwrap();
    for k in 0..2 {
        i.cores[k] = WORKER_CORE[k].load(Ordering::Relaxed);
        i.probe_khs[k] = f64::from(PROBE_KHS10[k].load(Ordering::Relaxed)) / 10.0;
        i.us_per_hash[k] = f64::from(US_PER_HASH100[k].load(Ordering::Relaxed)) / 100.0;
    }
    i
}

/// The ESP-IDF temperature sensor driver's handle, installed on first use.
/// The ESP-IDF temperature sensor driver's handle, installed on first
/// use: `None` before, `Some(None)` after a failure (warned once, never
/// retried), `Some(Some(handle))` when it reads.
static DIE_SENSOR: Mutex<Option<Option<usize>>> = Mutex::new(None);

/// The ESP32-S3's die temperature in °C: what the devkit shows as its
/// chip temperature, having no sensor on a chip it does not have. The
/// driver's 20..100 °C range (±2 °C per the data sheet).
pub fn die_temp_c() -> Option<f32> {
    let mut g = DIE_SENSOR.lock().unwrap();
    if g.is_none() {
        let cfg = sys::temperature_sensor_config_t {
            range_min: 20,
            range_max: 100,
            clk_src:
                sys::soc_periph_temperature_sensor_clk_src_t_TEMPERATURE_SENSOR_CLK_SRC_DEFAULT,
        };
        let mut h: sys::temperature_sensor_handle_t = core::ptr::null_mut();
        let rc = unsafe { sys::temperature_sensor_install(&cfg, &mut h) };
        if rc != sys::ESP_OK {
            log::warn!("cpu miner: temperature sensor install failed ({rc}); no die temperature");
            *g = Some(None);
            return None;
        }
        let rc = unsafe { sys::temperature_sensor_enable(h) };
        if rc != sys::ESP_OK {
            log::warn!("cpu miner: temperature sensor enable failed ({rc}); no die temperature");
            unsafe { sys::temperature_sensor_uninstall(h) };
            *g = Some(None);
            return None;
        }
        *g = Some(Some(h as usize));
    }
    let h = (*g)?? as sys::temperature_sensor_handle_t;
    let mut c = 0f32;
    (unsafe { sys::temperature_sensor_get_celsius(h, &mut c) } == sys::ESP_OK).then_some(c)
}

/// What the Sv2 channel reports as nominal hash rate, in H/s: the
/// 10-minute figure once there is one, else what this board does.
pub fn nominal_hash_rate() -> f64 {
    let k = INFO.lock().unwrap().as_ref().map_or(0.0, |i| i.khs_10m);
    if k > 0.0 {
        k * 1e3
    } else {
        230e3
    }
}

/// Start the coordinator and the two workers.
pub fn start() {
    log::info!("cpu miner: hashing on the ESP32-S3 itself (no chips on this board)");
    std::thread::Builder::new()
        .name("cpu-miner".into())
        .stack_size(12 * 1024)
        .spawn(run)
        .expect("cpu miner thread");
    // The peripheral loop is register-bound and loses 40 % of its batch
    // time to preemption on core 0 (Wi-Fi, lwIP, the pool link: 8.3 µs
    // a hash against 4.9 alone), so it gets core 1; the software worker
    // takes what core 0 leaves.
    spawn_pinned(c"cpu-hw", 1, 6 * 1024, || {
        worker(0, hw_batch, HW_BATCH, &HASHES_HW)
    });
    spawn_pinned(c"cpu-sw", 0, 6 * 1024, || {
        worker(1, sw_batch, SW_BATCH, &HASHES_SW)
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

fn now_us() -> i64 {
    unsafe { sys::esp_timer_get_time() }
}

fn worker(index: usize, scan: fn(&CpuJob, u32, u32), batch: u32, counter: &AtomicU32) {
    let null = core::ptr::null_mut();
    let core = unsafe { sys::xTaskGetCoreID(null) };
    let prio = unsafe { sys::uxTaskPriorityGet(null) };
    WORKER_CORE[index].store(core as u32, Ordering::Relaxed);
    // The ceiling: one batch at a priority above everything but the Wi-Fi
    // driver, on a probe job, before the real work starts.
    let probe = CpuJob::probe();
    unsafe { sys::vTaskPrioritySet(null, 20) };
    let t0 = now_us();
    scan(&probe, 0, batch);
    let dt = (now_us() - t0).max(1);
    unsafe { sys::vTaskPrioritySet(null, prio) };
    let khs = f64::from(batch) / dt as f64 * 1e3;
    PROBE_KHS10[index].store((khs * 10.0) as u32, Ordering::Relaxed);
    log::info!(
        "cpu miner: worker {index} on core {core} priority {prio}: {khs:.1} kH/s alone ({batch} hashes in {dt} us)"
    );
    let mut last_yield = now_us();
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
        let t0 = now_us();
        scan(&job, start, batch);
        let dt = (now_us() - t0).max(1);
        counter.fetch_add(batch, Ordering::Relaxed);
        BUSY_US[index].fetch_add(dt as u32, Ordering::Relaxed);
        US_PER_HASH100[index].store((dt * 100 / i64::from(batch)) as u32, Ordering::Relaxed);
        if now_us() - last_yield >= YIELD_EVERY_US {
            unsafe { sys::vTaskDelay(1) };
            last_yield = now_us();
        }
    }
}

fn sw_batch(job: &CpuJob, start: u32, n: u32) {
    // The C loop in components/warpaxe_glue/warpaxe_cpuhash.c (bound
    // through warpaxe_glue.h): returns at the first candidate, so the
    // batch continues behind it.
    let mut done = 0u32;
    while done < n {
        let mut hash = [0u8; 32];
        let left = n - done;
        let at = unsafe {
            sys::tusb::wa_sha256d_scan(
                job.midstate.as_ptr(),
                job.tail.as_ptr(),
                start.wrapping_add(done),
                left,
                hash.as_mut_ptr(),
            )
        };
        if at < left {
            found(job, start.wrapping_add(done).wrapping_add(at), hash, false);
            done += at + 1;
        } else {
            done = n;
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
    if job.probe {
        return;
    }
    if from_hw && sha256::sha256d_tail(&job.midstate, &job.tail, nonce) != hash {
        HW_MISMATCH.fetch_add(1, Ordering::Relaxed);
        log::warn!("cpu miner: peripheral hash for nonce {nonce:08x} failed the software re-hash");
        return;
    }
    CANDIDATES.fetch_add(1, Ordering::Relaxed);
    let diff = hash_difficulty(&hash);
    {
        let mut b = BEST_DIFF.lock().unwrap();
        if diff > *b {
            *b = diff;
        }
    }
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
    let mut last_busy = [0u32; 2];
    // The die temperature guard: off at the profile's limit (or the
    // configured shutdown temperature), on again 10 °C below.
    let mut hot = false;
    let mut last_temp = Instant::now();
    let mut last_tick = Instant::now();
    let mut hashes = 0f64;
    let mut hashes_since = Instant::now();
    let mut buckets: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
    miner::update_state(|s| {
        s.phase = "CPU".into();
        s.frequency_hz = 240_000_000;
    });
    loop {
        if last_temp.elapsed() >= Duration::from_secs(1) {
            last_temp = Instant::now();
            let temp = die_temp_c();
            board::set_devkit_temp(temp);
            if let Some(t) = temp {
                let cfg = config::get();
                let limit = if cfg.overheat_temp > 0 {
                    f32::from(cfg.overheat_temp)
                } else {
                    board::profile().max_temp_c
                };
                // A manual reset (/api/v1/overheat/reset, the same flag a
                // board with chips clears) ends the stop as well; the
                // next reading above the limit raises it again.
                if hot && !board::overheat() {
                    hot = false;
                    log::info!("cpu miner: overheat cleared by hand, hashing again");
                }
                if !hot && t >= limit {
                    hot = true;
                    board::set_devkit_overheat(true);
                    log::warn!(
                        "cpu miner: die {t:.1} °C at the limit {limit:.0} °C: hashing stopped"
                    );
                } else if hot && t <= limit - 10.0 {
                    hot = false;
                    board::set_devkit_overheat(false);
                    log::info!("cpu miner: die {t:.1} °C, 10 °C below the limit: hashing again");
                }
            }
        }
        if board::shutdown() || hot {
            if JOB.lock().unwrap().take().is_some() {
                log::info!("cpu miner: stopped");
            }
            miner::update_state(|s| {
                s.phase = if hot { "OVERHEAT" } else { "OFF" }.into();
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
                for k in 0..2 {
                    let b = BUSY_US[k].load(Ordering::Relaxed);
                    i.busy_pct[k] = f64::from(b.wrapping_sub(last_busy[k])) / dt / 1e4;
                    last_busy[k] = b;
                }
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

/// First start on the devkit: the wallet a NerdMiner left in flash, and
/// with it the Wi-Fi the ESP-IDF Wi-Fi library stored (NerdMiner's, as
/// the library keeps the station's SSID and password in its own NVS
/// namespace). Returns what was taken, never a value. The pool is
/// WarpPool, whatever the NerdMiner mined to.
///
/// The wallet decides: only a found config.json proves a NerdMiner was
/// here, and only then is the Wi-Fi taken over. Every firmware on an
/// ESP fills the Wi-Fi library's store, so a board with chips whose bus
/// stayed silent must not join an old network on its own. A password
/// that does not read as UTF-8 skips the Wi-Fi: an SSID without its
/// password would only fail and open the hotspot anyway.
pub fn import_nerdminer() -> Vec<&'static str> {
    let mut cfg = config::get();
    let mut took = Vec::new();
    if !cfg.stratum_user.is_empty() {
        return took;
    }
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
    let Some(user) = scan.result().and_then(|c| nerdminer::stratum_user(&c.btc)) else {
        log::info!("cpu miner: no NerdMiner config.json with a wallet in flash ({copies} copies)");
        return took;
    };
    cfg.stratum_user = user;
    cfg.stratum_url = nerdminer::POOL_HOST.into();
    cfg.stratum_port = nerdminer::POOL_PORT;
    cfg.stratum_protocol = "SV1".into();
    cfg.stratum_tls = 0;
    // No integer suggest: the session asks for SUGGESTED_DIFFICULTY
    // instead (an integer would only raise what the pool's NerdMiner band
    // starts at).
    cfg.suggested_difficulty = 0;
    took.push("wallet (nerdminer)");
    log::info!("cpu miner: wallet taken from NerdMiner's config.json ({copies} copies in flash)");
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
                    let mut pbuf = [0u8; 96];
                    let pass = nvs
                        .get_raw(nerdminer::WIFI_PASSWORD_KEY, &mut pbuf)
                        .ok()
                        .flatten()
                        .and_then(nerdminer::wifi_password);
                    match (ssid, pass) {
                        (Some(ssid), Some(pass)) => {
                            cfg.ssid = ssid;
                            cfg.wifi_pass = pass;
                            took.push("wifi (esp wifi store)");
                        }
                        (Some(_), None) => log::warn!(
                            "cpu miner: the Wi-Fi library's password is unreadable, Wi-Fi not taken"
                        ),
                        (None, _) => {
                            log::info!("cpu miner: no station SSID in the Wi-Fi library's storage")
                        }
                    }
                }
                Err(e) => log::info!("cpu miner: no Wi-Fi library namespace in NVS ({e})"),
            }
        }
    }
    if let Err(e) = config::save(&cfg) {
        log::warn!("cpu miner: saving the imported settings failed: {e}");
    }
    took
}
