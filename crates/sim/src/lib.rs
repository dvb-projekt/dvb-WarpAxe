//! BM13xx chip simulator — placeholder until M2.
//!
//! The simulator will own a `SimChip` per chip on the chain, decode the
//! frames the firmware sends through `warpaxe_core::frame`, keep a
//! register file, and answer with nonces it *constructs* for the current
//! work at the rate the frequency and ticket mask imply. It never hashes.
//! Fault injection (no response, garbage nonces, silent VR shutdown, …)
//! is the reason it exists.

#![forbid(unsafe_code)]
