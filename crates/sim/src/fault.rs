//! Things a chip does wrong in the field, on demand.
//!
//! Each fault is a switch on one [`crate::SimChip`]. They compose: a chip
//! can be hung above a frequency *and* flipping bits. The list follows
//! the design document's §6.3; the ones that need a board model (supply
//! voltage, temperature) are not here yet — this is the chip.

/// A misbehaviour to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The chip sends nothing at all (registers, nonces) for this long,
    /// then recovers. Simulates a chip that is briefly wedged.
    NoResponse {
        /// Silence in microseconds of simulated time.
        for_us: u64,
    },
    /// Some nonces are garbage: valid frame, valid CRC, but the payload
    /// is random. What a chip with marginal timing produces.
    GarbageNonces {
        /// Share of nonces affected, in ‰.
        per_mille: u16,
    },
    /// Random bit flips in bytes the chip sends. A bad cable, a
    /// marginal level shifter. The receiver sees CRC failures and
    /// desyncs.
    BitFlipTx {
        /// Probability per byte, in ‰.
        per_mille: u16,
    },
    /// Above this clock the chip stops producing nonces but keeps
    /// answering register reads — the chip's side of the silent VR
    /// shutdown of 2026-09-22, and of a plain over-clock hang. The
    /// firmware's only honest signal is that nonces stop.
    HangAbove {
        /// Threshold in Hz.
        hz: u32,
    },
    /// Some nonces are sent twice. The BM1370 erratum the HCN margin
    /// exists for; a firmware that trusts the chip submits duplicates
    /// and gets rejects.
    DuplicateNonce {
        /// Share of nonces duplicated, in ‰.
        per_mille: u16,
    },
    /// Reads of this register return a corrupted value (bits flipped),
    /// with a valid CRC. Tests that a firmware which reads back what it
    /// wrote actually checks.
    RegReadCorrupt {
        /// Register address.
        reg: u8,
    },
    /// The chip ignores the fast-UART write and stays at the initial
    /// baud rate. A firmware that switches its own UART anyway loses the
    /// chip.
    BaudSwitchIgnored,
    /// A share of the chip's hashes are errors: the error counter
    /// register advances at this rate relative to the total counter.
    /// What a chip pushed past its stable clock reports.
    HashErrors {
        /// Errors per mille of the hashes.
        per_mille: u16,
    },
}
