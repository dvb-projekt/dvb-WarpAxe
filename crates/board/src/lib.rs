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

pub mod detect;
pub mod emc2101;
pub mod emc2103;
pub mod emc2302;
pub mod fan;
pub mod font;
pub mod fxl6408;
pub mod i2c;
pub mod ina260;
pub mod ntc;
pub mod overheat;
pub mod pca9544;
pub mod pmbus;
pub mod power_fault;
pub mod profile;
pub mod setup;
pub mod ssd1306;
pub mod tmp1075;
pub mod tps53647;
pub mod tps546;

pub use i2c::I2c;
pub use profile::{
    Asic, PowerEstimate, Profile, Thermal, Vr, VrAlt, BITAXE_GAMMA, NERDNOS, T_DISPLAY_S3_DEVKIT,
};
