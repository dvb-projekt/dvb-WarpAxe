//! A chain of simulated chips on one UART.
//!
//! The host's bytes go through a [`CmdDecoder`]; each frame is routed
//! the way the chain wiring routes it — `GROUP_ALL` to every chip,
//! `GROUP_SINGLE` to the one with that address, `SETADDRESS` to the
//! first chip still waiting for one. Responses are concatenated in chip
//! order, which is what a daisy chain does.
//!
//! [`SimChain::feed`], [`SimChain::advance`] and [`SimChain::drain`] are
//! the whole interface. A transport (PTY, TCP, in-process) is a loop
//! around those three.

use warpaxe_core::chip::ChipProfile;
use warpaxe_core::cmd::{CmdDecoder, CmdStats};
use warpaxe_core::frame::Op;

use crate::chip::{ChipStats, SimChip};
use crate::fault::Fault;

/// n chips, one wire.
#[derive(Debug, Clone)]
pub struct SimChain {
    chips: Vec<SimChip>,
    decoder: CmdDecoder,
    now_us: u64,
    out: Vec<u8>,
    bytes_in: u64,
    bytes_out: u64,
}

impl SimChain {
    /// A chain of `n` chips of one profile, all in reset.
    pub fn new(profile: &'static ChipProfile, n: u8, seed: u64) -> Self {
        SimChain {
            chips: (0..n).map(|i| SimChip::new(profile, seed, i)).collect(),
            decoder: CmdDecoder::new(),
            now_us: 0,
            out: Vec::new(),
            bytes_in: 0,
            bytes_out: 0,
        }
    }

    /// The chips, in chain order.
    pub fn chips(&self) -> &[SimChip] {
        &self.chips
    }

    /// One chip, mutable (to inject faults or inspect).
    pub fn chip_mut(&mut self, index: usize) -> &mut SimChip {
        &mut self.chips[index]
    }

    /// Simulated time.
    pub fn now_us(&self) -> u64 {
        self.now_us
    }

    /// Host-frame decoder statistics: what the chain saw on the wire.
    pub fn wire_stats(&self) -> CmdStats {
        self.decoder.stats()
    }

    /// Bytes in and out.
    pub fn byte_counts(&self) -> (u64, u64) {
        (self.bytes_in, self.bytes_out)
    }

    /// Sum of every chip's counters.
    pub fn totals(&self) -> ChipStats {
        self.chips.iter().fold(ChipStats::default(), |a, c| {
            let s = c.stats();
            ChipStats {
                commands: a.commands + s.commands,
                writes: a.writes + s.writes,
                reads: a.reads + s.reads,
                jobs: a.jobs + s.jobs,
                nonces: a.nonces + s.nonces,
                violations: a.violations + s.violations,
                suppressed: a.suppressed + s.suppressed,
            }
        })
    }

    /// Inject a fault into every chip.
    pub fn inject_all(&mut self, fault: Fault) {
        for c in &mut self.chips {
            c.inject(fault);
        }
    }

    /// Bytes from the host. Frames are handled as they complete;
    /// responses accumulate for [`SimChain::drain`].
    pub fn feed(&mut self, bytes: &[u8]) {
        self.bytes_in += bytes.len() as u64;
        // Collect frames first: the decoder borrows self.
        let mut frames = Vec::new();
        self.decoder.push_slice(bytes, |f| frames.push(f));
        for f in frames {
            let header = f.header;
            let payload = f.payload();
            let chip_addr = payload.first().copied().unwrap_or(0);
            if !header.is_job() && header.op() == Op::SetAddress {
                // The first chip still in Inactive takes it. A chip that
                // is not Inactive counts it as a violation (see SimChip).
                let target = self
                    .chips
                    .iter()
                    .position(|c| c.state() == crate::chip::ChipState::Inactive)
                    .or_else(|| {
                        self.chips
                            .iter()
                            .position(|c| c.state() == crate::chip::ChipState::Reset)
                    })
                    .unwrap_or(0);
                if let Some(c) = self.chips.get_mut(target) {
                    c.handle(header, payload, &mut self.out);
                }
                continue;
            }
            for c in &mut self.chips {
                if c.accepts(header, chip_addr) {
                    c.handle(header, payload, &mut self.out);
                }
            }
        }
    }

    /// Let `dt_us` of simulated time pass. Nonces that fall due are
    /// emitted in time order across chips.
    pub fn advance(&mut self, dt_us: u64) {
        let target = self.now_us.saturating_add(dt_us);
        // Chips emit into a shared buffer; to keep cross-chip order
        // sensible we step in slices. 1 ms slices are far finer than any
        // nonce rate we model.
        let mut t = self.now_us;
        while t < target {
            t = (t + 1_000).min(target);
            for c in &mut self.chips {
                c.advance_to(t, &mut self.out);
            }
        }
        self.now_us = target;
    }

    /// Take everything the chain has sent since the last drain.
    pub fn drain(&mut self) -> Vec<u8> {
        self.bytes_out += self.out.len() as u64;
        core::mem::take(&mut self.out)
    }
}
