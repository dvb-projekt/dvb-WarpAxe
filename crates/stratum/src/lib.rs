//! Stratum V1, the parts that do not touch a socket.
//!
//! The firmware reads lines from a TCP stream and hands them to
//! [`parse_line`]; it builds requests with the `*_request` functions and
//! turns a [`Notify`] plus the session's extranonce into the block-header
//! fields the chip wants with [`Job::work`]. Everything here is tested on
//! the host against a session captured from WarpPool.
//!
//! Byte orders follow ESP-Miner (`components/stratum/mining.c`), which is
//! what the BM1370 was proven to hash correctly with.

#![forbid(unsafe_code)]

pub mod coinbase;
pub mod gate;
pub mod job;
pub mod line;
pub mod message;
pub mod payout;

pub use gate::WorkGate;
pub use job::{Job, Prepared, Session};
pub use message::{parse_line, Incoming, Notify, Request, Response};

/// Bitcoin's double SHA-256.
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    second.into()
}
