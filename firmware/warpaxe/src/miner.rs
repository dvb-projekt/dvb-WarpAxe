//! The loop that mines: chain driver on the UART, work from Stratum,
//! nonces checked against the pool difficulty and submitted. One thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_chain::{Chain, Config as ChainConfig, Event, Phase};
use warpaxe_stratum::job::hash_difficulty;
use warpaxe_stratum::{sha256d, Job, WorkGate};

use crate::uart::ChipUart;
use crate::{board, config, diag, stratum};
use warpaxe_chain::Uart;

/// A clock step after an overheat stop: from the clock that overheated
/// to the next lower option, at an uptime second.
#[derive(Clone, Debug, Serialize)]
pub struct ClockStep {
    #[serde(rename = "fromMhz")]
    pub from_mhz: u32,
    #[serde(rename = "toMhz")]
    pub to_mhz: u32,
    #[serde(rename = "atUptimeSeconds")]
    pub at_uptime_s: u64,
    pub cause: &'static str,
}

/// One chip in `hashrateMonitor.asics`, in AxeOS's shape.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AsicMonitor {
    /// GH/s from the chip's total counter.
    pub total: f32,
    /// The raw error counter value, as AxeOS reports it; null on a chip
    /// without an error counter (the BM1397).
    #[serde(rename = "errorCount")]
    pub error_count: Option<u32>,
    /// GH/s per domain, scaled as AxeOS reports them.
    pub domains: Vec<f32>,
    /// GH/s of the error counter (NerdOS `errorHashRate`, served by
    /// `/api/v2/dashboard`, not part of AxeOS's shape).
    #[serde(skip)]
    pub error_ghs: f32,
    /// GH/s of the total counter over the last full minute.
    #[serde(skip)]
    pub total_1m: f32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MinerState {
    pub phase: String,
    pub chips: u8,
    pub frequency_hz: u32,
    pub difficulty: u32,
    pub nonces: u64,
    pub low_diff: u64,
    /// Nonces whose header hashes below the chips' own ticket mask: the
    /// chip should never have reported them (a wrong job or version on
    /// its side, or a hardware error). Hidden inside `low_diff` before.
    pub below_ticket: u64,
    /// Nonces for a work serial this loop no longer knows (retired or
    /// never handed out): dropped without a hash.
    pub unknown_job: u64,
    pub shares_sent: u64,
    pub stale: u64,
    pub duplicates: u64,
    pub desyncs: u64,
    pub silent_chips: u8,
    /// Times the whole chain was started over because every chip had gone
    /// silent for a minute.
    pub chain_restarts: u32,
    /// Of those, the ones in a row at the core voltage of the last one:
    /// two or more say the chips do not run at this voltage (a tuner's
    /// cue to leave it). Back to 0 after ten minutes of nonces.
    pub silent_restarts_same_mv: u32,
    /// The core voltage of the last silent restart, mV.
    pub silent_restart_mv: u16,
    /// 10-minute window.
    pub hashrate_ghs: f64,
    /// Last 30 s bucket.
    pub hashrate_now_ghs: f64,
    pub last_nonce_uptime_s: u64,
    pub init_failures: u32,
    pub last_error: String,
    /// Jobs handed to the chain.
    pub jobs_sent: u64,
    /// The chain driver's own counters, for diagnosis.
    pub chain_stats: String,
    /// The last nonce with everything needed to re-hash it offline.
    pub last_found: String,
    /// Submits by job age in notifies: 0, 1, 2, 3+.
    pub submit_age_hist: [u32; 4],
    pub last_submit_age: String,
    /// Best nonce difficulty since boot (AxeOS `bestSessionDiff`).
    pub best_session_diff: u64,
    /// Blocks found since boot (a nonce at or above the network
    /// difficulty), and the last one's height and uptime second.
    /// The chips' own hash counters (AxeOS `hashrateMonitor`): one entry
    /// per chip.
    pub hashrate_monitor: Vec<AsicMonitor>,
    /// The clock step taken after an overheat stop on a board without a
    /// fan to drive (the NerdNOS): kept until a reboot or a clock set by
    /// hand; `frequencyActual` shows the stepped clock.
    pub clock_step: Option<ClockStep>,
    /// Hash errors as a percentage of the chips' total (AxeOS
    /// `error_percentage`), from the last second's readings.
    pub error_percent: Option<f32>,
    /// The same over the last full minute: what a controller acts on.
    pub error_percent_1m: Option<f32>,
    pub blocks_found: u32,
    pub last_block: Option<(u64, u64)>,
}

static STATE: Mutex<Option<MinerState>> = Mutex::new(None);
/// Clock the API asked for (MHz); the loop applies it as a ramp without
/// restarting the chain. 0 until the chain has started.
static WANTED_MHZ: AtomicU32 = AtomicU32::new(0);
/// A clock ceiling after an overheat stop on a board without a fan: the
/// next option below the clock that overheated. Lives in RAM only, so a
/// reboot starts at the configured clock again; a clock set by hand
/// clears it.
static STEP_CAP_MHZ: AtomicU32 = AtomicU32::new(0);

/// The configured clock under the overheat step, if one is in force.
fn capped_mhz(mhz: u32) -> u32 {
    let cap = STEP_CAP_MHZ.load(Ordering::Relaxed);
    if cap > 0 {
        mhz.min(cap)
    } else {
        mhz
    }
}

/// After an overheat stop on a board that cannot cool itself: the chip
/// comes back one clock option lower (the stock NerdNOS firmware drops to
/// a fixed 210 MHz). Returns (from, to); None at the lowest option.
pub fn step_down_after_overheat() -> Option<(u32, u32)> {
    let current = capped_mhz(config::get().frequency);
    let lower = board::profile()
        .frequency_options
        .iter()
        .copied()
        .filter(|&o| o < current)
        .max()?;
    STEP_CAP_MHZ.store(lower, Ordering::Relaxed);
    WANTED_MHZ.store(lower, Ordering::Relaxed);
    update(|s| {
        s.clock_step = Some(ClockStep {
            from_mhz: current,
            to_mhz: lower,
            at_uptime_s: diag::uptime_s(),
            cause: "hot",
        })
    });
    Some((current, lower))
}

/// A register scan asked for over the API (diagnostics, read-only): the
/// registers `from..=to` of one chip, read between the work like the
/// hash counters. `values[i]` is register `from + i`; `None` when the
/// chip did not answer within [`SCAN_WINDOW`].
#[derive(Clone, Debug, Default, Serialize)]
pub struct RegScan {
    pub chip: u8,
    pub from: u8,
    pub to: u8,
    pub done: bool,
    pub uptime_s: u64,
    pub values: Vec<Option<u32>>,
}
static REG_SCAN: Mutex<Option<RegScan>> = Mutex::new(None);
/// How long the replies to a scan are collected.
const SCAN_WINDOW: Duration = Duration::from_secs(2);
/// A scan older than this that never finished (chain restarted) is
/// forgotten.
const SCAN_STALE_S: u64 = 10;

/// Ask the mining loop for a register scan; `false` while one is
/// still running.
pub fn request_reg_scan(chip: u8, from: u8, to: u8) -> bool {
    let mut g = REG_SCAN.lock().unwrap();
    let now = diag::uptime_s();
    if g
        .as_ref()
        .is_some_and(|s| !s.done && now.saturating_sub(s.uptime_s) < SCAN_STALE_S)
    {
        return false;
    }
    *g = Some(RegScan {
        chip,
        from,
        to,
        done: false,
        uptime_s: now,
        values: vec![None; usize::from(to.saturating_sub(from)) + 1],
    });
    true
}

/// The last register scan, finished or not.
pub fn reg_scan() -> Option<RegScan> {
    REG_SCAN.lock().unwrap().clone()
}

/// Which cores the nonces of one chip came from since the chain started
/// (diagnostics): index = core id from the nonce's high bits, and the
/// small core id from the job id byte.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CoreHist {
    pub nonces: u32,
    pub cores: Vec<u32>,
    pub small_cores: Vec<u32>,
}
static CORES: Mutex<Vec<CoreHist>> = Mutex::new(Vec::new());

/// The core histogram of every chip, in chain order.
pub fn core_hist() -> Vec<CoreHist> {
    CORES.lock().unwrap().clone()
}

fn core_seen(chip: u8, core: u8, small: u8) {
    let mut g = CORES.lock().unwrap();
    let i = usize::from(chip);
    while g.len() <= i {
        g.push(CoreHist {
            nonces: 0,
            cores: vec![0; 128],
            small_cores: vec![0; 16],
        });
    }
    let h = &mut g[i];
    h.nonces = h.nonces.saturating_add(1);
    if let Some(c) = h.cores.get_mut(usize::from(core)) {
        *c = c.saturating_add(1);
    }
    if let Some(c) = h.small_cores.get_mut(usize::from(small)) {
        *c = c.saturating_add(1);
    }
}

/// A new clock from `PATCH /api/system`, applied live by the mining loop.
/// Forget the best difficulty since boot (web UI "reset best difficulty").
pub fn reset_best_session() {
    update(|s| s.best_session_diff = 0);
}

pub fn request_frequency(mhz: u32) {
    // A clock set by hand ends the overheat step.
    STEP_CAP_MHZ.store(0, Ordering::Relaxed);
    update(|s| s.clock_step = None);
    WANTED_MHZ.store(mhz, Ordering::Relaxed);
}

pub fn state() -> MinerState {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

fn update(f: impl FnOnce(&mut MinerState)) {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(MinerState::default));
}

/// The CPU miner (a T-Display-S3 without chips) reports into the same
/// state the API and the display read.
pub(crate) fn update_state(f: impl FnOnce(&mut MinerState)) {
    update(f);
}

pub fn start(uart: ChipUart) {
    std::thread::Builder::new()
        .stack_size(16 * 1024)
        .spawn(move || run(uart))
        .expect("miner thread");
}

fn run(mut uart: ChipUart) {
    let epoch = Instant::now();
    let now_us = || epoch.elapsed().as_micros() as u64;
    // TX driven low while the chips sit without their I/O rails
    // (board::parks_uart), back on the UART before the next reset pulse.
    let mut parked = false;
    loop {
        if board::chain_stopped() {
            if !parked && board::parks_uart() {
                match uart.park() {
                    Ok(()) => {
                        parked = true;
                        log::info!("miner: TX parked low while the chips are off");
                    }
                    Err(e) => log::warn!("miner: parking TX failed: {e:?}"),
                }
            }
            let why = if board::overheat() {
                "OVERHEAT"
            } else if board::shutdown() {
                "OFF"
            } else {
                "POWER"
            };
            // Not hashing: say so, instead of the last value standing.
            update(|s| {
                s.phase = why.into();
                s.hashrate_ghs = 0.0;
                s.hashrate_now_ghs = 0.0;
            });
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let cfg = config::get();
        let start_mhz = capped_mhz(cfg.frequency);
        // The chips filter at the profile's default ticket mask (256 on a
        // BM1370) regardless of the pool difficulty: enough nonces for a
        // steady hashrate estimate, the pool threshold is applied on the
        // re-hash before a submit. asicDifficulty overrides both.
        // The chain starts at the profile's default; a pool difficulty
        // below it is followed only once VarDiff has settled (below), so
        // the first seconds after a connect (difficulty 1, 4, 16 …) do
        // not flood the host with nonces.
        let chip_cap = board::chip().default_difficulty;
        let pool_diff = if cfg.asic_difficulty > 0 {
            cfg.asic_difficulty
        } else {
            chip_cap
        };
        let profile = board::profile();
        // Reset pulse (an ESP GPIO, or the expander pin on the Q13xx; a
        // no-op without a reset line), and the host back at the chips'
        // power-on baud rate: a restarted or re-powered chain talks
        // 115200 again, while the UART still ran at the fast rate of the
        // previous init (every restart after the first failed with
        // NoChips).
        // A 10 ms pulse restarts a BM1370; a board brought up in NerdOS's
        // order (the Gaia) holds it as long as its profile says (350 ms),
        // and so does one whose reset line is its only switch (the
        // NerdNOS: 100 ms, WantClue `_reset`).
        let pulse = match profile.vr {
            warpaxe_board::Vr::Tps546Nerd { enable: None, .. } | warpaxe_board::Vr::None => {
                profile.reset_low_ms.max(10)
            }
            _ => 10,
        };
        if parked {
            parked = false;
            match uart.unpark() {
                Ok(()) => log::info!("miner: TX back on the UART"),
                Err(e) => log::error!("miner: TX back on the UART failed: {e:?}"),
            }
        }
        board::reset_chain(pulse, profile.reset_release_ms);
        if let Err(e) = Uart::set_baud(&mut uart, 115_200) {
            log::warn!("miner: uart back to 115200: {e:?}");
        }
        let chain_cfg = ChainConfig {
            expected_chips: cfg.asic_count,
            frequency_hz: start_mhz.saturating_mul(1_000_000),
            difficulty: pool_diff,
            ..ChainConfig::for_profile(board::chip())
        };
        let mut chain = Chain::new(board::chip(), chain_cfg);
        if start_mhz != cfg.frequency {
            log::warn!(
                "miner: chain starts at {start_mhz} MHz under the overheat step, not the configured {} MHz",
                cfg.frequency
            );
        }
        chain.start(now_us());
        update(|s| {
            s.phase = "INIT".into();
            s.difficulty = pool_diff;
        });
        log::info!(
            "miner: chain start, target {start_mhz} MHz, difficulty {pool_diff}"
        );

        // serial -> (job, notify count when built, uptime s when built)
        let mut jobs: HashMap<u32, (Job, u64, u64)> = HashMap::new();
        let mut gate = WorkGate::new(stratum::state().clean_jobs);
        let mut applied_diff = pool_diff;
        // The pool difficulty last seen and since when: VarDiff has settled
        // after a minute without a change.
        let mut pool_diff_seen = stratum::state().difficulty;
        let mut pool_diff_since = Instant::now();
        let mut applied_mhz = start_mhz;
        WANTED_MHZ.store(start_mhz, Ordering::Relaxed);
        let mut hashes = 0f64;
        let mut hashes_since = epoch.elapsed();
        let mut buckets: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
        let mut counting = false;
        let mut last_phase = Phase::Idle;
        let mut failed_at: Option<Instant> = None;
        let mut last_work: Option<Instant> = None;
        let mut last_counters: Option<Instant> = None;
        let mut all_silent_since: Option<Instant> = None;
        let chain_started = Instant::now();
        // A register scan: the next register to read (one per loop pass,
        // like the counter reads), then when the last read went out.
        let mut scan_next: Option<u8> = None;
        let mut scan_sent: Option<Instant> = None;

        loop {
            let now = now_us();
            let mut events: Vec<Event> = Vec::new();
            if board::chain_stopped() {
                break;
            }
            // A new block (clean_jobs) retires everything before this
            // round's nonces are looked at: whatever the chip still returns
            // for the old block is stale at the pool, not worth a submit.
            if gate.block_changed(stratum::state().clean_jobs) {
                chain.retire_all();
                jobs.clear();
            }
            if let Err(e) = chain.poll(&mut uart, now, &mut |e| events.push(e)) {
                log::error!("miner: poll: {e}");
                update(|s| s.last_error = e.to_string());
            }
            for e in events {
                match e {
                    Event::Nonce(f) => {
                        core_seen(f.chip, f.core, f.small_core);
                        let Some((job, job_seq, job_t)) = jobs.get(&f.work.serial) else {
                            update(|s| s.unknown_job += 1);
                            continue;
                        };
                        let header = job.header(f.version, f.nonce);
                        let diff = hash_difficulty(&sha256d(&header));
                        let hex: String = header.iter().map(|b| format!("{b:02x}")).collect();
                        update(|s| {
                            s.last_found = format!(
                                "serial={} chip={} core={} version={:08x} job_version={:08x} nonce={:08x} ntime={:08x} diff={:.3} chip_diff={} header={hex}",
                                f.work.serial, f.chip, f.core, f.version, job.version, f.nonce, f.ntime, diff, chain.difficulty()
                            )
                        });
                        // The chips filter at the largest power of two at or
                        // below the requested difficulty (the ticket mask), so
                        // that is what each nonce is worth in hashes.
                        let requested = chain.difficulty().max(1);
                        let chip_diff = f64::from(1u32 << (31 - requested.leading_zeros()));
                        hashes += chip_diff * 4_294_967_296.0;
                        let want = stratum::state().difficulty.max(1.0);
                        let below_ticket = diff < chip_diff;
                        update(|s| {
                            s.nonces += 1;
                            if below_ticket {
                                s.below_ticket += 1;
                            }
                            s.last_nonce_uptime_s = diag::uptime_s();
                            s.best_session_diff = s.best_session_diff.max(diff as u64);
                        });
                        config::raise_best_diff(diff as u64);
                        let st_now = stratum::state();
                        if st_now.network_difficulty > 0.0 && diff >= st_now.network_difficulty {
                            log::warn!(
                                "miner: BLOCK FOUND at height {} (diff {diff:.0})",
                                st_now.block_height
                            );
                            update(|s| {
                                s.blocks_found += 1;
                                s.last_block = Some((st_now.block_height, diag::uptime_s()));
                            });
                        }
                        if diff >= want {
                            // How old the stratum job is at submit time, in
                            // notifies and seconds: a pool keeps only a few
                            // recent jobs, older ones come back "stale".
                            let age_n = stratum::state().jobs_received.saturating_sub(*job_seq);
                            let age_s = diag::uptime_s().saturating_sub(*job_t);
                            stratum::submit(job, f.ntime, f.nonce, f.version ^ job.version);
                            update(|s| {
                                s.shares_sent += 1;
                                s.submit_age_hist[(age_n as usize).min(3)] += 1;
                                s.last_submit_age = format!("{age_n} notifies, {age_s} s");
                            });
                        } else {
                            update(|s| s.low_diff += 1);
                        }
                    }
                    Event::Ready => {
                        log::info!("miner: chain ready, {} chip(s)", chain.chip_count());
                        CORES.lock().unwrap().clear();
                        board::chain_ready();
                        crate::update::mark_app_valid("chain ready");
                    }
                    Event::Register {
                        address,
                        register,
                        value,
                    } => {
                        if scan_sent.is_some() || scan_next.is_some() {
                            if let Some(i) = chain.chip_index(address) {
                                if let Some(s) = REG_SCAN.lock().unwrap().as_mut() {
                                    if !s.done
                                        && usize::from(s.chip) == i
                                        && (s.from..=s.to).contains(&register)
                                    {
                                        s.values[usize::from(register - s.from)] = Some(value);
                                    }
                                }
                            }
                        }
                    }
                    Event::Enumerated { count } => log::info!("miner: {count} chip(s) answered"),
                    Event::ChipCountMismatch { expected, found } => {
                        log::warn!("miner: expected {expected} chips, found {found}")
                    }
                    Event::InitFailed(why) => {
                        // What the UART saw meanwhile: nothing at all, garbage,
                        // or frames that failed their CRC tell different stories.
                        let d = chain.stats().decoder;
                        log::warn!(
                            "miner: init failed: {why:?} (rx frames {}, dropped bytes {}, crc failures {})",
                            d.frames,
                            d.dropped,
                            d.crc_failures
                        );
                        board::chain_ready();
                        update(|s| {
                            s.init_failures += 1;
                            s.last_error = format!(
                                "{why:?} (rx frames {}, dropped {}, crc {})",
                                d.frames, d.dropped, d.crc_failures
                            );
                        });
                    }
                    Event::ChipSilent { index } => log::warn!("miner: chip {index} silent"),
                    Event::ChipBack { index } => log::info!("miner: chip {index} back"),
                    Event::FrequencyApplied { hz } => {
                        log::info!("miner: clock {} MHz", hz / 1_000_000)
                    }
                    Event::FrequencyUnreachable { hz } => {
                        log::warn!("miner: clock {hz} Hz unreachable")
                    }
                    _ => {}
                }
            }

            let phase = chain.phase();
            if phase != last_phase {
                update(|s| s.phase = format!("{phase:?}").to_uppercase());
                last_phase = phase;
            }
            if phase == Phase::Failed {
                if failed_at.is_none() {
                    failed_at = Some(Instant::now());
                    update(|s| {
                        s.hashrate_ghs = 0.0;
                        s.hashrate_now_ghs = 0.0;
                    });
                }
                if failed_at.map_or(false, |t| t.elapsed() > Duration::from_secs(10)) {
                    break; // start over, maybe chips are there now
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }

            if chain.accepting_work() {
                // A register scan from the API: one read per pass (a burst
                // loses replies, see Chain::request_counters), the replies
                // collected for SCAN_WINDOW after the last, then it is done.
                let pending = REG_SCAN
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|s| !s.done)
                    .map(|s| (s.chip, s.from, s.to));
                match (pending, scan_next, scan_sent) {
                    (Some((_, from, _)), None, None) => scan_next = Some(from),
                    (Some((chip, _, to)), Some(r), None) => {
                        if let Err(e) = chain.read_chip_register(&mut uart, usize::from(chip), r) {
                            log::warn!("miner: register read: {e}");
                        }
                        if r < to {
                            scan_next = Some(r + 1);
                        } else {
                            scan_next = None;
                            scan_sent = Some(Instant::now());
                        }
                    }
                    (Some(_), _, Some(t)) if t.elapsed() >= SCAN_WINDOW => {
                        if let Some(s) = REG_SCAN.lock().unwrap().as_mut() {
                            s.done = true;
                        }
                        scan_sent = None;
                    }
                    (None, _, _) => {
                        scan_next = None;
                        scan_sent = None;
                    }
                    _ => {}
                }
                // The chips' hash counters, once a second (the reference
                // polls at 1 Hz); the replies fill `chain.monitor()`.
                if last_counters.map_or(true, |t| t.elapsed() >= Duration::from_secs(1)) {
                    last_counters = Some(Instant::now());
                    if let Err(e) = chain.request_counters(&mut uart) {
                        log::warn!("miner: counter read: {e}");
                    }
                    let mon = chain.monitor();
                    let asics: Vec<AsicMonitor> = mon
                        .chips()
                        .iter()
                        .enumerate()
                        .map(|(i, c)| AsicMonitor {
                            total: c.total.ghs,
                            error_count: mon.has_error_counter().then_some(c.error.value),
                            domains: (0..mon.domains()).map(|d| mon.domain_ghs(i, d)).collect(),
                            error_ghs: c.error.ghs,
                            total_1m: c.total.ghs_1m,
                        })
                        .collect();
                    let pct = mon.has_error_counter().then(|| mon.error_percent());
                    let pct_1m = mon.has_error_counter().then(|| mon.error_percent_1m());
                    update(|s| {
                        s.hashrate_monitor = asics;
                        s.error_percent = pct;
                        s.error_percent_1m = pct_1m;
                    });
                }
                let st = stratum::state();
                let cfg_now = config::get();
                let forced = cfg_now.asic_difficulty;
                if st.difficulty != pool_diff_seen {
                    pool_diff_seen = st.difficulty;
                    pool_diff_since = Instant::now();
                }
                let settled = pool_diff_since.elapsed() >= Duration::from_secs(60);
                let want = if forced > 0 {
                    forced
                } else if settled {
                    (st.difficulty.max(1.0) as u32).min(chip_cap)
                } else {
                    chip_cap
                };
                if want != applied_diff {
                    if chain.set_difficulty(&mut uart, want).is_ok() {
                        applied_diff = want;
                        update(|s| s.difficulty = want);
                    }
                }
                let want_mhz = WANTED_MHZ.load(Ordering::Relaxed);
                if want_mhz != applied_mhz && want_mhz > 0 {
                    match chain.set_frequency(want_mhz.saturating_mul(1_000_000)) {
                        Some(hz) => log::info!(
                            "miner: ramp to {} MHz (PLL {} MHz)",
                            want_mhz,
                            hz / 1_000_000
                        ),
                        None => log::warn!(
                            "miner: {} MHz has no PLL setting, keeping {} MHz",
                            want_mhz,
                            applied_mhz
                        ),
                    }
                    applied_mhz = want_mhz;
                }
                // New notify, or the chips ran out of nonce space: fresh work.
                // The counter is only a hint; the work fetched below carries
                // its own notify number, which decides.
                let fresh = st.jobs_received != gate.seen_notify();
                // A set job interval only ever brings work sooner than the
                // chips' own nonce-space timeout: the setting, else the
                // board's (as its stock firmware), else the chip's.
                let board_ms = board::profile().job_interval_ms;
                let interval_ms = if cfg_now.job_interval_ms > 0 {
                    cfg_now.job_interval_ms
                } else if board_ms > 0 {
                    board_ms
                } else {
                    board::chip().job_interval_ms
                };
                let paced = interval_ms > 0
                    && last_work.is_some_and(|t| {
                        t.elapsed() >= Duration::from_millis(u64::from(interval_ms))
                    });
                if fresh || paced || chain.needs_work(now) {
                    let fetched = stratum::next_job().filter(|(_, seq, _)| {
                        gate.is_fresh(*seq) || paced || chain.needs_work(now)
                    });
                    if let Some((job, seq, clean_seq)) = fetched {
                        // A block the loop top has not seen yet: retire the
                        // old work now, and only now (the gate remembers).
                        if gate.submitting(seq, clean_seq) {
                            chain.retire_all();
                            jobs.clear();
                        }
                        match chain.submit(&mut uart, &job.work(), now) {
                            Ok(r) => {
                                if !counting {
                                    // Hashrate time starts with the first
                                    // job, not with chain init: the PLL ramp
                                    // would read as a slow first bucket.
                                    counting = true;
                                    hashes = 0.0;
                                    hashes_since = epoch.elapsed();
                                }
                                last_work = Some(Instant::now());
                                update(|s| s.jobs_sent += 1);
                                jobs.insert(r.serial, (job, seq, diag::uptime_s()));
                                if jobs.len() > 32 {
                                    let oldest = r.serial.wrapping_sub(32);
                                    jobs.retain(|k, _| k.wrapping_sub(oldest) < 64);
                                }
                            }
                            Err(e) => log::warn!("miner: submit work: {e}"),
                        }
                    }
                }
            }

            let elapsed = (epoch.elapsed() - hashes_since).as_secs_f64();
            if counting && elapsed >= 30.0 {
                // One 30 s bucket into a 10-minute window: at a ticket mask
                // of 256 a BM1370 yields about a nonce a second, so the
                // window holds ~600 samples and the estimate is steady.
                // Buckets are keyed by their START, so the span covers every
                // bucket in the sum (keyed by end, it missed the oldest one:
                // +5 % on a full window, 2x right after boot).
                let now_s = epoch.elapsed().as_secs_f64();
                buckets.push_back((now_s - elapsed, hashes));
                while buckets.front().is_some_and(|(t, _)| now_s - *t > 600.0) {
                    buckets.pop_front();
                }
                let sum: f64 = buckets.iter().map(|(_, h)| *h).sum();
                let span = buckets
                    .front()
                    .map_or(elapsed, |(t, _)| (now_s - *t).max(elapsed));
                let ghs_10m = sum / span / 1e9;
                let ghs_now = hashes / elapsed / 1e9;
                // Ten minutes of nonces since the start: whatever silence
                // came before, this voltage runs.
                let settled = sum > 0.0 && chain_started.elapsed() >= Duration::from_secs(600);
                update(|s| {
                    s.hashrate_ghs = ghs_10m;
                    s.hashrate_now_ghs = ghs_now;
                    if settled {
                        s.silent_restarts_same_mv = 0;
                    }
                });
                hashes = 0.0;
                hashes_since = epoch.elapsed();
            }
            update(|s| {
                s.chips = chain.chip_count();
                s.frequency_hz = chain.frequency_hz();
                let st = chain.stats();
                s.chain_stats = format!("{st:?}");
                s.stale = st.stale_local;
                s.duplicates = st.duplicates;
                s.desyncs = st.desyncs;
                s.silent_chips = chain.chips().iter().filter(|c| c.silent).count() as u8;
            });
            // Every chip silent for a minute with work going out: the chain
            // hung (seen on a NerdOctaxe when the USB cable was pulled from
            // its LilyGO: all eight stopped at once, core still up). Start
            // over — reset pulse and a fresh init — as chips off/on would.
            let n = chain.chip_count();
            let silent = chain.chips().iter().filter(|c| c.silent).count();
            if n > 0 && silent == usize::from(n) && chain.accepting_work() {
                let since = *all_silent_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(60) {
                    // The restart brings the chain up at the same voltage
                    // again; counted per voltage so the API (and a tuner)
                    // can tell a dead voltage from a one-off hang.
                    let mv = config::get().core_voltage;
                    update(|s| {
                        s.chain_restarts += 1;
                        if s.silent_restart_mv == mv {
                            s.silent_restarts_same_mv += 1;
                        } else {
                            s.silent_restart_mv = mv;
                            s.silent_restarts_same_mv = 1;
                        }
                        s.hashrate_now_ghs = 0.0;
                        s.last_error = format!(
                            "all {n} chips silent for 60 s at {mv} mV (restart {} at this voltage)",
                            s.silent_restarts_same_mv
                        );
                    });
                    log::warn!(
                        "miner: all {n} chips silent for 60 s at {mv} mV, restarting the chain"
                    );
                    break;
                }
            } else {
                all_silent_since = None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
