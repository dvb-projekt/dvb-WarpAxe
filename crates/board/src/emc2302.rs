//! EMC2302: two-channel PWM fan controller, I²C 0x2E, on the Nerd
//! multi-chip boards. Manual drive (no RPM loop in the chip): the
//! firmware's fan PID sets the duty. Register use follows NerdOS
//! (`EMC2302.cpp`).
//!
//! NerdOS's channel numbers are crossed with the chip's: its channel 0
//! is the chip's FAN2 (the fan nearest the ASIC cooler, the one it
//! reports), channel 1 is FAN1. This driver names the chip's outputs
//! ([`Fan::One`], [`Fan::Two`]); [`Fan::from_nerdos`] maps the other way.

use crate::i2c::{Error, I2c};

/// I²C address.
pub const ADDR: u8 = 0x2E;

const REG_POLARITY: u8 = 0x2A;
const REG_OUTPUT_CONFIG: u8 = 0x2B;
const REG_BASE_F123: u8 = 0x2D;
const REG_PRODUCT_ID: u8 = 0xFD;
const REG_MANUFACTURER_ID: u8 = 0xFE;

const OFS_FAN_SETTING: u8 = 0x00;
const OFS_FAN_CONFIG1: u8 = 0x02;
const OFS_TACH_MSB: u8 = 0x0E;
const OFS_TACH_LSB: u8 = 0x0F;

/// Both outputs push-pull.
const OUTPUT_PUSH_PULL: u8 = 0x03;
/// PWM base frequency 19.53 kHz on outputs 1 and 2 (0b001 each).
const BASE_19K53: u8 = 0x01 | (0x01 << 3);
/// Manual drive, RANGE 500 RPM (multiplier 1), EDGES = 5 (two-pole fan).
const CONFIG1_MANUAL_5_EDGES: u8 = 0b01 << 3;

/// Tach clock, Hz, and the count that means "no pulses".
const F_TACH: u32 = 32_768;
const TACH_NO_FAN: u16 = 0x1FFF;
/// RPM = 60 · f_tach · m · (edges − 1) / poles / count, with m = 1,
/// five edges, two poles.
const RPM_NUMERATOR: u32 = 60 * F_TACH * (5 - 1) / 2;

/// One of the two fan outputs, by the chip's own numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fan {
    /// FAN1, registers at 0x30.
    One,
    /// FAN2, registers at 0x40.
    Two,
}

impl Fan {
    fn base(self) -> u8 {
        match self {
            Fan::One => 0x30,
            Fan::Two => 0x40,
        }
    }

    /// NerdOS channel number to output: 0 → FAN2, anything else → FAN1.
    pub fn from_nerdos(channel: u8) -> Fan {
        if channel == 0 {
            Fan::Two
        } else {
            Fan::One
        }
    }
}

/// The driver, owning the bus.
pub struct Emc2302<I> {
    bus: I,
}

impl<I: I2c> Emc2302<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I) -> Self {
        Emc2302 { bus }
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

    /// Product and manufacturer id, for the log. Not checked by
    /// [`init`](Self::init): NerdOS never reads them, and refusing to
    /// set up the fans on a surprising id would leave the chips without
    /// cooling.
    pub fn identify(&mut self) -> Result<(u8, u8), Error<I::Error>> {
        Ok((self.read(REG_PRODUCT_ID)?, self.read(REG_MANUFACTURER_ID)?))
    }

    /// NerdOS's init: polarity, push-pull outputs, 19.53 kHz PWM, both
    /// fans in manual drive with five-edge tach sampling.
    pub fn init(&mut self, invert_polarity: bool) -> Result<(), Error<I::Error>> {
        self.set_polarity(invert_polarity)?;
        self.write(REG_OUTPUT_CONFIG, OUTPUT_PUSH_PULL)?;
        self.write(REG_BASE_F123, BASE_19K53)?;
        self.write(Fan::One.base() + OFS_FAN_CONFIG1, CONFIG1_MANUAL_5_EDGES)?;
        self.write(Fan::Two.base() + OFS_FAN_CONFIG1, CONFIG1_MANUAL_5_EDGES)
    }

    /// Invert the PWM of both outputs (some fans read duty backwards).
    pub fn set_polarity(&mut self, invert: bool) -> Result<(), Error<I::Error>> {
        self.write(REG_POLARITY, if invert { 0x03 } else { 0x00 })
    }

    /// Drive `fan` at `percent` (0–100, clamped), as 0–255 duty rounded
    /// half up like NerdOS. Same signature as the EMC2101's.
    pub fn set_fan_percent(&mut self, fan: Fan, percent: u8) -> Result<(), Error<I::Error>> {
        let duty = ((u16::from(percent.min(100)) * 255 + 50) / 100) as u8;
        self.write(fan.base() + OFS_FAN_SETTING, duty)
    }

    /// Fan speed in RPM; 0 when no fan turns (the count saturates).
    pub fn rpm(&mut self, fan: Fan) -> Result<u16, Error<I::Error>> {
        let msb = self.read(fan.base() + OFS_TACH_MSB)?;
        let lsb = self.read(fan.base() + OFS_TACH_LSB)?;
        // 13-bit count: MSB then the top five bits of LSB.
        let count = (u16::from(msb) << 5) | (u16::from(lsb) >> 3);
        Ok(rpm_from_count(count))
    }
}

/// RPM from a 13-bit tach count. NerdOS divides by the count unguarded;
/// a zero count here reads as "no fan" like a saturated one.
fn rpm_from_count(count: u16) -> u16 {
    if count == 0 || count >= TACH_NO_FAN {
        return 0;
    }
    u16::try_from(RPM_NUMERATOR / u32::from(count)).unwrap_or(0)
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
    fn init_is_nerdos_register_for_register() {
        let mut e = Emc2302::new(Bus::new(vec![
            w(0x2A, 0x00),
            w(0x2B, 0x03),
            w(0x2D, 0x09),
            w(0x32, 0x08),
            w(0x42, 0x08),
        ]));
        e.init(false).unwrap();
        assert!(e.release().script.is_empty());
        let mut e = Emc2302::new(Bus::new(vec![w(0x2A, 0x03)]));
        e.init(true).unwrap();
    }

    #[test]
    fn nerdos_channel_zero_is_the_chips_fan_two() {
        assert_eq!(Fan::from_nerdos(0), Fan::Two);
        assert_eq!(Fan::from_nerdos(1), Fan::One);
    }

    #[test]
    fn percent_to_duty_rounds_and_clamps() {
        let mut e = Emc2302::new(Bus::new(vec![
            w(0x40, 0x00),
            w(0x40, 0x80), // 50 % → 128 (127.5 rounded up)
            w(0x40, 0x40), // 25 % → 64 (63.75)
            w(0x30, 0xFF),
            w(0x30, 0xFF), // 140 % clamps
        ]));
        e.set_fan_percent(Fan::Two, 0).unwrap();
        e.set_fan_percent(Fan::Two, 50).unwrap();
        e.set_fan_percent(Fan::Two, 25).unwrap();
        e.set_fan_percent(Fan::One, 100).unwrap();
        e.set_fan_percent(Fan::One, 140).unwrap();
        assert!(e.release().script.is_empty());
    }

    #[test]
    fn tach_count_to_rpm_like_nerdos_measured_it() {
        // NerdOS comment: an NF-A9x14 at 100 % reads 1487 → 2644 RPM.
        // 1487 = 0x5CF → MSB 0x2E, LSB 0x78 (0x0F << 3).
        let mut e = Emc2302::new(Bus::new(vec![r(0x4E, 0x2E), r(0x4F, 0x78)]));
        assert_eq!(e.rpm(Fan::Two), Ok(2644));
    }

    #[test]
    fn no_fan_reads_zero_not_a_division_by_zero() {
        assert_eq!(rpm_from_count(0x1FFF), 0);
        assert_eq!(rpm_from_count(0), 0);
        // Smallest count that still fits u16: 3_932_160 / 60 = 65 536 → 0.
        assert_eq!(rpm_from_count(60), 0);
        assert_eq!(rpm_from_count(61), 64_461);
    }

    #[test]
    fn identify_reads_product_and_manufacturer() {
        // The bytes are the mock's, not a claim about the part.
        let mut e = Emc2302::new(Bus::new(vec![r(0xFD, 0x36), r(0xFE, 0x5D)]));
        assert_eq!(e.identify(), Ok((0x36, 0x5D)));
    }
}
