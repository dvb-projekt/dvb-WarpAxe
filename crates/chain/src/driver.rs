//! The chain state machine.
//!
//! Life of a chain: the caller pulses the reset line, calls
//! [`Chain::start`], then [`Chain::poll`] on every loop iteration with
//! the current time. The driver walks the profile's init list, counts
//! the chips that answer the id read, re-renders the list for that
//! count, addresses the chips, switches the baud rate, ramps the PLL to
//! the configured frequency, writes the hash counting number and
//! raises [`Event::Ready`]. From then on [`Chain::submit`] sends work
//! and `poll` yields [`Event::Nonce`] for every nonce it can attribute
//! to live work and a known chip — and counts, rather than yields,
//! everything it cannot.
//!
//! Job ids: the chip echoes four bits of the id it was given (bits 6..3
//! of the byte), so there are sixteen distinguishable jobs in flight on
//! the BM1366/68/70/73 and thirty-two on the BM1397 (bits 7..2);
//! [`SLOTS`] holds the larger number, and a slot is reused after that
//! many submissions. The caller keeps its own map from
//! [`WorkRef::serial`] to whatever the pool gave it.
//!
//! The BM1397 path: no version rolling in the chip, so [`Chain::submit`]
//! sends the profile's number of midstates per job, one per version the
//! driver rolled with the BIP 320 mask, and a nonce's version is the
//! job's rolled as many times as the midstate number it carries.

use core::fmt;

use warpaxe_core::chip::{render, ChipProfile, Hcn, Render, Rendered};
use warpaxe_core::decoder::{self, RxDecoder};
use warpaxe_core::frame::{encode_cmd, encode_job, FrameError, Group, Op, MAX_FRAME_LEN};
use warpaxe_core::job::{self, Job, JobIds, MidstateJob, MIDSTATE_JOB_LEN_4};
use warpaxe_core::pll;
use warpaxe_core::regs::{self, read_payload, reg, write_payload};
use warpaxe_core::result::{JobResult, RegisterResult};

use crate::monitor::HashrateMonitor;
use crate::uart::Uart;

/// Chips the driver can address. Octaxe has 8; 16 leaves room.
pub const MAX_CHIPS: usize = 16;
/// Distinguishable jobs in flight at most: the BM1397 echoes five id
/// bits ([`ChipProfile::job_slots`]).
pub const SLOTS: usize = 32;
/// Recent nonce keys remembered per chip for duplicate detection.
const RECENT: usize = 8;
/// Key that no real nonce can produce (job id ≤ 120 in bits 48..55).
const NO_KEY: u64 = u64::MAX;

/// Tunables for one chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Chips the board is supposed to have; 0 accepts whatever answers.
    /// A mismatch is reported, not fatal — the chain runs with what it
    /// found, like the reference driver does.
    pub expected_chips: u8,
    /// Target clock after the ramp, Hz.
    pub frequency_hz: u32,
    /// Share difficulty for the ticket mask.
    pub difficulty: u32,
    /// How long to collect chip-id replies after the id read, µs.
    pub count_window_us: u64,
    /// Where the frequency ramp starts, Hz. The reference driver starts
    /// its ramp at 56.25 MHz (from `do_frequency_ramp_up`; unverified
    /// against a capture).
    pub ramp_start_hz: u32,
    /// Ramp step, Hz. 6.25 MHz in the reference driver.
    pub ramp_step_hz: u32,
    /// Wait between ramp steps, µs. The reference driver's per-step
    /// delay is not known from a capture; 100 ms is conservative.
    pub ramp_delay_us: u64,
    /// A chip with work that has been quiet for this many expected
    /// nonce intervals is reported silent. The interval at difficulty
    /// `d` and clock `f` is `d·2³² / (f·small_cores)`; at 20 intervals
    /// the chance of a false alarm on a healthy chip is `e⁻²⁰`.
    pub silence_factor: u32,
}

impl Config {
    /// Defaults for a profile: stock clock, default difficulty, the
    /// reference driver's ramp.
    pub fn for_profile(p: &ChipProfile) -> Self {
        Config {
            expected_chips: 0,
            frequency_hz: p.stock.0,
            difficulty: p.default_difficulty,
            count_window_us: u64::from(p.count_window_ms) * 1000,
            ramp_start_hz: 56_250_000,
            ramp_step_hz: 6_250_000,
            ramp_delay_us: 100_000,
            silence_factor: 20,
        }
    }
}

/// Work as the caller hands it in: a block header minus the nonce and
/// minus the job id, which the driver assigns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Work {
    /// First nonce to try.
    pub starting_nonce: u32,
    /// Compact target.
    pub nbits: u32,
    /// Block time.
    pub ntime: u32,
    /// Merkle root, header byte order.
    pub merkle_root: [u8; 32],
    /// Previous block hash, header byte order.
    pub prev_block_hash: [u8; 32],
    /// Block version before rolling.
    pub version: u32,
}

/// Handle for submitted work. `serial` is unique for the life of the
/// driver; `job_id` is what went on the wire and is reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkRef {
    /// Monotonic per submission.
    pub serial: u32,
    /// The chip-side id (multiple of the profile's step).
    pub job_id: u8,
}

/// A nonce attributed to live work and a known chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// The work it belongs to.
    pub work: WorkRef,
    /// The nonce, as it goes to the pool.
    pub nonce: u32,
    /// Header version that hashed: the work's version with the rolled
    /// bits ORed in.
    pub version: u32,
    /// The work's ntime, for convenience.
    pub ntime: u32,
    /// Chip index in chain order.
    pub chip: u8,
    /// Core within the chip.
    pub core: u8,
    /// Small core within the core.
    pub small_core: u8,
}

/// Why bring-up stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    /// No chip answered the id read.
    NoChips,
    /// The profile's init list has no `CountChips` step and no chip
    /// count was configured.
    NoCountStep,
}

/// Things the driver reports through `poll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A chip answered the id read with the expected id.
    ChipFound {
        /// Its index in order of answering.
        index: u8,
        /// The id it reported.
        chip_id: u16,
    },
    /// A reply to the id read carried a different chip id.
    UnexpectedChip {
        /// The id it reported.
        chip_id: u16,
    },
    /// Chip count settled; addressing follows.
    Enumerated {
        /// Chips on the chain.
        count: u8,
    },
    /// Fewer or more chips than configured. The chain continues with
    /// what it found.
    ChipCountMismatch {
        /// From [`Config::expected_chips`].
        expected: u8,
        /// What answered.
        found: u8,
    },
    /// Bring-up complete; work is accepted.
    Ready,
    /// The PLL is at its final word for the requested clock; `hz` is the
    /// clock the chips actually run at (solved, not the request).
    FrequencyApplied {
        /// Real clock, Hz.
        hz: u32,
    },
    /// No PLL word reaches this clock within the profile's ranges. At
    /// bring-up the configured clock is replaced by the profile's stock
    /// clock; at runtime the previous clock stays.
    FrequencyUnreachable {
        /// The requested clock, Hz.
        hz: u32,
    },
    /// A nonce for live work.
    Nonce(Found),
    /// A nonce for work that was retired. Counted, not submitted.
    Stale {
        /// The chip-side id.
        job_id: u8,
        /// Chip index.
        chip: u8,
    },
    /// The same nonce twice from one chip (the BM1370 erratum).
    Duplicate {
        /// Chip index.
        chip: u8,
    },
    /// A nonce stamped with an address no chip on this chain has.
    UnknownChip {
        /// The stamped address.
        address: u8,
    },
    /// A register read came back.
    Register {
        /// Chain address of the answering chip.
        address: u8,
        /// Register address.
        register: u8,
        /// Value.
        value: u32,
    },
    /// Bytes were dropped before a frame could be accepted.
    Desync {
        /// How many.
        dropped: u32,
    },
    /// A chip with work has produced nothing for the silence window.
    ChipSilent {
        /// Chip index.
        index: u8,
    },
    /// A silent chip produced a nonce again.
    ChipBack {
        /// Chip index.
        index: u8,
    },
    /// Bring-up failed; the chain is dead until `start` is called again.
    InitFailed(InitError),
}

/// Driver errors.
#[derive(Debug)]
pub enum Error<E> {
    /// The transport failed.
    Uart(E),
    /// Work was submitted before the chain was ready (or after it failed).
    NotReady,
    /// A frame could not be encoded — a bug, not a field condition.
    Frame(FrameError),
}

impl<E: fmt::Debug> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Uart(e) => write!(f, "uart: {e:?}"),
            Error::NotReady => f.write_str("chain not ready"),
            Error::Frame(e) => write!(f, "frame: {e:?}"),
        }
    }
}

/// Where the chain is, coarsely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `start` not called yet.
    Idle,
    /// Walking the init list.
    Init,
    /// Collecting chip-id replies.
    Counting,
    /// Stepping the PLL towards the target.
    Ramping,
    /// Accepting work.
    Ready,
    /// Bring-up failed.
    Failed,
}

/// One chip's health, as the driver sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChipStatus {
    /// Chain address.
    pub address: u8,
    /// Nonces attributed to live work.
    pub nonces: u64,
    /// Duplicates dropped.
    pub duplicates: u32,
    /// Nonces for retired work.
    pub stale: u32,
    /// When the last live nonce arrived.
    pub last_nonce_us: Option<u64>,
    /// Currently flagged silent.
    pub silent: bool,
}

/// Chain-wide counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Jobs sent.
    pub jobs: u64,
    /// Nonces yielded.
    pub nonces: u64,
    /// Nonces for retired work.
    pub stale_local: u64,
    /// Duplicates dropped.
    pub duplicates: u64,
    /// Nonces with an address that is not on the chain.
    pub unknown_chip: u64,
    /// Accepted frames that were preceded by dropped bytes.
    pub desyncs: u64,
    /// Times a chip was flagged silent.
    pub silences: u32,
    /// The RX decoder's own counters.
    pub decoder: decoder::Stats,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    serial: u32,
    version: u32,
    ntime: u32,
    live: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Steps {
        wait_until: u64,
    },
    Counting {
        until: u64,
    },
    Ramp {
        current_hz: u32,
        next_at: u64,
    },
    /// The chips were told the fast rate; the host follows at `until`.
    BaudSettle {
        until: u64,
    },
    Ready,
    Failed,
}

/// The driver. One per UART.
#[derive(Debug, Clone)]
pub struct Chain {
    profile: &'static ChipProfile,
    config: Config,
    mode: Mode,
    render: Option<Render<'static>>,
    init_done: bool,
    chip_count: u8,
    found: u8,
    chips: [ChipStatus; MAX_CHIPS],
    monitor: HashrateMonitor<MAX_CHIPS>,
    recent: [[u64; RECENT]; MAX_CHIPS],
    recent_at: [u8; MAX_CHIPS],
    slots: [Slot; SLOTS],
    ids: JobIds,
    serial: u32,
    decoder: RxDecoder,
    stats: Stats,
    frequency_hz: u32,
    target_hz: u32,
    /// The clock the last finished ramp was asked for; 0 before the
    /// first. A request that arrives while the driver waits for the
    /// baud switch moves `target_hz` away from it.
    ramped_to_hz: u32,
    difficulty: u32,
    first_job_us: Option<u64>,
    last_job_us: Option<u64>,
    /// The counter round in progress: next (chip, register index) to
    /// read, one per [`Chain::poll`]. See [`Chain::request_counters`].
    counter_cursor: Option<(u8, u8)>,
}

impl Chain {
    /// A driver for `profile`, idle until [`Chain::start`].
    pub fn new(profile: &'static ChipProfile, config: Config) -> Self {
        Chain {
            profile,
            config,
            mode: Mode::Idle,
            render: None,
            init_done: false,
            chip_count: 0,
            found: 0,
            chips: [ChipStatus::default(); MAX_CHIPS],
            monitor: HashrateMonitor::new(
                profile.domains,
                profile.domain_hashrate_scale,
                profile.rate_register,
            ),
            recent: [[NO_KEY; RECENT]; MAX_CHIPS],
            recent_at: [0; MAX_CHIPS],
            slots: [Slot::default(); SLOTS],
            ids: JobIds::with_step(profile.job_id_step),
            serial: 0,
            decoder: RxDecoder::new(),
            stats: Stats::default(),
            frequency_hz: 0,
            target_hz: config.frequency_hz,
            ramped_to_hz: 0,
            difficulty: config.difficulty,
            first_job_us: None,
            last_job_us: None,
            counter_cursor: None,
        }
    }

    /// Begin bring-up. Call after pulsing the chips' reset line; the
    /// host UART must be at the profile's initial rate.
    pub fn start(&mut self, now_us: u64) {
        self.mode = Mode::Steps { wait_until: now_us };
        self.render = Some(render(self.profile, 0));
        self.init_done = false;
        self.chip_count = 0;
        self.found = 0;
        self.chips = [ChipStatus::default(); MAX_CHIPS];
        self.monitor.reset();
        self.monitor.set_chip_count(0);
        self.recent = [[NO_KEY; RECENT]; MAX_CHIPS];
        self.recent_at = [0; MAX_CHIPS];
        self.slots = [Slot::default(); SLOTS];
        self.decoder.reset();
        self.decoder.set_frame_len(self.profile.response_len);
        self.frequency_hz = 0;
        self.target_hz = self.config.frequency_hz;
        self.ramped_to_hz = 0;
        self.difficulty = self.config.difficulty;
        self.first_job_us = None;
        self.last_job_us = None;
    }

    /// Coarse state.
    pub fn phase(&self) -> Phase {
        match self.mode {
            Mode::Idle => Phase::Idle,
            Mode::Steps { .. } => Phase::Init,
            Mode::Counting { .. } => Phase::Counting,
            Mode::Ramp { .. } | Mode::BaudSettle { .. } if !self.init_done => Phase::Ramping,
            Mode::BaudSettle { .. } => Phase::Ready,
            Mode::Ramp { .. } | Mode::Ready => Phase::Ready,
            Mode::Failed => Phase::Failed,
        }
    }

    /// Work is accepted: bring-up is complete, whatever the PLL is doing.
    pub fn accepting_work(&self) -> bool {
        self.init_done && matches!(self.mode, Mode::Ready | Mode::Ramp { .. })
    }

    /// Chips found.
    pub fn chip_count(&self) -> u8 {
        self.chip_count
    }

    /// Per-chip status, chain order.
    pub fn chips(&self) -> &[ChipStatus] {
        &self.chips[..usize::from(self.chip_count)]
    }

    /// Counters.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// The clock the chips run at now (solved), 0 before the first PLL write.
    pub fn frequency_hz(&self) -> u32 {
        self.frequency_hz
    }

    /// The chain is up and no ramp is under way: the clock asked for
    /// last is the one the chips run at.
    pub fn clock_settled(&self) -> bool {
        self.init_done && self.mode == Mode::Ready
    }

    /// Current ticket-mask difficulty.
    pub fn difficulty(&self) -> u32 {
        self.difficulty
    }

    /// How long one job keeps a chip busy before its nonce space is
    /// scanned through, µs. Submit work at least this often.
    pub fn work_timeout_us(&self) -> u64 {
        let ms = regs::job_timeout_ms(
            self.frequency_hz,
            u16::from(self.chip_count),
            self.profile.small_cores,
            self.profile.cores,
            self.profile.versions_per_job(),
            1000,
            u32::MAX,
        );
        u64::from(ms).saturating_mul(1000)
    }

    /// The chips are, or are about to be, out of work.
    pub fn needs_work(&self, now_us: u64) -> bool {
        if !self.accepting_work() {
            return false;
        }
        match self.last_job_us {
            None => true,
            Some(t) => now_us.saturating_sub(t) >= self.work_timeout_us() * 9 / 10,
        }
    }

    /// Ask for a clock. Returns the clock the chips will actually run at
    /// (solved on the PLL grid), or `None` if no PLL word reaches `hz`,
    /// in which case nothing changes. Applied by `poll` as a ramp; the
    /// end is reported as [`Event::FrequencyApplied`].
    pub fn set_frequency(&mut self, hz: u32) -> Option<u32> {
        let word = pll::solve(&self.profile.pll, hz)?;
        self.target_hz = hz;
        if self.mode == Mode::Ready {
            self.mode = Mode::Ramp {
                current_hz: self.frequency_hz,
                next_at: 0,
            };
        }
        Some(
            word.frequency_hz(self.profile.pll.xtal_hz)
                .min(u64::from(u32::MAX)) as u32,
        )
    }

    /// Write a new ticket mask to every chip.
    pub fn set_difficulty<U: Uart>(
        &mut self,
        uart: &mut U,
        difficulty: u32,
    ) -> Result<(), Error<U::Error>> {
        self.difficulty = difficulty;
        self.write_all(
            uart,
            reg::TICKET_MASK,
            regs::ticket_mask(f64::from(difficulty)),
        )
    }

    /// Ask every chip for its hash counters (total, domains, errors);
    /// the replies feed [`Chain::monitor`] and also arrive as
    /// [`Event::Register`]. Once a second, like the reference; a chip
    /// that does not answer only keeps its old reading.
    ///
    /// The reads go out one per [`Chain::poll`], not as one burst: every
    /// reply is 11 bytes and the chips answer back to back, so a burst
    /// of chips × 6 reads (an 8-chip Octaxe: 528 bytes) overruns the
    /// host's receive buffer and the tail of the burst is lost in
    /// pieces. Seen 2026-10-01 on a NerdAxe Gaia (one BM1373): 17 of 32
    /// burst reads answered, dropped bytes and CRC failures rising with
    /// each burst, every single read answered; AxeOS reads the same way
    /// and sees only two of the BM1373's domain counters.
    pub fn request_counters<U: Uart>(&mut self, _uart: &mut U) -> Result<(), Error<U::Error>> {
        if !self.accepting_work() || self.chip_count == 0 {
            return Ok(());
        }
        if self.counter_cursor.is_none() {
            self.counter_cursor = Some((0, 0));
        }
        Ok(())
    }

    /// One read of the counter round, if one is in progress.
    fn step_counters<U: Uart>(&mut self, uart: &mut U) -> Result<(), Error<U::Error>> {
        let Some((chip, i)) = self.counter_cursor else {
            return Ok(());
        };
        if !self.accepting_work() || chip >= self.chip_count {
            self.counter_cursor = None;
            return Ok(());
        }
        let regs = self.monitor.registers();
        if regs.is_empty() {
            // No counter or rate registers known on this chip.
            self.counter_cursor = None;
            return Ok(());
        }
        let addr = self
            .profile
            .command_address(self.chips[usize::from(chip)].address);
        self.send_cmd(
            uart,
            Group::Single,
            Op::Read,
            &read_payload(addr, regs[usize::from(i)]),
        )?;
        let next_i = i + 1;
        self.counter_cursor = if usize::from(next_i) < regs.len() {
            Some((chip, next_i))
        } else if chip + 1 < self.chip_count {
            Some((chip + 1, 0))
        } else {
            None
        };
        Ok(())
    }

    /// A counter round is still being sent.
    pub fn counters_pending(&self) -> bool {
        self.counter_cursor.is_some()
    }

    /// The chips' hash counters, as last read.
    pub fn monitor(&self) -> &HashrateMonitor<MAX_CHIPS> {
        &self.monitor
    }

    /// Read one register of one chip (chain index); the reply arrives as
    /// [`Event::Register`] with the chip's address ([`Chain::chip_index`]
    /// maps it back). Diagnostics: the API's register scan. Nothing is
    /// sent for an index the chain does not have.
    pub fn read_chip_register<U: Uart>(
        &mut self,
        uart: &mut U,
        chip: usize,
        register: u8,
    ) -> Result<(), Error<U::Error>> {
        if chip >= usize::from(self.chip_count) {
            return Ok(());
        }
        let addr = self.profile.command_address(self.chips[chip].address);
        self.send_cmd(uart, Group::Single, Op::Read, &read_payload(addr, register))
    }

    /// Chain index of the chip that answers a register read with
    /// `address`, if the chain has it.
    pub fn chip_index(&self, address: u8) -> Option<usize> {
        self.profile.chip_index(self.chip_count, address)
    }

    /// Read a register on every chip; replies arrive as [`Event::Register`].
    pub fn read_register<U: Uart>(
        &mut self,
        uart: &mut U,
        register: u8,
    ) -> Result<(), Error<U::Error>> {
        self.send_cmd(uart, Group::All, Op::Read, &read_payload(0, register))
    }

    /// Send work to the chain.
    pub fn submit<U: Uart>(
        &mut self,
        uart: &mut U,
        work: &Work,
        now_us: u64,
    ) -> Result<WorkRef, Error<U::Error>> {
        if !self.accepting_work() {
            return Err(Error::NotReady);
        }
        let job_id = self.ids.advance();
        self.serial = self.serial.wrapping_add(1);
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = if self.profile.host_midstates > 0 {
            // The BM1397 wants the header's first block hashed here, in
            // header order: the work carries both hashes word-reversed,
            // as the BM1370 takes them.
            let job = MidstateJob::build(
                job_id,
                self.profile.host_midstates,
                work.starting_nonce,
                work.nbits,
                work.ntime,
                &job::reverse_words(&work.prev_block_hash),
                &job::reverse_words(&work.merkle_root),
                work.version,
                regs::BIP320_VERSION_MASK,
            );
            let mut payload = [0u8; MIDSTATE_JOB_LEN_4];
            let len = job.write(&mut payload);
            encode_job(&mut buf, &payload[..len]).map_err(Error::Frame)?
        } else {
            let job = Job {
                job_id,
                starting_nonce: work.starting_nonce,
                nbits: work.nbits,
                ntime: work.ntime,
                merkle_root: work.merkle_root,
                prev_block_hash: work.prev_block_hash,
                version: work.version,
            };
            encode_job(&mut buf, &job.to_bytes()).map_err(Error::Frame)?
        };
        uart.write(&buf[..n]).map_err(Error::Uart)?;
        self.slots[self.profile.job_slot(job_id)] = Slot {
            serial: self.serial,
            version: work.version,
            ntime: work.ntime,
            live: true,
        };
        self.stats.jobs += 1;
        self.last_job_us = Some(now_us);
        if self.first_job_us.is_none() {
            self.first_job_us = Some(now_us);
        }
        Ok(WorkRef {
            serial: self.serial,
            job_id,
        })
    }

    /// Retire every job: a new block arrived, nothing in flight can pay.
    /// Nonces that still come in are counted as stale, not yielded.
    pub fn retire_all(&mut self) {
        for s in &mut self.slots {
            s.live = false;
        }
    }

    /// Retire one job, if it is still the one in its slot.
    pub fn retire(&mut self, work: WorkRef) {
        let s = &mut self.slots[self.profile.job_slot(work.job_id)];
        if s.serial == work.serial {
            s.live = false;
        }
    }

    /// Do what is due: read the wire, advance bring-up, step the ramp,
    /// run the watchdog. Call often; it never blocks.
    pub fn poll<U: Uart>(
        &mut self,
        uart: &mut U,
        now_us: u64,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(), Error<U::Error>> {
        self.drain_rx(uart, now_us, on_event)?;
        self.step_counters(uart)?;
        loop {
            match self.mode {
                Mode::Idle | Mode::Failed => break,
                Mode::Ready => {
                    self.watchdog(now_us, on_event);
                    break;
                }
                Mode::Steps { wait_until } => {
                    if now_us < wait_until {
                        break;
                    }
                    let next = self.render.as_mut().and_then(Iterator::next);
                    match next {
                        Some(Rendered::Cmd {
                            group,
                            op,
                            payload,
                            len,
                        }) => {
                            self.send_cmd(uart, group, op, &payload[..usize::from(len)])?;
                        }
                        Some(Rendered::Delay(ms)) => {
                            self.mode = Mode::Steps {
                                wait_until: now_us + u64::from(ms) * 1000,
                            };
                        }
                        Some(Rendered::CountChips) => {
                            self.found = 0;
                            self.decoder.set_frame_len(self.profile.chip_id_reply_len);
                            // A chip may answer with the full 11 bytes where
                            // the profile expects 9 (BM1373 on the Gaia); not
                            // on a chip whose every reply is 9 (BM1397).
                            let alt = (self.profile.chip_id_reply_len < self.profile.response_len)
                                .then_some(self.profile.response_len);
                            self.decoder.set_alt_frame_len(alt);
                            self.mode = Mode::Counting {
                                until: now_us + self.config.count_window_us,
                            };
                        }
                        None => {
                            if self.chip_count == 0 {
                                self.fail(InitError::NoCountStep, on_event);
                            } else {
                                self.finish_steps(uart, now_us, on_event)?;
                            }
                        }
                    }
                }
                Mode::Counting { until } => {
                    if now_us < until {
                        break;
                    }
                    self.finish_counting(now_us, on_event);
                }
                Mode::BaudSettle { until } => {
                    if now_us < until {
                        break;
                    }
                    uart.set_baud(self.profile.fast_uart.1)
                        .map_err(Error::Uart)?;
                    // A clock asked for during the settle time: the
                    // bring-up ramp is over, so it gets one of its own
                    // (Ready alone would leave the chips at the old one).
                    self.mode = if self.target_hz != self.ramped_to_hz {
                        Mode::Ramp {
                            current_hz: self.ramped_to_hz,
                            next_at: now_us,
                        }
                    } else {
                        Mode::Ready
                    };
                    self.init_done = true;
                    on_event(Event::Ready);
                }
                Mode::Ramp {
                    current_hz,
                    next_at,
                } => {
                    if now_us < next_at {
                        break;
                    }
                    if current_hz == self.target_hz {
                        self.finish_ramp(uart, now_us, on_event)?;
                    } else {
                        let step = self.config.ramp_step_hz.max(1);
                        let next = if self.target_hz > current_hz {
                            current_hz.saturating_add(step).min(self.target_hz)
                        } else {
                            current_hz.saturating_sub(step).max(self.target_hz)
                        };
                        self.write_pll(uart, next, on_event)?;
                        self.mode = Mode::Ramp {
                            current_hz: next,
                            next_at: now_us + self.config.ramp_delay_us,
                        };
                    }
                }
            }
        }
        Ok(())
    }

    fn fail(&mut self, why: InitError, on_event: &mut dyn FnMut(Event)) {
        self.mode = Mode::Failed;
        self.render = None;
        on_event(Event::InitFailed(why));
    }

    fn finish_counting(&mut self, now_us: u64, on_event: &mut dyn FnMut(Event)) {
        self.decoder.set_alt_frame_len(None);
        self.decoder.set_frame_len(self.profile.response_len);
        let found = self.found;
        if found == 0 {
            self.fail(InitError::NoChips, on_event);
            return;
        }
        let expected = self.config.expected_chips;
        if expected != 0 && found != expected {
            on_event(Event::ChipCountMismatch { expected, found });
        }
        self.chip_count = found;
        for (i, c) in self.chips.iter_mut().enumerate().take(usize::from(found)) {
            c.address = self.profile.chip_address(found, i as u8);
        }
        self.monitor.set_chip_count(found);
        on_event(Event::Enumerated { count: found });
        // The list for the real count, resumed after the count marker:
        // everything before it has already gone out once.
        let mut r = render(self.profile, found);
        for step in r.by_ref() {
            if step == Rendered::CountChips {
                break;
            }
        }
        self.render = Some(r);
        self.mode = Mode::Steps { wait_until: now_us };
    }

    /// After the profile's lists: baud switch, ticket mask if it differs
    /// from the profile default, then the ramp.
    fn finish_steps<U: Uart>(
        &mut self,
        uart: &mut U,
        now_us: u64,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(), Error<U::Error>> {
        self.render = None;
        if self.profile.baud_switch_after_ramp_ms.is_none() {
            let (word, baud) = self.profile.fast_uart;
            self.write_all(uart, self.profile.fast_uart_reg, word)?;
            uart.set_baud(baud).map_err(Error::Uart)?;
        }
        if self.difficulty != self.profile.default_difficulty {
            let d = self.difficulty;
            self.write_all(uart, reg::TICKET_MASK, regs::ticket_mask(f64::from(d)))?;
        }
        if pll::solve(&self.profile.pll, self.target_hz).is_none() {
            // A configured clock off the grid: say so, run stock.
            on_event(Event::FrequencyUnreachable { hz: self.target_hz });
            self.target_hz = self.profile.stock.0;
        }
        let start = self.config.ramp_start_hz.min(self.target_hz);
        self.write_pll(uart, start, on_event)?;
        self.mode = Mode::Ramp {
            current_hz: start,
            next_at: now_us + self.config.ramp_delay_us,
        };
        Ok(())
    }

    fn finish_ramp<U: Uart>(
        &mut self,
        uart: &mut U,
        now_us: u64,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(), Error<U::Error>> {
        self.ramped_to_hz = self.target_hz;
        let hcn = match self.profile.hcn {
            Hcn::Computed { errata } => Some(regs::hash_counting_number(
                self.frequency_hz,
                self.profile.cores,
                u16::from(self.chip_count),
                errata,
            )),
            Hcn::Fixed(v) => Some(v),
            Hcn::None => None,
        };
        if let Some(hcn) = hcn {
            self.write_all(uart, reg::HASH_COUNTING_NUMBER, hcn.to_be_bytes())?;
        }
        match self.profile.baud_switch_after_ramp_ms {
            // NerdOS's tail for the BM1373, first init only: version
            // rolling on, the chips told the fast rate, the host follows
            // after the settle time. The BM1397 (host midstates) gets no
            // rolling mask: WantClue `nerdnos_init` sets the baud right
            // after `BM1397_init` and waits 100 ms.
            Some(settle_ms) if !self.init_done => {
                if self.profile.host_midstates == 0 {
                    self.write_all(
                        uart,
                        reg::VERSION_ROLLING,
                        regs::version_mask(regs::BIP320_VERSION_MASK),
                    )?;
                }
                self.write_all(uart, self.profile.fast_uart_reg, self.profile.fast_uart.0)?;
                self.mode = Mode::BaudSettle {
                    until: now_us + u64::from(settle_ms) * 1000,
                };
            }
            _ => {
                self.mode = Mode::Ready;
                if !self.init_done {
                    self.init_done = true;
                    on_event(Event::Ready);
                }
            }
        }
        on_event(Event::FrequencyApplied {
            hz: self.frequency_hz,
        });
        Ok(())
    }

    fn write_pll<U: Uart>(
        &mut self,
        uart: &mut U,
        hz: u32,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(), Error<U::Error>> {
        match pll::solve(&self.profile.pll, hz) {
            Some(word) => {
                let bytes = word.register_bytes(&self.profile.pll);
                // The BM1397's PLL0 divider first, twice, then the word
                // twice (cgminer `calc_gsf_freq`, WantClue and ESP-Miner
                // `BM1397_send_hash_frequency`, 10 ms apart there; a
                // frame takes about a millisecond at 115200).
                if let Some((pre_reg, pre_val)) = self.profile.pll_prewrite {
                    self.write_all(uart, pre_reg, pre_val)?;
                    self.write_all(uart, pre_reg, pre_val)?;
                    self.write_all(uart, reg::PLL0_PARAMETER, bytes)?;
                }
                self.write_all(uart, reg::PLL0_PARAMETER, bytes)?;
                self.frequency_hz = word
                    .frequency_hz(self.profile.pll.xtal_hz)
                    .min(u64::from(u32::MAX)) as u32;
            }
            None => on_event(Event::FrequencyUnreachable { hz }),
        }
        Ok(())
    }

    fn write_all<U: Uart>(
        &mut self,
        uart: &mut U,
        register: u8,
        value: [u8; 4],
    ) -> Result<(), Error<U::Error>> {
        self.send_cmd(
            uart,
            Group::All,
            Op::Write,
            &write_payload(0, register, value),
        )
    }

    fn send_cmd<U: Uart>(
        &mut self,
        uart: &mut U,
        group: Group,
        op: Op,
        payload: &[u8],
    ) -> Result<(), Error<U::Error>> {
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = encode_cmd(&mut buf, group, op, payload).map_err(Error::Frame)?;
        uart.write(&buf[..n]).map_err(Error::Uart)
    }

    fn drain_rx<U: Uart>(
        &mut self,
        uart: &mut U,
        now_us: u64,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(), Error<U::Error>> {
        let mut buf = [0u8; 64];
        loop {
            let n = uart.read(&mut buf).map_err(Error::Uart)?;
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if let Some(frame) = self.decoder.push(b) {
                    self.on_frame(frame, now_us, on_event);
                }
            }
        }
        self.stats.decoder = self.decoder.stats();
        Ok(())
    }

    fn on_frame(&mut self, f: decoder::Frame, now_us: u64, on_event: &mut dyn FnMut(Event)) {
        if f.dropped_before > 0 {
            self.stats.desyncs += 1;
            on_event(Event::Desync {
                dropped: f.dropped_before,
            });
        }
        let p = &f.response.payload;
        if f.response.is_job() {
            let r = if self.profile.host_midstates > 0 {
                JobResult::parse_bm1397(p)
            } else {
                JobResult::parse(p)
            };
            self.on_nonce(r, now_us, on_event);
        } else if matches!(self.mode, Mode::Counting { .. }) {
            // Chip-id reply: `[id_hi, id_lo, core_num, addr, 0, 0, 0, 0]`.
            let chip_id = u16::from_be_bytes([p[0], p[1]]);
            if self.profile.is_chip_id(chip_id) {
                if usize::from(self.found) < MAX_CHIPS {
                    on_event(Event::ChipFound {
                        index: self.found,
                        chip_id,
                    });
                    self.found += 1;
                }
            } else {
                on_event(Event::UnexpectedChip { chip_id });
            }
        } else {
            let r = RegisterResult::parse(p);
            if let Some(i) = self.profile.chip_index(self.chip_count, r.asic_address) {
                self.monitor.on_register(i, r.register, r.value, now_us);
            }
            on_event(Event::Register {
                address: r.asic_address,
                register: r.register,
                value: r.value,
            });
        }
    }

    fn on_nonce(&mut self, r: JobResult, now_us: u64, on_event: &mut dyn FnMut(Event)) {
        let Some(chip) = self
            .profile
            .chip_index_from_nonce(self.chip_count, r.asic_address)
        else {
            self.stats.unknown_chip += 1;
            on_event(Event::UnknownChip {
                address: r.asic_address,
            });
            return;
        };
        let slot = self.slots[self.profile.job_slot(r.job_id)];
        if !slot.live {
            self.stats.stale_local += 1;
            self.chips[chip].stale += 1;
            on_event(Event::Stale {
                job_id: r.job_id,
                chip: chip as u8,
            });
            return;
        }
        // The version that hashed: the chip's rolled bits, or (BM1397)
        // the job's version rolled once per midstate number.
        let version = if self.profile.host_midstates > 0 {
            job::rolled_version(slot.version, regs::BIP320_VERSION_MASK, r.midstate_num)
        } else {
            r.rolled_version(slot.version)
        };
        let key = (u64::from(r.job_id) << 48)
            | (u64::from((r.version_bits >> 13) | u32::from(r.midstate_num)) << 32)
            | u64::from(r.nonce);
        if self.recent[chip].contains(&key) {
            self.stats.duplicates += 1;
            self.chips[chip].duplicates += 1;
            on_event(Event::Duplicate { chip: chip as u8 });
            return;
        }
        let at = usize::from(self.recent_at[chip]);
        self.recent[chip][at] = key;
        self.recent_at[chip] = ((at + 1) % RECENT) as u8;

        let c = &mut self.chips[chip];
        c.nonces += 1;
        c.last_nonce_us = Some(now_us);
        self.stats.nonces += 1;
        if c.silent {
            c.silent = false;
            on_event(Event::ChipBack { index: chip as u8 });
        }
        on_event(Event::Nonce(Found {
            work: WorkRef {
                serial: slot.serial,
                job_id: r.job_id,
            },
            nonce: r.nonce,
            version,
            ntime: slot.ntime,
            chip: chip as u8,
            core: r.core_id,
            small_core: r.small_core_id,
        }));
    }

    /// Expected time between nonces on one chip, µs.
    fn nonce_interval_us(&self) -> u64 {
        let f = u128::from(self.frequency_hz) * u128::from(self.profile.small_cores);
        if f == 0 {
            return u64::MAX;
        }
        let space = (u128::from(self.difficulty) << 32) * 1_000_000;
        (space / f).min(u128::from(u64::MAX)) as u64
    }

    fn watchdog(&mut self, now_us: u64, on_event: &mut dyn FnMut(Event)) {
        let (Some(first), Some(last)) = (self.first_job_us, self.last_job_us) else {
            return;
        };
        // Only while the chips have something to do.
        if now_us.saturating_sub(last) >= self.work_timeout_us() {
            return;
        }
        let window = self
            .nonce_interval_us()
            .saturating_mul(u64::from(self.config.silence_factor));
        for i in 0..usize::from(self.chip_count) {
            let c = &mut self.chips[i];
            let since = c.last_nonce_us.unwrap_or(first);
            if !c.silent && now_us.saturating_sub(since) > window {
                c.silent = true;
                self.stats.silences += 1;
                on_event(Event::ChipSilent { index: i as u8 });
            }
        }
    }
}
