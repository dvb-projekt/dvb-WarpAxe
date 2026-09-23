//! Framing on the BM13xx UART: preamble, header, length, payload, checksum.
//!
//! Two frame kinds go *to* the chip:
//!
//! ```text
//! command  55 AA | hdr | len | payload… | crc5
//! job      55 AA | hdr | len | payload… | crc16 (big-endian)
//! ```
//!
//! `hdr` packs the frame type, the addressing group and the operation
//! (see [`Header`]). `len` counts everything after the preamble, checksum
//! included: `payload + 3` for a command, `payload + 4` for a job.
//!
//! One frame kind comes *from* the chip, fixed at eleven bytes on the
//! BM1366/68/70/73:
//!
//! ```text
//! response AA 55 | 8 bytes | flags:3 crc:5
//! ```
//!
//! Bit 7 of the last byte says "job result" (a nonce) as opposed to a
//! register read; bits 6 and 5 are not used by the reference driver. The
//! CRC covers the eight payload bytes *and* the three flag bits, so the
//! receiver checks it by running CRC-5 over all nine bytes and expecting
//! zero (see [`crate::crc`] for why that works here and not for commands).
//!
//! What the eight payload bytes *mean* (nonce, job id, version bits, or
//! register value) is the chip's business and lives in the profile
//! modules, not here. This module knows bytes, not semantics.
//!
//! Source: ESP-Miner `components/asic/bm1370.c` (`_send_BM1370`,
//! `bm1370_asic_result_t`) and `asic_common.c` (`receive_work`).

use crate::crc::{crc16, crc5, response_tag, verify5};

/// Preamble the host sends.
pub const TX_PREAMBLE: [u8; 2] = [0x55, 0xAA];
/// Preamble the chip sends.
pub const RX_PREAMBLE: [u8; 2] = [0xAA, 0x55];

/// Fixed length of a chip response on the BM1366/68/70/73.
pub const RESPONSE_LEN: usize = 11;

/// Largest frame the reference driver will build. Same cap here so a
/// caller-supplied buffer of this size always suffices.
pub const MAX_FRAME_LEN: usize = 128;

/// Bit 7 of a response's last byte: job result rather than register read.
pub const FLAG_JOB: u8 = 0x80;
/// The three flag bits of a response's last byte.
pub const FLAG_MASK: u8 = 0xE0;

const TYPE_JOB: u8 = 0x20;
const TYPE_CMD: u8 = 0x40;

/// Whether a command addresses one chip or the whole chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Group {
    /// One chip, selected by the address in the first payload byte.
    Single = 0x00,
    /// Every chip on the chain.
    All = 0x10,
}

/// The four command operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    /// Assign a chain address to the next unaddressed chip.
    SetAddress = 0x00,
    /// Write a register.
    Write = 0x01,
    /// Read a register; the chip answers with a response frame.
    Read = 0x02,
    /// Chain-inactive: precedes the address assignment pass.
    Inactive = 0x03,
}

/// The header byte: type | group | op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header(u8);

impl Header {
    /// Header of a command frame.
    pub const fn cmd(group: Group, op: Op) -> Self {
        Header(TYPE_CMD | group as u8 | op as u8)
    }

    /// Header of a job frame. The reference driver always uses
    /// `TYPE_JOB | GROUP_SINGLE | CMD_WRITE`.
    pub const fn job() -> Self {
        Header(TYPE_JOB | Group::Single as u8 | Op::Write as u8)
    }

    /// The raw byte.
    pub const fn byte(self) -> u8 {
        self.0
    }

    /// True for a job frame (CRC-16), false for a command (CRC-5).
    pub const fn is_job(self) -> bool {
        self.0 & TYPE_JOB != 0
    }

    /// Addressing group of a command header.
    pub const fn group(self) -> Group {
        if self.0 & 0x10 != 0 {
            Group::All
        } else {
            Group::Single
        }
    }

    /// Operation of a command header.
    pub const fn op(self) -> Op {
        match self.0 & 0x03 {
            0 => Op::SetAddress,
            1 => Op::Write,
            2 => Op::Read,
            _ => Op::Inactive,
        }
    }

    /// Parse a header byte. Rejects bits the protocol does not define and
    /// bytes that claim to be both a job and a command (or neither).
    pub const fn from_byte(b: u8) -> Option<Self> {
        if b & !(TYPE_JOB | TYPE_CMD | 0x10 | 0x03) != 0 {
            return None;
        }
        if (b & TYPE_JOB != 0) == (b & TYPE_CMD != 0) {
            return None;
        }
        Some(Header(b))
    }
}

/// Why a frame could not be built or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The output buffer is too small for the frame.
    BufferTooSmall {
        /// Bytes the frame needs.
        needed: usize,
    },
    /// Payload longer than the frame cap allows.
    PayloadTooLong,
    /// Response does not start with `AA 55`.
    BadPreamble,
    /// Response failed its CRC-5.
    BadCrc,
    /// Response slice is not [`RESPONSE_LEN`] bytes.
    BadLength,
}

/// Encode a command frame into `buf`. Returns the frame length.
///
/// The first payload byte is the chip address (ignored by the chip for
/// [`Group::All`], but still transmitted — the reference driver sends
/// `0x00`).
pub fn encode_cmd(
    buf: &mut [u8],
    group: Group,
    op: Op,
    payload: &[u8],
) -> Result<usize, FrameError> {
    encode(buf, Header::cmd(group, op), payload)
}

/// Encode a job frame into `buf`. Returns the frame length.
pub fn encode_job(buf: &mut [u8], payload: &[u8]) -> Result<usize, FrameError> {
    encode(buf, Header::job(), payload)
}

fn encode(buf: &mut [u8], header: Header, payload: &[u8]) -> Result<usize, FrameError> {
    let crc_len = if header.is_job() { 2 } else { 1 };
    // preamble(2) + header(1) + len(1) + payload + crc
    let total = 4 + payload.len() + crc_len;
    if total > MAX_FRAME_LEN {
        return Err(FrameError::PayloadTooLong);
    }
    if buf.len() < total {
        return Err(FrameError::BufferTooSmall { needed: total });
    }
    // `len` counts header + len byte + payload + crc: everything after the
    // preamble. That is payload + 3 for commands and payload + 4 for jobs.
    let len_byte =
        u8::try_from(payload.len() + 2 + crc_len).map_err(|_| FrameError::PayloadTooLong)?;

    buf[0..2].copy_from_slice(&TX_PREAMBLE);
    buf[2] = header.byte();
    buf[3] = len_byte;
    buf[4..4 + payload.len()].copy_from_slice(payload);

    let body = &buf[2..4 + payload.len()];
    if header.is_job() {
        let c = crc16(body).to_be_bytes();
        buf[4 + payload.len()] = c[0];
        buf[5 + payload.len()] = c[1];
    } else {
        buf[4 + payload.len()] = crc5(body);
    }
    Ok(total)
}

/// Build a response frame the way the chip does. This is what the
/// simulator sends; firmware never calls it.
///
/// Only the top three bits of `flags` are used ([`FLAG_MASK`]); the low
/// five are replaced by the CRC.
pub fn encode_response(buf: &mut [u8; RESPONSE_LEN], payload: &[u8; 8], flags: u8) {
    buf[0..2].copy_from_slice(&RX_PREAMBLE);
    buf[2..10].copy_from_slice(payload);
    buf[10] = (flags & FLAG_MASK) | response_tag(payload, flags);
}

/// A chip response after framing and CRC have been checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Response {
    /// The eight payload bytes, meaning per chip profile.
    pub payload: [u8; 8],
    /// The three flag bits (bits 7..5), low bits zero.
    pub flags: u8,
}

impl Response {
    /// Job result (nonce) rather than a register read.
    pub const fn is_job(&self) -> bool {
        self.flags & FLAG_JOB != 0
    }
}

/// Parse and verify one response frame.
///
/// Rejects wrong length, wrong preamble and bad CRC — the caller decides
/// whether to resynchronise or count a `Desync`. Nothing here consumes
/// a stream; feeding bytes one at a time is the decoder's job (M1, part 2).
pub fn decode_response(bytes: &[u8]) -> Result<Response, FrameError> {
    if bytes.len() != RESPONSE_LEN {
        return Err(FrameError::BadLength);
    }
    if bytes[0..2] != RX_PREAMBLE {
        return Err(FrameError::BadPreamble);
    }
    if !verify5(&bytes[2..]) {
        return Err(FrameError::BadCrc);
    }
    let mut payload = [0u8; 8];
    payload.copy_from_slice(&bytes[2..10]);
    Ok(Response {
        payload,
        flags: bytes[10] & FLAG_MASK,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_bytes_match_the_reference_driver() {
        assert_eq!(Header::cmd(Group::All, Op::Write).byte(), 0x51);
        assert_eq!(Header::cmd(Group::All, Op::Read).byte(), 0x52);
        assert_eq!(Header::cmd(Group::All, Op::Inactive).byte(), 0x53);
        assert_eq!(Header::cmd(Group::Single, Op::SetAddress).byte(), 0x40);
        assert_eq!(Header::cmd(Group::Single, Op::Write).byte(), 0x41);
        assert_eq!(Header::cmd(Group::Single, Op::Read).byte(), 0x42);
        assert_eq!(Header::job().byte(), 0x21);
        assert!(Header::job().is_job());
        assert!(!Header::cmd(Group::All, Op::Write).is_job());
    }

    #[test]
    fn header_roundtrips_and_rejects_undefined_bits() {
        for b in [0x40u8, 0x41, 0x42, 0x43, 0x50, 0x51, 0x52, 0x53, 0x21, 0x31] {
            let h = Header::from_byte(b).expect("defined header");
            assert_eq!(h.byte(), b);
            if !h.is_job() {
                assert_eq!(Header::cmd(h.group(), h.op()), h);
            }
        }
        assert_eq!(Header::from_byte(0x00), None, "no type bit");
        assert_eq!(Header::from_byte(0x61), None, "both type bits");
        assert_eq!(Header::from_byte(0x44), None, "undefined bit 2");
        assert_eq!(Header::from_byte(0xC1), None, "undefined bit 7");
    }

    /// Version-mask command, byte exact, from `test_version_rolling.c`.
    #[test]
    fn encode_cmd_reproduces_the_version_mask_frame() {
        let mut buf = [0u8; 16];
        let n = encode_cmd(
            &mut buf,
            Group::All,
            Op::Write,
            &[0x00, 0xa4, 0x90, 0x00, 0xff, 0xff],
        )
        .unwrap();
        assert_eq!(
            &buf[..n],
            &[0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c]
        );
    }

    /// Chain-inactive and set-address, from `bm1370.c` comments.
    #[test]
    fn encode_cmd_reproduces_chain_inactive_and_set_address() {
        let mut buf = [0u8; 16];
        let n = encode_cmd(&mut buf, Group::All, Op::Inactive, &[0x00, 0x00]).unwrap();
        assert_eq!(&buf[..n], &[0x55, 0xAA, 0x53, 0x05, 0x00, 0x00, 0x03]);

        let n = encode_cmd(&mut buf, Group::Single, Op::SetAddress, &[0x00, 0x00]).unwrap();
        assert_eq!(&buf[..n], &[0x55, 0xAA, 0x40, 0x05, 0x00, 0x00, 0x1C]);
    }

    /// The 88-byte job frame from `test_bitmain_job_packets.c`, BM1370
    /// variant (job id 0x18, CRC 0x05d4).
    #[test]
    fn encode_job_reproduces_the_golden_bm1370_work_packet() {
        let mut payload = vec![0x18, 0x01];
        payload.extend_from_slice(&[0x78, 0x56, 0x34, 0x12]); // starting nonce LE
        payload.extend_from_slice(&[0x01, 0xdd, 0x05, 0x17]); // nbits LE
        payload.extend_from_slice(&[0xd8, 0x8b, 0x65, 0x64]); // ntime LE
        payload.extend(0x00u8..0x20); // merkle root
        payload.extend(0x20u8..0x40); // prev block hash
        payload.extend_from_slice(&[0x04, 0x00, 0x00, 0x20]); // version LE
        assert_eq!(payload.len(), 82);

        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = encode_job(&mut buf, &payload).unwrap();
        assert_eq!(n, 88);
        assert_eq!(&buf[..4], &[0x55, 0xaa, 0x21, 0x56]);
        assert_eq!(&buf[4..86], &payload[..]);
        assert_eq!(&buf[86..88], &[0x05, 0xd4]);
    }

    #[test]
    fn encode_reports_a_short_buffer_with_the_needed_size() {
        let mut buf = [0u8; 6];
        let e = encode_cmd(&mut buf, Group::All, Op::Write, &[0; 6]).unwrap_err();
        assert_eq!(e, FrameError::BufferTooSmall { needed: 11 });
    }

    #[test]
    fn encode_refuses_a_payload_past_the_frame_cap() {
        let mut buf = [0u8; 256];
        let e = encode_job(&mut buf, &[0; 200]).unwrap_err();
        assert_eq!(e, FrameError::PayloadTooLong);
    }

    /// The BM1368 chip-id reply from the `bm1370.c` comment, rebuilt by
    /// `encode_response` and read back by `decode_response`.
    #[test]
    fn encode_response_reproduces_the_chip_id_reply_from_a_comment() {
        let payload = [0x13, 0x68, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut buf = [0u8; RESPONSE_LEN];
        encode_response(&mut buf, &payload, 0x00);
        assert_eq!(
            buf,
            [0xAA, 0x55, 0x13, 0x68, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0F]
        );
        let r = decode_response(&buf).unwrap();
        assert_eq!(r.payload, payload);
        assert!(!r.is_job());
    }

    #[test]
    fn decode_response_accepts_a_job_reply_and_reads_its_flag() {
        let payload = [0x9b, 0x04, 0x4c, 0x0a, 0x00, 0x23, 0x00, 0x01];
        let mut buf = [0u8; RESPONSE_LEN];
        encode_response(&mut buf, &payload, FLAG_JOB);
        let r = decode_response(&buf).unwrap();
        assert!(r.is_job());
        assert_eq!(r.flags, FLAG_JOB);
        assert_eq!(r.payload, payload);
    }

    /// The flag bits are under the CRC. Flipping one must fail the check —
    /// this pins the assumption a hardware capture can confirm or refute
    /// in one place.
    #[test]
    fn decode_response_rejects_bad_preamble_crc_flag_and_length() {
        let mut good = [0u8; RESPONSE_LEN];
        encode_response(&mut good, &[1, 2, 3, 4, 5, 6, 7, 8], FLAG_JOB);
        assert!(decode_response(&good).is_ok());

        let mut bad = good;
        bad[0] = 0x55;
        assert_eq!(decode_response(&bad), Err(FrameError::BadPreamble));

        let mut bad = good;
        bad[5] ^= 0x10;
        assert_eq!(decode_response(&bad), Err(FrameError::BadCrc));

        let mut bad = good;
        bad[10] ^= FLAG_JOB;
        assert_eq!(
            decode_response(&bad),
            Err(FrameError::BadCrc),
            "flag is covered"
        );

        assert_eq!(decode_response(&good[..10]), Err(FrameError::BadLength));
    }

    proptest::proptest! {
        /// Whatever we encode as a command, its trailing CRC matches the body.
        #[test]
        fn encoded_commands_carry_a_matching_crc5(
            payload in proptest::collection::vec(0u8..=255, 2..64),
            all in proptest::bool::ANY,
            op in 0u8..4,
        ) {
            let group = if all { Group::All } else { Group::Single };
            let op = match op { 0 => Op::SetAddress, 1 => Op::Write, 2 => Op::Read, _ => Op::Inactive };
            let mut buf = [0u8; MAX_FRAME_LEN];
            let n = encode_cmd(&mut buf, group, op, &payload).unwrap();
            proptest::prop_assert_eq!(n, payload.len() + 5);
            proptest::prop_assert_eq!(buf[3] as usize, payload.len() + 3);
            proptest::prop_assert_eq!(crc5(&buf[2..n - 1]), buf[n - 1]);
            proptest::prop_assert_eq!(Header::from_byte(buf[2]), Some(Header::cmd(group, op)));
        }

        #[test]
        fn encoded_jobs_carry_a_matching_crc16(
            payload in proptest::collection::vec(0u8..=255, 1..120),
        ) {
            let mut buf = [0u8; MAX_FRAME_LEN];
            let n = encode_job(&mut buf, &payload).unwrap();
            proptest::prop_assert_eq!(n, payload.len() + 6);
            proptest::prop_assert_eq!(buf[3] as usize, payload.len() + 4);
            let want = crc16(&buf[2..n - 2]).to_be_bytes();
            proptest::prop_assert_eq!(&buf[n - 2..n], &want[..]);
        }

        /// Simulator → decoder round trip: what the chip side builds, the
        /// firmware side reads back unchanged.
        #[test]
        fn responses_round_trip(
            payload in proptest::array::uniform8(0u8..=255),
            flags in 0u8..=255,
        ) {
            let mut buf = [0u8; RESPONSE_LEN];
            encode_response(&mut buf, &payload, flags);
            let r = decode_response(&buf).unwrap();
            proptest::prop_assert_eq!(r.payload, payload);
            proptest::prop_assert_eq!(r.flags, flags & FLAG_MASK);
        }

        /// Any eleven bytes: decode never panics, and the only way in is
        /// the right preamble plus a zero CRC.
        #[test]
        fn decode_response_never_panics(bytes in proptest::array::uniform11(0u8..=255)) {
            let r = decode_response(&bytes);
            let should_pass = bytes[0..2] == RX_PREAMBLE && verify5(&bytes[2..]);
            proptest::prop_assert_eq!(r.is_ok(), should_pass);
        }
    }
}
