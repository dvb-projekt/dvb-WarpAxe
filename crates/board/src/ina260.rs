//! INA260: the input-side power monitor of the BitForge Nano (12 V
//! rail, before the regulator). Sixteen-bit registers, MSB first:
//! bus voltage in 1.25 mV, current in 1.25 mA (signed), power in 10 mW,
//! as ESP-Miner's `INA260.c` reads them. The part measures on its own
//! in continuous mode after power-up; nothing is configured here.

use crate::i2c::{Error, I2c};

/// I²C address with A0/A1 to ground (the Nano).
pub const ADDR: u8 = 0x40;
const REG_CURRENT: u8 = 0x01;
const REG_BUS_VOLTAGE: u8 = 0x02;
const REG_POWER: u8 = 0x03;
const REG_MANUFACTURER_ID: u8 = 0xFE;
const REG_DIE_ID: u8 = 0xFF;
/// "TI".
const MANUFACTURER_ID: u16 = 0x5449;
/// Die id in bits 15..4 of the DIE_ID register (the low nibble is the
/// revision).
const DIE_ID: u16 = 0x227;

/// The driver, owning the bus.
pub struct Ina260<I> {
    bus: I,
    addr: u8,
}

impl<I: I2c> Ina260<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I, addr: u8) -> Self {
        Ina260 { bus, addr }
    }

    /// Give the bus back.
    pub fn release(self) -> I {
        self.bus
    }

    fn read_word(&mut self, reg: u8) -> Result<u16, Error<I::Error>> {
        let mut b = [0u8; 2];
        self.bus.write_read(self.addr, &[reg], &mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    /// Check the manufacturer and die ids.
    pub fn init(&mut self) -> Result<(), Error<I::Error>> {
        if self.read_word(REG_MANUFACTURER_ID)? != MANUFACTURER_ID {
            return Err(Error::WrongDevice);
        }
        if self.read_word(REG_DIE_ID)? >> 4 != DIE_ID {
            return Err(Error::WrongDevice);
        }
        Ok(())
    }

    /// Bus voltage, mV.
    pub fn bus_voltage_mv(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(f32::from(self.read_word(REG_BUS_VOLTAGE)?) * 1.25)
    }

    /// Current, mA (negative when flowing backwards).
    pub fn current_ma(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(f32::from(self.read_word(REG_CURRENT)? as i16) * 1.25)
    }

    /// Power, mW.
    pub fn power_mw(&mut self) -> Result<f32, Error<I::Error>> {
        Ok(f32::from(self.read_word(REG_POWER)?) * 10.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    #[test]
    fn init_checks_both_ids_and_readings_scale_like_esp_miner() {
        let bus = Bus::new(vec![
            Tx::WriteRead(ADDR, vec![REG_MANUFACTURER_ID], vec![0x54, 0x49]),
            Tx::WriteRead(ADDR, vec![REG_DIE_ID], vec![0x22, 0x70]),
            // 11 982.5 mV = 9586 · 1.25; 3928.75 mA = 3143 · 1.25; 46.93 W = 4693 · 10 mW.
            Tx::WriteRead(ADDR, vec![REG_BUS_VOLTAGE], vec![0x25, 0x72]),
            Tx::WriteRead(ADDR, vec![REG_CURRENT], vec![0x0C, 0x47]),
            Tx::WriteRead(ADDR, vec![REG_POWER], vec![0x12, 0x55]),
        ]);
        let mut ina = Ina260::new(bus, ADDR);
        ina.init().unwrap();
        assert_eq!(ina.bus_voltage_mv().unwrap(), 11_982.5);
        assert_eq!(ina.current_ma().unwrap(), 3_928.75);
        assert_eq!(ina.power_mw().unwrap(), 46_930.0);
    }

    #[test]
    fn a_different_part_is_refused_before_any_reading() {
        let bus = Bus::new(vec![Tx::WriteRead(
            ADDR,
            vec![REG_MANUFACTURER_ID],
            vec![0x00, 0x00],
        )]);
        let mut ina = Ina260::new(bus, ADDR);
        assert_eq!(ina.init(), Err(Error::WrongDevice));
        assert_eq!(ina.release().log.len(), 1);
        // Negative current reads as such.
        let bus = Bus::new(vec![Tx::WriteRead(
            ADDR,
            vec![REG_CURRENT],
            vec![0xFF, 0xFC],
        )]);
        let mut ina = Ina260::new(bus, ADDR);
        assert_eq!(ina.current_ma().unwrap(), -5.0);
    }
}
