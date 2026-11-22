//! Discord notifications, everything but the sending.
//!
//! The miner posts a short message to a Discord webhook the owner
//! entered when something worth a look happens: a block found, a new
//! all-time best difficulty, a switch to or from the fallback pool, a
//! restart after a crash. This crate holds the rules, kept apart from
//! any I/O so they are host tests; the firmware's `alerts` module queues
//! and sends (`docs/22`).
//!
//! - [`webhook`]: which URLs are taken (https, Discord's own hosts, the
//!   webhook path) and the token part a scrubber must know.
//! - [`message`]: what each message says, and the JSON body with its
//!   escaping.
//! - [`pace`]: when a message goes out: coalescing (one best-difficulty
//!   message per ten minutes at most, the newest value), the retry after
//!   a failure (at most one) and Discord's `retry_after`.

#![forbid(unsafe_code)]

pub mod message;
pub mod pace;
pub mod webhook;

pub use message::{Alert, Kind};
