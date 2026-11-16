//! EMC2302: two-channel PWM fan controller with tachometer inputs,
//! I²C address 0x2E. Written from Microchip's EMC2301/2/3/5 datasheet
//! (DS20006532A); section and register numbers below are that
//! document's.
//!
//! The driver runs both fans in Direct Setting mode (§4.1): the caller
//! sets the duty, the chip's closed-loop RPM algorithm stays off, and
//! the tachometer only measures.

use crate::i2c::{Error, I2c};

/// I²C address of the EMC2302 (`-1` variant).
pub const ADDR: u8 = 0x2E;
/// Product ID register value of the EMC2302 (table 6-1).
pub const PRODUCT_ID: u8 = 0x36;
/// Manufacturer ID register value, all variants (register 6-28).
pub const MANUFACTURER_ID: u8 = 0x5D;

// Register map, table 6-1.
const PWM_POLARITY: u8 = 0x2A;
const PWM_OUTPUT: u8 = 0x2B;
const PWM_BASE_F123: u8 = 0x2D;
const PRODUCT: u8 = 0xFD;
const MANUFACTURER: u8 = 0xFE;
// Per-fan registers: fan 1 from 0x30, fan 2 from 0x40.
const FAN_SETTING: u8 = 0x0;
const FAN_CONFIG: u8 = 0x2;
const TACH_HIGH: u8 = 0xE;
const TACH_LOW: u8 = 0xF;

/// The tach reading when no edges arrive: all 13 bits set (the POR
/// value of the reading registers, FFh/F8h).
const TACH_NONE: u16 = 0x1FFF;
/// Equation 4-3: RPM = 3 932 160 · m / COUNT for a two-pole fan with
/// five edges at f_TACH = 32.768 kHz.
const RPM_PER_COUNT: u32 = 3_932_160;

/// One fan output, by the chip's numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fan {
    /// FAN1 / PWM1 / TACH1.
    One,
    /// FAN2 / PWM2 / TACH2.
    Two,
}

impl Fan {
    fn reg(self, offset: u8) -> u8 {
        let base = match self {
            Fan::One => 0x30,
            Fan::Two => 0x40,
        };
        base + offset
    }

    /// The fan's bit in the polarity and output registers (bit 0 for
    /// PWM1, bit 1 for PWM2).
    fn bit(self) -> u8 {
        match self {
            Fan::One => 0b01,
            Fan::Two => 0b10,
        }
    }

    /// The fan's two-bit field in PWM BaseF123 (register 6-10): PMB1 in
    /// bits 1–0, PMB2 in bits 3–2.
    fn base_shift(self) -> u8 {
        match self {
            Fan::One => 0,
            Fan::Two => 2,
        }
    }
}

/// PWM base frequency (register 6-10). The PWM divide registers stay
/// at their reset value 1, so this is the output frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PwmBase {
    /// 26.00 kHz (reset value).
    Khz26,
    /// 19.53 kHz.
    Khz19_53,
    /// 4.882 kHz.
    Khz4_882,
    /// 2.441 kHz.
    Khz2_441,
}

impl PwmBase {
    fn code(self) -> u8 {
        match self {
            PwmBase::Khz26 => 0b00,
            PwmBase::Khz19_53 => 0b01,
            PwmBase::Khz4_882 => 0b10,
            PwmBase::Khz2_441 => 0b11,
        }
    }
}

/// Lowest speed the tachometer reports, with its count multiplier
/// (register 6-13, RNG).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// 500 RPM minimum, multiplier 1.
    Rpm500,
    /// 1000 RPM minimum, multiplier 2 (reset value).
    Rpm1000,
    /// 2000 RPM minimum, multiplier 4.
    Rpm2000,
    /// 4000 RPM minimum, multiplier 8.
    Rpm4000,
}

impl Range {
    fn code(self) -> u8 {
        match self {
            Range::Rpm500 => 0b00,
            Range::Rpm1000 => 0b01,
            Range::Rpm2000 => 0b10,
            Range::Rpm4000 => 0b11,
        }
    }

    fn multiplier(self) -> u32 {
        1 << self.code()
    }
}

/// How the driver sets the chip up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Invert both PWM outputs: duty 0 drives 100 % (register 6-7).
    pub invert_polarity: bool,
    /// Push-pull outputs instead of open drain (register 6-8).
    pub push_pull: bool,
    /// PWM frequency of both outputs.
    pub pwm_base: PwmBase,
    /// Tachometer range of both inputs. Five edges are sampled, the
    /// datasheet's setting for a two-pole fan.
    pub range: Range,
}

impl Config {
    /// Two-pole 4-wire fans driven push-pull at 19.53 kHz, measured down
    /// to 500 RPM, polarity as given.
    pub const fn two_pole(invert_polarity: bool) -> Config {
        Config {
            invert_polarity,
            push_pull: true,
            pwm_base: PwmBase::Khz19_53,
            range: Range::Rpm500,
        }
    }
}

/// The driver, owning the bus.
pub struct Emc2302<I> {
    bus: I,
    range: Range,
}

impl<I: I2c> Emc2302<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init). Until
    /// then RPM assumes the chip's reset range, which a warm restart of
    /// the host does not restore: call `init` before reading speeds.
    pub fn new(bus: I) -> Self {
        Emc2302 {
            bus,
            range: Range::Rpm1000,
        }
    }

    /// Give the bus back.
    pub fn release(self) -> I {
        self.bus
    }

    fn get(&mut self, reg: u8) -> Result<u8, Error<I::Error>> {
        let mut v = [0u8];
        self.bus.write_read(ADDR, &[reg], &mut v)?;
        Ok(v[0])
    }

    fn put(&mut self, reg: u8, v: u8) -> Result<(), Error<I::Error>> {
        self.bus.write(ADDR, &[reg, v]).map_err(Error::Bus)
    }

    /// Product and manufacturer ID. [`init`](Self::init) does not check
    /// them, so a caller that must have its fans running is not stopped
    /// by an unexpected ID; compare with [`PRODUCT_ID`] and
    /// [`MANUFACTURER_ID`] where that matters.
    pub fn identify(&mut self) -> Result<(u8, u8), Error<I::Error>> {
        Ok((self.get(PRODUCT)?, self.get(MANUFACTURER)?))
    }

    /// Set the chip up. Both fans' configuration is written first: Direct
    /// Setting mode (ENAG 0, also its reset value), the tach range and
    /// five edges. Then polarity and PWM frequency are set while the outputs are
    /// still open drain, and push-pull comes last.
    pub fn init(&mut self, cfg: &Config) -> Result<(), Error<I::Error>> {
        // Register 6-13: ENAG 0, RNG, EDG 01 (five edges), UDT 000.
        let fan_config = (cfg.range.code() << 5) | (0b01 << 3);
        for fan in [Fan::One, Fan::Two] {
            self.put(fan.reg(FAN_CONFIG), fan_config)?;
        }
        self.range = cfg.range;
        self.set_polarity(cfg.invert_polarity)?;
        let code = cfg.pwm_base.code();
        let base = (code << Fan::One.base_shift()) | (code << Fan::Two.base_shift());
        self.put(PWM_BASE_F123, base)?;
        let both = Fan::One.bit() | Fan::Two.bit();
        self.put(PWM_OUTPUT, if cfg.push_pull { both } else { 0 })
    }

    /// Invert both PWM outputs or set them back to normal (register 6-7),
    /// for fans that expect inverted PWM.
    pub fn set_polarity(&mut self, invert: bool) -> Result<(), Error<I::Error>> {
        let both = Fan::One.bit() | Fan::Two.bit();
        self.put(PWM_POLARITY, if invert { both } else { 0 })
    }

    /// Set the drive of `fan` directly, 0–255 (equation 4-1: duty =
    /// value / 255).
    pub fn set_drive(&mut self, fan: Fan, value: u8) -> Result<(), Error<I::Error>> {
        self.put(fan.reg(FAN_SETTING), value)
    }

    /// Drive `fan` at `percent` (clamped to 100), to the nearest of the
    /// 256 drive steps.
    pub fn set_fan_percent(&mut self, fan: Fan, percent: u8) -> Result<(), Error<I::Error>> {
        let p = u32::from(percent.min(100));
        let value = (p * 255 + 50) / 100;
        self.set_drive(fan, value as u8)
    }

    /// The 13-bit tach count of `fan`, high byte first. §6.1 describes
    /// the interlock (a high-byte read latches the low byte) for its
    /// temperature registers; it is assumed here for the tach registers.
    pub fn tach_count(&mut self, fan: Fan) -> Result<u16, Error<I::Error>> {
        let high = self.get(fan.reg(TACH_HIGH))?;
        let low = self.get(fan.reg(TACH_LOW))?;
        Ok((u16::from(high) << 5) | u16::from(low >> 3))
    }

    /// Speed of `fan` in RPM; 0 when it does not turn.
    pub fn rpm(&mut self, fan: Fan) -> Result<u16, Error<I::Error>> {
        let count = self.tach_count(fan)?;
        Ok(rpm(count, self.range))
    }
}

/// Equation 4-3 for a count in `range`. No edges (the all-ones count),
/// a zero count and a count too small for any fan (a result beyond
/// `u16`, a misread) all read as 0.
pub fn rpm(count: u16, range: Range) -> u16 {
    if count == 0 || count >= TACH_NONE {
        return 0;
    }
    let r = RPM_PER_COUNT * range.multiplier() / u32::from(count);
    u16::try_from(r).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn put(reg: u8, v: u8) -> Tx {
        Tx::Write(ADDR, vec![reg, v])
    }
    fn get(reg: u8, v: u8) -> Tx {
        Tx::WriteRead(ADDR, vec![reg], vec![v])
    }

    #[test]
    fn init_direct_mode_then_polarity_frequency_and_push_pull_last() {
        let mut e = Emc2302::new(Bus::new(vec![
            put(0x32, 0x08),
            put(0x42, 0x08),
            put(0x2A, 0x00),
            put(0x2D, 0x05), // PMB2 = PMB1 = 01: 19.53 kHz on both
            put(0x2B, 0x03),
        ]));
        e.init(&Config::two_pole(false)).unwrap();
        assert!(e.release().script.is_empty());
    }

    #[test]
    fn init_fields_follow_the_register_layout() {
        let cfg = Config {
            invert_polarity: true,
            push_pull: false,
            pwm_base: PwmBase::Khz2_441,
            range: Range::Rpm4000,
        };
        let mut e = Emc2302::new(Bus::new(vec![
            put(0x32, 0x68), // RNG 11, EDG 01
            put(0x42, 0x68),
            put(0x2A, 0x03),
            put(0x2D, 0x0F),
            put(0x2B, 0x00),
        ]));
        e.init(&cfg).unwrap();
        assert!(e.release().script.is_empty());
    }

    #[test]
    fn percent_maps_to_the_nearest_drive_step() {
        let mut e = Emc2302::new(Bus::new(vec![
            put(0x40, 0),
            put(0x40, 128), // 127.5 → 128
            put(0x40, 64),  // 63.75 → 64
            put(0x30, 255),
            put(0x30, 255), // clamped
        ]));
        for (fan, p) in [
            (Fan::Two, 0),
            (Fan::Two, 50),
            (Fan::Two, 25),
            (Fan::One, 100),
            (Fan::One, 200),
        ] {
            e.set_fan_percent(fan, p).unwrap();
        }
        assert!(e.release().script.is_empty());
    }

    #[test]
    fn tach_reads_high_then_low_and_joins_13_bits() {
        // Count 3932 = 0b1_1110_1011_100: high 0x7A, low 0b11100 << 3.
        let mut e = Emc2302::new(Bus::new(vec![
            put(0x32, 0x08),
            put(0x42, 0x08),
            put(0x2A, 0x00),
            put(0x2D, 0x05),
            put(0x2B, 0x03),
            get(0x4E, 0x7A),
            get(0x4F, 0xE0),
        ]));
        e.init(&Config::two_pole(false)).unwrap();
        assert_eq!(e.rpm(Fan::Two), Ok(1000));
    }

    #[test]
    fn rpm_follows_equation_4_3_with_the_range_multiplier() {
        assert_eq!(rpm(3932, Range::Rpm500), 1000);
        assert_eq!(rpm(3932, Range::Rpm1000), 2000);
        assert_eq!(rpm(1966, Range::Rpm4000), 16_000);
    }

    #[test]
    fn no_edges_zero_and_impossible_counts_read_as_stopped() {
        assert_eq!(rpm(TACH_NONE, Range::Rpm500), 0);
        assert_eq!(rpm(0, Range::Rpm500), 0);
        assert_eq!(rpm(1, Range::Rpm500), 0);
        assert_eq!(rpm(61, Range::Rpm500), 64_461);
    }

    #[test]
    fn polarity_sets_both_outputs() {
        let mut e = Emc2302::new(Bus::new(vec![put(0x2A, 0x03), put(0x2A, 0x00)]));
        e.set_polarity(true).unwrap();
        e.set_polarity(false).unwrap();
        assert!(e.release().script.is_empty());
    }

    #[test]
    fn identify_reads_product_then_manufacturer() {
        let mut e = Emc2302::new(Bus::new(vec![
            get(0xFD, PRODUCT_ID),
            get(0xFE, MANUFACTURER_ID),
        ]));
        assert_eq!(e.identify(), Ok((0x36, 0x5D)));
    }
}
