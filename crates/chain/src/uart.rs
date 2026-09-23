//! The one thing the driver needs from the outside world.

/// A byte pipe to the chain, non-blocking on the read side.
///
/// `read` returns what is available now — zero means nothing yet, not
/// end of stream. `write` may block for the length of the frame; frames
/// are at most 128 bytes. `set_baud` reconfigures the host side after
/// the chips have been told to switch.
pub trait Uart {
    /// Transport error; the driver passes it through untouched.
    type Error: core::fmt::Debug;

    /// Send bytes to the chain.
    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;

    /// Read what has arrived, up to `buf.len()` bytes. Returns 0 when
    /// nothing is pending.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;

    /// Switch the host's baud rate.
    fn set_baud(&mut self, baud: u32) -> Result<(), Self::Error>;
}
