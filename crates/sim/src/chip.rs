//! One simulated BM13xx.
//!
//! A [`SimChip`] has a register file, a chain address, a current job and
//! a clock it does not really have — it never hashes. When a job is
//! active and the PLL register says the chip runs at `f`, nonces are
//! *scheduled* as a Poisson process at the rate a real chip would find
//! them, `f · small_cores / (ticket_difficulty · 2³²)`, and each one is
//! constructed to look like the chip's: its address and a core id stamped
//! into the nonce's high bits, the job id in the id byte, version bits
//! inside the rolling mask. To the firmware that is indistinguishable
//! from a nonce that hashed — until it verifies the hash, which is not
//! the simulator's concern.
//!
//! Everything observable is a function of (profile, seed, bytes fed,
//! time advanced, faults injected). Time is the caller's: nothing here
//! reads a clock.

use warpaxe_core::chip::ChipProfile;
use warpaxe_core::frame::{encode_response, Group, Header, Op, FLAG_JOB, RESPONSE_LEN};
use warpaxe_core::job::{Job, JOB_LEN};
use warpaxe_core::pll::PllWord;
use warpaxe_core::regs::{self, reg};
use warpaxe_core::result::{job_result_payload, register_result_payload};

use crate::fault::Fault;
use crate::rng::Pcg32;

/// Where the chip is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipState {
    /// Power-on: address 0, forwards nothing, answers `GROUP_ALL`.
    Reset,
    /// `CMD_INACTIVE` seen: the next `CMD_SETADDRESS` is for this chip
    /// (if no chip before it in the chain is still waiting).
    Inactive,
    /// Addressed and configurable.
    Addressed,
}

/// Counters a test can read to see what the chip experienced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChipStats {
    /// Command frames that applied to this chip.
    pub commands: u32,
    /// Register writes.
    pub writes: u32,
    /// Register reads answered.
    pub reads: u32,
    /// Jobs accepted.
    pub jobs: u32,
    /// Nonces sent (including garbage and duplicates).
    pub nonces: u32,
    /// Sequence violations: `SETADDRESS` without `INACTIVE`, a job before
    /// addressing, a write to a chip that is not addressed yet.
    pub violations: u32,
    /// Responses suppressed by a `NoResponse` fault.
    pub suppressed: u32,
}

/// One chip on the chain.
#[derive(Debug, Clone)]
pub struct SimChip {
    profile: &'static ChipProfile,
    regs: [u32; 256],
    address: u8,
    state: ChipState,
    now_us: u64,
    job: Option<Job>,
    job_ends_us: u64,
    next_nonce_us: u64,
    baud: u32,
    rng: Pcg32,
    faults: Vec<Fault>,
    silent_until_us: u64,
    last_nonce: Option<[u8; 8]>,
    stats: ChipStats,
}

impl SimChip {
    /// A chip fresh from reset. `index` seeds a distinct random stream
    /// per chip so two chips on one chain do not move in lockstep.
    pub fn new(profile: &'static ChipProfile, seed: u64, index: u8) -> Self {
        SimChip {
            profile,
            regs: [0; 256],
            address: 0,
            state: ChipState::Reset,
            now_us: 0,
            job: None,
            job_ends_us: 0,
            next_nonce_us: u64::MAX,
            baud: 115_200,
            rng: Pcg32::new(seed, u64::from(index) + 1),
            faults: Vec::new(),
            silent_until_us: 0,
            last_nonce: None,
            stats: ChipStats::default(),
        }
    }

    /// The profile this chip simulates.
    pub fn profile(&self) -> &'static ChipProfile {
        self.profile
    }

    /// Chain address (0 until addressed).
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Life-cycle state.
    pub fn state(&self) -> ChipState {
        self.state
    }

    /// Counters.
    pub fn stats(&self) -> ChipStats {
        self.stats
    }

    /// A register's current value.
    pub fn register(&self, reg: u8) -> u32 {
        self.regs[usize::from(reg)]
    }

    /// Hash-clock frequency implied by the PLL register, Hz. Zero until
    /// the PLL has been written — a chip that was never clocked does not
    /// hash.
    pub fn frequency_hz(&self) -> u64 {
        let v = self.regs[usize::from(reg::PLL0_PARAMETER)].to_be_bytes();
        PllWord::from_register_bytes(v).map_or(0, |w| w.frequency_hz(self.profile.pll.xtal_hz))
    }

    /// Difficulty implied by the ticket-mask register.
    pub fn ticket_difficulty(&self) -> u32 {
        regs::ticket_mask_to_difficulty(self.regs[usize::from(reg::TICKET_MASK)].to_be_bytes())
    }

    /// UART rate the chip believes it is at.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// The job being worked on.
    pub fn job(&self) -> Option<&Job> {
        self.job.as_ref()
    }

    /// Inject a fault. Faults accumulate; see [`Fault`].
    pub fn inject(&mut self, fault: Fault) {
        if let Fault::NoResponse { for_us } = fault {
            self.silent_until_us = self.now_us.saturating_add(for_us);
        }
        self.faults.push(fault);
    }

    /// Remove every fault.
    pub fn clear_faults(&mut self) {
        self.faults.clear();
        self.silent_until_us = 0;
    }

    fn has<F: Fn(&Fault) -> bool>(&self, f: F) -> bool {
        self.faults.iter().any(f)
    }

    fn silenced(&self) -> bool {
        self.now_us < self.silent_until_us
    }

    fn hung(&self) -> bool {
        let f = self.frequency_hz();
        self.faults
            .iter()
            .any(|x| matches!(x, Fault::HangAbove { hz } if f > u64::from(*hz)))
    }

    /// Expected nonces per second at the current clock and difficulty.
    pub fn nonce_rate_per_s(&self) -> f64 {
        let f = self.frequency_hz() as f64;
        let diff = f64::from(self.ticket_difficulty());
        f * f64::from(self.profile.small_cores) / (diff * 4_294_967_296.0)
    }

    fn schedule_next_nonce(&mut self) {
        let rate = self.nonce_rate_per_s();
        self.next_nonce_us = if self.job.is_some() && rate > 0.0 && !self.hung() {
            // At least 1 µs apart, so a zero draw cannot stall the clock.
            self.now_us
                .saturating_add(self.rng.exp_interval_us(rate).max(1))
        } else {
            u64::MAX
        };
    }

    /// Does a command with this header and first payload byte apply to
    /// this chip? Chain semantics: a job goes to every chip (the header
    /// says `GROUP_SINGLE`, but its first byte is the job id, and the
    /// chips split the nonce space by their stamped address); `GROUP_ALL`
    /// to everyone; `GROUP_SINGLE` to the chip whose address matches;
    /// `SETADDRESS` is routed by the chain, not here.
    pub(crate) fn accepts(&self, header: Header, chip_addr: u8) -> bool {
        if header.is_job() {
            return true;
        }
        match header.group() {
            Group::All => true,
            Group::Single => header.op() == Op::SetAddress || chip_addr == self.address,
        }
    }

    /// Handle one host frame addressed to this chip. Returns response
    /// bytes to send, if any.
    pub(crate) fn handle(&mut self, header: Header, payload: &[u8], out: &mut Vec<u8>) {
        self.stats.commands += 1;
        if header.is_job() {
            self.handle_job(payload);
            return;
        }
        match header.op() {
            Op::Inactive => {
                self.state = ChipState::Inactive;
            }
            Op::SetAddress => {
                if self.state != ChipState::Inactive {
                    self.stats.violations += 1;
                }
                self.address = payload.first().copied().unwrap_or(0);
                self.state = ChipState::Addressed;
            }
            Op::Write => {
                if payload.len() < 6 {
                    self.stats.violations += 1;
                    return;
                }
                if header.group() == Group::Single && self.state != ChipState::Addressed {
                    self.stats.violations += 1;
                }
                let r = payload[1];
                let v = u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]);
                self.write_register(r, v);
            }
            Op::Read => {
                if payload.len() < 2 {
                    self.stats.violations += 1;
                    return;
                }
                let r = payload[1];
                self.read_register(r, out);
            }
        }
    }

    fn write_register(&mut self, r: u8, v: u32) {
        self.stats.writes += 1;
        if r == reg::FAST_UART_CONFIGURATION && self.has(|f| matches!(f, Fault::BaudSwitchIgnored))
        {
            return;
        }
        self.regs[usize::from(r)] = v;
        if r == reg::FAST_UART_CONFIGURATION && v.to_be_bytes() == self.profile.fast_uart.0 {
            self.baud = self.profile.fast_uart.1;
        }
        if r == reg::PLL0_PARAMETER || r == reg::TICKET_MASK {
            self.schedule_next_nonce();
        }
    }

    fn read_register(&mut self, r: u8, out: &mut Vec<u8>) {
        self.stats.reads += 1;
        if self.silenced() {
            self.stats.suppressed += 1;
            return;
        }
        let payload = if r == reg::CHIP_ID {
            // `[id_hi, id_lo, core_num, addr, 0, 0, 0, 0]` — the reference
            // driver logs bytes 4 and 5 of the frame as CORE_NUM and ADDR.
            let id = self.profile.chip_id.to_be_bytes();
            [id[0], id[1], 0x00, self.address, 0, 0, 0, 0]
        } else {
            let mut v = self.regs[usize::from(r)];
            if self.has(|f| matches!(f, Fault::RegReadCorrupt { reg } if *reg == r)) {
                v ^= 0x0001_0100;
            }
            register_result_payload(v, self.address, r)
        };
        self.emit(&payload, 0x00, out);
    }

    fn handle_job(&mut self, payload: &[u8]) {
        if self.state != ChipState::Addressed {
            self.stats.violations += 1;
        }
        if payload.len() != JOB_LEN {
            self.stats.violations += 1;
            return;
        }
        let mut b = [0u8; JOB_LEN];
        b.copy_from_slice(payload);
        let job = Job::from_bytes(&b);
        self.stats.jobs += 1;
        // How long the chip works this job before it runs out of nonce
        // space (with version rolling): the reference driver's timeout.
        let versions = 1u32 << 16;
        let ms = regs::job_timeout_ms(
            self.frequency_hz().min(u64::from(u32::MAX)) as u32,
            1,
            self.profile.small_cores,
            self.profile.cores,
            versions,
            1000,
            u32::MAX,
        );
        self.job_ends_us = self.now_us.saturating_add(u64::from(ms) * 1000);
        self.job = Some(job);
        self.schedule_next_nonce();
    }

    /// Advance simulated time to `now_us`, emitting every nonce that
    /// falls due into `out`.
    pub(crate) fn advance_to(&mut self, now_us: u64, out: &mut Vec<u8>) {
        while self.next_nonce_us <= now_us {
            self.now_us = self.next_nonce_us;
            if self.now_us > self.job_ends_us {
                // Nonce space exhausted: the chip goes quiet until a new job.
                self.next_nonce_us = u64::MAX;
                break;
            }
            self.emit_nonce(out);
            self.schedule_next_nonce();
        }
        self.now_us = now_us;
    }

    fn emit_nonce(&mut self, out: &mut Vec<u8>) {
        let Some(job) = self.job.as_ref() else { return };
        let job_id = job.job_id;
        let mask16 = (self.regs[usize::from(reg::VERSION_ROLLING)] & 0xFFFF) as u16;
        let payload = if self.has(
            |f| matches!(f, Fault::GarbageNonces { per_mille } if self.rng_peek_chance(*per_mille)),
        ) {
            let mut p = [0u8; 8];
            for b in &mut p {
                *b = (self.rng.next_u32() & 0xFF) as u8;
            }
            self.rng.next_u32();
            p
        } else {
            let low17 = self.rng.next_u32() & 0x1_FFFF;
            let core = self.rng.below(u32::from(self.profile.cores)) as u8;
            let small = self.rng.below(16) as u8;
            let vbits = (self.rng.next_u32() as u16) & mask16;
            job_result_payload(job_id, low17, self.address, core, small, vbits)
        };
        self.stats.nonces += 1;
        if self.silenced() {
            self.stats.suppressed += 1;
            return;
        }
        self.emit(&payload, FLAG_JOB, out);
        if self.has(|f| matches!(f, Fault::DuplicateNonce { per_mille } if self.rng_peek_chance(*per_mille))) {
            self.rng.next_u32();
            self.stats.nonces += 1;
            self.emit(&payload, FLAG_JOB, out);
        }
        self.last_nonce = Some(payload);
    }

    /// Decide a per-mille chance without disturbing the main stream's
    /// determinism more than one draw. (A separate stream would be
    /// cleaner; one draw is enough for tests to stay reproducible.)
    fn rng_peek_chance(&self, per_mille: u16) -> bool {
        let mut r = self.rng.clone();
        r.chance_per_mille(per_mille)
    }

    fn emit(&mut self, payload: &[u8; 8], flags: u8, out: &mut Vec<u8>) {
        let mut frame = [0u8; RESPONSE_LEN];
        encode_response(&mut frame, payload, flags);
        if let Some(Fault::BitFlipTx { per_mille }) = self
            .faults
            .iter()
            .find(|f| matches!(f, Fault::BitFlipTx { .. }))
            .copied()
        {
            for b in &mut frame {
                if self.rng.chance_per_mille(per_mille) {
                    *b ^= 1 << self.rng.below(8);
                }
            }
        }
        out.extend_from_slice(&frame);
    }
}
