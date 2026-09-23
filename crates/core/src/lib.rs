//! BM13xx ASIC wire protocol, host-testable and `no_std`.
//!
//! Everything in this crate is pure: bytes in, bytes or values out. No
//! I/O, no clock, no allocation. Time and randomness are passed in by the
//! caller where a module needs them. That is what lets the same code run
//! under `cargo test` on a laptop, inside the simulator, and on the chip.
//!
//! Provenance: the BM1370/BM1373 protocol has no public datasheet. What
//! is known comes from reverse engineering, chiefly ESP-Miner
//! (`components/asic/`, GPL-3.0). Every constant that was taken from
//! there says so in its doc comment, and every test vector that was
//! lifted from an ESP-Miner test says which one. Values marked
//! "from a comment" were read from source comments, not from a test
//! assertion, and rank lower until a hardware capture confirms them.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod crc;
pub mod decoder;
pub mod frame;
pub mod pll;
