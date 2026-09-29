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
    pub blocks_found: u32,
    pub last_block: Option<(u64, u64)>,
}

static STATE: Mutex<Option<MinerState>> = Mutex::new(None);
/// Clock the API asked for (MHz); the loop applies it as a ramp without
/// restarting the chain. 0 until the chain has started.
static WANTED_MHZ: AtomicU32 = AtomicU32::new(0);

/// A new clock from `PATCH /api/system`, applied live by the mining loop.
/// Forget the best difficulty since boot (web UI "reset best difficulty").
pub fn reset_best_session() {
    update(|s| s.best_session_diff = 0);
}

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
        if board::chain_stopped() {
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
        // The chips filter at the profile's default ticket mask (256 on a
        // BM1370) regardless of the pool difficulty: enough nonces for a
        // steady hashrate estimate, the pool threshold is applied on the
        // re-hash before a submit. asicDifficulty overrides both.
        let chip_cap = board::chip().default_difficulty;
        let pool_diff = if cfg.asic_difficulty > 0 {
            cfg.asic_difficulty
        } else {
            (stratum::state().difficulty.max(1.0) as u32).min(chip_cap)
        };
        let profile = board::profile();
        // Reset pulse (an ESP GPIO, or the expander pin on the Q13xx; a
        // no-op without a reset line), and the host back at the chips'
        // power-on baud rate: a restarted or re-powered chain talks
        // 115200 again, while the UART still ran at the fast rate of the
        // previous init (every restart after the first failed with
        // NoChips).
        // A 10 ms pulse restarts a BM1370; a board brought up in NerdOS's
        // order (the Gaia) holds it as long as its profile says (350 ms).
        let pulse = match profile.vr {
            warpaxe_board::Vr::Tps546Nerd { enable: None, .. } => profile.reset_low_ms.max(10),
            _ => 10,
        };
        board::reset_chain(pulse, profile.reset_release_ms);
        if let Err(e) = Uart::set_baud(&mut uart, 115_200) {
            log::warn!("miner: uart back to 115200: {e:?}");
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

        // serial -> (job, notify count when built, uptime s when built)
        let mut jobs: HashMap<u32, (Job, u64, u64)> = HashMap::new();
        let mut gate = WorkGate::new(stratum::state().clean_jobs);
        let mut applied_diff = pool_diff;
        let mut applied_mhz = cfg.frequency;
        WANTED_MHZ.store(cfg.frequency, Ordering::Relaxed);
        let mut hashes = 0f64;
        let mut hashes_since = epoch.elapsed();
        let mut buckets: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
        let mut counting = false;
        let mut last_phase = Phase::Idle;
        let mut failed_at: Option<Instant> = None;
        let mut last_work: Option<Instant> = None;
        let mut all_silent_since: Option<Instant> = None;

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
                        board::chain_ready();
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
                let st = stratum::state();
                let cfg_now = config::get();
                let forced = cfg_now.asic_difficulty;
                let want = if forced > 0 {
                    forced
                } else {
                    (st.difficulty.max(1.0) as u32).min(chip_cap)
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
                // chips' own nonce-space timeout.
                let interval_ms = if cfg_now.job_interval_ms > 0 {
                    cfg_now.job_interval_ms
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
                update(|s| {
                    s.hashrate_ghs = ghs_10m;
                    s.hashrate_now_ghs = ghs_now;
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
                    log::warn!("miner: all {n} chips silent for 60 s, restarting the chain");
                    update(|s| {
                        s.chain_restarts += 1;
                        s.hashrate_now_ghs = 0.0;
                    });
                    break;
                }
            } else {
                all_silent_since = None;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
