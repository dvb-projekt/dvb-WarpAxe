//! TMP1075: 12-bit I²C temperature sensor, 0x48 + n. On the NerdQAxe
//! family several sit on the board next to the ASICs and one on the back
//! at the voltage regulator. Layout and detection follow NerdOS
//! (`NerdQaxePlus::detectNumTempSensors`, `TMP1075.cpp`).

use crate::i2c::{Error, I2c};

/// Address of sensor `n` is `BASE_ADDR + n` (A0..A2 strapping).
pub const BASE_ADDR: u8 = 0x48;
/// The sensor on the back, at the voltage regulator (NerdOS
/// `VR_TEMP1075_ADDR` = 1).
pub const VR_ADDR: u8 = BASE_ADDR + 1;
/// The ASIC-side sensors in the order NerdOS probes them; detection
/// stops at the first one that does not answer.
pub const ASIC_ADDRS: [u8; 3] = [BASE_ADDR, BASE_ADDR + 2, BASE_ADDR + 3];

const REG_TEMP: u8 = 0x00;
/// Datasheet operating range; anything outside is a bad reading.
const MIN_C: f32 = -55.0;
const MAX_C: f32 = 125.0;

/// Read one sensor in °C. The temperature register is a left-aligned
/// 12-bit two's-complement value, 0.0625 °C per LSB (NerdOS treats it
/// as unsigned; signed is the datasheet and costs nothing).
pub fn read_celsius<I: I2c>(bus: &mut I, addr: u8) -> Result<f32, Error<I::Error>> {
    let mut b = [0u8; 2];
    bus.write_read(addr, &[REG_TEMP], &mut b)?;
    let raw = i16::from_be_bytes(b) >> 4;
    let c = f32::from(raw) * 0.0625;
    if !(MIN_C..=MAX_C).contains(&c) {
        return Err(Error::SensorFault);
    }
    Ok(c)
}

/// Which ASIC-side sensors answer, in probe order; stops at the first
/// gap like NerdOS. At most three.
pub fn detect<I: I2c>(bus: &mut I) -> Addrs {
    let mut found = Addrs::default();
    for &a in &ASIC_ADDRS {
        if read_celsius(bus, a).is_err() {
            break;
        }
        found.push(a);
    }
    found
}

/// The hottest of the detected ASIC-side sensors — what NerdOS feeds the
/// fan PID and the overheat check. Sensors that fail this round are
/// skipped; `None` when none answered.
pub fn max_celsius<I: I2c>(bus: &mut I, addrs: &[u8]) -> Option<f32> {
    addrs
        .iter()
        .filter_map(|&a| read_celsius(bus, a).ok())
        .fold(None, |m, c| Some(m.map_or(c, |m: f32| m.max(c))))
}

/// Up to three sensor addresses: a fixed list, so the crate stays
/// `no_std` without an allocator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Addrs {
    a: [u8; 3],
    n: usize,
}

impl Addrs {
    fn push(&mut self, addr: u8) {
        if self.n < self.a.len() {
            self.a[self.n] = addr;
            self.n += 1;
        }
    }
    /// The addresses found.
    pub fn as_slice(&self) -> &[u8] {
        &self.a[..self.n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn temp(addr: u8, raw: u16) -> Tx {
        Tx::WriteRead(addr, vec![REG_TEMP], raw.to_be_bytes().to_vec())
    }

    #[test]
    fn converts_left_aligned_twelve_bit_values() {
        // 0x3200 = 50.0 °C, 0x4B80 = 75.5 °C, 0xFF00 = -1.0 °C (datasheet table).
        let mut bus = Bus::new(vec![
            temp(0x48, 0x3200),
            temp(0x48, 0x4B80),
            temp(0x48, 0xFF00),
        ]);
        assert_eq!(read_celsius(&mut bus, 0x48), Ok(50.0));
        assert_eq!(read_celsius(&mut bus, 0x48), Ok(75.5));
        assert_eq!(read_celsius(&mut bus, 0x48), Ok(-1.0));
    }

    #[test]
    fn a_reading_above_the_rated_range_is_a_fault() {
        // 0x7FF0 = 127.94 °C: NerdOS's "invalid" threshold, above 125.
        let mut bus = Bus::new(vec![temp(0x48, 0x7FF0)]);
        assert_eq!(read_celsius(&mut bus, 0x48), Err(Error::SensorFault));
    }

    #[test]
    fn detection_skips_the_vr_sensor_and_stops_at_the_first_gap() {
        let mut bus = Bus::new(vec![
            temp(0x48, 0x3200),
            temp(0x4A, 0x3400),
            Tx::Nack(0x4B, vec![REG_TEMP]),
        ]);
        assert_eq!(detect(&mut bus).as_slice(), &[0x48, 0x4A]);
        assert!(bus
            .log
            .iter()
            .all(|t| !matches!(t, Tx::WriteRead(0x49, ..))));
    }

    #[test]
    fn nothing_answers_nothing_found() {
        let mut bus = Bus::new(vec![Tx::Nack(0x48, vec![REG_TEMP])]);
        assert!(detect(&mut bus).as_slice().is_empty());
    }

    #[test]
    fn max_takes_the_hottest_and_skips_a_failed_read() {
        let mut bus = Bus::new(vec![
            temp(0x48, 0x3200),
            Tx::Nack(0x4A, vec![REG_TEMP]),
            temp(0x4B, 0x3C00),
        ]);
        assert_eq!(max_celsius(&mut bus, &[0x48, 0x4A, 0x4B]), Some(60.0));
        let mut bus = Bus::new(vec![Tx::Nack(0x48, vec![REG_TEMP])]);
        assert_eq!(max_celsius(&mut bus, &[0x48]), None);
    }
}
