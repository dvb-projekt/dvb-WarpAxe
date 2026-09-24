//! The board around the chain: the core-voltage regulator, the thermal
//! sensor with its fan output, the fan regulator, and a profile per
//! board that says which of these exist and on which pins.
//!
//! Everything talks to hardware through the [`I2c`] trait only, so the
//! drivers are tested on the host against a recorded bus, and the
//! firmware supplies the ESP-IDF implementation. Register sequences
//! follow ESP-Miner (the reference firmware for the Bitaxe), noted per
//! function where they matter.
#![cfg_attr(not(test), no_std)]

pub mod emc2101;
pub mod fan;
pub mod i2c;
pub mod pmbus;
pub mod profile;
pub mod tps546;

pub use i2c::I2c;
pub use profile::{Profile, BITAXE_GAMMA, T_DISPLAY_S3_DEVKIT};
