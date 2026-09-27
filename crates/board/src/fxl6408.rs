//! FXL6408: 8-bit I²C GPIO expander, 0x43 on the Nerd boards. On the
//! Q1370/Q1373 carriers it drives the ASIC reset, the regulator and LDO
//! enables, and reads a strap. Register use follows NerdOS
//! (`fxl6408.cpp`); bit meanings follow the datasheet, which NerdOS's
//! header comment gets backwards for the direction register (1 = output).

use crate::i2c::{Error, I2c};

/// I²C address on the Nerd boards (ADDR strapped high).
pub const ADDR: u8 = 0x43;

/// Pin use on the Q1370 / Q1373 carriers (NerdOS `q1370.cpp`, `q1373.cpp`).
pub mod q13xx {
    /// ASIC reset, active low.
    pub const ASIC_RESET: u8 = 0;
    /// Core regulator enable.
    pub const VREG_ENABLE: u8 = 1;
    /// LDO enable.
    pub const LDO_ENABLE: u8 = 2;
    /// Select lines of the TMP451 mux (chip temperatures, Q1370).
    pub const TMUX_SELECT: [u8; 2] = [3, 4];
    /// DIP switch, input with pull-up: pulled low = CAN slave board.
    pub const CAN_SLAVE: u8 = 5;
    /// Q1373: low = six-phase TPS53667, high = four-phase TPS53647.
    pub const PHASE_DETECT: u8 = 6;
    /// Q1373: high = Ethernet populated.
    pub const ETH_DETECT: u8 = 7;
    /// ESP32 GPIO that holds the expander itself in reset while high
    /// (through a BSS138). The firmware drives it low before the first
    /// access, or the expander never answers.
    pub const EXPANDER_RESET_GPIO: u8 = 3;
}

const REG_ID_CTRL: u8 = 0x01;
const REG_DIRECTION: u8 = 0x03;
const REG_OUTPUT_STATE: u8 = 0x05;
const REG_OUTPUT_HIGH_Z: u8 = 0x07;
const REG_PULL_ENABLE: u8 = 0x0B;
const REG_PULL_SELECT: u8 = 0x0D;
const REG_INPUT_STATUS: u8 = 0x0F;
/// Bits 7..5 of the ID register: the manufacturer, 0b101.
const MFR_MASK: u8 = 0xE0;
const MFR_ID: u8 = 0xA0;
const SW_RESET: u8 = 0x01;

/// The driver. It shadows direction, output and pull registers, as
/// NerdOS does, so a pin change is one register write.
pub struct Fxl6408<I> {
    bus: I,
    direction: u8,
    output: u8,
    pull_enable: u8,
    pull_select: u8,
}

impl<I: I2c> Fxl6408<I> {
    /// Wrap the bus; nothing is sent until [`init`](Self::init).
    pub fn new(bus: I) -> Self {
        Fxl6408 {
            bus,
            direction: 0,
            output: 0,
            pull_enable: 0,
            pull_select: 0,
        }
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

    /// Check the manufacturer bits, software reset, and take the outputs
    /// out of high-Z (after reset every pin is an input, so nothing is
    /// driven until [`set_output`](Self::set_output)). Returns the ID
    /// register. The caller waits ~100 ms after this, as NerdOS does
    /// after each of the two writes; the crate has no clock.
    pub fn init(&mut self) -> Result<u8, Error<I::Error>> {
        let id = self.read(REG_ID_CTRL)?;
        if id & MFR_MASK != MFR_ID {
            return Err(Error::WrongDevice);
        }
        self.write(REG_ID_CTRL, SW_RESET)?;
        self.direction = 0;
        self.output = 0;
        self.pull_enable = 0;
        self.pull_select = 0;
        self.write(REG_OUTPUT_HIGH_Z, 0x00)?;
        Ok(id)
    }

    fn check(pin: u8) -> Result<u8, Error<I::Error>> {
        if pin > 7 {
            return Err(Error::OutOfRange);
        }
        Ok(1 << pin)
    }

    /// Make `pin` an output driving `level`. The level is written before
    /// the direction, so the pin never drives the old shadow value
    /// (NerdOS switches direction first; with outputs reset to 0 and
    /// every first write a 0, that order is harmless there too).
    pub fn set_output(&mut self, pin: u8, level: bool) -> Result<(), Error<I::Error>> {
        let m = Self::check(pin)?;
        self.write_level(m, level)?;
        if self.direction & m == 0 {
            self.direction |= m;
            let d = self.direction;
            self.write(REG_DIRECTION, d)?;
        }
        Ok(())
    }

    /// Change the level of a pin already set up with [`set_output`](Self::set_output).
    pub fn write_pin(&mut self, pin: u8, level: bool) -> Result<(), Error<I::Error>> {
        let m = Self::check(pin)?;
        self.write_level(m, level)
    }

    fn write_level(&mut self, m: u8, level: bool) -> Result<(), Error<I::Error>> {
        if level {
            self.output |= m;
        } else {
            self.output &= !m;
        }
        let o = self.output;
        self.write(REG_OUTPUT_STATE, o)
    }

    /// Make `pin` an input with its pull-up on (NerdOS: the CAN-slave
    /// strap on the Q13xx carriers).
    pub fn set_input_pull_up(&mut self, pin: u8) -> Result<(), Error<I::Error>> {
        let m = Self::check(pin)?;
        if self.direction & m != 0 {
            self.direction &= !m;
            let d = self.direction;
            self.write(REG_DIRECTION, d)?;
        }
        self.pull_select |= m;
        let s = self.pull_select;
        self.write(REG_PULL_SELECT, s)?;
        self.pull_enable |= m;
        let e = self.pull_enable;
        self.write(REG_PULL_ENABLE, e)
    }

    /// The level on `pin`, from the input status register.
    pub fn read_pin(&mut self, pin: u8) -> Result<bool, Error<I::Error>> {
        let m = Self::check(pin)?;
        Ok(self.read(REG_INPUT_STATUS)? & m != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    fn w(reg: u8, v: u8) -> Tx {
        Tx::Write(ADDR, vec![reg, v])
    }

    fn ready(extra: Vec<Tx>) -> Fxl6408<Bus> {
        let mut script = vec![
            Tx::WriteRead(ADDR, vec![REG_ID_CTRL], vec![0xA2]),
            w(REG_ID_CTRL, SW_RESET),
            w(REG_OUTPUT_HIGH_Z, 0x00),
        ];
        script.extend(extra);
        let mut x = Fxl6408::new(Bus::new(script));
        assert_eq!(x.init(), Ok(0xA2));
        x
    }

    #[test]
    fn init_checks_the_manufacturer_resets_and_enables_the_outputs() {
        let x = ready(vec![]);
        assert!(x.release().script.is_empty());
    }

    #[test]
    fn another_part_at_the_address_is_refused_untouched() {
        let mut x = Fxl6408::new(Bus::new(vec![Tx::WriteRead(
            ADDR,
            vec![REG_ID_CTRL],
            vec![0x42],
        )]));
        assert_eq!(x.init(), Err(Error::WrongDevice));
        assert_eq!(x.release().log.len(), 1, "no write after a wrong id");
    }

    #[test]
    fn q1370_init_sequence_as_nerdos_leaves_everything_off() {
        // NerdOS Q1370B::init: regulator, LDO, ASIC reset as outputs, all
        // low; CAN-slave strap as input with pull-up. Level before
        // direction, each bit shadowed.
        use q13xx::*;
        let mut x = ready(vec![
            w(REG_OUTPUT_STATE, 0x00),
            w(REG_DIRECTION, 0x02),
            w(REG_OUTPUT_STATE, 0x00),
            w(REG_DIRECTION, 0x06),
            w(REG_OUTPUT_STATE, 0x00),
            w(REG_DIRECTION, 0x07),
            w(REG_PULL_SELECT, 0x20),
            w(REG_PULL_ENABLE, 0x20),
            // later: LDO on, regulator on, reset released
            w(REG_OUTPUT_STATE, 0x04),
            w(REG_OUTPUT_STATE, 0x06),
            w(REG_OUTPUT_STATE, 0x07),
        ]);
        x.set_output(VREG_ENABLE, false).unwrap();
        x.set_output(LDO_ENABLE, false).unwrap();
        x.set_output(ASIC_RESET, false).unwrap();
        x.set_input_pull_up(CAN_SLAVE).unwrap();
        x.write_pin(LDO_ENABLE, true).unwrap();
        x.write_pin(VREG_ENABLE, true).unwrap();
        x.write_pin(ASIC_RESET, true).unwrap();
        assert!(x.release().script.is_empty());
    }

    #[test]
    fn pull_up_input_and_read_back() {
        let mut x = ready(vec![
            w(REG_PULL_SELECT, 0x08),
            w(REG_PULL_ENABLE, 0x08),
            Tx::WriteRead(ADDR, vec![REG_INPUT_STATUS], vec![0x08]),
            Tx::WriteRead(ADDR, vec![REG_INPUT_STATUS], vec![0xF7]),
        ]);
        x.set_input_pull_up(3).unwrap();
        assert_eq!(x.read_pin(3), Ok(true));
        assert_eq!(x.read_pin(3), Ok(false));
    }

    #[test]
    fn pin_eight_does_not_exist() {
        let mut x = ready(vec![]);
        assert_eq!(x.set_output(8, true), Err(Error::OutOfRange));
        assert_eq!(x.read_pin(9), Err(Error::OutOfRange));
    }
}
