//! PCA9544A four-channel I²C multiplexer. The BitForge Nano puts its two
//! EMC2101 (fixed address 0x4C) behind one on channels 2 and 3, one per
//! chip. A single control byte at the mux address selects a channel:
//! bit 2 enables the output, bits 1..0 pick it; reading the byte back
//! gives the selection plus the four interrupt inputs in bits 7..4.

use crate::i2c::I2c;

/// I²C address with A0–A2 to ground (the Nano).
pub const ADDR: u8 = 0x70;
/// Control byte: outputs enabled.
const ENABLE: u8 = 0x04;

/// A view of `bus` through one channel of the mux: every transaction
/// first selects the channel, then goes through. Two views of the same
/// bus may be held at once; each selects before it talks.
pub struct Muxed<I> {
    bus: I,
    mux: u8,
    channel: u8,
}

impl<I: I2c> Muxed<I> {
    /// Channel `channel` (0–3) of the mux at `mux` on `bus`.
    pub fn new(bus: I, mux: u8, channel: u8) -> Self {
        Muxed {
            bus,
            mux,
            channel: channel & 0x03,
        }
    }

    /// Give the bus back.
    pub fn release(self) -> I {
        self.bus
    }

    fn select(&mut self) -> Result<(), I::Error> {
        self.bus.write(self.mux, &[ENABLE | self.channel])
    }
}

impl<I: I2c> I2c for Muxed<I> {
    type Error = I::Error;
    fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), I::Error> {
        self.select()?;
        self.bus.write(addr, bytes)
    }
    fn write_read(&mut self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), I::Error> {
        self.select()?;
        self.bus.write_read(addr, wr, rd)
    }
}

/// The control byte as the mux reports it (a NACK means no mux).
pub fn probe<I: I2c>(bus: &mut I, mux: u8) -> Result<u8, I::Error> {
    let mut b = [0u8; 1];
    bus.read(mux, &mut b)?;
    Ok(b[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i2c::mock::{Bus, Tx};

    #[test]
    fn every_transaction_selects_its_channel_first() {
        let bus = Bus::new(vec![
            Tx::Write(ADDR, vec![0x06]),
            Tx::WriteRead(0x4C, vec![0xFD], vec![0x16]),
            Tx::Write(ADDR, vec![0x07]),
            Tx::Write(0x4C, vec![0x4C, 0x3F]),
        ]);
        let mut ch2 = Muxed::new(bus, ADDR, 2);
        let mut id = [0u8; 1];
        ch2.write_read(0x4C, &[0xFD], &mut id).unwrap();
        assert_eq!(id, [0x16]);
        let bus = ch2.release();
        let mut ch3 = Muxed::new(bus, ADDR, 3);
        ch3.write(0x4C, &[0x4C, 0x3F]).unwrap();
        let bus = ch3.release();
        assert!(bus.script.is_empty(), "sequence complete");
        assert_eq!(bus.log.len(), 4);
    }

    #[test]
    fn channel_is_masked_to_two_bits_and_probe_reads_the_control_byte() {
        let bus = Bus::new(vec![Tx::Write(ADDR, vec![0x05])]);
        let mut m = Muxed::new(bus, ADDR, 5);
        m.write(0x4C, &[0x00]).unwrap();
        let mut bus = Bus::new(vec![Tx::WriteRead(ADDR, vec![], vec![0x06])]);
        assert_eq!(probe(&mut bus, ADDR), Ok(0x06));
        let mut absent = Bus::new(vec![Tx::Nack(ADDR, vec![])]);
        assert!(probe(&mut absent, ADDR).is_err());
    }
}
