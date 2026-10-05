//! The bus the drivers need: a write, and a write followed by a read
//! without a stop in between (register access). Addresses are 7-bit.

/// A 7-bit-addressed I²C master.
pub trait I2c {
    /// The bus error.
    type Error;
    /// Write `bytes` to `addr`.
    fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), Self::Error>;
    /// Write `wr` to `addr`, then read `rd` after a repeated start.
    fn write_read(&mut self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), Self::Error>;
    /// Read `rd` from `addr` with no command byte (the SMBus alert
    /// response address, for one). Default: a write of nothing, then
    /// the read.
    fn read(&mut self, addr: u8, rd: &mut [u8]) -> Result<(), Self::Error> {
        self.write_read(addr, &[], rd)
    }
}

/// A driver error: the bus failed, or the device answered but is not
/// what the driver expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The bus failed.
    Bus(E),
    /// The identity register did not match; carries what was read.
    WrongDevice,
    /// A block read announced a different length than requested.
    BadLength,
    /// A value outside the range the device was configured for.
    OutOfRange,
    /// The sensor reports an open or shorted diode.
    SensorFault,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Self {
        Error::Bus(e)
    }
}

#[cfg(test)]
pub(crate) mod mock {
    //! A scripted bus for tests: every expected transaction in order,
    //! with the bytes the device answers.
    use super::I2c;
    use std::collections::VecDeque;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Tx {
        Write(u8, Vec<u8>),
        WriteRead(u8, Vec<u8>, Vec<u8>),
        /// A register read the device does not acknowledge (absent part).
        Nack(u8, Vec<u8>),
        /// A write the device does not acknowledge (a value it refuses).
        NackWrite(u8, Vec<u8>),
    }

    #[derive(Default)]
    pub struct Bus {
        pub script: VecDeque<Tx>,
        pub log: Vec<Tx>,
    }

    impl Bus {
        pub fn new(script: Vec<Tx>) -> Self {
            Bus {
                script: script.into(),
                log: Vec::new(),
            }
        }
        /// A bus that answers every read with zeros and accepts every
        /// write; the log holds what was sent.
        pub fn sink() -> Self {
            Bus::default()
        }
    }

    impl I2c for Bus {
        type Error = ();
        fn write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), ()> {
            self.log.push(Tx::Write(addr, bytes.to_vec()));
            match self.script.pop_front() {
                Some(Tx::NackWrite(a, b)) => {
                    assert_eq!((a, b.as_slice()), (addr, bytes), "unexpected write");
                    return Err(());
                }
                Some(next) => {
                    assert_eq!(next, Tx::Write(addr, bytes.to_vec()), "unexpected write");
                }
                None => {}
            }
            Ok(())
        }
        fn write_read(&mut self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), ()> {
            self.log.push(Tx::WriteRead(addr, wr.to_vec(), Vec::new()));
            match self.script.pop_front() {
                Some(Tx::WriteRead(a, w, answer)) => {
                    assert_eq!((a, w.as_slice()), (addr, wr), "unexpected read");
                    assert_eq!(answer.len(), rd.len(), "answer length");
                    rd.copy_from_slice(&answer);
                }
                Some(Tx::Nack(a, w)) => {
                    assert_eq!((a, w.as_slice()), (addr, wr), "unexpected read");
                    return Err(());
                }
                Some(other) => panic!("expected {other:?}, got read of {wr:?}"),
                None => rd.iter_mut().for_each(|b| *b = 0),
            }
            Ok(())
        }
    }
}
