//! What the firmware's mining loop keeps in its stack frame.
//!
//! The loop (`firmware/warpaxe/src/miner.rs`, `run`) owns the chain
//! driver and the two policies as locals, so their size is stack: 13 KB
//! of frame in a 20 KB stack (`miner::STACK_BYTES`), the rest for what
//! the loop calls. On 2026-11-12 the per-chip clock state took 0.35 KB
//! of a margin that was 3.5 KB of 16 KB, and a Naja Duo's loop
//! overflowed every few minutes; nothing on the host said so. This is
//! the tripwire: whoever grows these past the line raises the stack
//! with them (and the line), or moves the growth off the stack.
//!
//! The sizes are the host's; the ESP32-S3's are the same within a few
//! bytes (the arrays are of integers and floats, one reference each).

use core::mem::size_of;

use warpaxe_chain::{Chain, DomainLatch, LoneSilence};

/// What the three may take of the loop's frame together, bytes.
const FRAME_SHARE: usize = 10 * 1024;

#[test]
fn the_driver_and_the_policies_fit_their_share_of_the_mining_loops_stack() {
    let (chain, latch, lone) = (
        size_of::<Chain>(),
        size_of::<DomainLatch>(),
        size_of::<LoneSilence>(),
    );
    let all = chain + latch + lone;
    assert!(
        all <= FRAME_SHARE,
        "Chain {chain} + DomainLatch {latch} + LoneSilence {lone} = {all} bytes, \
         more than {FRAME_SHARE}: the mining loop's stack (miner::STACK_BYTES) \
         has to grow with them"
    );
    // And not far below it without anyone noticing either: the line is
    // meant to sit close above the real figure.
    assert!(all + 512 > FRAME_SHARE, "{all} bytes: lower FRAME_SHARE");
}
