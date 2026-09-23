//! Reading what the host sends: the chip's (and so the simulator's) side
//! of [`crate::frame::encode_cmd`] and [`crate::frame::encode_job`].
//!
//! Host frames are `55 AA | hdr | len | payload… | crc`, with `len`
//! counting everything after the preamble, checksum included, and the
//! checksum's width decided by the header (CRC-5 for commands, CRC-16 for
//! jobs). Unlike chip responses they are not fixed-size, so a stream
//! decoder has to trust the length byte just far enough to know where to
//! look for the checksum — and no further: a length that does not fit
//! the frame cap, or a header byte the protocol does not define, is
//! junk and costs one byte of resync like any other.
//!
//! Firmware never calls this. The simulator does, for every byte the
//! firmware writes to the UART; and a bus sniffer would.

use crate::crc::{crc16, crc5};
use crate::frame::{Header, MAX_FRAME_LEN, TX_PREAMBLE};

/// Largest payload a host frame can carry within [`MAX_FRAME_LEN`]:
/// the cap minus preamble, header, length byte and a CRC-5.
pub const MAX_PAYLOAD_LEN: usize = MAX_FRAME_LEN - 5;

/// Why a host frame could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdError {
    /// Not `55 AA`.
    BadPreamble,
    /// Header byte with undefined bits, or both/neither type bit.
    BadHeader,
    /// Length byte too small for a frame or too large for the cap.
    BadLength,
    /// Fewer bytes than the length byte promises.
    Incomplete,
    /// Checksum mismatch.
    BadCrc,
}

/// A decoded host frame, borrowing its payload from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cmd<'a> {
    /// Type, group and operation.
    pub header: Header,
    /// The payload: `chip_addr | register | value` for a write, the
    /// 82-byte work packet for a job, and so on.
    pub payload: &'a [u8],
}

/// Frame length implied by a header and length byte, if they make sense.
fn frame_len(header: Header, len_byte: u8) -> Result<usize, CmdError> {
    let crc_len = if header.is_job() { 2 } else { 1 };
    let total = 2 + usize::from(len_byte);
    // Header + length byte + CRC at minimum; the cap at maximum.
    if usize::from(len_byte) < 2 + crc_len || total > MAX_FRAME_LEN {
        return Err(CmdError::BadLength);
    }
    Ok(total)
}

/// Decode one host frame at the start of `bytes`.
///
/// Returns the frame and how many bytes it occupied; anything after is
/// untouched. The checksum is verified.
pub fn decode_cmd(bytes: &[u8]) -> Result<(Cmd<'_>, usize), CmdError> {
    if bytes.len() < 2 {
        return Err(CmdError::Incomplete);
    }
    if bytes[0..2] != TX_PREAMBLE {
        return Err(CmdError::BadPreamble);
    }
    if bytes.len() < 4 {
        return Err(CmdError::Incomplete);
    }
    let header = Header::from_byte(bytes[2]).ok_or(CmdError::BadHeader)?;
    let total = frame_len(header, bytes[3])?;
    if bytes.len() < total {
        return Err(CmdError::Incomplete);
    }
    let crc_len = if header.is_job() { 2 } else { 1 };
    let body = &bytes[2..total - crc_len];
    let ok = if header.is_job() {
        crc16(body).to_be_bytes() == bytes[total - 2..total]
    } else {
        crc5(body) == bytes[total - 1]
    };
    if !ok {
        return Err(CmdError::BadCrc);
    }
    Ok((
        Cmd {
            header,
            payload: &bytes[4..total - crc_len],
        },
        total,
    ))
}

/// A host frame the stream decoder found, payload copied out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmdFrame {
    /// Type, group and operation.
    pub header: Header,
    payload: [u8; MAX_PAYLOAD_LEN],
    len: u8,
    /// Bytes dropped between the previous accepted frame (or the start)
    /// and this one.
    pub dropped_before: u32,
}

impl CmdFrame {
    /// The payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload[..usize::from(self.len)]
    }
}

/// Running totals of a [`CmdDecoder`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CmdStats {
    /// Frames accepted.
    pub frames: u32,
    /// Bytes that were not part of an accepted frame.
    pub dropped: u32,
    /// Frames that were complete and well-formed but failed their CRC.
    pub crc_failures: u32,
}

/// Incremental decoder for the host→chip byte stream.
///
/// Same discipline as [`crate::decoder::RxDecoder`]: a window, filled
/// byte by byte, checked when the length byte says the frame is
/// complete; on any failure one byte is dropped and the preamble search
/// resumes inside the window. Every byte ends up in exactly one of
/// accepted frames, the dropped count, or the pending window.
#[derive(Debug, Clone)]
pub struct CmdDecoder {
    buf: [u8; MAX_FRAME_LEN],
    len: usize,
    dropped_since_frame: u32,
    stats: CmdStats,
}

impl Default for CmdDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CmdDecoder {
    /// A decoder with an empty window.
    pub const fn new() -> Self {
        CmdDecoder {
            buf: [0; MAX_FRAME_LEN],
            len: 0,
            dropped_since_frame: 0,
            stats: CmdStats {
                frames: 0,
                dropped: 0,
                crc_failures: 0,
            },
        }
    }

    /// Running totals.
    pub const fn stats(&self) -> CmdStats {
        self.stats
    }

    /// Bytes held that may still become a frame.
    pub const fn pending(&self) -> usize {
        self.len
    }

    /// Feed one byte. Returns a frame when this byte completes one.
    pub fn push(&mut self, byte: u8) -> Option<CmdFrame> {
        self.buf[self.len] = byte;
        self.len += 1;
        loop {
            // Validate what we have so far, earliest failure first.
            match self.check() {
                Check::Need => return None,
                Check::Frame(total) => {
                    let (cmd, _) = decode_cmd(&self.buf[..total]).expect("checked");
                    let mut payload = [0u8; MAX_PAYLOAD_LEN];
                    payload[..cmd.payload.len()].copy_from_slice(cmd.payload);
                    let frame = CmdFrame {
                        header: cmd.header,
                        payload,
                        len: cmd.payload.len() as u8,
                        dropped_before: self.dropped_since_frame,
                    };
                    self.dropped_since_frame = 0;
                    self.stats.frames += 1;
                    // Anything after the frame stays for the next one.
                    self.buf.copy_within(total..self.len, 0);
                    self.len -= total;
                    return Some(frame);
                }
                Check::Bad(crc) => {
                    if crc {
                        self.stats.crc_failures += 1;
                    }
                    self.drop_one();
                    if self.len == 0 {
                        return None;
                    }
                    // Re-check the shifted window before reading more.
                }
            }
        }
    }

    /// Feed a slice, calling `on_frame` for each completed frame.
    pub fn push_slice(&mut self, bytes: &[u8], mut on_frame: impl FnMut(CmdFrame)) {
        for &b in bytes {
            if let Some(f) = self.push(b) {
                on_frame(f);
            }
        }
    }

    /// Discard the window; its bytes count as dropped.
    pub fn reset(&mut self) {
        let n = self.len as u32;
        self.len = 0;
        self.dropped_since_frame = self.dropped_since_frame.saturating_add(n);
        self.stats.dropped = self.stats.dropped.saturating_add(n);
    }

    fn drop_one(&mut self) {
        self.buf.copy_within(1..self.len, 0);
        self.len -= 1;
        self.dropped_since_frame = self.dropped_since_frame.saturating_add(1);
        self.stats.dropped = self.stats.dropped.saturating_add(1);
    }

    /// Is the window a frame, a prefix of one, or junk?
    fn check(&self) -> Check {
        let b = &self.buf[..self.len];
        if b.is_empty() {
            return Check::Need;
        }
        if b[0] != TX_PREAMBLE[0] {
            return Check::Bad(false);
        }
        if b.len() < 2 {
            return Check::Need;
        }
        if b[1] != TX_PREAMBLE[1] {
            return Check::Bad(false);
        }
        if b.len() < 3 {
            return Check::Need;
        }
        let Some(header) = Header::from_byte(b[2]) else {
            return Check::Bad(false);
        };
        if b.len() < 4 {
            return Check::Need;
        }
        let Ok(total) = frame_len(header, b[3]) else {
            return Check::Bad(false);
        };
        if b.len() < total {
            return Check::Need;
        }
        match decode_cmd(&b[..total]) {
            Ok(_) => Check::Frame(total),
            Err(CmdError::BadCrc) => Check::Bad(true),
            Err(_) => Check::Bad(false),
        }
    }
}

enum Check {
    Need,
    Frame(usize),
    /// Junk at the front; `true` if it was a well-formed frame with a
    /// bad checksum.
    Bad(bool),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode_cmd, encode_job, Group, Op};

    fn collect(d: &mut CmdDecoder, bytes: &[u8]) -> Vec<CmdFrame> {
        let mut out = Vec::new();
        d.push_slice(bytes, |f| out.push(f));
        out
    }

    /// The version-mask frame from `test_version_rolling.c`.
    #[test]
    fn decodes_the_version_mask_frame() {
        let f = [
            0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c,
        ];
        let (cmd, n) = decode_cmd(&f).unwrap();
        assert_eq!(n, 11);
        assert_eq!(cmd.header, Header::cmd(Group::All, Op::Write));
        assert_eq!(cmd.payload, &[0x00, 0xa4, 0x90, 0x00, 0xff, 0xff]);
    }

    /// Chain-inactive and set-address: two-byte payloads.
    #[test]
    fn decodes_short_commands() {
        let (c, n) = decode_cmd(&[0x55, 0xAA, 0x53, 0x05, 0x00, 0x00, 0x03]).unwrap();
        assert_eq!(
            (n, c.header.op(), c.payload),
            (7, Op::Inactive, &[0x00, 0x00][..])
        );
        let (c, _) = decode_cmd(&[0x55, 0xAA, 0x40, 0x05, 0x00, 0x00, 0x1C]).unwrap();
        assert_eq!(
            (c.header.group(), c.header.op()),
            (Group::Single, Op::SetAddress)
        );
    }

    /// The golden 88-byte job frame decodes to its 82-byte payload.
    #[test]
    fn decodes_the_golden_job_frame() {
        let mut payload = vec![0x18, 0x01, 0x78, 0x56, 0x34, 0x12, 0x01, 0xdd, 0x05, 0x17];
        payload.extend_from_slice(&[0xd8, 0x8b, 0x65, 0x64]);
        payload.extend(0x00u8..0x40);
        payload.extend_from_slice(&[0x04, 0x00, 0x00, 0x20]);
        let mut frame = vec![0x55, 0xaa, 0x21, 0x56];
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&[0x05, 0xd4]);
        let (cmd, n) = decode_cmd(&frame).unwrap();
        assert_eq!(n, 88);
        assert!(cmd.header.is_job());
        assert_eq!(cmd.payload, &payload[..]);
    }

    #[test]
    fn rejects_each_kind_of_damage() {
        let good = [
            0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c,
        ];
        assert_eq!(decode_cmd(&good[..1]), Err(CmdError::Incomplete));
        assert_eq!(decode_cmd(&good[..3]), Err(CmdError::Incomplete));
        assert_eq!(decode_cmd(&good[..10]), Err(CmdError::Incomplete));
        let mut bad = good;
        bad[0] = 0xAA;
        assert_eq!(decode_cmd(&bad), Err(CmdError::BadPreamble));
        let mut bad = good;
        bad[2] = 0x61;
        assert_eq!(decode_cmd(&bad), Err(CmdError::BadHeader));
        let mut bad = good;
        bad[3] = 0x02;
        assert_eq!(
            decode_cmd(&bad),
            Err(CmdError::BadLength),
            "len below minimum"
        );
        let mut bad = good;
        bad[3] = 0xFF;
        assert_eq!(
            decode_cmd(&bad),
            Err(CmdError::BadLength),
            "len past the cap"
        );
        let mut bad = good;
        bad[6] ^= 0x01;
        assert_eq!(decode_cmd(&bad), Err(CmdError::BadCrc));
    }

    #[test]
    fn a_job_needs_a_length_that_leaves_room_for_two_crc_bytes() {
        // header 0x21 (job) with len 3: only header+len+1 byte — no room for CRC-16.
        assert_eq!(
            decode_cmd(&[0x55, 0xAA, 0x21, 0x03, 0x00]),
            Err(CmdError::BadLength)
        );
    }

    #[test]
    fn stream_finds_frames_and_counts_junk() {
        let mut d = CmdDecoder::new();
        let mut s = vec![0x00, 0x55, 0x55, 0xAA, 0x00]; // junk incl. a false preamble
        s.extend_from_slice(&[0x55, 0xAA, 0x53, 0x05, 0x00, 0x00, 0x03]);
        s.extend_from_slice(&[
            0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c,
        ]);
        let got = collect(&mut d, &s);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].header.op(), Op::Inactive);
        assert_eq!(got[0].dropped_before, 5);
        assert_eq!(got[1].payload(), &[0x00, 0xa4, 0x90, 0x00, 0xff, 0xff]);
        assert_eq!(got[1].dropped_before, 0);
        assert_eq!(
            d.stats(),
            CmdStats {
                frames: 2,
                dropped: 5,
                crc_failures: 0
            }
        );
    }

    #[test]
    fn a_corrupt_frame_costs_its_bytes_and_the_next_one_is_found() {
        let mut d = CmdDecoder::new();
        let mut bad = [
            0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c,
        ];
        bad[7] ^= 0x40;
        let good = [0x55, 0xAA, 0x40, 0x05, 0x00, 0x00, 0x1C];
        let mut s = bad.to_vec();
        s.extend_from_slice(&good);
        let got = collect(&mut d, &s);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].header.op(), Op::SetAddress);
        assert_eq!(got[0].dropped_before, 11);
        assert_eq!(d.stats().crc_failures, 1);
    }

    /// A payload byte that looks like a preamble start (`55 AA` inside
    /// the merkle root, say) must not split a job frame.
    #[test]
    fn a_preamble_inside_a_job_payload_does_not_split_it() {
        let mut payload = vec![0x18, 0x01];
        payload.extend_from_slice(&[0x55, 0xAA, 0x51, 0x09]); // looks like a command start
        payload.resize(82, 0x11);
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = encode_job(&mut buf, &payload).unwrap();
        let mut d = CmdDecoder::new();
        let got = collect(&mut d, &buf[..n]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].payload(), &payload[..]);
        assert_eq!(d.stats().dropped, 0);
    }

    proptest::proptest! {
        /// encode ∘ decode = id, for commands and jobs.
        #[test]
        fn round_trips(payload in proptest::collection::vec(0u8..=255, 1..=MAX_PAYLOAD_LEN - 1),
                       job in proptest::bool::ANY, all in proptest::bool::ANY, op in 0u8..4) {
            let mut buf = [0u8; MAX_FRAME_LEN];
            let n = if job {
                encode_job(&mut buf, &payload).unwrap()
            } else {
                let group = if all { Group::All } else { Group::Single };
                let op = match op { 0 => Op::SetAddress, 1 => Op::Write, 2 => Op::Read, _ => Op::Inactive };
                encode_cmd(&mut buf, group, op, &payload).unwrap()
            };
            let (cmd, used) = decode_cmd(&buf[..n]).unwrap();
            proptest::prop_assert_eq!(used, n);
            proptest::prop_assert_eq!(cmd.header.is_job(), job);
            proptest::prop_assert_eq!(cmd.payload, &payload[..]);
        }

        /// Frames separated by junk that never contains 0x55: all found,
        /// junk charged exactly.
        #[test]
        fn stream_finds_every_frame_between_preamble_free_junk(
            frames in proptest::collection::vec(
                (proptest::collection::vec(0u8..=255, 2..40), proptest::bool::ANY), 0..6),
            gaps in proptest::collection::vec(proptest::collection::vec(0u8..=255, 0..12), 7),
        ) {
            let clean = |g: &Vec<u8>| g.iter().map(|&b| if b == 0x55 { 0x56 } else { b }).collect::<Vec<u8>>();
            let mut stream = Vec::new();
            let mut expect = Vec::new();
            for (i, (payload, job)) in frames.iter().enumerate() {
                let junk = clean(&gaps[i]);
                stream.extend_from_slice(&junk);
                let mut buf = [0u8; MAX_FRAME_LEN];
                let n = if *job { encode_job(&mut buf, payload).unwrap() }
                        else { encode_cmd(&mut buf, Group::All, Op::Write, payload).unwrap() };
                stream.extend_from_slice(&buf[..n]);
                expect.push((payload.clone(), *job, junk.len() as u32));
            }
            let trailing = clean(&gaps[frames.len()]);
            stream.extend_from_slice(&trailing);
            let mut d = CmdDecoder::new();
            let got = collect(&mut d, &stream);
            proptest::prop_assert_eq!(got.len(), expect.len());
            for (g, (p, job, junk)) in got.iter().zip(&expect) {
                proptest::prop_assert_eq!(g.payload(), &p[..]);
                proptest::prop_assert_eq!(g.header.is_job(), *job);
                proptest::prop_assert_eq!(g.dropped_before, *junk);
            }
            let total_junk: u32 = expect.iter().map(|e| e.2).sum::<u32>() + trailing.len() as u32;
            proptest::prop_assert_eq!(d.stats().dropped, total_junk);
        }

        /// Any bytes: no panic; every accepted frame re-encodes to a
        /// valid frame; bytes are conserved.
        #[test]
        fn arbitrary_bytes_never_panic_and_are_conserved(
            stream in proptest::collection::vec(0u8..=255, 0..300),
        ) {
            let mut d = CmdDecoder::new();
            let mut in_frames = 0usize;
            d.push_slice(&stream, |f| {
                in_frames += f.payload().len() + 4 + if f.header.is_job() { 2 } else { 1 };
            });
            let s = d.stats();
            proptest::prop_assert_eq!(in_frames + s.dropped as usize + d.pending(), stream.len());
        }

        #[test]
        fn chunking_is_irrelevant(stream in proptest::collection::vec(0u8..=255, 0..200),
                                  cut in 0usize..200) {
            let cut = cut.min(stream.len());
            let mut a = CmdDecoder::new();
            let ga = collect(&mut a, &stream);
            let mut b = CmdDecoder::new();
            let mut gb = collect(&mut b, &stream[..cut]);
            gb.extend(collect(&mut b, &stream[cut..]));
            proptest::prop_assert_eq!(ga, gb);
            proptest::prop_assert_eq!(a.stats(), b.stats());
        }
    }
}
