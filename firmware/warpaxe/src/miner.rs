//! The loop that mines: chain driver on the UART, work from Stratum,
//! nonces checked against the pool difficulty and submitted. One thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_chain::{Chain, Config as ChainConfig, Event, Phase};
use warpaxe_stratum::job::hash_difficulty;
use warpaxe_stratum::{sha256d, Job};

use crate::uart::ChipUart;
use crate::{board, config, diag, stratum};

#[derive(Clone, Debug, Default, Serialize)]
pub struct MinerState {
    pub phase: String,
    pub chips: u8,
    pub frequency_hz: u32,
    pub difficulty: u32,
    pub nonces: u64,
    pub low_diff: u64,
    pub shares_sent: u64,
    pub stale: u64,
    pub duplicates: u64,
    pub desyncs: u64,
    pub silent_chips: u8,
    pub hashrate_ghs: f64,
    pub last_nonce_uptime_s: u64,
    pub init_failures: u32,
    pub last_error: String,
    /// Jobs handed to the chain.
    pub jobs_sent: u64,
    /// The chain driver's own counters, for diagnosis.
    pub chain_stats: String,
    /// The last nonce with everything needed to re-hash it offline.
    pub last_found: String,
}

static STATE: Mutex<Option<MinerState>> = Mutex::new(None);
/// Clock the API asked for (MHz); the loop applies it as a ramp without
/// restarting the chain. 0 until the chain has started.
static WANTED_MHZ: AtomicU32 = AtomicU32::new(0);

/// A new clock from `PATCH /api/system`, applied live by the mining loop.
pub fn request_frequency(mhz: u32) {
    WANTED_MHZ.store(mhz, Ordering::Relaxed);
}

pub fn state() -> MinerState {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

fn update(f: impl FnOnce(&mut MinerState)) {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(MinerState::default));
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
    loop {
        if board::overheat() {
            update(|s| s.phase = "OVERHEAT".into());
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let cfg = config::get();
        let pool_diff = if cfg.asic_difficulty > 0 {
            cfg.asic_difficulty
        } else {
            stratum::state().difficulty.max(1.0) as u32
        };
        let profile = board::profile();
        if profile.asic_reset.is_some() {
            board::reset_chain(10, profile.reset_release_ms);
        }
        let chain_cfg = ChainConfig {
            expected_chips: cfg.asic_count,
            frequency_hz: cfg.frequency.saturating_mul(1_000_000),
            difficulty: pool_diff,
            ..ChainConfig::for_profile(board::chip())
        };
        let mut chain = Chain::new(board::chip(), chain_cfg);
        chain.start(now_us());
        update(|s| {
            s.phase = "INIT".into();
            s.difficulty = pool_diff;
        });
        log::info!(
            "miner: chain start, target {} MHz, difficulty {pool_diff}",
            cfg.frequency
        );

        let mut jobs: HashMap<u32, Job> = HashMap::new();
        let mut seen_notify = 0u64;
        let mut applied_diff = pool_diff;
        let mut applied_mhz = cfg.frequency;
        WANTED_MHZ.store(cfg.frequency, Ordering::Relaxed);
        let mut hashes = 0f64;
        let mut hashes_since = epoch.elapsed();
        let mut last_phase = Phase::Idle;
        let mut failed_at: Option<Instant> = None;

        loop {
            let now = now_us();
            let mut events: Vec<Event> = Vec::new();
            if board::overheat() {
                break;
            }
            if let Err(e) = chain.poll(&mut uart, now, &mut |e| events.push(e)) {
                log::error!("miner: poll: {e}");
                update(|s| s.last_error = e.to_string());
            }
            for e in events {
                match e {
                    Event::Nonce(f) => {
                        let Some(job) = jobs.get(&f.work.serial) else {
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
                        update(|s| {
                            s.nonces += 1;
                            s.last_nonce_uptime_s = diag::uptime_s();
                        });
                        if diff >= want {
                            stratum::submit(job, f.ntime, f.nonce, f.version ^ job.version);
                            update(|s| s.shares_sent += 1);
                        } else {
                            update(|s| s.low_diff += 1);
                        }
                    }
                    Event::Ready => {
                        log::info!("miner: chain ready, {} chip(s)", chain.chip_count())
                    }
                    Event::Enumerated { count } => log::info!("miner: {count} chip(s) answered"),
                    Event::ChipCountMismatch { expected, found } => {
                        log::warn!("miner: expected {expected} chips, found {found}")
                    }
                    Event::InitFailed(why) => {
                        log::warn!("miner: init failed: {why:?}");
                        update(|s| {
                            s.init_failures += 1;
                            s.last_error = format!("{why:?}");
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
                }
                if failed_at.map_or(false, |t| t.elapsed() > Duration::from_secs(10)) {
                    break; // start over, maybe chips are there now
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }

            if chain.accepting_work() {
                let st = stratum::state();
                let forced = config::get().asic_difficulty;
                let want = if forced > 0 {
                    forced
                } else {
                    st.difficulty.max(1.0) as u32
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
                let fresh = st.jobs_received != seen_notify;
                if fresh || chain.needs_work(now) {
                    if let Some(job) = stratum::next_job() {
                        if fresh && job.clean_jobs {
                            chain.retire_all();
                            jobs.clear();
                        }
                        match chain.submit(&mut uart, &job.work(), now) {
                            Ok(r) => {
                                update(|s| s.jobs_sent += 1);
                                jobs.insert(r.serial, job);
                                if jobs.len() > 32 {
                                    let oldest = r.serial.wrapping_sub(32);
                                    jobs.retain(|k, _| k.wrapping_sub(oldest) < 64);
                                }
                                seen_notify = st.jobs_received;
                            }
                            Err(e) => log::warn!("miner: submit work: {e}"),
                        }
                    }
                }
            }

            let elapsed = (epoch.elapsed() - hashes_since).as_secs_f64();
            if elapsed >= 30.0 {
                let ghs = hashes / elapsed / 1e9;
                update(|s| s.hashrate_ghs = ghs);
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
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
