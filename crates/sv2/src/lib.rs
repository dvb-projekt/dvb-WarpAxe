//! Stratum V2 for the firmware, the parts that do not touch a socket.
//!
//! The firmware connects a plain TCP stream, runs the [`Handshake`]
//! (Noise NX, 2024 spec, the same `noise_sv2` the WarpPool server uses),
//! then pumps bytes through a [`Transport`], which yields decrypted
//! [`Frame`]s and seals outgoing ones. A [`Client`] turns frames into
//! [`Event`]s and replies: connection setup, one extended mining channel,
//! jobs, targets and share results. A [`Template`] is the Sv2 counterpart
//! of a V1 notify plus session: the miner rolls its extranonce through
//! [`Template::build`] and gets the same [`Job`] the V1 path produces.
//!
//! Only the extended channel is implemented: the pool sends coinbase
//! prefix, suffix and merkle path, the miner builds the coinbase and the
//! merkle root itself, exactly as with V1. Standard channels (pool-built
//! merkle root, no extranonce) are what NerdOS and AxeOS avoid too.
//!
//! Wire facts follow the WarpPool `stratum-v2` crate, which is live with
//! NerdQAxe++, Bitaxe and NerdAxe Gamma: targets and `prev_hash` travel
//! little-endian, `min_ntime` in a job is an option, channel messages
//! carry the 0x8000 extension flag. Everything here is tested on the host
//! against that server (crates/scenarios).

#![forbid(unsafe_code)]

pub mod channel;
pub mod key;
pub mod messages;
pub mod noise;
pub mod release;
pub mod wire;

pub use channel::{Client, ClientConfig, Event, Template};
pub use key::authority_key_from_b58;
pub use noise::{Handshake, Transport, ACT_1_LEN, ACT_2_LEN};
pub use warpaxe_stratum::Job;
pub use wire::{Frame, CHANNEL_MSG_FLAG};

/// Everything that can go wrong between the socket and the miner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A message ended before its last field.
    Truncated,
    /// A length field exceeds what the wire format allows.
    TooLong,
    /// A string field is not UTF-8.
    BadUtf8,
    /// The authority key is not a valid Base58Check x-only key.
    BadKey,
    /// The Noise handshake failed (wrong authority key, expired
    /// certificate, garbage from the peer).
    Handshake(String),
    /// A frame did not decrypt: nonce out of step or tampering.
    Decrypt,
    /// A frame announces more payload than the client will hold.
    FrameTooLarge(usize),
    /// The peer broke the protocol (message out of phase, unknown type
    /// where one was required, channel id mismatch).
    Protocol(String),
    /// The pool refused the connection or the channel, with its code.
    Refused(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated => write!(f, "message truncated"),
            Error::TooLong => write!(f, "field too long"),
            Error::BadUtf8 => write!(f, "string field is not utf-8"),
            Error::BadKey => write!(f, "authority key is not a valid base58check key"),
            Error::Handshake(s) => write!(f, "noise handshake: {s}"),
            Error::Decrypt => write!(f, "frame did not decrypt"),
            Error::FrameTooLarge(n) => write!(f, "frame of {n} bytes too large"),
            Error::Protocol(s) => write!(f, "protocol: {s}"),
            Error::Refused(s) => write!(f, "refused by the pool: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// The message type codes of the mining protocol (spec §7) the client
/// sends or understands.
pub mod msg {
    /// Client → pool, first message after the handshake.
    pub const SETUP_CONNECTION: u8 = 0x00;
    /// Pool → client.
    pub const SETUP_CONNECTION_SUCCESS: u8 = 0x01;
    /// Pool → client, the connection is over.
    pub const SETUP_CONNECTION_ERROR: u8 = 0x02;
    /// Pool → client.
    pub const OPEN_MINING_CHANNEL_ERROR: u8 = 0x12;
    /// Client → pool.
    pub const OPEN_EXTENDED_MINING_CHANNEL: u8 = 0x13;
    /// Pool → client.
    pub const OPEN_EXTENDED_MINING_CHANNEL_SUCCESS: u8 = 0x14;
    /// Pool → client (standard channels only; ignored).
    pub const NEW_MINING_JOB: u8 = 0x15;
    /// Pool → client, the pool closes the channel.
    pub const CLOSE_CHANNEL: u8 = 0x18;
    /// Pool → client, a new extranonce prefix for the channel.
    pub const SET_EXTRANONCE_PREFIX: u8 = 0x19;
    /// Client → pool.
    pub const SUBMIT_SHARES_EXTENDED: u8 = 0x1B;
    /// Pool → client.
    pub const SUBMIT_SHARES_SUCCESS: u8 = 0x1C;
    /// Pool → client.
    pub const SUBMIT_SHARES_ERROR: u8 = 0x1D;
    /// Pool → client.
    pub const NEW_EXTENDED_MINING_JOB: u8 = 0x1F;
    /// Pool → client.
    pub const SET_NEW_PREV_HASH: u8 = 0x20;
    /// Pool → client.
    pub const SET_TARGET: u8 = 0x21;
    /// Pool → client, connect elsewhere.
    pub const RECONNECT: u8 = 0x25;
}
