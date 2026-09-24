//! EMC2101: fan controller with one external temperature diode, I²C
//! 0x4C. On the Bitaxe the diode is the BM1370's, so "external" is the
//! chip temperature. Register use follows ESP-Miner's EMC2101.c.

use crate::i2c::{Error, I2c};

/// I²C address of the EMC2101.
pub const ADDR: u8 = 0x4C;
const REG_INTERNAL_TEMP: u8 = 0x00;
const REG_EXTERNAL_TEMP_MSB: u8 = 0x01;
const REG_CONFIG: u8 = 0x03;
const REG_EXTERNAL_TEMP_LSB: u8 = 0x10;
const REG_IDEALITY: u8 = 0x17;
const REG_BETA: u8 = 0x18;
const REG_TACH_LSB: u8 = 0x46;
const REG_TACH_MSB: u8 = 0x47;
const REG_FAN_CONFIG: u8 = 0x4A;
const REG_FAN_SETTING: u8 = 0x4C;
const REG_PART_ID: u8 = 0xFD;
const PART_ID: u8 = 0x16;
const PART_ID_ALT: u8 = 0x28;
/// Tach counts to RPM (datasheet, 2 pulses per revolution).
const TACH_NUMERATOR: u32 = 5_400_000;
const TEMP_FAULT_OPEN: i16 = 0x3F8;
const TEMP_FAULT_SHORT: i16 = 0x3FF;

/// Diode tuning per board: the BM1370 diode wants ideality 0x24 and
/// no beta compensation (ESP-Miner board table).
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
    /// The BM1370's diode as ESP-Miner tunes it: ideality 0x24, beta off.
    pub const BM1370_DIODE: Config = Config {
        ideality: Some(0x24),
        beta: Some(0x00),
        temp_offset_c: 0,
    };
}

/// The driver, owning the bus.
pub struct Emc2101<I> {
    bus: I,
    cfg: Config,
}

impl<I: I2c> Emc2101<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, cfg: Config) -> Self {
        Emc2101 { bus, cfg }
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

    /// Check the part id, enable the TACH input and the fan driver in
    /// direct PWM mode (~22.5 kHz, 2 pulses/rev), apply the diode tuning.
    pub fn init(&mut self) -> Result<(), Error<I::Error>> {
        let id = self.read(REG_PART_ID)?;
        if id != PART_ID && id != PART_ID_ALT {
            return Err(Error::WrongDevice);
        }
        self.write(REG_CONFIG, 0x04)?;
        self.write(REG_FAN_CONFIG, 0b0010_0011)?;
        if let Some(i) = self.cfg.ideality {
            self.write(REG_IDEALITY, i)?;
        }
        if let Some(b) = self.cfg.beta {
            self.write(REG_BETA, b)?;
        }
        Ok(())
    }

    /// Fan duty, 0–100 %; the register holds 0–63.
    pub fn set_fan_percent(&mut self, percent: u8) -> Result<(), Error<I::Error>> {
        let duty = (u16::from(percent.min(100)) * 63 / 100) as u8;
        self.write(REG_FAN_SETTING, duty)
    }

    /// Fan speed from the tach input; 0 when the fan does not turn
    /// (the counter saturates at 0xFFFF, which would read as 82 RPM).
    pub fn fan_rpm(&mut self) -> Result<u16, Error<I::Error>> {
        let lsb = self.read(REG_TACH_LSB)?;
        let msb = self.read(REG_TACH_MSB)?;
        let count = u32::from(u16::from_le_bytes([lsb, msb]));
        if count == 0 || count == 0xFFFF {
            return Ok(0);
        }
        Ok((TACH_NUMERATOR / count).min(u32::from(u16::MAX)) as u16)
    }

    /// The diode temperature, °C, with the board offset applied.
    /// An open or shorted diode is a fault, not a value.
    pub fn external_temp_c(&mut self) -> Result<f32, Error<I::Error>> {
        let msb = self.read(REG_EXTERNAL_TEMP_MSB)?;
        let lsb = self.read(REG_EXTERNAL_TEMP_LSB)?;
        let raw = (u16::from_be_bytes([msb, lsb]) >> 5) as i16;
        if raw == TEMP_FAULT_OPEN || raw == TEMP_FAULT_SHORT {
            return Err(Error::SensorFault);
        }
        // 11-bit two's complement, 0.125 °C per LSB.
        let signed = if raw & 0x0400 != 0 {
            raw | !0x07FF
        } else {
            raw
        };
        Ok(f32::from(signed) / 8.0 + f32::from(self.cfg.temp_offset_c))
    }

    /// The controller's own die temperature, °C.
    pub fn internal_temp_c(&mut self) -> Result<i8, Error<I::Error>> {
        Ok(self.read(REG_INTERNAL_TEMP)? as i8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    #[test]
    fn init_checks_the_part_and_configures_fan_and_diode() {
        let bus = Bus::new(vec![Tx::WriteRead(ADDR, vec![REG_PART_ID], vec![0x16])]);
        let mut s = Emc2101::new(bus, Config::BM1370_DIODE);
        s.init().unwrap();
        let log = s.release().log;
        assert_eq!(
            &log[1..],
            &[
                Tx::Write(ADDR, vec![REG_CONFIG, 0x04]),
                Tx::Write(ADDR, vec![REG_FAN_CONFIG, 0x23]),
                Tx::Write(ADDR, vec![REG_IDEALITY, 0x24]),
                Tx::Write(ADDR, vec![REG_BETA, 0x00]),
            ]
        );
    }

    #[test]
    fn wrong_part_id_is_an_error() {
        let bus = Bus::new(vec![Tx::WriteRead(ADDR, vec![REG_PART_ID], vec![0x00])]);
        assert_eq!(
            Emc2101::new(bus, Config::BM1370_DIODE).init(),
            Err(Error::WrongDevice)
        );
    }

    #[test]
    fn fan_percent_maps_to_63_steps() {
        let mut s = Emc2101::new(Bus::sink(), Config::BM1370_DIODE);
        s.set_fan_percent(100).unwrap();
        s.set_fan_percent(50).unwrap();
        s.set_fan_percent(0).unwrap();
        s.set_fan_percent(200).unwrap();
        let log = s.release().log;
        let duties: Vec<u8> = log
            .iter()
            .map(|t| match t {
                Tx::Write(_, b) => b[1],
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(duties, vec![63, 31, 0, 63]);
    }

    #[test]
    fn tach_converts_to_rpm_and_a_stalled_fan_reads_zero() {
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![REG_TACH_LSB], vec![0xD0]),
            Tx::WriteRead(ADDR, vec![REG_TACH_MSB], vec![0x07]), // 2000 counts
            Tx::WriteRead(ADDR, vec![REG_TACH_LSB], vec![0xFF]),
            Tx::WriteRead(ADDR, vec![REG_TACH_MSB], vec![0xFF]),
        ]);
        let mut s = Emc2101::new(bus, Config::BM1370_DIODE);
        assert_eq!(s.fan_rpm().unwrap(), 2700);
        assert_eq!(s.fan_rpm().unwrap(), 0);
    }

    #[test]
    fn external_temp_decodes_eighths_and_flags_faults() {
        // 58.125 °C = 465 eighths = 0b001_1101_0001 → MSB 0x3A, LSB 0x20
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![REG_EXTERNAL_TEMP_MSB], vec![0x3A]),
            Tx::WriteRead(ADDR, vec![REG_EXTERNAL_TEMP_LSB], vec![0x20]),
            // -2.0 °C = -16 eighths → 11-bit 0x7F0 → MSB 0xFE, LSB 0x00
            Tx::WriteRead(ADDR, vec![REG_EXTERNAL_TEMP_MSB], vec![0xFE]),
            Tx::WriteRead(ADDR, vec![REG_EXTERNAL_TEMP_LSB], vec![0x00]),
            // open diode: 0x3F8 → MSB 0x7F, LSB 0x00
            Tx::WriteRead(ADDR, vec![REG_EXTERNAL_TEMP_MSB], vec![0x7F]),
            Tx::WriteRead(ADDR, vec![REG_EXTERNAL_TEMP_LSB], vec![0x00]),
        ]);
        let mut s = Emc2101::new(
            bus,
            Config {
                temp_offset_c: 1,
                ..Config::BM1370_DIODE
            },
        );
        assert!((s.external_temp_c().unwrap() - 59.125).abs() < 1e-6);
        assert!((s.external_temp_c().unwrap() - -1.0).abs() < 1e-6);
        assert_eq!(s.external_temp_c(), Err(Error::SensorFault));
    }
}
