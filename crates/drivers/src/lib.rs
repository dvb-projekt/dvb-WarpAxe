//! Small `no_std` drivers for the parts around a BM13xx miner board:
//! the EMC2302 fan controller, the INA260 power monitor, the TMP1075 temperature sensor, the PCA9544
//! I²C multiplexer, the SSD1306 128×32 OLED with a 5×7 font, the PMBus
//! number encodings (LINEAR11, ULINEAR16) and an NTC thermistor.
//!
//! The bus drivers talk to the hardware through the small [`I2c`]
//! trait, so they run on the host against a scripted bus in the tests and on any
//! target the caller writes the trait for.
//!
//! Licensed under MIT OR Apache-2.0, unlike the rest of WarpAxe
//! (GPL-3.0-or-later): written from the datasheets and published board
//! schematics (the font is a widely circulated 5×7 table), free for
//! other firmware to use.
#![cfg_attr(not(any(test, feature = "mock")), no_std)]

pub mod emc2302;
pub mod font;
pub mod i2c;
pub mod ina260;
pub mod ntc;
pub mod pca9544;
pub mod pmbus;
pub mod ssd1306;
pub mod tmp1075;

pub use i2c::I2c;
