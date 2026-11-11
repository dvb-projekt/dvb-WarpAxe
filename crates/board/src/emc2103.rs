//! EMC2103: fan controller with two external diode channels, on the
//! Bitaxe Gamma Turbo, Naja Duo and Gamma Hex at 0x2E. Register use
//! follows ESP-Miner's `EMC2103.c`: configuration 1 cleared, direct PWM
//! on the Hex (LUT off, push-pull, 26 kHz base, divide by 1), the diode
//! ideality/beta pair written to both channels when the board sets
//! them, tach in 1/8192 minutes, temperatures as 11-bit signed eighths.
//! Which diode is the chip is a board property (`flip`); the BM1373
//! boards carry a per-diode calibration.

use crate::i2c::{Error, I2c};

/// I²C address (same as the EMC2302, on another board's bus).
pub const ADDR: u8 = 0x2E;
const REG_EXT1_MSB: u8 = 0x02;
const REG_EXT1_LSB: u8 = 0x03;
const REG_EXT2_MSB: u8 = 0x04;
const REG_EXT2_LSB: u8 = 0x05;
const REG_IDEALITY1: u8 = 0x11;
const REG_IDEALITY2: u8 = 0x12;
const REG_BETA1: u8 = 0x14;
const REG_BETA2: u8 = 0x15;
const REG_CONFIG1: u8 = 0x20;
const REG_PWM_CONFIG: u8 = 0x2A;
const REG_PWM_BASE_FREQ: u8 = 0x2B;
const REG_FAN_SETTING: u8 = 0x40;
const REG_PWM_DIVIDE: u8 = 0x41;
const REG_FAN_CONFIG1: u8 = 0x42;
const REG_TACH_MSB: u8 = 0x4E;
const REG_TACH_LSB: u8 = 0x4F;
const REG_LUT_CONFIG1: u8 = 0x50;
const REG_MANUFACTURER_ID: u8 = 0xFE;
/// SMSC.
const MANUFACTURER_ID: u8 = 0x5D;
const PWM_CONFIG_PUSH_PULL_NORMAL: u8 = 0x10;
const FAN_CONFIG1_DIRECT_PWM: u8 = 0x2B;
const TEMP_DIODE_FAULT: u16 = 0x8000;
/// The top of the 11-bit scale, 127.875 °C: what a channel reads with
/// no working diode on it (a Gamma Turbo's second diode without
/// contact read it from the first sample, John11, 2026-11-11).
const TEMP_CEILING_EIGHTHS: i16 = 0x3FF;
/// Tach counts per minute at the 32.768 kHz clock, two pulses per turn.
const TACH_NUMERATOR: u32 = 7_864_320;

/// A diode channel's calibration: reading × scale + offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// Multiplier on the raw reading.
    pub scale: f32,
    /// Added afterwards, °C.
    pub offset_c: f32,
}

impl Calibration {
    /// As read.
    pub const NONE: Calibration = Calibration {
        scale: 1.0,
        offset_c: 0.0,
    };
    /// ESP-Miner's BM1373 diode fit (`thermal.c`).
    pub const BM1373: Calibration = Calibration {
        scale: 0.961_354_6,
        offset_c: -5.085_802_6,
    };
}

/// Per-board setup (ESP-Miner's board table).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Diode ideality register, written to both channels when set.
    pub ideality: Option<u8>,
    /// Beta compensation register, written to both channels when set.
    pub beta: Option<u8>,
    /// The chip sits on diode 2, not diode 1 (`temp_flip`).
    pub flip: bool,
    /// Drive the fan by the setting register directly, LUT off
    /// (`emc_direct_pwm`, Gamma Hex).
    pub direct_pwm: bool,
    /// Added to every chip reading, °C.
    pub temp_offset_c: i8,
    /// Calibration of diode 1 and diode 2.
    pub calibration: [Calibration; 2],
    /// The other diode sits at a second chip: fan and overheat stop
    /// act on the hotter of the two (`crate::chip_temps`). ESP-Miner
    /// reads it as `temp2` on every EMC2103 board.
    pub second_chip: bool,
}

impl Config {
    /// Bitaxe Gamma Turbo (801): two chips; the first on diode 2
    /// (`temp_flip`), the second on diode 1; no diode tuning.
    pub const GAMMA_TURBO: Config = Config {
        ideality: None,
        beta: None,
        flip: true,
        direct_pwm: false,
        temp_offset_c: 0,
        calibration: [Calibration::NONE, Calibration::NONE],
        second_chip: true,
    };
    /// Bitaxe Naja Duo (1201): BM1373 diodes, ideality 0x17, beta 0x10,
    /// diode 2 a further 10.7 °C lower (ESP-Miner
    /// `NAJA_DUO_DIODE2_TEMP_OFFSET`).
    pub const NAJA_DUO: Config = Config {
        ideality: Some(0x17),
        beta: Some(0x10),
        flip: false,
        direct_pwm: false,
        temp_offset_c: 0,
        calibration: [
            Calibration::BM1373,
            Calibration {
                scale: Calibration::BM1373.scale,
                offset_c: Calibration::BM1373.offset_c - 10.7,
            },
        ],
        second_chip: true,
    };
    /// Bitaxe Gamma Hex (1300): direct PWM, chip on diode 2. Six chips
    /// on two diodes: what diode 1 sits at is not known here (the board
    /// has not been run), so it is not taken for a chip.
    pub const GAMMA_HEX: Config = Config {
        ideality: None,
        beta: None,
        flip: true,
        direct_pwm: true,
        temp_offset_c: 0,
        calibration: [Calibration::NONE, Calibration::NONE],
        second_chip: false,
    };
}

/// The driver, owning the bus.
pub struct Emc2103<I> {
    bus: I,
    cfg: Config,
}

impl<I: I2c> Emc2103<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, cfg: Config) -> Self {
        Emc2103 { bus, cfg }
    }

    /// Give the bus back.
    pub fn release(self) -> I {
        self.bus
    }

    fn read(&mut self, reg: u8) -> Result<u8, Error<I::Error>> {
        let mut b = [0u8; 1];
        self.bus.write_read(ADDR, &[reg], &mut b)?;
        Ok(b[0])
    }

    fn write(&mut self, reg: u8, v: u8) -> Result<(), Error<I::Error>> {
        Ok(self.bus.write(ADDR, &[reg, v])?)
    }

    /// Check the manufacturer, clear configuration 1, set the PWM path
    /// and the diode tuning.
    pub fn init(&mut self) -> Result<(), Error<I::Error>> {
        if self.read(REG_MANUFACTURER_ID)? != MANUFACTURER_ID {
            return Err(Error::WrongDevice);
        }
        self.write(REG_CONFIG1, 0x00)?;
        if self.cfg.direct_pwm {
            self.write(REG_LUT_CONFIG1, 0x00)?;
            self.write(REG_FAN_CONFIG1, FAN_CONFIG1_DIRECT_PWM)?;
            self.write(REG_PWM_CONFIG, PWM_CONFIG_PUSH_PULL_NORMAL)?;
            self.write(REG_PWM_BASE_FREQ, 0x00)?;
            self.write(REG_PWM_DIVIDE, 0x01)?;
        } else {
            self.write(REG_PWM_CONFIG, 0x00)?;
        }
        if let Some(i) = self.cfg.ideality {
            self.write(REG_IDEALITY1, i)?;
            self.write(REG_IDEALITY2, i)?;
        }
        if let Some(b) = self.cfg.beta {
            self.write(REG_BETA1, b)?;
            self.write(REG_BETA2, b)?;
        }
        Ok(())
    }

    /// Fan duty, 0–100 % (clamped), as 0–255.
    pub fn set_fan_percent(&mut self, percent: u8) -> Result<(), Error<I::Error>> {
        let p = u32::from(percent.min(100));
        self.write(REG_FAN_SETTING, ((p * 255 + 50) / 100) as u8)
    }

    /// Fan speed, rpm; 0 without a tach signal.
    pub fn fan_rpm(&mut self) -> Result<u16, Error<I::Error>> {
        let lsb = self.read(REG_TACH_LSB)?;
        let msb = self.read(REG_TACH_MSB)?;
        let reading = u32::from(u16::from_be_bytes([msb, lsb]) >> 3);
        if reading == 0 || reading >= 0x1FFF {
            return Ok(0);
        }
        let rpm = TACH_NUMERATOR / reading;
        // ESP-Miner's sentinel for "no fan".
        if rpm == 82 {
            return Ok(0);
        }
        Ok(rpm.min(u32::from(u16::MAX)) as u16)
    }

    /// `ceiling_is_fault`: the scale's top counts as no reading (the
    /// second chip's diode, see [`Self::second_temp_c`]).
    fn diode_c(&mut self, diode: usize, ceiling_is_fault: bool) -> Result<f32, Error<I::Error>> {
        let (msb, lsb) = if diode == 0 {
            (REG_EXT1_MSB, REG_EXT1_LSB)
        } else {
            (REG_EXT2_MSB, REG_EXT2_LSB)
        };
        let hi = self.read(msb)?;
        let lo = self.read(lsb)?;
        let raw = u16::from_be_bytes([hi, lo]);
        if raw == TEMP_DIODE_FAULT {
            return Err(Error::SensorFault);
        }
        // 11-bit signed, eighths of a degree.
        let v = ((raw >> 5) as i16) << 5 >> 5;
        if ceiling_is_fault && v == TEMP_CEILING_EIGHTHS {
            return Err(Error::SensorFault);
        }
        let c = self.cfg.calibration[diode];
        Ok(f32::from(v) / 8.0 * c.scale + c.offset_c + f32::from(self.cfg.temp_offset_c))
    }

    /// The chip's diode (diode 2 when flipped), calibrated, °C.
    pub fn chip_temp_c(&mut self) -> Result<f32, Error<I::Error>> {
        self.diode_c(usize::from(self.cfg.flip), false)
    }

    /// The other diode, calibrated, °C. The scale's top (127.875 °C raw)
    /// is no reading here: a diode that gives it from boot never read,
    /// so its chip is left out with the fans at full and the stop line
    /// on the other chip (`crate::chip_temps`), instead of a stop on
    /// every boot. One that read before and climbs there has crossed
    /// the stop line on the way and went unknown at the top, which
    /// keeps the chips off. The chip's own diode keeps the old rule:
    /// it is the board's only stop line.
    pub fn second_temp_c(&mut self) -> Result<f32, Error<I::Error>> {
        self.diode_c(usize::from(!self.cfg.flip), true)
    }

    /// The other diode sits at a second chip ([`Config::second_chip`]).
    pub fn second_chip(&self) -> bool {
        self.cfg.second_chip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn w(reg: u8, v: u8) -> Tx {
        Tx::Write(ADDR, vec![reg, v])
    }
    fn r(reg: u8, v: u8) -> Tx {
        Tx::WriteRead(ADDR, vec![reg], vec![v])
    }

    #[test]
    fn hex_init_is_esp_miners_direct_pwm_sequence() {
        let bus = Bus::new(vec![
            r(REG_MANUFACTURER_ID, 0x5D),
            w(REG_CONFIG1, 0x00),
            w(REG_LUT_CONFIG1, 0x00),
            w(REG_FAN_CONFIG1, 0x2B),
            w(REG_PWM_CONFIG, 0x10),
            w(REG_PWM_BASE_FREQ, 0x00),
            w(REG_PWM_DIVIDE, 0x01),
        ]);
        let mut t = Emc2103::new(bus, Config::GAMMA_HEX);
        t.init().unwrap();
        let bus = t.release();
        assert!(bus.script.is_empty());
        assert_eq!(bus.log.len(), 7);
    }

    #[test]
    fn naja_init_tunes_both_diodes_and_a_wrong_part_is_refused() {
        let bus = Bus::new(vec![
            r(REG_MANUFACTURER_ID, 0x5D),
            w(REG_CONFIG1, 0x00),
            w(REG_PWM_CONFIG, 0x00),
            w(REG_IDEALITY1, 0x17),
            w(REG_IDEALITY2, 0x17),
            w(REG_BETA1, 0x10),
            w(REG_BETA2, 0x10),
        ]);
        let mut t = Emc2103::new(bus, Config::NAJA_DUO);
        t.init().unwrap();
        assert!(t.release().script.is_empty());
        let mut t = Emc2103::new(
            Bus::new(vec![r(REG_MANUFACTURER_ID, 0x00)]),
            Config::GAMMA_TURBO,
        );
        assert_eq!(t.init(), Err(Error::WrongDevice));
        assert_eq!(t.release().log.len(), 1);
    }

    #[test]
    fn fan_percent_and_tach_follow_the_reference() {
        let bus = Bus::new(vec![
            w(REG_FAN_SETTING, 128),
            w(REG_FAN_SETTING, 255),
            // tach 0x0F00 >> 3 = 480 → 7 864 320 / 480 = 16 384 rpm; use
            // 0x7D00 >> 3 = 4000 → 1966 rpm.
            r(REG_TACH_LSB, 0x00),
            r(REG_TACH_MSB, 0x7D),
            r(REG_TACH_LSB, 0xFF),
            r(REG_TACH_MSB, 0xFF),
        ]);
        let mut t = Emc2103::new(bus, Config::GAMMA_TURBO);
        t.set_fan_percent(50).unwrap();
        t.set_fan_percent(150).unwrap();
        assert_eq!(t.fan_rpm().unwrap(), 1966);
        assert_eq!(t.fan_rpm().unwrap(), 0, "no tach signal");
    }

    #[test]
    fn temperatures_decode_as_eighths_with_flip_and_calibration() {
        // 50.0 °C = 400 eighths = 0x190 << 5 = 0x3200; -2.5 °C = -20 = 0x7EC << 5 = 0xFD80.
        let bus = Bus::new(vec![
            r(REG_EXT2_MSB, 0x32),
            r(REG_EXT2_LSB, 0x00),
            r(REG_EXT1_MSB, 0xFD),
            r(REG_EXT1_LSB, 0x80),
        ]);
        let mut t = Emc2103::new(bus, Config::GAMMA_TURBO);
        assert_eq!(
            t.chip_temp_c().unwrap(),
            50.0,
            "flipped: diode 2 is the chip"
        );
        assert_eq!(t.second_temp_c().unwrap(), -2.5);
        // Naja: diode 1, BM1373 fit: 50 × 0.9613546 − 5.0858 = 42.98.
        let bus = Bus::new(vec![
            r(REG_EXT1_MSB, 0x32),
            r(REG_EXT1_LSB, 0x00),
            r(REG_EXT2_MSB, 0x80),
            r(REG_EXT2_LSB, 0x00),
        ]);
        let mut t = Emc2103::new(bus, Config::NAJA_DUO);
        assert!((t.chip_temp_c().unwrap() - 42.98).abs() < 0.01);
        assert_eq!(t.second_temp_c(), Err(Error::SensorFault));
    }

    #[test]
    fn the_naja_s_second_chip_reads_diode_2_another_10_7_degrees_lower() {
        // Both diodes at a raw 80.0 °C (640 eighths = 0x280 << 5 = 0x5000):
        // chip 1 = 80 × 0.9613546 − 5.0858 = 71.82, chip 2 = 61.12
        // (ESP-Miner NAJA_DUO_DIODE2_TEMP_OFFSET).
        let bus = Bus::new(vec![
            r(REG_EXT1_MSB, 0x50),
            r(REG_EXT1_LSB, 0x00),
            r(REG_EXT2_MSB, 0x50),
            r(REG_EXT2_LSB, 0x00),
        ]);
        let mut t = Emc2103::new(bus, Config::NAJA_DUO);
        assert!(t.second_chip());
        let first = t.chip_temp_c().unwrap();
        let second = t.second_temp_c().unwrap();
        assert!((first - 71.82).abs() < 0.01, "{first}");
        assert!((second - 61.12).abs() < 0.01, "{second}");
        assert!(t.release().script.is_empty());
    }

    #[test]
    fn a_second_diode_at_the_top_of_the_scale_is_no_reading() {
        // 127.875 °C = 0x3FF << 5 = 0x7FE0 (John11's Gamma Turbo, diode 1
        // without contact). The chip's own diode at the top still reads
        // 127.875 and stops the chips as before.
        let bus = Bus::new(vec![
            r(REG_EXT1_MSB, 0x7F),
            r(REG_EXT1_LSB, 0xE0),
            r(REG_EXT2_MSB, 0x7F),
            r(REG_EXT2_LSB, 0xE0),
            r(REG_EXT1_MSB, 0x7F),
            r(REG_EXT1_LSB, 0xC0),
        ]);
        let mut t = Emc2103::new(bus, Config::GAMMA_TURBO);
        assert_eq!(t.second_temp_c(), Err(Error::SensorFault));
        assert_eq!(t.chip_temp_c().unwrap(), 127.875);
        // One step below the top is a reading: 127.75 °C.
        assert_eq!(t.second_temp_c().unwrap(), 127.75);
        assert!(t.release().script.is_empty());
    }

    #[test]
    fn which_boards_have_a_second_chip_on_the_other_diode() {
        let second = |cfg: Config| Emc2103::new(Bus::new(Vec::new()), cfg).second_chip();
        assert!(second(Config::NAJA_DUO));
        assert!(second(Config::GAMMA_TURBO));
        // The Hex: diode 1 is not taken for a chip.
        assert!(!second(Config::GAMMA_HEX));
        // The Turbo's second chip is on diode 1 (its first on diode 2).
        let bus = Bus::new(vec![r(REG_EXT1_MSB, 0x32), r(REG_EXT1_LSB, 0x00)]);
        let mut t = Emc2103::new(bus, Config::GAMMA_TURBO);
        assert_eq!(t.second_temp_c().unwrap(), 50.0);
    }
}
