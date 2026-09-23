//! The chain driver: what the firmware does with a row of BM13xx chips.
//!
//! [`Chain`] owns the state machine — bring-up from the chip profile's
//! init list, chip enumeration, baud switch, frequency ramp, work
//! dispatch, nonce attribution to work and chip, duplicate and stale
//! filtering, and a per-chip silence watchdog. It talks to the chips
//! through the [`Uart`] trait only and takes time as a parameter, so
//! the same code runs against `warpaxe-sim` under `cargo test` and
//! against the ESP32-S3's UART on the board. No allocation, no
//! blocking: `poll` does what is due and returns.
//!
//! What the driver does *not* do: it does not know Stratum, it does not
//! verify shares, it does not decide frequencies. It executes.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod driver;
pub mod uart;

pub use driver::{
    Chain, ChipStatus, Config, Error, Event, Found, InitError, Phase, Stats, Work, WorkRef,
    MAX_CHIPS, SLOTS,
};
pub use uart::Uart;
