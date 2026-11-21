//! EMC2101: fan controller with one external temperature diode, I²C
//! 0x4C. On the Bitaxe the diode is the BM1370's, so "external" is the
//! chip temperature.
//!
//! The register work is the `emc2101` crate's (GPTechinno, MIT OR
//! Apache-2.0); this module holds what is board-specific: the diode
//! tuning per board, the fan as a percentage, and an open or shorted
//! diode as a fault instead of a temperature.
//!
//! The CTF2301B (Sensylink) sits in the EMC2101's place on some boards,
//! the StarMiner among them: the same address and, in its compatibility
//! mode, the same temperature, tach and fan-setting registers, but its
//! own ID registers, its own diode-tuning registers, and a fan output
//! that stays off until three registers are set. The StarMiner's
//! ESP-Miner fork (github.com/awxhw/StarMiner, `main/thermal/EMC2101.c`)
//! tells the two apart by 0xFE/0xFF and runs that sequence; without it
//! the fan never turned under WarpAxe (2026-11-21). The CTF2301B is
//! driven here directly, the `emc2101` crate refusing its ID.

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
/// STANDBY, FAN_STANDBY, DAC or other bit a previous firmware left set
/// across a warm restart is cleared. The crate's `set_standby(false)`
/// (4.2) clears only STANDBY, and its `new()` leaves the rest as the
/// chip holds it by design (emc2101-rs#7).
const CONFIG: u8 = 0x04;
const REG_CONFIG: u8 = 0x03;
/// Product ID register; EMC2101 0x16, EMC2101-R 0x28 (the crate's own list).
const REG_PRODUCT_ID: u8 = 0xFD;
const PRODUCT_IDS: [u8; 2] = [0x16, 0x28];
/// CTF2301B chip and manufacturer ID registers and their values (the
/// StarMiner fork's `EMC2101.h`).
const REG_CTF_CHIP_ID: u8 = 0xFE;
const REG_CTF_MFG_ID: u8 = 0xFF;
const CTF_CHIP_ID: u8 = 0x41;
const CTF_MFG_ID: u8 = 0x59;
/// CTF2301B: the registers its fan output needs (the fork's init, run
/// as it runs it: Configuration bit 1 set, 0x19 = 0x7F, Configuration
/// back, 0x21 = 0x02).
const CTF_CONFIG_UNLOCK: u8 = 0x02;
const CTF_REG_19: u8 = 0x19;
const CTF_REG_19_VALUE: u8 = 0x7F;
const CTF_REG_21: u8 = 0x21;
const CTF_REG_21_VALUE: u8 = 0x02;
/// CTF2301B diode tuning: beta and NC (ideality) registers. Its NC table
/// differs: the BM1370's EMC2101 ideality 0x24 (1.0319) is NC 0x08
/// (1.0356), the closest; any other value is written as given, as the
/// fork does.
const CTF_REG_BETA: u8 = 0x30;
const CTF_REG_NC: u8 = 0x34;
/// Registers both parts share in compatibility mode.
const REG_INTERNAL_TEMP: u8 = 0x00;
const REG_EXT_TEMP_MSB: u8 = 0x01;
const REG_EXT_TEMP_LSB: u8 = 0x10;
const REG_TACH_LSB: u8 = 0x46;
const REG_TACH_MSB: u8 = 0x47;
const REG_FAN_SETTING: u8 = 0x4C;
/// Fan Configuration (§6.16) as the init leaves it: PROG (fan setting
/// written directly), 360 kHz base clock, not inverted, TACH_M 11
/// (ignored at 360 kHz, Note 6.2). With PROG set here the crate's
/// first `set_fan_power` reads Fan Config once, finds the Look-Up Table
/// off, and from then on writes only the fan setting (4.2,
/// emc2101-rs#7): a corrupted read inside the fan loop can no longer be
/// written back over polarity or clock.
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
    /// A CTF2301B, after a successful init: driven register by register.
    Ctf(I),
    /// An init failed (or ran once already); the bus is gone with it.
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
        // The part first, before anything is written: with a wrong board
        // choice another chip may sit at this address (as up to v0.1.45).
        // The CTF2301B first, by its own registers, as the StarMiner's
        // fork tells the two apart (an EMC2101 reads its manufacturer,
        // 0x5D, and revision there): what it answers at 0xFD is unknown.
        let mut chip = [0u8];
        let mut mfg = [0u8];
        bus.write_read(ADDR, &[REG_CTF_CHIP_ID], &mut chip)
            .map_err(Error::Bus)?;
        bus.write_read(ADDR, &[REG_CTF_MFG_ID], &mut mfg)
            .map_err(Error::Bus)?;
        if (chip[0], mfg[0]) == (CTF_CHIP_ID, CTF_MFG_ID) {
            return self.init_ctf(bus);
        }
        let mut id = [0u8];
        bus.write_read(ADDR, &[REG_PRODUCT_ID], &mut id)
            .map_err(Error::Bus)?;
        if !PRODUCT_IDS.contains(&id[0]) {
            return Err(Error::WrongDevice);
        }
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

    /// The CTF2301B's init: Configuration and Fan Configuration as for
    /// the EMC2101, then the fan output switched on, then the diode
    /// tuning in its own registers.
    fn init_ctf(&mut self, mut bus: I) -> Result<(), Error<I::Error>> {
        bus.write(ADDR, &[REG_CONFIG, CONFIG]).map_err(Error::Bus)?;
        bus.write(ADDR, &[REG_FAN_CONFIG, FAN_CONFIG])
            .map_err(Error::Bus)?;
        let mut cfg = [0u8];
        bus.write_read(ADDR, &[REG_CONFIG], &mut cfg)
            .map_err(Error::Bus)?;
        bus.write(ADDR, &[REG_CONFIG, cfg[0] | CTF_CONFIG_UNLOCK])
            .map_err(Error::Bus)?;
        bus.write(ADDR, &[CTF_REG_19, CTF_REG_19_VALUE])
            .map_err(Error::Bus)?;
        bus.write(ADDR, &[REG_CONFIG, cfg[0]]).map_err(Error::Bus)?;
        bus.write(ADDR, &[CTF_REG_21, CTF_REG_21_VALUE])
            .map_err(Error::Bus)?;
        // Only the BM1370 tuning has a known CTF2301B value (the fork's
        // NC 0x08 for ideality 0x24, beta 0x00): its NC and beta tables are
        // not the EMC2101's, so any other value is left at the part's own
        // default rather than written raw.
        if self.cfg.ideality == Some(0x24) {
            bus.write(ADDR, &[CTF_REG_NC, 0x08]).map_err(Error::Bus)?;
        }
        if self.cfg.beta == Some(0x00) {
            bus.write(ADDR, &[CTF_REG_BETA, 0x00]).map_err(Error::Bus)?;
        }
        self.state = State::Ctf(bus);
        Ok(())
    }

    /// Whether the part found at init is a CTF2301B.
    pub fn is_ctf2301b(&self) -> bool {
        matches!(self.state, State::Ctf(_))
    }

    fn dev(&mut self) -> Result<&mut EMC2101<I>, Error<I::Error>> {
        match &mut self.state {
            State::Ready(dev) => Ok(dev),
            _ => Err(Error::WrongDevice),
        }
    }

    fn ctf_read(bus: &mut I, reg: u8) -> Result<u8, Error<I::Error>> {
        let mut v = [0u8];
        bus.write_read(ADDR, &[reg], &mut v).map_err(Error::Bus)?;
        Ok(v[0])
    }

    /// Fan duty, 0–100 %, onto the 64 fan steps.
    pub fn set_fan_percent(&mut self, percent: u8) -> Result<(), Error<I::Error>> {
        let step = (u16::from(percent.min(100)) * FAN_STEPS / 100) as u8;
        if let State::Ctf(bus) = &mut self.state {
            return bus
                .write(ADDR, &[REG_FAN_SETTING, step])
                .map_err(Error::Bus);
        }
        let step = BoundedU8::new(step).ok_or(Error::OutOfRange)?;
        self.dev()?.set_fan_power(step).map_err(map)?;
        Ok(())
    }

    /// Fan speed from the tach input; 0 when the fan does not turn.
    pub fn fan_rpm(&mut self) -> Result<u16, Error<I::Error>> {
        if let State::Ctf(bus) = &mut self.state {
            // LSB first: it latches the MSB (as on the EMC2101).
            let lsb = Self::ctf_read(bus, REG_TACH_LSB)?;
            let msb = Self::ctf_read(bus, REG_TACH_MSB)?;
            let count = u16::from_be_bytes([msb, lsb]);
            return Ok(if count == 0 || count == 0xFFFF {
                0
            } else {
                (5_400_000 / u32::from(count)).min(u32::from(u16::MAX)) as u16
            });
        }
        self.dev()?.fan_rpm().map_err(map)
    }

    /// The diode temperature, °C, with the board offset applied. An open
    /// or shorted diode is a fault, not a value.
    pub fn external_temp_c(&mut self) -> Result<f32, Error<I::Error>> {
        let c = if let State::Ctf(bus) = &mut self.state {
            // MSB first: it latches the LSB. 11 bits, eighths of a degree.
            let msb = Self::ctf_read(bus, REG_EXT_TEMP_MSB)?;
            let lsb = Self::ctf_read(bus, REG_EXT_TEMP_LSB)?;
            let raw = i16::from_be_bytes([msb, lsb]) >> 5;
            f32::from(raw) / 8.0
        } else {
            self.dev()?.temp_external_precise().map_err(map)?
        };
        if c >= FAULT_FROM_C {
            return Err(Error::SensorFault);
        }
        Ok(c + f32::from(self.cfg.temp_offset_c))
    }

    /// The controller's own die temperature, °C.
    pub fn internal_temp_c(&mut self) -> Result<i8, Error<I::Error>> {
        if let State::Ctf(bus) = &mut self.state {
            return Ok(Self::ctf_read(bus, REG_INTERNAL_TEMP)? as i8);
        }
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
            get(0xFE, 0x5D), // not a CTF2301B: SMSC's manufacturer id
            get(0xFF, 0x02),
            get(0xFD, 0x16), // product id, before any write
            put(0x03, 0x04), // Configuration: ALT_TCH only
            put(0x4A, 0x23), // Fan Config: PROG, 360 kHz, TACH_M 11
            get(0xFD, 0x16), // product id (the crate's own check)
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
        // Nothing is written to a part that is not an EMC2101 (or a
        // CTF2301B).
        let script = vec![get(0xFE, 0x00), get(0xFF, 0x00), get(0xFD, 0x00)];
        let mut s = Emc2101::new(Bus::new(script), Config::BM1370_DIODE);
        assert_eq!(s.init(), Err(Error::WrongDevice));
        assert_eq!(s.fan_rpm(), Err(Error::WrongDevice));
    }

    /// A CTF2301B: told apart by 0xFE/0xFF, its fan output switched on
    /// as the StarMiner's firmware does it, its own diode registers (NC
    /// 0x08 for the BM1370's 0x24), then fan, tach and temperature on the
    /// shared registers.
    #[test]
    fn a_ctf2301b_gets_its_fan_output_switched_on() {
        let script = vec![
            get(0xFE, 0x41), // CTF2301B chip id
            get(0xFF, 0x59), // and manufacturer id: nothing else read
            put(0x03, 0x04),
            put(0x4A, 0x23),
            get(0x03, 0x04),
            put(0x03, 0x06),
            put(0x19, 0x7F),
            put(0x03, 0x04),
            put(0x21, 0x02),
            put(0x34, 0x08),
            put(0x30, 0x00),
            put(0x4C, 63),
            get(0x46, 0xD0),
            get(0x47, 0x07), // 2000 counts
            get(0x01, 0x3A),
            get(0x10, 0x20), // 58.125 °C
            get(0x46, 0xFF),
            get(0x47, 0xFF), // stalled
        ];
        let mut s = Emc2101::new(Bus::new(script), Config::BM1370_DIODE);
        s.init().unwrap();
        assert!(s.is_ctf2301b());
        s.set_fan_percent(100).unwrap();
        assert_eq!(s.fan_rpm().unwrap(), 2700);
        assert!((s.external_temp_c().unwrap() - 58.125).abs() < 1e-6);
        assert_eq!(s.fan_rpm().unwrap(), 0);
    }

    /// Neither an EMC2101 nor a CTF2301B: nothing written.
    #[test]
    fn another_part_is_still_refused() {
        let script = vec![get(0xFE, 0x5D), get(0xFF, 0x01), get(0xFD, 0x00)];
        let mut s = Emc2101::new(Bus::new(script), Config::BM1370_DIODE);
        assert_eq!(s.init(), Err(Error::WrongDevice));
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

    /// Fan Config is read on the first fan write only; after that each
    /// percentage is one write of the fan setting.
    #[test]
    fn fan_percent_maps_to_63_steps() {
        let mut s = ready(
            vec![
                get(0x4A, 0x23), // once: PROG set, Look-Up Table off
                put(0x4C, 63),
                put(0x4C, 31),
                put(0x4C, 0),
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
