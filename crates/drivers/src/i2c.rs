//! The bus the drivers need: embedded-hal 1.0's [`I2c`] with 7-bit
//! addresses (a write, a write followed by a read after a repeated
//! start, a plain read), re-exported here with the pieces an
//! implementation needs, and the drivers' own [`Error`].

pub use embedded_hal::i2c::{
    Error as BusError, ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation,
};

/// A driver error: the bus failed, or the device answered but is not
/// what the driver expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The bus failed.
    Bus(E),
    /// The identity register did not match.
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

/// A scripted bus for tests (this crate's, and with the `mock` feature
/// a dependent crate's): every expected transaction in order, with the
/// bytes the device answers. Needs std.
#[cfg(any(test, feature = "mock"))]
#[allow(missing_docs)]
pub mod mock {
    use super::{ErrorKind, ErrorType, I2c, Operation};
    use std::collections::VecDeque;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Tx {
        Write(u8, Vec<u8>),
        /// A register read; a plain read is logged with an empty write.
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

        fn do_write(&mut self, addr: u8, bytes: &[u8]) -> Result<(), ErrorKind> {
            self.log.push(Tx::Write(addr, bytes.to_vec()));
            match self.script.pop_front() {
                Some(Tx::NackWrite(a, b)) => {
                    assert_eq!((a, b.as_slice()), (addr, bytes), "unexpected write");
                    return Err(ErrorKind::Other);
                }
                Some(next) => {
                    assert_eq!(next, Tx::Write(addr, bytes.to_vec()), "unexpected write");
                }
                None => {}
            }
            Ok(())
        }

        fn do_write_read(&mut self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), ErrorKind> {
            self.log.push(Tx::WriteRead(addr, wr.to_vec(), Vec::new()));
            match self.script.pop_front() {
                Some(Tx::WriteRead(a, w, answer)) => {
                    assert_eq!((a, w.as_slice()), (addr, wr), "unexpected read");
                    assert_eq!(answer.len(), rd.len(), "answer length");
                    rd.copy_from_slice(&answer);
                }
                Some(Tx::Nack(a, w)) => {
                    assert_eq!((a, w.as_slice()), (addr, wr), "unexpected read");
                    return Err(ErrorKind::Other);
                }
                Some(other) => panic!("expected {other:?}, got read of {wr:?}"),
                None => rd.iter_mut().for_each(|b| *b = 0),
            }
            Ok(())
        }
    }

    impl ErrorType for Bus {
        type Error = ErrorKind;
    }

    impl I2c for Bus {
        fn transaction(&mut self, addr: u8, ops: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
            match ops {
                [Operation::Write(w)] => self.do_write(addr, w),
                [Operation::Write(w), Operation::Read(r)] => self.do_write_read(addr, w, r),
                [Operation::Read(r)] => self.do_write_read(addr, &[], r),
                _ => panic!("the scripted bus knows write, write_read and read"),
            }
        }
    }
}
