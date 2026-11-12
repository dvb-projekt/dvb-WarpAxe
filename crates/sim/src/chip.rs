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
use warpaxe_core::frame::{
    encode_response, encode_short_response, Group, Header, Op, FLAG_JOB, RESPONSE_LEN,
    SHORT_RESPONSE_LEN,
};
use warpaxe_core::job::{Job, MidstateJob, JOB_LEN};
use warpaxe_core::pll::PllWord;
use warpaxe_core::regs::{self, reg};
use warpaxe_core::result::{
    bm1397_job_result_payload, job_result_payload, register_result_payload,
};

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
    /// Midstates the current job came with (BM1397), 0 for a header job.
    job_midstates: u8,
    job_ends_us: u64,
    next_nonce_us: u64,
    baud: u32,
    nonce_address: u8,
    rng: Pcg32,
    faults: Vec<Fault>,
    silent_until_us: u64,
    last_nonce: Option<[u8; 8]>,
    stats: ChipStats,
    /// Hashes done but not yet counted into the counter registers.
    hash_acc: f64,
    /// The domain the next counted step goes to (round robin, so the
    /// domain counters stay even at one step per poll).
    next_domain: u8,
    /// Steps the weak domain has earned but not yet counted (`Fault::WeakDomain`).
    weak_acc: f64,
    /// Error hashes not yet counted.
    error_acc: f64,
    /// Rate-register reads so far: the windows alternate, short first.
    rate_reads: u32,
}

impl SimChip {
    /// A chip fresh from reset. `index` seeds a distinct random stream
    /// per chip so two chips on one chain do not move in lockstep.
    /// The eight address bits this chip puts into its nonces: its index
    /// times `256 / chips`, set by the chain (the reference divides the
    /// bits by that interval; the chain address itself may differ).
    pub fn set_nonce_address(&mut self, bits: u8) {
        self.nonce_address = bits;
    }

    /// A chip fresh from reset. `index` seeds a distinct random stream
    /// per chip.
    pub fn new(profile: &'static ChipProfile, seed: u64, index: u8) -> Self {
        SimChip {
            profile,
            regs: [0; 256],
            address: 0,
            state: ChipState::Reset,
            now_us: 0,
            job: None,
            job_midstates: 0,
            job_ends_us: 0,
            next_nonce_us: u64::MAX,
            baud: 115_200,
            nonce_address: 0,
            rng: Pcg32::new(seed, u64::from(index) + 1),
            faults: Vec::new(),
            silent_until_us: 0,
            last_nonce: None,
            stats: ChipStats::default(),
            hash_acc: 0.0,
            next_domain: 0,
            weak_acc: 0.0,
            error_acc: 0.0,
            rate_reads: 0,
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
        PllWord::from_register_bytes(v, &self.profile.pll)
            .map_or(0, |w| w.frequency_hz(self.profile.pll.xtal_hz))
    }

    /// Difficulty implied by the ticket-mask register.
    pub fn ticket_difficulty(&self) -> u32 {
        regs::ticket_mask_to_difficulty(self.regs[usize::from(reg::TICKET_MASK)].to_be_bytes())
    }

    /// UART rate the chip believes it is at.
    pub fn baud(&self) -> u32 {
        self.baud
    }

    /// The job being worked on (for a BM1397 midstate job: its id,
    /// nonce, nbits and ntime; the hashes are zero).
    pub fn job(&self) -> Option<&Job> {
        self.job.as_ref()
    }

    /// Midstates the current job carries (BM1397); 0 for a header job.
    pub fn job_midstates(&self) -> u8 {
        self.job_midstates
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
            Group::Single => {
                // A chip type that both reference drivers only ever read
                // by broadcast does not answer a single-chip read at a
                // non-zero address: seen on a Naja Duo (2026-11-12), two
                // BM1373, chip 1 silent to reads at its command address
                // while chip 0 (address 0) answered and both hashed.
                // Single-chip writes are the references' and stay.
                if header.op() == Op::Read && self.profile.broadcast_reads && self.address != 0 {
                    return false;
                }
                // And a single-chip write to such a chip goes by what the
                // same Naja Duo did with a PLL write (2026-11-12): one to
                // command address 0 slowed both chips, one to chip 1's
                // command address (4) slowed neither. So address 0 is
                // taken by every chip and any other by none. (What the
                // references' own single-chip writes do there is the
                // same: the domain-end strength reaches both chips by
                // the write for chip 0.)
                if header.op() == Op::Write && !self.profile.single_chip_clock {
                    return chip_addr == 0;
                }
                header.op() == Op::SetAddress
                    || chip_addr == self.profile.command_address(self.address)
            }
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
        let baud_reg = r == self.profile.fast_uart_reg;
        if baud_reg && self.has(|f| matches!(f, Fault::BaudSwitchIgnored)) {
            return;
        }
        self.regs[usize::from(r)] = v;
        if baud_reg && v.to_be_bytes() == self.profile.fast_uart.0 {
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
            // A BM1397 answers `13 97 18 00` (WantClue's `chip_id[]`).
            let core_num = if self.profile.chip_id == 0x1397 {
                0x18
            } else {
                0x00
            };
            if self.profile.chip_id_reply_len == SHORT_RESPONSE_LEN {
                // BM1373, BM1397: nine bytes, six of payload.
                self.emit(&[id[0], id[1], core_num, self.address, 0, 0], 0x00, out);
                return;
            }
            [id[0], id[1], core_num, self.address, 0, 0, 0, 0]
        } else if self.profile.rate_register == Some(r) {
            // The BM1397 reports what it hashes: clock × small cores while
            // a job is in, in units of 2²⁰ H/s, the two windows in turn.
            // Nobody has captured whether the real chip alternates or
            // which window comes first.
            let busy = self.job.is_some() && self.now_us <= self.job_ends_us && !self.hung();
            let hs = if busy {
                self.frequency_hz() * u64::from(self.profile.small_cores)
            } else {
                0
            };
            let long = self.rate_reads % 2 == 1;
            self.rate_reads += 1;
            register_result_payload(regs::bm1397_hashrate_value(hs, long), self.address, r)
        } else {
            let mut v = self.regs[usize::from(r)];
            if self.has(|f| matches!(f, Fault::RegReadCorrupt { reg } if *reg == r)) {
                v ^= 0x0001_0100;
            }
            register_result_payload(v, self.address, r)
        };
        if self.profile.response_len == SHORT_RESPONSE_LEN {
            // Six bytes: value, address, register (`bm1397_asic_result_cmd_t`).
            self.emit(&payload[..6], 0x00, out);
        } else {
            self.emit(&payload, 0x00, out);
        }
    }

    fn handle_job(&mut self, payload: &[u8]) {
        if self.state != ChipState::Addressed {
            self.stats.violations += 1;
        }
        let job = if self.profile.host_midstates > 0 {
            // A midstate job: the chip never sees the header fields.
            let Some(m) = MidstateJob::from_bytes(payload) else {
                self.stats.violations += 1;
                return;
            };
            self.job_midstates = m.num_midstates;
            Job {
                job_id: m.job_id,
                starting_nonce: m.starting_nonce,
                nbits: m.nbits,
                ntime: m.ntime,
                merkle_root: [0; 32],
                prev_block_hash: [0; 32],
                version: 0,
            }
        } else {
            if payload.len() != JOB_LEN {
                self.stats.violations += 1;
                return;
            }
            let mut b = [0u8; JOB_LEN];
            b.copy_from_slice(payload);
            self.job_midstates = 0;
            Job::from_bytes(&b)
        };
        self.stats.jobs += 1;
        // How long the chip works this job before it runs out of nonce
        // space (with version rolling, or over the midstates it was
        // given): the reference driver's timeout.
        let versions = if self.job_midstates > 0 {
            u32::from(self.job_midstates)
        } else {
            1u32 << 16
        };
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
        self.count_hashes(now_us);
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

    /// Advance the hash counter registers (0x88..=0x8C, one step per
    /// 2^32 hashes; 0x4C errors) for the time hashed since the last
    /// call: the clock times the small cores while a job is active and
    /// the chip is not hung. This is what the firmware's hashrate
    /// monitor reads back.
    fn count_hashes(&mut self, now_us: u64) {
        let until = if self.job.is_some() {
            now_us.min(self.job_ends_us)
        } else {
            0
        };
        if until <= self.now_us {
            return;
        }
        let hz = self.frequency_hz();
        let hung = self.has(|f| matches!(f, Fault::HangAbove { hz: h } if hz > u64::from(*h)));
        if hz == 0 || hung {
            return;
        }
        let dt_s = (until - self.now_us) as f64 / 1e6;
        self.hash_acc += dt_s * hz as f64 * f64::from(self.profile.small_cores);
        const UNIT: f64 = 4_294_967_296.0;
        let steps = (self.hash_acc / UNIT).floor();
        if steps < 1.0 {
            return;
        }
        self.hash_acc -= steps * UNIT;
        let steps = steps as u32;
        let total = usize::from(reg::TOTAL_COUNT);
        self.regs[total] = self.regs[total].wrapping_add(steps);
        let domains = self.profile.domains.clamp(1, 8);
        let weak = self.faults.iter().find_map(|f| match f {
            Fault::WeakDomain { domain, per_mille } => Some((*domain, *per_mille)),
            _ => None,
        });
        for _ in 0..steps {
            let d = self.next_domain;
            self.next_domain = (self.next_domain + 1) % domains;
            if let Some((wd, per_mille)) = weak {
                if wd == d {
                    // The weak domain keeps only its share of the steps;
                    // the rest never happened, in the total either.
                    self.weak_acc += f64::from(per_mille) / 1000.0;
                    if self.weak_acc < 1.0 {
                        self.regs[total] = self.regs[total].wrapping_sub(1);
                        continue;
                    }
                    self.weak_acc -= 1.0;
                }
            }
            let r = usize::from(reg::domain_count(d));
            self.regs[r] = self.regs[r].wrapping_add(1);
        }
        if let Some(Fault::HashErrors { per_mille }) = self
            .faults
            .iter()
            .find(|f| matches!(f, Fault::HashErrors { .. }))
            .copied()
        {
            self.error_acc += f64::from(steps) * f64::from(per_mille) / 1000.0;
            let e = self.error_acc.floor();
            self.error_acc -= e;
            let r = usize::from(reg::ERROR_COUNT);
            self.regs[r] = self.regs[r].wrapping_add(e as u32);
        }
    }

    fn emit_nonce(&mut self, out: &mut Vec<u8>) {
        let Some(job) = self.job.as_ref() else { return };
        let job_id = job.job_id;
        let mask16 = (self.regs[usize::from(reg::VERSION_ROLLING)] & 0xFFFF) as u16;
        let short = self.profile.response_len == SHORT_RESPONSE_LEN;
        let len = if short { 6 } else { 8 };
        let mut payload = [0u8; 8];
        if self.has(
            |f| matches!(f, Fault::GarbageNonces { per_mille } if self.rng_peek_chance(*per_mille)),
        ) {
            for b in &mut payload[..len] {
                *b = (self.rng.next_u32() & 0xFF) as u8;
            }
            self.rng.next_u32();
        } else {
            let low17 = self.rng.next_u32() & 0x1_FFFF;
            let core = self.rng.below(u32::from(self.profile.cores)) as u8;
            if self.job_midstates > 0 {
                // BM1397: which of the midstates hit, in the id byte.
                let k = self.rng.below(u32::from(self.job_midstates)) as u8;
                payload[..6].copy_from_slice(&bm1397_job_result_payload(
                    job_id,
                    low17,
                    self.nonce_address,
                    core,
                    k,
                ));
            } else {
                let small = self.rng.below(16) as u8;
                let vbits = (self.rng.next_u32() as u16) & mask16;
                payload = job_result_payload(job_id, low17, self.nonce_address, core, small, vbits);
            }
        }
        self.stats.nonces += 1;
        if self.silenced() {
            self.stats.suppressed += 1;
            return;
        }
        self.emit(&payload[..len], FLAG_JOB, out);
        if self.has(|f| matches!(f, Fault::DuplicateNonce { per_mille } if self.rng_peek_chance(*per_mille))) {
            self.rng.next_u32();
            self.stats.nonces += 1;
            self.emit(&payload[..len], FLAG_JOB, out);
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

    /// Send a response: eleven bytes for an eight-byte payload, nine for
    /// a six-byte one (BM1373 chip id, every BM1397 reply).
    fn emit(&mut self, payload: &[u8], flags: u8, out: &mut Vec<u8>) {
        let mut frame = [0u8; RESPONSE_LEN];
        let n = if payload.len() == 6 {
            let mut six = [0u8; 6];
            six.copy_from_slice(payload);
            let mut short = [0u8; SHORT_RESPONSE_LEN];
            encode_short_response(&mut short, &six, flags);
            frame[..SHORT_RESPONSE_LEN].copy_from_slice(&short);
            SHORT_RESPONSE_LEN
        } else {
            let mut eight = [0u8; 8];
            eight.copy_from_slice(payload);
            encode_response(&mut frame, &eight, flags);
            RESPONSE_LEN
        };
        if let Some(Fault::BitFlipTx { per_mille }) = self
            .faults
            .iter()
            .find(|f| matches!(f, Fault::BitFlipTx { .. }))
            .copied()
        {
            for b in &mut frame[..n] {
                if self.rng.chance_per_mille(per_mille) {
                    *b ^= 1 << self.rng.below(8);
                }
            }
        }
        out.extend_from_slice(&frame[..n]);
    }
}
