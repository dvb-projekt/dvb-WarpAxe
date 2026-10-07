//! Mining on the ESP32-S3 itself, for the bare LilyGO T-Display-S3
//! (the NerdMiner devkit): no BM13xx, the header is hashed by the S3's
//! SHA peripheral and, on the second core, in software. Part of the
//! shared firmware image since v0.1.18, marked experimental: it runs
//! whenever the board profile has no chips (`board::devkit`).
//!
//! What lives here is everything that runs on the host too: the SHA-256
//! arithmetic with a midstate over the header's first 64 bytes
//! ([`sha256`]), the word layout the peripheral wants for the two
//! remaining blocks (modelled and tested here, driven by the firmware),
//! and the parser for a NerdMiner's leftovers in flash ([`nerdminer`]),
//! so the devkit joins the owner's Wi-Fi and mines to their wallet on
//! the first start, as the AxeOS/NerdOS import does on a miner.

#![forbid(unsafe_code)]

pub mod nerdminer;
pub mod sha256;
