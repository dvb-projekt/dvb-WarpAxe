//! The loop that mines: chain driver on the UART, work from Stratum,
//! nonces checked against the pool difficulty and submitted. One thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use warpaxe_board::choice::{self, FirstStart};
use warpaxe_chain::{
    Chain, ChipMonitor, Config as ChainConfig, DomainLatch, Event, LatchSample, LatchVerdict,
    LoneSilence, LoneVerdict, NonceWorth, Phase,
};
use warpaxe_stratum::job::hash_difficulty;
use warpaxe_stratum::{block, sha256d, Job, WorkGate};

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
    /// The chip's total hash counter as read: one step per 2^32 hashes,
    /// like `errorCount`; it wraps at 32 bits (after months) and starts
    /// over with the chain. The difference of two readings over the
    /// time between them is the chip's rate at the counter's own
    /// precision, where `total` is one second's reading. Null on a chip
    /// without counters (the BM1397). Not part of AxeOS's shape.
    #[serde(rename = "hashCount")]
    pub hash_count: Option<u32>,
    /// Times this chip's hash counter went back since the chain
    /// started: the chip began to count anew while the chain ran on, a
    /// chip that reset itself (left with too little voltage, on a board
    /// with chips in series). `hashCount` and `errorCount` start over
    /// with it. Not part of AxeOS's shape.
    #[serde(rename = "counterRestarts")]
    pub counter_restarts: u32,
    /// GH/s per domain, scaled as AxeOS reports them.
    pub domains: Vec<f32>,
    /// GH/s of the error counter (NerdOS `errorHashRate`, served by
    /// `/api/v2/dashboard`, not part of AxeOS's shape).
    #[serde(skip)]
    pub error_ghs: f32,
    /// GH/s of the total counter over the last full minute.
    #[serde(skip)]
    pub total_1m: f32,
    /// The chip answered a counter read within the last few seconds; a
    /// chip that stopped answering keeps its last rates here, and this
    /// says not to trust them (the self-test counts it as hashing nothing).
    #[serde(skip)]
    pub fresh: bool,
}

/// A counter reading older than this is no longer the chip's rate.
const COUNTER_FRESH_US: u64 = 5_000_000;

/// The chip answered a counter read within [`COUNTER_FRESH_US`].
fn counters_fresh(c: &ChipMonitor, now_us: u64) -> bool {
    c.total.time_us != 0 && now_us.saturating_sub(c.total.time_us) < COUNTER_FRESH_US
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct MinerState {
    pub phase: String,
    pub chips: u8,
    pub frequency_hz: u32,
    /// The clock each chip runs at, Hz, chain order: `frequency_hz`
    /// plus the chip's own offset; `[..chips]` count. All 0 without a
    /// chain (the CPU miner). `asicFrequencies` in the API.
    #[serde(skip)]
    pub chip_hz: [u32; warpaxe_chain::MAX_CHIPS],
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
    /// Times the chain was started over for one chip silent for ten
    /// minutes while the others hashed ([`LoneSilence`]).
    pub chip_restarts: u32,
    /// The last of those: chip, core voltage (mV), uptime (s).
    pub last_chip_restart: Option<(u8, u16, u64)>,
    /// Chips whose restarts did not keep them running: left silent
    /// until the next boot.
    pub chips_given_up: Vec<u8>,
    /// Times the chips were switched off and on for one hash domain
    /// stuck for ten minutes while its chip kept hashing
    /// ([`DomainLatch`]).
    pub domain_latch_resets: u32,
    /// The last of those: chip, domain, the domain's share of the chip's
    /// other domains (%), uptime (s).
    pub last_domain_latch_reset: Option<(u8, u8, u8, u64)>,
    /// Domains whose resets did not keep them running: (chip, domain),
    /// left alone until a new clock or voltage, or the next boot.
    pub domains_given_up: Vec<(u8, u8)>,
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
    /// Nonces since boot whose header hash met the target of its own
    /// nbits (AxeOS `blockFound`). On a chain at minimum difficulty
    /// that is every nonce; the stored count and `last_block` move once
    /// per block (`config::record_block`).
    pub blocks_found: u32,
    /// The last recorded block's height and the uptime second it was
    /// found at.
    pub last_block: Option<(u64, u64)>,
}

impl MinerState {
    /// What the chips are expected to hash, H/s: small cores × clock,
    /// chip by chip at its own clock. Without per-chip clocks to go by
    /// (no chain yet, the CPU miner) the chain's clock times the chips,
    /// which is the same figure whenever no chip has an offset.
    #[inline(never)]
    pub fn expected_hs(&self, small_cores: u16) -> f64 {
        let own: f64 = self.chip_clocks().iter().map(|&hz| f64::from(hz)).sum();
        let hz = if own > 0.0 {
            own
        } else {
            f64::from(self.frequency_hz) * f64::from(self.chips.max(1))
        };
        f64::from(small_cores) * hz
    }

    /// The clock of each chip on the chain, Hz.
    pub fn chip_clocks(&self) -> &[u32] {
        &self.chip_hz[..usize::from(self.chips).min(self.chip_hz.len())]
    }
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

/// The self-test that follows a verified first start is due (20 s after
/// the enumeration): until it has started, the chips are not switched
/// off for want of a pool (`board::no_pool`).
static SELFTEST_DUE: AtomicBool = AtomicBool::new(false);

/// The first start's self-test is due and has not started yet.
pub fn selftest_due() -> bool {
    SELFTEST_DUE.load(Ordering::Relaxed)
}

/// The clock asked for has no PLL setting: the chips stay at the clock
/// they have (`warpaxe_board::core_hold::Clock::unreachable`).
static CLOCK_UNREACHABLE: AtomicBool = AtomicBool::new(false);

/// The chips' clock this moment, Hz (the last PLL word written); 0 while
/// no chain runs. With [`SETTLED_MHZ`] what the board task holds a
/// voltage drop on (`warpaxe_board::core_hold`).
static CLOCK_NOW_HZ: AtomicU32 = AtomicU32::new(0);
/// The [`WANTED_MHZ`] request the chips' clock has arrived at: handed to
/// the chain and its ramp over. 0 when none has.
static SETTLED_MHZ: AtomicU32 = AtomicU32::new(0);

/// Counts the requests for per-chip clock offsets (`chipFreqOffsets`,
/// kept by `config::chip_offsets`, which is where the loop reads them):
/// the loop hands the list to the chain when this moves.
static OFFSETS_GEN: AtomicU32 = AtomicU32::new(0);
/// The [`OFFSETS_GEN`] request every chip's clock has arrived at.
static OFFSETS_SETTLED_GEN: AtomicU32 = AtomicU32::new(0);

/// The [`OFFSETS_GEN`] of this moment: another one later says the
/// offsets were asked for anew in between.
pub fn chip_offsets_request() -> u32 {
    OFFSETS_GEN.load(Ordering::SeqCst)
}

/// New clock offsets per chip are stored: the loop ramps the chips
/// concerned there without restarting the chain. Before the
/// voltage that may come with them is released, like a clock
/// ([`request_frequency`]): from this store on the board task sees a
/// chip's ramp under way and holds a voltage drop until it is over.
pub fn request_chip_offsets() {
    OFFSETS_GEN.fetch_add(1, Ordering::SeqCst);
}

/// The clock as the board task needs it to time a voltage drop: what is
/// asked for, what the chips have arrived at, what they run at now.
pub fn clock() -> warpaxe_board::core_hold::Clock {
    // A request for per-chip offsets that the chips have not arrived at
    // yet: nothing has settled, whatever the chain's own clock says.
    let chip_ramp =
        OFFSETS_GEN.load(Ordering::SeqCst) != OFFSETS_SETTLED_GEN.load(Ordering::Relaxed);
    warpaxe_board::core_hold::Clock {
        wanted_mhz: WANTED_MHZ.load(Ordering::SeqCst),
        settled_mhz: if chip_ramp {
            0
        } else {
            SETTLED_MHZ.load(Ordering::Relaxed)
        },
        now_hz: CLOCK_NOW_HZ.load(Ordering::Relaxed),
        unreachable: CLOCK_UNREACHABLE.load(Ordering::Relaxed),
        chip_ramp,
    }
}

/// The stored clock offsets as the chain gets them: the list as it is
/// when it has one entry per chip that answered (`chips` 0: not counted
/// yet); none, with a word in the log, when it does not. Offsets for
/// another chip count are from another board, or the places in the
/// chain are not the ones they were found for.
///
/// The board's limits are the API's to keep for a PATCH; a clock that
/// changes without one (replaced at boot, stepped down after an overheat
/// stop) is checked here against the same limits, and the list is not
/// applied when a chip's own clock would leave them.
fn offsets_for(chips: u8) -> config::ChipOffsets {
    let stored = config::chip_offsets();
    if stored.len == 0 {
        return stored;
    }
    if chips != 0 && stored.len != usize::from(chips) {
        log::warn!(
            "miner: {chips} chip(s) answered, the clock offsets are for {}: every chip at the chain's clock",
            stored.len
        );
        return config::ChipOffsets::NONE;
    }
    let base = capped_mhz(config::get().frequency);
    // The same ceiling as the API's (`api::choices`): the NerdOctaxe's
    // depends on its regulator part.
    let max = board::profile()
        .choices(board::state().vr_part.starts_with("Tps53667"))
        .abs_max_frequency_mhz;
    if let Err(why) = warpaxe_chain::offsets::check(
        &board::chip().pll,
        base,
        stored.list(),
        board::MIN_FREQUENCY_MHZ,
        max,
        board::profile().voltage_domains > 1,
    ) {
        log::warn!("miner: clock offsets not applied at {base} MHz: {why}");
        return config::ChipOffsets::NONE;
    }
    stored
}

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
    if g.as_ref()
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

/// A block: counted for good (the web UI's banner and all-time count)
/// with its hash and its parent's (header byte order here, stored as
/// explorers show them), and in the event log. From the mining threads,
/// whose stacks are internal (an NVS write).
///
/// `false`, and nothing stored, logged or put into the event log, for
/// another nonce that meets the target on the parent of the block
/// already recorded, or at its height unless that one was orphaned
/// (`config::record_block`): at minimum difficulty that is every
/// nonce.
pub fn block_found(height: u64, hash: &[u8; 32], prev: &[u8; 32]) -> bool {
    let hex = block::hash_hex(hash);
    if !config::record_block(
        height,
        crate::clock::unix_now().unwrap_or(0),
        &hex,
        &block::hash_hex(prev),
    ) {
        return false;
    }
    log::warn!("miner: BLOCK FOUND at height {height} (hash {hex})");
    let coin = config::get().coin_ticker;
    crate::events::record(
        "block",
        warpaxe_diag::events::detail::block_found(height, &coin),
    );
    crate::alerts::block(height, hash_difficulty(hash), coin);
    // The watch looks at the newest work again, with this block in
    // hand (the CPU miner finds on another thread than it watches on).
    *WATCHED_TIP.lock().unwrap() = None;
    true
}

/// What the block watch looked at last: the newest work's parent and
/// height.
static WATCHED_TIP: Mutex<Option<([u8; 32], u64)>> = Mutex::new(None);

/// What became of the last found block, read from the work the pool
/// sends (`warpaxe_stratum::block::judge`): confirmed when work builds
/// on it, orphaned when work for the next height builds on another
/// block, unknown when it is still pending and the work is already two
/// heights on. The newest work is looked at once a second, so on a
/// chain with blocks less than a second apart the work for the next
/// height can go by unseen and the block ends as unknown. From the
/// mining loops, whose stacks are internal
/// (a change is one NVS write and an event); nothing happens until the
/// newest work's parent or height changes. Work for another coin (a
/// pool switch, a fallback on another chain) says nothing.
pub fn watch_block() {
    let Some(tip) = stratum::tip() else {
        return;
    };
    {
        let mut g = WATCHED_TIP.lock().unwrap();
        if *g == Some(tip) {
            return;
        }
        *g = Some(tip);
    }
    let b = config::blocks();
    let (Some(hash), Some(now)) = (
        block::hash_from_hex(&b.hash),
        block::Status::parse(&b.status),
    ) else {
        return;
    };
    if b.coin != config::get().coin_ticker {
        return;
    }
    let Some(next) = block::judge(&hash, b.h, now, &tip.0, tip.1) else {
        return;
    };
    // Only for the block that was judged: the CPU miner may have found
    // another one on its own thread since.
    if !config::set_block_status(&b.hash, next.as_str()) {
        return;
    }
    log::warn!(
        "miner: block {} ({}) is {}: the pool's work for height {} builds on {}",
        b.h,
        b.hash,
        next.as_str(),
        tip.1,
        block::hash_hex(&tip.0)
    );
    crate::events::record(
        "block",
        match next {
            block::Status::Orphaned => warpaxe_diag::events::detail::block_orphaned(b.h),
            block::Status::Unknown => warpaxe_diag::events::detail::block_unknown(b.h),
            _ => warpaxe_diag::events::detail::block_confirmed(b.h),
        },
    );
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

/// How often each value of the nonce's eight address bits (bits 17–24,
/// `(nonce >> 17) & 0xFF`) was seen since the chain started: what the
/// chip attribution divides by `256 / chips` (ESP-Miner; NerdOS reads
/// bits 24–25 instead). On a two-chip board the values should fall into
/// two halves, 0–127 for chip 0 and 128–255 for chip 1, each about half
/// of all nonces; anything else is the attribution question answered.
static NONCE_ADDRESS_BITS: Mutex<[u32; 256]> = Mutex::new([0; 256]);

fn address_seen(nonce: u32) {
    // `nonce` is the wire bytes read little endian (what goes into the
    // header); the chip's fields are in the big-endian reading, as in
    // `JobResult::parse`. Without the swap this counted bits 9..15 and
    // bit 0 of that reading: evenly spread on any board.
    let bits = ((nonce.swap_bytes() >> 17) & 0xFF) as usize;
    let mut g = NONCE_ADDRESS_BITS.lock().unwrap();
    g[bits] = g[bits].saturating_add(1);
}

/// The address-bit values seen and how often, ascending (diagnostics).
pub fn address_bits() -> Vec<(u8, u32)> {
    NONCE_ADDRESS_BITS
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(b, n)| (b as u8, *n))
        .collect()
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
    // Sequentially consistent: the voltage that comes with it is
    // released after this store (`board::request_core_mv`).
    WANTED_MHZ.store(mhz, Ordering::SeqCst);
}

pub fn state() -> MinerState {
    STATE.lock().unwrap().clone().unwrap_or_default()
}

fn update(f: impl FnOnce(&mut MinerState)) {
    let mut g = STATE.lock().unwrap();
    f(state_mut(&mut g));
}

/// The state, made on first use. One copy of that in the image: inlined
/// into every `update` it cost 20 KB of flash once the state had grown
/// by two lists (measured 2026-11-12).
#[inline(never)]
fn state_mut(state: &mut Option<MinerState>) -> &mut MinerState {
    state.get_or_insert_with(MinerState::default)
}

/// The CPU miner (a T-Display-S3 without chips) reports into the same
/// state the API and the display read.
pub(crate) fn update_state(f: impl FnOnce(&mut MinerState)) {
    update(f);
}

/// A silent-chip event at most once per [`SILENT_EVENT_S`] per chip: a
/// chip that flaps would otherwise fill the event log and wear the
/// flash (the ring coalesces repeats within a minute only).
const SILENT_EVENT_S: u64 = 600;
static LAST_SILENT_EVENT: Mutex<[u64; 16]> = Mutex::new([0; 16]);

fn silent_event(index: u8) {
    let now = diag::uptime_s();
    let mut last = LAST_SILENT_EVENT.lock().unwrap();
    let Some(slot) = last.get_mut(usize::from(index)) else {
        return;
    };
    if *slot != 0 && now.saturating_sub(*slot) < SILENT_EVENT_S {
        return;
    }
    *slot = now;
    drop(last);
    crate::events::record("chain", warpaxe_diag::events::detail::chip_silent(index));
}

/// The mining loop's stack, bytes: internal RAM, since the loop writes
/// flash (a new best difficulty, a block, a verified board).
///
/// The chain driver with its counters for [`warpaxe_chain::MAX_CHIPS`]
/// chips (7.8 KB) and the two policies (2.1 KB) used to be locals of
/// the loop: a frame of 12,720 bytes at main c24b901, which left 3.5 KB
/// and ran for months, and of 13,072 bytes once the chain kept a clock
/// per chip. That was not enough: on a Naja Duo (2026-11-12, main
/// c372ac4) the loop overflowed every one to four minutes, in
/// `format!("{:.3}")` of a nonce's difficulty, whose rare exact path
/// (grisu gives up, dragon takes over) needs 2.7 KB, with an interrupt
/// landing on top. A larger stack (20 KB, 9f3a31a) cured that and took
/// 4 KB of internal RAM, which a board with a TLS pool does not have to
/// give (a Gaia: web API at a crawl, an update that timed out).
///
/// So the three live in PSRAM now ([`Held`]) and that formatting is
/// gone from the loop. The loop's frame is 3.3 KB, and of a 12 KB
/// stack 8.9 KB are left for what it calls, against the 3.5 KB that
/// were enough before; the 4 KB less than v0.1.43 took go back to the
/// internal heap (main has grown 1.3 KB of static data since that
/// release, so the heap ends about 2.7 KB above it). The least that
/// was ever free is in `minerStackFree` ([`stack_free`]): 16 KB again
/// is this one number, should a board show less than it ought to.
const STACK_BYTES: usize = 12 * 1024;
/// Below this much stack never touched, the loop says so in the log.
const STACK_WARN_BYTES: u32 = 3072;

/// The least stack the mining loop has had to spare since boot, bytes
/// (FreeRTOS's high-water mark, looked at once a second); 0 until the
/// loop has run a second.
static STACK_FREE: AtomicU32 = AtomicU32::new(0);

/// The least stack the mining loop has had to spare since boot, bytes.
pub fn stack_free() -> u32 {
    STACK_FREE.load(Ordering::Relaxed)
}

/// What the mining loop keeps for as long as the firmware runs and
/// that is too large for its stack: the chain driver (made anew at
/// every chain start, in the same place: `Chain::renew`) and the two
/// policies, which outlive the chains. 10 KB.
struct Held {
    chain: Chain,
    lone: LoneSilence,
    latch: DomainLatch,
}

/// The place for [`Held`], taken once and never given back: PSRAM,
/// asked for by name (a plain allocation of this size would come from
/// internal RAM, `CONFIG_SPIRAM_MALLOC_ALWAYSINTERNAL`). The loop's
/// stack stays internal; data in PSRAM is no trouble for its flash
/// writes, which touch none of it while the cache is off. Without
/// PSRAM (no board is built that way) internal RAM has to do, said in
/// the log.
///
/// A first driver has to be built by value (7.8 KB on the stack of
/// whoever calls this), so the mining loop does not call it itself
/// ([`held`]).
fn hold() -> *mut Held {
    use esp_idf_svc::sys;
    let (size, align) = (core::mem::size_of::<Held>(), core::mem::align_of::<Held>());
    let mut p = unsafe {
        sys::heap_caps_aligned_alloc(align, size, sys::MALLOC_CAP_SPIRAM | sys::MALLOC_CAP_8BIT)
    };
    if p.is_null() {
        log::warn!("miner: no PSRAM for the chain's state: {size} bytes of internal RAM");
        p = unsafe { sys::heap_caps_aligned_alloc(align, size, sys::MALLOC_CAP_8BIT) };
    }
    assert!(!p.is_null(), "miner: no memory for the chain's state");
    let p = p.cast::<Held>();
    // Field by field: every one a whole value before anyone looks.
    unsafe {
        let chip = board::chip();
        core::ptr::addr_of_mut!((*p).chain).write(Chain::new(chip, ChainConfig::for_profile(chip)));
        core::ptr::addr_of_mut!((*p).lone).write(LoneSilence::new());
        core::ptr::addr_of_mut!((*p).latch).write(DomainLatch::new());
    }
    p
}

/// [`Held`] for the mining loop, made on a stack that is not the
/// loop's: a thread that lives for this one call, its stack in PSRAM
/// (it writes no flash). The loop's own stack never carries a driver by
/// value, so `minerStackFree` shows what the loop itself needs.
fn held() -> &'static mut Held {
    static PLACE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // hold() and the driver it builds take 9.4 KB of this stack.
    const STACK: usize = 16 * 1024;
    let make = || PLACE.store(hold() as usize, Ordering::SeqCst);
    // Without PSRAM for that stack, one from internal RAM, as short-lived.
    let thread = diag::spawn_psram("held", STACK, make)
        .or_else(|_| std::thread::Builder::new().stack_size(STACK).spawn(make));
    match thread {
        Ok(thread) => {
            let _ = thread.join();
        }
        Err(e) => log::warn!("miner: no thread for the chain's state: {e}"),
    }
    let mut p = PLACE.load(Ordering::SeqCst) as *mut Held;
    if p.is_null() {
        // No thread at all: here after all, with more stack than the
        // loop has by rights. Nothing else is left to try.
        p = hold();
    }
    // Made once, for this thread alone.
    unsafe { &mut *p }
}

pub fn start(uart: ChipUart) {
    use esp_idf_svc::sys;
    // The task carries a name of its own, so a panic of this loop says
    // "task miner" rather than the "pthread" every unnamed thread has.
    unsafe {
        let mut cfg = sys::esp_pthread_get_default_config();
        cfg.thread_name = c"miner".as_ptr();
        sys::esp_pthread_set_cfg(&cfg);
    }
    let spawned = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(move || run(uart));
    unsafe {
        let d = sys::esp_pthread_get_default_config();
        sys::esp_pthread_set_cfg(&d);
    }
    spawned.expect("miner thread");
}

fn run(mut uart: ChipUart) {
    let epoch = Instant::now();
    let now_us = || epoch.elapsed().as_micros() as u64;
    // TX driven low while the chips sit without their I/O rails
    // (board::parks_uart), back on the UART before the next reset pulse.
    let mut parked = false;
    // A board not verified yet (board::choice_pending): the chain comes
    // up at the cautious voltage and its enumeration decides whether the
    // choice stands. Three inits without a chip refuse it; a passed
    // count runs the self-test once, a little after the chain is up.
    let mut first_start_attempts: u8 = 0;
    let mut selftest_due: Option<Instant> = None;
    // One chip silent while the others hash, and one hash domain stuck
    // while its chip hashes: both kept across chain restarts, and both
    // share the hour between two interventions. A board whose nonce-to-
    // chip attribution is not confirmed on a device (the Naja Duo) only
    // reports the silence (`Profile::silent_chip_restart`).
    // In PSRAM with the chain driver, not on this stack (`Held`).
    let Held { chain, lone, latch } = held();
    if !board::profile().silent_chip_restart {
        log::warn!(
            "miner: the silent-chip watchdog on this board reports only, no restart for one silent chip (profile)"
        );
        *lone = LoneSilence::report_only();
    }
    loop {
        // No chain runs from here until the next one's first PLL write:
        // nothing is coming down, a voltage drop need not wait.
        CLOCK_NOW_HZ.store(0, Ordering::Relaxed);
        SETTLED_MHZ.store(0, Ordering::Relaxed);
        CLOCK_UNREACHABLE.store(false, Ordering::Relaxed);
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
            } else if board::no_pool() {
                "NOPOOL"
            } else {
                "POWER"
            };
            // Not hashing: say so, instead of the last value standing.
            update(|s| {
                s.phase = why.into();
                s.hashrate_ghs = 0.0;
                s.hashrate_now_ghs = 0.0;
            });
            // The pool's work goes on while the chips are off.
            watch_block();
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        // The clock the chain starts with becomes the one asked for,
        // before the reset pulse (up to 350 ms): a request from here on
        // differs from it and goes to the chain on the loop's first
        // pass. A request that fell between reading the config and the
        // store would be overwritten by it, so the config is read again
        // afterwards: the request has changed it (the PATCH saves, then
        // asks), and the store is done over with the new clock.
        // The offsets request the chain starts with, read before the
        // config: one that lands in between is seen as new by the loop
        // and handed over again (the same list: nothing happens).
        let mut applied_offsets_gen = OFFSETS_GEN.load(Ordering::SeqCst);
        let (cfg, start_mhz) = loop {
            let cfg = config::get();
            let start_mhz = capped_mhz(cfg.frequency);
            WANTED_MHZ.store(start_mhz, Ordering::Relaxed);
            if capped_mhz(config::get().frequency) == start_mhz {
                break (cfg, start_mhz);
            }
        };
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
        // Which BM1373 init this start uses: the setting, else the board's.
        board::select_chip_init(&cfg.chip_init);
        let chain_cfg = ChainConfig {
            expected_chips: cfg.asic_count,
            frequency_hz: start_mhz.saturating_mul(1_000_000),
            difficulty: pool_diff,
            ..ChainConfig::for_profile(board::chip())
        };
        chain.renew(board::chip(), chain_cfg);
        // Each chip's own clock is part of the bring-up ramp: no chip
        // passes its clock on the way up.
        // (What each chip then runs at is in `asicFrequencies`.)
        let mut chip_offsets = offsets_for(0);
        chain.set_chip_offsets(chip_offsets.list());
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
            "miner: chain start, target {start_mhz} MHz, difficulty {pool_diff}, {} init",
            board::chip_init_name()
        );

        // serial -> (job, notify count when built, uptime s when built)
        let mut jobs: HashMap<u32, (Job, u64, u64)> = HashMap::new();
        let mut gate = WorkGate::new(stratum::state().clean_jobs);
        let mut applied_diff = pool_diff;
        let mut worth = NonceWorth::new(pool_diff);
        // The pool difficulty last seen and since when: VarDiff has settled
        // after a minute without a change.
        let mut pool_diff_seen = stratum::state().difficulty;
        let mut pool_diff_since = Instant::now();
        let mut applied_mhz = start_mhz;
        // The clock asked for last has a PLL setting.
        let mut clock_reachable = true;
        let mut hashes = 0f64;
        let mut hashes_since = epoch.elapsed();
        let mut buckets: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
        let mut counting = false;
        let mut last_phase = Phase::Idle;
        let mut failed_at: Option<Instant> = None;
        // Nonces since this chain start: a chain that never produced one
        // under ESP-Miner's BM1373 init is not a hang, it never came up.
        let mut nonces_since_start: u64 = 0;
        let mut last_work: Option<Instant> = None;
        let mut last_counters: Option<Instant> = None;
        // Counter restarts already said, per chip (`ChipMonitor::restarts`).
        let mut restarts_seen = [0u32; warpaxe_chain::MAX_CHIPS];
        let mut all_silent_since: Option<Instant> = None;
        let mut lone_at = Instant::now();
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
                        nonces_since_start += 1;
                        core_seen(f.chip, f.core, f.small_core);
                        address_seen(f.nonce);
                        let Some((job, job_seq, job_t)) = jobs.get(&f.work.serial) else {
                            update(|s| s.unknown_job += 1);
                            continue;
                        };
                        let header = job.header(f.version, f.nonce);
                        let hash = sha256d(&header);
                        let diff = hash_difficulty(&hash);
                        let hex: String = header.iter().map(|b| format!("{b:02x}")).collect();
                        // Three decimals by whole numbers: `{:.3}` of a
                        // float takes up to 2.7 KB of stack on its exact
                        // path, which this loop does not have to give
                        // for every nonce (see STACK_BYTES).
                        let milli = (diff * 1000.0).round() as u64;
                        update(|s| {
                            s.last_found = format!(
                                "serial={} chip={} core={} version={:08x} job_version={:08x} nonce={:08x} ntime={:08x} diff={}.{:03} chip_diff={} header={hex}",
                                f.work.serial, f.chip, f.core, f.version, job.version, f.nonce, f.ntime, milli / 1000, milli % 1000, chain.difficulty()
                            )
                        });
                        // Each nonce is worth the ticket mask in hashes; right
                        // after a raise, one still from the lower mask counts
                        // at that (warpaxe_chain::worth).
                        // Follows the chain also where it set the mask itself
                        // (a re-init goes back to the configured one).
                        worth.set(chain.difficulty(), now);
                        let chip_diff = f64::from(worth.mask());
                        hashes += worth.hashes(diff, now);
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
                        // The share first: what follows may write to flash
                        // (a new best difficulty, a block's record), and a
                        // block's share must not wait for that.
                        if diff >= want {
                            // How old the stratum job is at submit time, in
                            // notifies and seconds: a pool keeps only a few
                            // recent jobs, older ones come back "stale".
                            let age_n = stratum::state().jobs_received.saturating_sub(*job_seq);
                            let age_s = diag::uptime_s().saturating_sub(*job_t);
                            let block = block::meets_target(&hash, job.nbits);
                            if stratum::submit(job, f.ntime, f.nonce, f.version ^ job.version, block) {
                                update(|s| {
                                    s.shares_sent += 1;
                                    s.submit_age_hist[(age_n as usize).min(3)] += 1;
                                    s.last_submit_age = format!("{age_n} notifies, {age_s} s");
                                });
                            }
                        } else {
                            update(|s| s.low_diff += 1);
                        }
                        config::raise_best_diff(diff as u64);
                        // A block: the hash at or below the target in
                        // the header's own nbits, exactly (not the rounded
                        // network difficulty of whatever job is current).
                        if block::meets_target(&hash, job.nbits) {
                            // Counted in RAM every time; stored, logged
                            // and shown once per block.
                            let height = job.height;
                            let first = block_found(height, &hash, &job.prev_hash);
                            update(|s| {
                                s.blocks_found += 1;
                                if first {
                                    s.last_block = Some((height, diag::uptime_s()));
                                }
                            });
                        }
                    }
                    Event::Ready => {
                        log::info!("miner: chain ready, {} chip(s)", chain.chip_count());
                        CORES.lock().unwrap().clear();
                        *NONCE_ADDRESS_BITS.lock().unwrap() = [0; 256];
                        board::chain_ready();
                        // The chain's first window gets the early look
                        // for a domain dead from the start.
                        latch.chain_ready();
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
                    Event::Enumerated { count } => {
                        log::info!("miner: {count} chip(s) answered");
                        // Before the ramp: offsets for another chip
                        // count are no chip's here. Looked at again
                        // below, now that the chips are counted.
                        applied_offsets_gen = applied_offsets_gen.wrapping_sub(1);
                        // A second chip's diode reads from about here
                        // on: its settle window counts from the answer.
                        board::chips_started();
                        // And both diodes count as readings from here.
                        board::chips_answered();
                        if board::choice_pending() {
                            // Against the profile's own count, never the
                            // configured asicCount: that is the one tell
                            // the NerdQAxe++/Octaxe pair has.
                            let expected = board::profile().asic_count;
                            match choice::first_start_verdict(
                                count,
                                expected,
                                None,
                                &board::sibling_counts(),
                            ) {
                                FirstStart::Verified { chips }
                                | FirstStart::ShortCount { found: chips, .. } => {
                                    // Due before the choice counts as
                                    // verified: no moment in between at
                                    // which a missing pool could switch
                                    // the chips off.
                                    SELFTEST_DUE.store(true, Ordering::Relaxed);
                                    board::verify_choice(chips, expected);
                                    selftest_due = Some(Instant::now() + Duration::from_secs(20));
                                }
                                v => board::reject_choice(v),
                            }
                        }
                    }
                    Event::UnexpectedChip { chip_id } => {
                        log::warn!(
                            "miner: a chip answered as {chip_id:#06x}, not a {}",
                            board::chip().name
                        );
                        if board::choice_pending() {
                            board::reject_choice(FirstStart::WrongChip { chip_id });
                        }
                    }
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
                        // No chip answered the id read: that read comes
                        // before anything of the init, so another init
                        // would not change it (a regulator still coming
                        // up, a loose contact). Only a chain that
                        // answered and then failed steps down the ladder.
                        if !matches!(why, warpaxe_chain::InitError::NoChips) {
                            if let Some((from, to)) = board::chip_init_fall_back() {
                                log::warn!(
                                    "miner: {from} init: the chain did not come up, {to} init from the next start until reboot"
                                );
                            }
                        }
                        update(|s| {
                            s.init_failures += 1;
                            s.last_error = format!(
                                "{why:?} (rx frames {}, dropped {}, crc {})",
                                d.frames, d.dropped, d.crc_failures
                            );
                        });
                        if board::choice_pending() {
                            // A regulator that did not come up is a board
                            // fault, not a wrong board: no verdict then.
                            let no_regulator = (profile.tps546().is_some()
                                || profile.tps53647().is_some())
                                && !board::state().has_vr;
                            if no_regulator {
                                log::warn!(
                                    "miner: first start without the regulator up: the choice stays pending"
                                );
                            } else {
                                first_start_attempts += 1;
                                if first_start_attempts >= 3 {
                                    board::reject_choice(FirstStart::NoChips);
                                } else {
                                    log::warn!(
                                        "miner: first start attempt {first_start_attempts} of 3 found no chips"
                                    );
                                }
                            }
                        }
                    }
                    Event::ChipSilent { index } => {
                        log::warn!("miner: chip {index} silent");
                        silent_event(index);
                    }
                    Event::ChipBack { index } => log::info!("miner: chip {index} back"),
                    Event::FrequencyApplied { hz } => {
                        log::info!("miner: clock {} MHz", hz / 1_000_000)
                    }
                    Event::FrequencyUnreachable { hz } => {
                        // The chain starts at the chip's stock clock
                        // instead. Only a stored clock can get here
                        // (the API refuses one without a PLL setting,
                        // the boot replaces one in the config), and it
                        // is not a request under way: the voltage is
                        // not kept waiting for it.
                        log::warn!("miner: clock {hz} Hz unreachable")
                    }
                    _ => {}
                }
            }

            if selftest_due.is_some_and(|t| Instant::now() >= t) {
                selftest_due = None;
                match crate::selftest::start() {
                    Ok(id) => log::info!("miner: self-test {id} started after the first start"),
                    Err(e) => log::warn!("miner: self-test after the first start not started: {e}"),
                }
                // Running now (or refused): its own flag takes over.
                SELFTEST_DUE.store(false, Ordering::Relaxed);
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

            // A clock asked for over the API goes to the chain in every
            // phase: during the bring-up it becomes the ramp's target
            // (the chips never pass it), afterwards a ramp of its own.
            let want_mhz = WANTED_MHZ.load(Ordering::Relaxed);
            if want_mhz != applied_mhz && want_mhz > 0 {
                // A new request: whatever clock had settled no longer
                // has (a return to an earlier value in the middle of
                // another ramp would otherwise read as arrived).
                SETTLED_MHZ.store(0, Ordering::Relaxed);
                clock_reachable = match chain.set_frequency(want_mhz.saturating_mul(1_000_000)) {
                    Some(hz) => {
                        log::info!(
                            "miner: ramp to {} MHz (PLL {} MHz)",
                            want_mhz,
                            hz / 1_000_000
                        );
                        true
                    }
                    None => {
                        log::warn!(
                            "miner: {} MHz has no PLL setting, keeping {} MHz",
                            want_mhz,
                            applied_mhz
                        );
                        false
                    }
                };
                applied_mhz = want_mhz;
            }
            // New clock offsets per chip, in every phase like the clock:
            // the chain ramps each chip concerned to its own.
            let offsets_gen = OFFSETS_GEN.load(Ordering::SeqCst);
            if offsets_gen != applied_offsets_gen {
                applied_offsets_gen = offsets_gen;
                chip_offsets = offsets_for(chain.chip_count());
                // Nothing happens for the list the chain already has.
                chain.set_chip_offsets(chip_offsets.list());
            }
            // The request is with the chain and its ramp is over: from
            // here a lower voltage that came with it may go. A clock the
            // PLL cannot make never settles, and the board keeps such a
            // drop waiting (the API refuses such a clock in the first
            // place).
            CLOCK_UNREACHABLE.store(!clock_reachable, Ordering::Relaxed);
            if clock_reachable && applied_mhz == want_mhz && chain.clock_settled() {
                SETTLED_MHZ.store(want_mhz, Ordering::Relaxed);
                // Every chip is at its own clock too.
                OFFSETS_SETTLED_GEN.store(applied_offsets_gen, Ordering::Relaxed);
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
                    let now_read = now_us();
                    let asics: Vec<AsicMonitor> = mon
                        .chips()
                        .iter()
                        .enumerate()
                        .map(|(i, c)| AsicMonitor {
                            total: c.total.ghs,
                            error_count: mon.has_error_counter().then_some(c.error.value),
                            hash_count: mon.has_error_counter().then_some(c.total.value),
                            counter_restarts: c.restarts,
                            domains: (0..mon.domains()).map(|d| mon.domain_ghs(i, d)).collect(),
                            error_ghs: c.error.ghs,
                            total_1m: c.total.ghs_1m,
                            fresh: counters_fresh(c, now_read),
                        })
                        .collect();
                    // A chip whose counter went back: it reset itself and
                    // nothing else here would say so.
                    for (i, (seen, c)) in restarts_seen.iter_mut().zip(mon.chips()).enumerate() {
                        if c.restarts > *seen {
                            *seen = c.restarts;
                            log::warn!(
                                "miner: chip {i}: its hash counter went back, the chip has reset itself ({} since the chain start)",
                                c.restarts
                            );
                            update(|s| {
                                s.last_error =
                                    format!("chip {i} reset itself (its hash counter went back)")
                            });
                        }
                    }
                    // Queued events to flash from this internal stack.
                    crate::events::flush();
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
                        worth.set(want, now_us());
                        update(|s| s.difficulty = want);
                    }
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
            CLOCK_NOW_HZ.store(chain.frequency_hz(), Ordering::Relaxed);
            update(|s| {
                s.chips = chain.chip_count();
                s.frequency_hz = chain.frequency_hz();
                for (own, &hz) in s.chip_hz.iter_mut().zip(chain.chip_frequencies_hz()) {
                    *own = hz;
                }
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
                    crate::events::record(
                        "chain",
                        warpaxe_diag::events::detail::chain_restart(n, mv),
                    );
                    // A chain that answered the id read but never hashed
                    // is the init, not a hang: one rung down the BM1373
                    // ladder (ESP-Miner 3 MBaud → 1 MBaud → NerdOS).
                    if nonces_since_start == 0 {
                        if let Some((from, to)) = board::chip_init_fall_back() {
                            log::warn!(
                                "miner: {from} init: no nonce since the chain start, {to} init from the next start until reboot"
                            );
                        }
                    }
                    break;
                }
            } else {
                all_silent_since = None;
            }
            // The single-chip policy works in seconds: once a second is
            // enough, and spares the config copy on every 2 ms pass.
            if lone_at.elapsed() >= Duration::from_secs(1) {
                lone_at = Instant::now();
                // The stack's low mark, and a word when it gets thin.
                let free = diag::stack_unused();
                if free < STACK_WARN_BYTES && STACK_FREE.load(Ordering::Relaxed) >= STACK_WARN_BYTES
                {
                    log::warn!("miner: only {free} bytes of stack were never used");
                }
                STACK_FREE.store(free, Ordering::Relaxed);
                watch_block();
                let mut flags = [false; warpaxe_chain::MAX_CHIPS];
                for (f, c) in flags.iter_mut().zip(chain.chips()) {
                    *f = c.silent;
                }
                let flags = &flags[..usize::from(n).min(flags.len())];
                // Work going out right now, not just a chain past bring-up:
                // with the pool down a chip shows nothing either way.
                let working = chain.accepting_work()
                    && last_work.is_some_and(|t| {
                        t.elapsed() < Duration::from_micros(chain.work_timeout_us())
                    });
                let cfg = config::get();
                let mv = cfg.core_voltage;
                let now_s = diag::uptime_s();
                lone.set_point(cfg.frequency, mv);
                latch.set_point(cfg.frequency, mv);
                // A chip given another clock of its own is at a new
                // point too, that chip alone.
                lone.set_chip_offsets(chip_offsets.list());
                latch.set_chip_offsets(chip_offsets.list());
                let verdict = lone.observe(now_s, working, flags);
                let shown = state();
                // Also after a new point gave given-up chips another chance.
                let given_up: Vec<u8> = lone.given_up().collect();
                if shown.chips_given_up != given_up {
                    update(|s| s.chips_given_up = given_up);
                }
                match verdict {
                    Some(LoneVerdict::Restart { chip }) => {
                        latch.note_reset(now_s);
                        update(|s| {
                            s.chip_restarts += 1;
                            s.last_chip_restart = Some((chip, mv, diag::uptime_s()));
                            s.hashrate_now_ghs = 0.0;
                            s.last_error = format!(
                                "chip {chip} silent for 10 min at {mv} mV, chain restarted"
                            );
                        });
                        log::warn!(
                            "miner: chip {chip} silent for 10 min at {mv} mV, restarting the chain"
                        );
                        crate::events::record(
                            "chain",
                            warpaxe_diag::events::detail::chip_restart(chip, mv),
                        );
                        break;
                    }
                    Some(LoneVerdict::GiveUp { chip }) => {
                        log::warn!(
                            "miner: chip {chip} still silent after 3 restarts, leaving it until a new point or the next boot"
                        );
                        crate::events::record(
                            "chain",
                            warpaxe_diag::events::detail::chip_given_up(chip),
                        );
                    }
                    // Report-only board: said every ten minutes with
                    // what would tell a wrong attribution from a dead
                    // chip (the nonces each chip was credited with, the
                    // address bits they carried), nothing restarted.
                    Some(LoneVerdict::Report { chip }) => {
                        let credited: Vec<u32> = core_hist().iter().map(|h| h.nonces).collect();
                        let bits = address_bits();
                        log::warn!(
                            "miner: chip {chip} silent for 10 min at {mv} mV; this board only reports (no restart): nonces per chip {credited:?}, nonce address bits {bits:?}, hash counters in hashrateMonitor"
                        );
                        update(|s| {
                            s.last_error = format!(
                                "chip {chip} silent for 10 min at {mv} mV (report only on this board)"
                            );
                        });
                    }
                    None => {}
                }
                // One hash domain stuck while its chip hashes: the
                // domain rates the counter reads above filled in, fresh
                // or not per chip, with the watchdog's silent flag (a
                // silent chip is the case above, not this one).
                let mon = chain.monitor();
                let now_read = now_us();
                let mut rates = [[0f32; warpaxe_chain::monitor::MAX_DOMAINS]; warpaxe_chain::MAX_CHIPS];
                let domains = mon.domains().min(warpaxe_chain::monitor::MAX_DOMAINS);
                for (r, c) in rates.iter_mut().zip(mon.chips()) {
                    for (d, m) in r.iter_mut().zip(&c.domains).take(domains) {
                        // Unscaled (the BM1373's ×2 of `hashrateMonitor`
                        // cancels in a ratio within one chip).
                        *d = m.ghs;
                    }
                }
                let mut samples = [LatchSample {
                    silent: false,
                    fresh: false,
                    domains: &[],
                }; warpaxe_chain::MAX_CHIPS];
                let n_mon = mon.chips().len().min(samples.len());
                for (i, ((s, c), r)) in samples
                    .iter_mut()
                    .zip(mon.chips())
                    .zip(&rates)
                    .enumerate()
                {
                    *s = LatchSample {
                        silent: chain.chips().get(i).is_some_and(|c| c.silent),
                        fresh: counters_fresh(c, now_read),
                        domains: &r[..domains],
                    };
                }
                // A self-test under way ends with "chainStopped" if the
                // chips go off: its 80 s count as no work for the latch.
                let latch_working = working && !crate::selftest::running();
                let verdict = latch.observe(now_s, latch_working, &samples[..n_mon]);
                let given_up: Vec<(u8, u8)> = latch.given_up().collect();
                if shown.domains_given_up != given_up {
                    // The event from the list, not the verdict: a reset
                    // for another chip on the same second outranks a
                    // give-up in the verdict, the list has both.
                    for &(chip, domain) in given_up
                        .iter()
                        .filter(|d| !shown.domains_given_up.contains(d))
                    {
                        log::warn!(
                            "miner: chip {chip} domain {domain} still stuck after 3 resets, leaving it until a new point or the next boot"
                        );
                        crate::events::record(
                            "chain",
                            warpaxe_diag::events::detail::domain_given_up(chip, domain),
                        );
                    }
                    update(|s| s.domains_given_up = given_up);
                }
                match verdict {
                    Some(LatchVerdict::Reset {
                        chip,
                        domain,
                        percent,
                        early,
                    }) => {
                        // Dead from the chain's start: found after three
                        // minutes instead of ten.
                        let (mins, when) = if early {
                            (
                                warpaxe_chain::latch::EARLY_WINDOW_S / 60,
                                " (early, from the chain's start)",
                            )
                        } else {
                            (warpaxe_chain::latch::WINDOW_S / 60, "")
                        };
                        lone.note_restart(now_s);
                        // The full off/on path. `false` only without the
                        // board thread (no chips to speak of) or in the
                        // moment a stop landed since this pass's
                        // chain_stopped check: the chain restart below is
                        // all there is then, and the hour's slot and the
                        // try are spent on it all the same.
                        let cycled = board::cycle_chips("domain latch");
                        update(|s| {
                            s.domain_latch_resets += 1;
                            s.last_domain_latch_reset = Some((chip, domain, percent, now_s));
                            s.hashrate_now_ghs = 0.0;
                            s.last_error = format!(
                                "chip {chip} domain {domain} at {percent} % of the chip's other domains for {mins} min, chips {}",
                                if cycled { "switched off and on" } else { "restarted" }
                            );
                        });
                        log::warn!(
                            "miner: chip {chip} domain {domain} at {percent} % of the chip's other domains for {mins} min{when} at {} MHz / {mv} mV, chips {}",
                            cfg.frequency,
                            if cycled { "off and on" } else { "restarted" }
                        );
                        crate::events::record(
                            "chain",
                            if early {
                                warpaxe_diag::events::detail::domain_latch_early(
                                    chip, domain, percent, mins,
                                )
                            } else {
                                warpaxe_diag::events::detail::domain_latch(chip, domain, percent)
                            },
                        );
                        break;
                    }
                    // Said above, from the list.
                    Some(LatchVerdict::GiveUp { .. }) | None => {}
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
