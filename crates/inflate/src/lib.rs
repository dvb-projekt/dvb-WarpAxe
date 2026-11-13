//! A small DEFLATE decoder (RFC 1951, the raw stream without a zlib or
//! gzip wrapper) that unpacks into a slice.
//!
//! Why it exists: the boot screen's pictures (the 320×170 background and
//! the makers' wordmarks) took 203 KB of the firmware image as raw
//! RGB565, and the image has to stay below the NerdMiner import window
//! (`tools/release-firmware.sh`). Packed with DEFLATE they take 35 KB.
//! The firmware unpacks the background straight into the display's
//! frame buffer, which exists anyway, so no memory is added (docs/16).
//!
//! What it is built for:
//!
//! - the whole output is one slice, and a match copies from that slice,
//!   so there is no window and no allocation;
//! - the working state is about 1.3 KB on the caller's stack and nothing
//!   in static RAM (the fixed code's tables are built when a block asks
//!   for them);
//! - damaged or cut-off input ends in an [`Error`], never in a panic or
//!   a read or write outside the two slices: on the miner a panic is a
//!   restart;
//! - size before speed: codes are read bit by bit, which is a few
//!   milliseconds for the splash and saves the lookup tables.
//!
//! The method is the one of Mark Adler's `puff.c` (canonical codes
//! decoded by counting, zlib's contrib/puff).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Why a stream could not be unpacked.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Error {
    /// The input ended before the last block did.
    Truncated,
    /// The input is not a valid DEFLATE stream.
    Corrupt,
    /// The stream holds more than the output slice takes.
    Overflow,
}

/// Longest code, in bits.
const MAX_BITS: usize = 15;
/// Literal/length codes, at most.
const MAX_LCODES: usize = 286;
/// Distance codes, at most.
const MAX_DCODES: usize = 30;
/// Literal/length codes of the fixed code.
const FIX_LCODES: usize = 288;

/// A canonical Huffman code: how many codes there are of each length,
/// and the symbols ordered by code.
struct Huffman<const N: usize> {
    count: [u16; MAX_BITS + 1],
    symbol: [u16; N],
}

impl<const N: usize> Huffman<N> {
    const fn new() -> Self {
        Self {
            count: [0; MAX_BITS + 1],
            symbol: [0; N],
        }
    }

    /// Build the code from each symbol's length (0: not used). Returns
    /// how many codes are left unused: 0 for a complete code, negative
    /// for one that claims more codes than the lengths allow.
    fn build(&mut self, lengths: &[u8]) -> i32 {
        self.count = [0; MAX_BITS + 1];
        for &l in lengths {
            self.count[usize::from(l) & MAX_BITS] += 1;
        }
        if usize::from(self.count[0]) == lengths.len() {
            // No codes at all: complete, and nothing decodes with it.
            return 0;
        }
        let mut left: i32 = 1;
        for &count in &self.count[1..] {
            left <<= 1;
            left -= i32::from(count);
            if left < 0 {
                return left;
            }
        }
        let mut offs = [0u16; MAX_BITS + 1];
        for len in 1..MAX_BITS {
            offs[len + 1] = offs[len] + self.count[len];
        }
        for (sym, &l) in lengths.iter().enumerate() {
            let l = usize::from(l) & MAX_BITS;
            if l != 0 {
                if let Some(slot) = self.symbol.get_mut(usize::from(offs[l])) {
                    *slot = sym as u16;
                }
                offs[l] += 1;
            }
        }
        left
    }
}

/// The input, read bit by bit from the low end of each byte.
struct Bits<'a> {
    src: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl Bits<'_> {
    /// The next `need` bits (at most 16) as a number, first bit lowest.
    fn bits(&mut self, need: u32) -> Result<u32, Error> {
        while self.cnt < need {
            let b = *self.src.get(self.pos).ok_or(Error::Truncated)?;
            self.pos += 1;
            self.buf |= u32::from(b) << self.cnt;
            self.cnt += 8;
        }
        let v = self.buf & ((1u32 << need) - 1);
        self.buf >>= need;
        self.cnt -= need;
        Ok(v)
    }

    /// The next symbol of the code `h`.
    fn decode<const N: usize>(&mut self, h: &Huffman<N>) -> Result<u16, Error> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for &count in &h.count[1..] {
            code |= self.bits(1)? as i32;
            let count = i32::from(count);
            if code - count < first {
                return h
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or(Error::Corrupt);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Error::Corrupt)
    }
}

/// Base length and extra bits of the length symbols 257..285.
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base distance and extra bits of the distance symbols 0..29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order in which a dynamic block sends its code length code.
const CLEN_ORDER: [u8; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct Inflater<'a, 'b> {
    inp: Bits<'a>,
    out: &'b mut [u8],
    len: usize,
    lcode: Huffman<FIX_LCODES>,
    dcode: Huffman<MAX_DCODES>,
}

impl Inflater<'_, '_> {
    /// A stored block: a length, its complement, the bytes.
    fn stored(&mut self) -> Result<(), Error> {
        // The rest of the current byte is skipped.
        self.inp.buf = 0;
        self.inp.cnt = 0;
        let head = self
            .inp
            .src
            .get(self.inp.pos..self.inp.pos + 4)
            .ok_or(Error::Truncated)?;
        let n = u16::from_le_bytes([head[0], head[1]]);
        if n != !u16::from_le_bytes([head[2], head[3]]) {
            return Err(Error::Corrupt);
        }
        let n = usize::from(n);
        let from = self.inp.pos + 4;
        let data = self.inp.src.get(from..from + n).ok_or(Error::Truncated)?;
        self.out
            .get_mut(self.len..self.len + n)
            .ok_or(Error::Overflow)?
            .copy_from_slice(data);
        self.inp.pos = from + n;
        self.len += n;
        Ok(())
    }

    /// The fixed code of RFC 1951, 3.2.6.
    fn fixed(&mut self) {
        let mut lengths = [0u8; FIX_LCODES];
        for (sym, l) in lengths.iter_mut().enumerate() {
            *l = match sym {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        self.lcode.build(&lengths);
        self.dcode.build(&[5; MAX_DCODES]);
    }

    /// A dynamic block's two codes, sent as code lengths that are
    /// themselves coded (RFC 1951, 3.2.7).
    fn dynamic(&mut self) -> Result<(), Error> {
        let nlen = self.inp.bits(5)? as usize + 257;
        let ndist = self.inp.bits(5)? as usize + 1;
        let ncode = self.inp.bits(4)? as usize + 4;
        if nlen > MAX_LCODES || ndist > MAX_DCODES {
            return Err(Error::Corrupt);
        }
        let mut lengths = [0u8; MAX_LCODES + MAX_DCODES];
        for &at in &CLEN_ORDER[..ncode] {
            lengths[usize::from(at)] = self.inp.bits(3)? as u8;
        }
        let mut clen = Huffman::<19>::new();
        if clen.build(&lengths[..19]) != 0 {
            return Err(Error::Corrupt);
        }
        lengths = [0; MAX_LCODES + MAX_DCODES];
        let mut index = 0;
        while index < nlen + ndist {
            let sym = self.inp.decode(&clen)?;
            if sym < 16 {
                lengths[index] = sym as u8;
                index += 1;
                continue;
            }
            let (value, repeat) = match sym {
                16 => {
                    if index == 0 {
                        return Err(Error::Corrupt);
                    }
                    (lengths[index - 1], 3 + self.inp.bits(2)? as usize)
                }
                17 => (0, 3 + self.inp.bits(3)? as usize),
                _ => (0, 11 + self.inp.bits(7)? as usize),
            };
            lengths
                .get_mut(index..index + repeat)
                .filter(|_| index + repeat <= nlen + ndist)
                .ok_or(Error::Corrupt)?
                .fill(value);
            index += repeat;
        }
        // Without an end-of-block code the block could never end.
        if lengths[256] == 0 {
            return Err(Error::Corrupt);
        }
        // An incomplete code is allowed only when it has a single code
        // (a block with one distance, or none).
        let left = self.lcode.build(&lengths[..nlen]);
        if left < 0 || (left > 0 && nlen - usize::from(self.lcode.count[0]) != 1) {
            return Err(Error::Corrupt);
        }
        let left = self.dcode.build(&lengths[nlen..nlen + ndist]);
        if left < 0 || (left > 0 && ndist - usize::from(self.dcode.count[0]) != 1) {
            return Err(Error::Corrupt);
        }
        Ok(())
    }

    /// Literals and matches until the end-of-block code.
    fn codes(&mut self) -> Result<(), Error> {
        loop {
            let sym = usize::from(self.inp.decode(&self.lcode)?);
            if sym < 256 {
                *self.out.get_mut(self.len).ok_or(Error::Overflow)? = sym as u8;
                self.len += 1;
                continue;
            }
            if sym == 256 {
                return Ok(());
            }
            let at = sym - 257;
            let base = *LEN_BASE.get(at).ok_or(Error::Corrupt)?;
            let n = usize::from(base) + self.inp.bits(u32::from(LEN_EXTRA[at]))? as usize;
            let at = usize::from(self.inp.decode(&self.dcode)?);
            let base = *DIST_BASE.get(at).ok_or(Error::Corrupt)?;
            let dist = usize::from(base) + self.inp.bits(u32::from(DIST_EXTRA[at]))? as usize;
            if dist > self.len {
                return Err(Error::Corrupt);
            }
            if self.len + n > self.out.len() {
                return Err(Error::Overflow);
            }
            let from = self.len - dist;
            if dist >= n {
                self.out.copy_within(from..from + n, self.len);
            } else {
                // The match runs into itself: byte by byte, so the
                // bytes just written are read again.
                for i in 0..n {
                    self.out[self.len + i] = self.out[from + i];
                }
            }
            self.len += n;
        }
    }
}

/// Unpack the raw DEFLATE stream `src` into `out` and return how many
/// bytes it held. `out` may be longer than that; the rest is not
/// touched. On an error `out` holds whatever was unpacked up to it.
pub fn inflate(src: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let mut s = Inflater {
        inp: Bits {
            src,
            pos: 0,
            buf: 0,
            cnt: 0,
        },
        out,
        len: 0,
        lcode: Huffman::new(),
        dcode: Huffman::new(),
    };
    loop {
        let last = s.inp.bits(1)? == 1;
        match s.inp.bits(2)? {
            0 => s.stored()?,
            1 => {
                s.fixed();
                s.codes()?;
            }
            2 => {
                s.dynamic()?;
                s.codes()?;
            }
            _ => return Err(Error::Corrupt),
        }
        if last {
            return Ok(s.len);
        }
    }
}
