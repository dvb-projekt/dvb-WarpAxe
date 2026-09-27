//! The Sv2 wire primitives: little-endian integers, `STR0_255`, `B0_32`,
//! `B0_64K`, `U256`, `SEQ0_255[U256]`, and the 6-byte frame header
//! `extension_type u16 | msg_type u8 | msg_length u24`.

use crate::Error;

/// Set in `extension_type` on every channel-specific message (spec §3.1).
pub const CHANNEL_MSG_FLAG: u16 = 0x8000;
/// The plain frame header.
pub const HEADER_LEN: usize = 6;

/// One Sv2 message with its header fields, payload in the clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Extension type; bit 15 marks a channel message.
    pub extension_type: u16,
    /// Message type code (spec §7).
    pub message_type: u8,
    /// The message body.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A frame of the base protocol (no extension, no channel flag).
    pub fn new(message_type: u8, payload: Vec<u8>) -> Frame {
        Frame {
            extension_type: 0,
            message_type,
            payload,
        }
    }

    /// A channel-specific frame (extension flag set).
    pub fn channel(message_type: u8, payload: Vec<u8>) -> Frame {
        Frame {
            extension_type: CHANNEL_MSG_FLAG,
            message_type,
            payload,
        }
    }

    /// The 6-byte header for this frame.
    pub fn header(&self) -> [u8; HEADER_LEN] {
        let len = self.payload.len() as u32;
        let [e0, e1] = self.extension_type.to_le_bytes();
        [
            e0,
            e1,
            self.message_type,
            (len & 0xFF) as u8,
            ((len >> 8) & 0xFF) as u8,
            ((len >> 16) & 0xFF) as u8,
        ]
    }

    /// Split a plain header into (extension_type, message_type, length).
    pub fn parse_header(h: &[u8; HEADER_LEN]) -> (u16, u8, usize) {
        let ext = u16::from_le_bytes([h[0], h[1]]);
        let len = (h[3] as usize) | ((h[4] as usize) << 8) | ((h[5] as usize) << 16);
        (ext, h[2], len)
    }
}

/// Appends fields in wire order.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// Empty writer.
    pub fn new() -> Writer {
        Writer::default()
    }
    /// The bytes written so far.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
    /// `u8`.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    /// `u16` little-endian.
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// `u32` little-endian.
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// `f32` little-endian.
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// `bool` as one byte.
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.buf.push(u8::from(v));
        self
    }
    /// `U256`: 32 bytes as they are.
    pub fn u256(&mut self, v: &[u8; 32]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
    /// `STR0_255`: one length byte, then the UTF-8 bytes. Longer strings
    /// are cut at 255 bytes on a character boundary.
    pub fn str0_255(&mut self, s: &str) -> &mut Self {
        let mut end = s.len().min(255);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.buf.push(end as u8);
        self.buf.extend_from_slice(&s.as_bytes()[..end]);
        self
    }
    /// `B0_32` / `B0_255`: one length byte, then the bytes.
    pub fn b0_255(&mut self, b: &[u8]) -> Result<&mut Self, Error> {
        if b.len() > 255 {
            return Err(Error::TooLong);
        }
        self.buf.push(b.len() as u8);
        self.buf.extend_from_slice(b);
        Ok(self)
    }
}

/// Reads fields in wire order with bounds checks.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Reader over `buf`.
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }
    /// Bytes not consumed yet.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    /// The next `n` bytes.
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < n {
            return Err(Error::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    /// `u8`.
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }
    /// `u16` little-endian.
    pub fn u16(&mut self) -> Result<u16, Error> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    /// `u32` little-endian.
    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// `u64` little-endian.
    pub fn u64(&mut self) -> Result<u64, Error> {
        let b = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }
    /// `bool`: one byte, nonzero = true.
    pub fn bool(&mut self) -> Result<bool, Error> {
        Ok(self.u8()? != 0)
    }
    /// `U256`.
    pub fn u256(&mut self) -> Result<[u8; 32], Error> {
        let mut a = [0u8; 32];
        a.copy_from_slice(self.bytes(32)?);
        Ok(a)
    }
    /// `STR0_255`.
    pub fn str0_255(&mut self) -> Result<String, Error> {
        let n = self.u8()? as usize;
        let b = self.bytes(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| Error::BadUtf8)
    }
    /// `B0_32` / `B0_255`.
    pub fn b0_255(&mut self) -> Result<Vec<u8>, Error> {
        let n = self.u8()? as usize;
        Ok(self.bytes(n)?.to_vec())
    }
    /// `B0_64K`.
    pub fn b0_64k(&mut self) -> Result<Vec<u8>, Error> {
        let n = self.u16()? as usize;
        Ok(self.bytes(n)?.to_vec())
    }
    /// `SEQ0_255[U256]`.
    pub fn seq0_255_u256(&mut self) -> Result<Vec<[u8; 32]>, Error> {
        let n = self.u8()? as usize;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(self.u256()?);
        }
        Ok(v)
    }
    /// `Option<u32>`: one tag byte, then the value when the tag is 1.
    pub fn option_u32(&mut self) -> Result<Option<u32>, Error> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.u32()?)),
            _ => Err(Error::Protocol("bad option tag".into())),
        }
    }
}

/// Difficulty 1 as a float: `0xFFFF << 208`.
pub const DIFF1: f64 = 65535.0 * (1u128 << 104) as f64 * (1u128 << 104) as f64;

/// Pool difficulty of a little-endian 256-bit target: `diff1 / target`.
/// A zero target reads as the largest difficulty.
pub fn target_to_difficulty(target_le: &[u8; 32]) -> f64 {
    let mut t = 0.0f64;
    for i in (0..4).rev() {
        let mut w = [0u8; 8];
        w.copy_from_slice(&target_le[i * 8..i * 8 + 8]);
        t = t * 18446744073709551616.0 + u64::from_le_bytes(w) as f64;
    }
    if t == 0.0 {
        return f64::MAX;
    }
    DIFF1 / t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips_and_carries_a_u24_length() {
        let f = Frame::channel(0x1F, vec![0u8; 0x012345]);
        let h = f.header();
        assert_eq!(h, [0x00, 0x80, 0x1F, 0x45, 0x23, 0x01]);
        assert_eq!(Frame::parse_header(&h), (CHANNEL_MSG_FLAG, 0x1F, 0x012345));
    }

    #[test]
    fn fields_round_trip() {
        let mut w = Writer::new();
        w.u8(7).u16(0x1234).u32(0xdeadbeef).f32(1.5).bool(true);
        w.str0_255("héllo").b0_255(&[1, 2, 3]).unwrap();
        w.u256(&[9u8; 32]);
        let bytes = w.finish();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u8().unwrap(), 7);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0xdeadbeef);
        assert_eq!(r.bytes(4).unwrap(), &1.5f32.to_le_bytes());
        assert!(r.bool().unwrap());
        assert_eq!(r.str0_255().unwrap(), "héllo");
        assert_eq!(r.b0_255().unwrap(), vec![1, 2, 3]);
        assert_eq!(r.u256().unwrap(), [9u8; 32]);
        assert_eq!(r.remaining(), 0);
        assert_eq!(r.u8(), Err(Error::Truncated));
    }

    #[test]
    fn long_strings_are_cut_on_a_char_boundary() {
        let s = "ä".repeat(200); // 400 bytes
        let mut w = Writer::new();
        w.str0_255(&s);
        let b = w.finish();
        assert_eq!(b[0], 254);
        assert!(String::from_utf8(b[1..].to_vec()).is_ok());
    }

    #[test]
    fn truncated_sequences_and_options_are_errors_not_panics() {
        let mut r = Reader::new(&[2, 0, 0]);
        assert_eq!(r.seq0_255_u256(), Err(Error::Truncated));
        let mut r = Reader::new(&[1, 0, 0]);
        assert_eq!(r.option_u32(), Err(Error::Truncated));
        let mut r = Reader::new(&[5]);
        assert!(r.option_u32().is_err());
        let mut r = Reader::new(&[3, 0xff, 0xfe]);
        assert_eq!(r.str0_255(), Err(Error::Truncated));
        let mut r = Reader::new(&[2, 0xff, 0xfe]);
        assert_eq!(r.str0_255(), Err(Error::BadUtf8));
    }

    #[test]
    fn difficulty_of_the_diff1_target_is_one() {
        // 0x00000000FFFF0000…: little-endian, the 0xFFFF sits in bytes 26/27.
        let mut t = [0u8; 32];
        t[26] = 0xFF;
        t[27] = 0xFF;
        let d = target_to_difficulty(&t);
        assert!((d - 1.0).abs() < 1e-9, "{d}");
        t[26] = 0xFF;
        t[27] = 0x0F;
        let d = target_to_difficulty(&t);
        assert!((d - 65535.0 / 4095.0).abs() < 1e-6, "{d}");
        assert_eq!(target_to_difficulty(&[0u8; 32]), f64::MAX);
        let d = target_to_difficulty(&[0xFF; 32]);
        assert!(d < 1.0 && d > 0.0, "{d}");
    }
}
