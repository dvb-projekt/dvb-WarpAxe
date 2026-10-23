//! What a miner tells support about itself, and what it must not.
//!
//! Two things the firmware's diagnostics need to get right every time,
//! kept apart from any I/O so they are host tests:
//!
//! - [`scrub`]: the privacy rules of the diagnostics file. No password
//!   is ever written (the config goes in redacted), the worker's address
//!   and the Wi-Fi name are masked, and the log excerpt is scrubbed of
//!   every secret the firmware knows, in case a line ever carried one.
//! - [`events`]: the event log that survives restarts: a ring of the
//!   last few dozen events (restarts, stops, pool switches, updates,
//!   silent chips, self-test results) stored as one NVS string like the
//!   config patches, written only when an event happens, never on a
//!   timer, and never twice for the same thing within a minute.
//! - [`restart`]: the marker stored before an intentional restart and
//!   read at the next boot, so a crash in that restart leaves a trace
//!   even when no core dump could be written.

#![forbid(unsafe_code)]

pub mod events;
pub mod restart;
pub mod scrub;
