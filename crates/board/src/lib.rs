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

pub mod chip_temps;
pub mod choice;
pub mod core_hold;
pub mod detect;
pub mod emc2101;
pub mod emc2103;
pub mod fan;
pub mod fxl6408;
pub mod overheat;
pub mod power_fault;
pub mod profile;
pub mod psu_sag;
pub mod selftest;
pub mod setup;
pub mod tps53647;
pub mod tps546;
pub mod warm_boot;

// The drivers that live in warpaxe-drivers (MIT OR Apache-2.0), under
// their old paths.
pub use warpaxe_drivers::{emc2302, font, i2c, ina260, ntc, pca9544, pmbus, ssd1306, tmp1075};

pub use i2c::I2c;
pub use profile::{
    Asic, EthPins, PowerEstimate, Profile, Thermal, Vr, VrAlt, BITAXE_GAMMA, NERDNOS,
    T_DISPLAY_S3_DEVKIT,
};
