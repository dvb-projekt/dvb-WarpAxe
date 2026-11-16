//! EMC2101: fan controller with one external temperature diode, I²C
//! 0x4C. On the Bitaxe the diode is the BM1370's, so "external" is the
//! chip temperature.
//!
//! The register work is the `emc2101` crate's (GPTechinno, MIT OR
//! Apache-2.0); this module holds what is board-specific: the diode
//! tuning per board, the fan as a percentage, and an open or shorted
//! diode as a fault instead of a temperature.

use bounded_integer::BoundedU8;
use emc2101::{BetaCompensation, PwmClock, EMC2101};

use crate::i2c::{Error, I2c};

/// I²C address of the EMC2101.
pub const ADDR: u8 = 0x4C;

/// PWM_F at power-on (datasheet table 6.24): with the 360 kHz base
/// clock that is 360 kHz / (2 · 23) ≈ 7.8 kHz, the frequency the Bitaxe
/// firmware has always run its fan at. Full duty from fan step 46.
const PWM_F_POWER_ON: u8 = 0x17;
/// The highest fan step; the percentage maps onto 0–63.
const FAN_STEPS: u16 = 63;
/// Configuration register (datasheet §6.5) as the init leaves it: only
/// ALT_TCH set (ALERT/TACH pin as tach input). Written whole, so a
/// STANDBY or other bit a previous firmware left set across a warm
/// restart is cleared; the crate only sets and clears single bits.
const CONFIG: u8 = 0x04;
const REG_CONFIG: u8 = 0x03;
/// Fan Configuration (§6.16) as the init leaves it: PROG (fan setting
/// written directly), 360 kHz base clock, not inverted, TACH_M 11
/// (ignored at 360 kHz, Note 6.2).
const FAN_CONFIG: u8 = 0x23;
const REG_FAN_CONFIG: u8 = 0x4A;
/// From 127 °C up the reading is the open-diode code (0x3F8 in eighths),
/// the short (0x3FF) or an unpowered chip's diode (0x3FE seen on a Gamma
/// with the core rail off), never a temperature.
const FAULT_FROM_C: f32 = 127.0;

/// Diode tuning per board: the BM1370 diode wants ideality 0x24 and
/// beta register 0x00 (ESP-Miner board table); the BitForge Nano's
/// maker tunes the same chip's diode to 0x37 ([`Config::BITFORGE_NANO_DIODE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Diode ideality factor register, written when set.
    pub ideality: Option<u8>,
    /// Beta compensation register, written when set.
    pub beta: Option<u8>,
    /// Added to every external reading, °C.
    pub temp_offset_c: i8,
}

impl Config {
    /// The BM1370's diode as ESP-Miner tunes it: ideality 0x24, beta
    /// register 0x00.
    pub const BM1370_DIODE: Config = Config {
        ideality: Some(0x24),
        beta: Some(0x00),
        temp_offset_c: 0,
    };

    /// The BM1370 diode as ForgeOS tunes it on the BitForge Nano:
    /// ideality 0x37 (1.0566), beta register 0x00 (ForgeOS v1.7 `f75f3ff`,
    /// `main/ThermalMonitoring/src/ThermalMonitoring.c` 27–28, 43–44;
    /// values `EMC2101.h` 8, 80; unchanged since v1.0). ESP-Miner's 0x24
    /// (1.0319) on the same chip reads higher: measured on the Nano .44,
    /// 2026-10-23, both settings back to back on each chip, 106 pairs,
    /// warm at 525 MHz / 1150 mV: 0x24 − 0x37 = 8.25 °C on chip 1
    /// (7.9–8.6) and 8.22 °C on chip 2 (59.1 / 59.9 °C against 51.0 /
    /// 51.5 °C), error rate unchanged. With 0x37 the 75 °C stop, the fan
    /// target and the figures in the API are on the maker's scale, as
    /// ForgeOS and the tuning tools see the board. Taken 2026-10-23
    /// (Oliver).
    pub const BITFORGE_NANO_DIODE: Config = Config {
        ideality: Some(0x37),
        beta: Some(0x00),
        temp_offset_c: 0,
    };
}

enum State<I: I2c> {
    /// The bus, before [`Emc2101::init`].
    Bus(I),
    /// The crate's driver, after a successful init.
    Ready(EMC2101<I>),
    /// An init failed; the crate kept the bus.
    Failed,
}

/// The driver: the bus until [`init`](Self::init), the crate's driver
/// after it.
pub struct Emc2101<I: I2c> {
    state: State<I>,
    cfg: Config,
}

fn map<E>(e: emc2101::Error<E>) -> Error<E> {
    match e {
        emc2101::Error::I2c(e) => Error::Bus(e),
        emc2101::Error::InvalidID => Error::WrongDevice,
        _ => Error::OutOfRange,
    }
}

impl<I: I2c> Emc2101<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, cfg: Config) -> Self {
        Emc2101 {
            state: State::Bus(bus),
            cfg,
        }
    }

    /// Put Configuration and Fan Configuration to known values, check
    /// the part, make ALERT/TACH the tach input, run the fan output as
    /// PWM at the power-on frequency, non-inverted, and apply the diode
    /// tuning. Only once: a second call, or a call after a failed
    /// one, is [`Error::WrongDevice`].
    pub fn init(&mut self) -> Result<(), Error<I::Error>> {
        let mut bus = match core::mem::replace(&mut self.state, State::Failed) {
            State::Bus(bus) => bus,
            other => {
                self.state = other;
                return Err(Error::WrongDevice);
            }
        };
        bus.write(ADDR, &[REG_CONFIG, CONFIG]).map_err(Error::Bus)?;
        bus.write(ADDR, &[REG_FAN_CONFIG, FAN_CONFIG])
            .map_err(Error::Bus)?;
        let mut dev = EMC2101::new(bus).map_err(map)?;
        dev.enable_tach_input().map_err(map)?;
        let pwm_f = BoundedU8::new(PWM_F_POWER_ON).ok_or(Error::OutOfRange)?;
        dev.set_fan_pwm_advanced(PwmClock::Base360kHz, pwm_f, false)
            .map_err(map)?;
        if let Some(i) = self.cfg.ideality {
            let i = BoundedU8::new(i).ok_or(Error::OutOfRange)?;
            dev.set_ideality_factor(i).map_err(map)?;
        }
        if let Some(b) = self.cfg.beta {
            let b = BetaCompensation::try_from(b).map_err(|_| Error::OutOfRange)?;
            dev.set_beta_compensation(b).map_err(map)?;
        }
        self.state = State::Ready(dev);
        Ok(())
    }

    fn dev(&mut self) -> Result<&mut EMC2101<I>, Error<I::Error>> {
        match &mut self.state {
            State::Ready(dev) => Ok(dev),
            _ => Err(Error::WrongDevice),
        }
    }

    /// Fan duty, 0–100 %, onto the 64 fan steps.
    pub fn set_fan_percent(&mut self, percent: u8) -> Result<(), Error<I::Error>> {
        let step = (u16::from(percent.min(100)) * FAN_STEPS / 100) as u8;
        let step = BoundedU8::new(step).ok_or(Error::OutOfRange)?;
        self.dev()?.set_fan_power(step).map_err(map)?;
        Ok(())
    }

    /// Fan speed from the tach input; 0 when the fan does not turn.
    pub fn fan_rpm(&mut self) -> Result<u16, Error<I::Error>> {
        self.dev()?.fan_rpm().map_err(map)
    }

    /// The diode temperature, °C, with the board offset applied. An open
    /// or shorted diode is a fault, not a value.
    pub fn external_temp_c(&mut self) -> Result<f32, Error<I::Error>> {
        let c = self.dev()?.temp_external_precise().map_err(map)?;
        if c >= FAULT_FROM_C {
            return Err(Error::SensorFault);
        }
        Ok(c + f32::from(self.cfg.temp_offset_c))
    }

    /// The controller's own die temperature, °C.
    pub fn internal_temp_c(&mut self) -> Result<i8, Error<I::Error>> {
        self.dev()?.temp_internal().map_err(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn get(reg: u8, v: u8) -> Tx {
        Tx::WriteRead(ADDR, vec![reg], vec![v])
    }
    fn put(reg: u8, v: u8) -> Tx {
        Tx::Write(ADDR, vec![reg, v])
    }

    /// The init: both configuration registers written whole, then the
    /// crate's steps (which find nothing to change), then the diode.
    fn init_script(ideality: u8) -> Vec<Tx> {
        vec![
            put(0x03, 0x04), // Configuration: ALT_TCH only
            put(0x4A, 0x23), // Fan Config: PROG, 360 kHz, TACH_M 11
            get(0xFD, 0x16), // product id
            put(0x16, 0xFF), // alert mask
            get(0x03, 0x04), // ALT_TCH already set
            put(0x4D, 0x17), // PWM_F at its power-on value
            get(0x4A, 0x23), // 360 kHz, non-inverted: no change
            get(0x03, 0x04), // PWM, not DAC: no change
            put(0x17, ideality),
            put(0x18, 0x00),
        ]
    }

    #[test]
    fn init_sets_tach_pwm_and_the_diode() {
        let mut s = Emc2101::new(Bus::new(init_script(0x24)), Config::BM1370_DIODE);
        s.init().unwrap();
        assert_eq!(s.init(), Err(Error::WrongDevice), "only once");
    }

    /// The Nano's sensors get the maker's ideality; every other
    /// EMC2101 board stays on ESP-Miner's.
    #[test]
    fn the_nano_diode_is_forgeos_tuning() {
        let mut s = Emc2101::new(Bus::new(init_script(0x37)), Config::BITFORGE_NANO_DIODE);
        s.init().unwrap();
        assert_eq!(Config::BM1370_DIODE.ideality, Some(0x24));
        assert_eq!(
            Config::BITFORGE_NANO_DIODE.temp_offset_c,
            0,
            "the scale is the register's, no offset on top"
        );
    }

    #[test]
    fn wrong_part_id_is_an_error_and_nothing_works_after() {
        let script = vec![put(0x03, 0x04), put(0x4A, 0x23), get(0xFD, 0x00)];
        let mut s = Emc2101::new(Bus::new(script), Config::BM1370_DIODE);
        assert_eq!(s.init(), Err(Error::WrongDevice));
        assert_eq!(s.fan_rpm(), Err(Error::WrongDevice));
    }

    #[test]
    fn nothing_is_sent_before_init() {
        let mut s = Emc2101::new(Bus::new(vec![]), Config::BM1370_DIODE);
        assert_eq!(s.set_fan_percent(50), Err(Error::WrongDevice));
        assert_eq!(s.external_temp_c(), Err(Error::WrongDevice));
    }

    fn ready(extra: Vec<Tx>, cfg: Config) -> Emc2101<Bus> {
        let mut script = init_script(cfg.ideality.unwrap_or(0));
        script.extend(extra);
        let mut s = Emc2101::new(Bus::new(script), cfg);
        s.init().unwrap();
        s
    }

    #[test]
    fn fan_percent_maps_to_63_steps() {
        let mut s = ready(
            vec![
                get(0x4A, 0x23),
                put(0x4C, 63),
                get(0x4A, 0x23),
                put(0x4C, 31),
                get(0x4A, 0x23),
                put(0x4C, 0),
                get(0x4A, 0x23),
                put(0x4C, 63),
            ],
            Config::BM1370_DIODE,
        );
        for p in [100, 50, 0, 200] {
            s.set_fan_percent(p).unwrap();
        }
    }

    #[test]
    fn tach_converts_to_rpm_and_a_stalled_fan_reads_zero() {
        let mut s = ready(
            vec![
                get(0x46, 0xD0),
                get(0x47, 0x07), // 2000 counts
                get(0x46, 0xFF),
                get(0x47, 0xFF),
            ],
            Config::BM1370_DIODE,
        );
        assert_eq!(s.fan_rpm().unwrap(), 2700);
        assert_eq!(s.fan_rpm().unwrap(), 0);
    }

    #[test]
    fn external_temp_decodes_eighths_and_flags_faults() {
        let mut s = ready(
            vec![
                // 58.125 °C = 465 eighths → MSB 0x3A, LSB 0x20
                get(0x01, 0x3A),
                get(0x10, 0x20),
                // -2.0 °C = -16 eighths → MSB 0xFE, LSB 0x00
                get(0x01, 0xFE),
                get(0x10, 0x00),
                // open diode: 0x3F8 eighths → MSB 0x7F, LSB 0x00
                get(0x01, 0x7F),
                get(0x10, 0x00),
                // 126.875 °C is still a reading: MSB 0x7E, LSB 0xE0
                get(0x01, 0x7E),
                get(0x10, 0xE0),
            ],
            Config {
                temp_offset_c: 1,
                ..Config::BM1370_DIODE
            },
        );
        assert!((s.external_temp_c().unwrap() - 59.125).abs() < 1e-6);
        assert!((s.external_temp_c().unwrap() - -1.0).abs() < 1e-6);
        assert_eq!(s.external_temp_c(), Err(Error::SensorFault));
        assert!((s.external_temp_c().unwrap() - 127.875).abs() < 1e-6);
    }
}
