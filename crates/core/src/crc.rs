//! The two checksums on the BM13xx wire.
//!
//! Command frames carry a 5-bit CRC, job frames a 16-bit one. Both are
//! computed over the frame *without* its two preamble bytes: from the
//! header byte up to (not including) the checksum itself.
//!
//! Responses from the chip also carry a 5-bit CRC, but with a twist that
//! is easy to get wrong (this crate did, once): the last byte of a
//! response is `flags:3 | crc:5`, and the chip runs its CRC over the
//! eight payload bytes **and the three flag bits** before appending the
//! five CRC bits. A receiver therefore verifies by running [`crc5`] over
//! all nine bytes and expecting zero — the CRC bits cancel the register
//! exactly because nothing sits between them and the data they cover.
//! Command frames do *not* have that property: their last byte is
//! `000 | crc:5`, and the three zero bits between data and CRC shift the
//! register before the CRC bits arrive. So a command is checked with
//! `crc5(body) == last`, a response with `crc5(all nine) == 0`. See
//! [`response_tag`] and [`verify5`].
//!
//! Bit-serial implementations, on purpose. A few dozen bytes per frame at
//! a handful of frames per second is nothing on any core we target, and a
//! bit-serial CRC has no table to get subtly wrong. If profiling on the
//! ESP32-S3 ever says otherwise, a `const fn`-generated table fits here
//! without changing the API.
//!
//! Source: ESP-Miner `components/asic/crc.c` (polynomials, init values,
//! bit order) and `asic_common.c` (`receive_work`, the zero check). Test
//! vectors: see the test module.

/// One step of the CRC-5 register: polynomial x⁵ + x² + 1, MSB first.
#[inline]
const fn crc5_step(crc: u8, bit: u8) -> u8 {
    let new_bit = ((crc >> 4) ^ bit) & 1;
    (((crc << 1) | new_bit) ^ (new_bit << 2)) & 0x1F
}

/// Feed the top `n` bits of `byte` (MSB first) into the register.
#[inline]
const fn crc5_bits(mut crc: u8, byte: u8, n: u8) -> u8 {
    let mut i = 0;
    while i < n {
        crc = crc5_step(crc, (byte >> (7 - i)) & 1);
        i += 1;
    }
    crc
}

/// CRC-5 as the BM13xx uses it on command frames and responses.
///
/// Polynomial x⁵ + x² + 1 (0x05), initial value 0x1F, MSB first, no final
/// XOR. Returns the 5-bit remainder in the low bits.
///
/// For a *command* frame this is the byte to send after the payload. For
/// a *response* see [`response_tag`] (to build one) and [`verify5`] (to
/// check one).
pub fn crc5(data: &[u8]) -> u8 {
    let mut crc: u8 = 0x1F;
    for &byte in data {
        crc = crc5_bits(crc, byte, 8);
    }
    crc
}

/// The five CRC bits the chip puts into the last byte of a response.
///
/// Covers the eight payload bytes and the three flag bits (bits 7..5 of
/// `flags`; the rest of `flags` is ignored). The caller assembles the
/// last byte as `(flags & 0xE0) | response_tag(payload, flags)`, and a
/// response built that way passes [`verify5`].
pub fn response_tag(payload: &[u8; 8], flags: u8) -> u8 {
    crc5_bits(crc5(payload), flags, 3)
}

/// True when `response_body` (everything after the `AA 55` preamble,
/// last byte included) carries a CRC-5 that matches.
///
/// This is the receiver-side check for chip responses: feeding all nine
/// bytes through the register must leave it at zero. It is *not* the
/// check for command frames — those are verified with
/// `crc5(body) == trailing byte`.
pub fn verify5(response_body: &[u8]) -> bool {
    !response_body.is_empty() && crc5(response_body) == 0
}

/// CRC-16 as the BM13xx uses it on job frames.
///
/// CRC-16/CCITT-FALSE: polynomial 0x1021, initial value 0xFFFF, MSB
/// first, no reflection, no final XOR. Transmitted big-endian after the
/// job payload.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whole command frames as they go over the wire, checksum included.
    /// `crc5` over bytes[2..len-1] must equal bytes[len-1].
    ///
    /// Provenance is per vector. "test" = an assertion in an ESP-Miner
    /// test (belt and braces); "comment" = a `// TX: …` comment in
    /// `bm1370.c` (a captured dump, but nobody re-checks comments).
    #[rustfmt::skip]
    const CMD_FRAMES: &[(&str, &[u8])] = &[
        // components/asic/test/test_version_rolling.c, version mask cases
        ("test",    &[0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0x00, 0x00, 0x17]),
        ("test",    &[0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0x00, 0x01, 0x12]),
        ("test",    &[0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0x01, 0x00, 0x0b]),
        ("test",    &[0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0x12, 0x34, 0x06]),
        ("test",    &[0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c]),
        // components/asic/bm1370.c init sequence comments
        ("comment", &[0x55, 0xAA, 0x51, 0x09, 0x00, 0xA8, 0x00, 0x07, 0x00, 0x00, 0x03]),
        ("comment", &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x18, 0xF0, 0x00, 0xC1, 0x00, 0x04]),
        ("comment", &[0x55, 0xAA, 0x53, 0x05, 0x00, 0x00, 0x03]),
        ("comment", &[0x55, 0xAA, 0x40, 0x05, 0x00, 0x00, 0x1C]),
        ("comment", &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x8B, 0x00, 0x12]),
        ("comment", &[0x55, 0xAA, 0x41, 0x09, 0x00, 0xA8, 0x00, 0x07, 0x01, 0xF0, 0x15]),
        ("comment", &[0x55, 0xAA, 0x41, 0x09, 0x00, 0x18, 0xF0, 0x00, 0xC1, 0x00, 0x0C]),
        ("comment", &[0x55, 0xAA, 0x41, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x8B, 0x00, 0x1A]),
        ("comment", &[0x55, 0xAA, 0x41, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x82, 0xAA, 0x05]),
        ("comment", &[0x55, 0xAA, 0x51, 0x09, 0x00, 0xB9, 0x00, 0x00, 0x44, 0x80, 0x0D]),
        ("comment", &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x54, 0x00, 0x00, 0x00, 0x02, 0x18]),
        ("comment", &[0x55, 0xAA, 0x51, 0x09, 0x00, 0x3C, 0x80, 0x00, 0x8D, 0xEE, 0x1B]),
    ];

    #[test]
    fn crc5_matches_every_known_command_frame() {
        for (src, frame) in CMD_FRAMES {
            let body = &frame[2..frame.len() - 1];
            let want = frame[frame.len() - 1];
            assert_eq!(
                crc5(body),
                want,
                "[{src}] frame {frame:02x?}: crc5 over {body:02x?}"
            );
        }
    }

    #[test]
    fn a_flipped_bit_in_a_command_body_changes_its_crc5() {
        for (_, frame) in CMD_FRAMES {
            let want = frame[frame.len() - 1];
            let mut bad = frame[2..frame.len() - 1].to_vec();
            bad[2] ^= 0x01;
            assert_ne!(crc5(&bad), want, "flipped {bad:02x?} must not match");
        }
    }

    /// The trap this module's doc comment describes. A command frame's
    /// last byte is `000|crc`, so the nine-byte zero check that works for
    /// responses does NOT hold for commands. Pinned so nobody "fixes"
    /// `verify5` to accept commands and breaks response checking.
    #[test]
    fn command_frames_do_not_pass_the_response_zero_check() {
        let frame = &[
            0x55, 0xaa, 0x51, 0x09, 0x00, 0xa4, 0x90, 0x00, 0xff, 0xff, 0x1c,
        ];
        assert!(!verify5(&frame[2..]));
    }

    /// The chip-id reply a BM1368 gives to a register-0 read, from a
    /// comment in `bm1370.c`: `AA 55 13 68 00 00 00 00 00 00 0F`.
    ///
    /// This is the one response vector we have, and it pins the flag-bit
    /// rule: the CRC over the eight bytes alone is 0x0C, over the eight
    /// bytes plus the three (zero) flag bits it is 0x0F — the comment's
    /// value — and the nine-byte zero check passes. Marked "comment", but
    /// the agreement with ESP-Miner's own receiver check makes it
    /// trustworthy.
    #[test]
    fn response_tag_reproduces_the_bm1368_chip_id_reply_from_a_comment() {
        let payload = [0x13, 0x68, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(
            crc5(&payload),
            0x0C,
            "eight bytes alone: not the wire value"
        );
        assert_eq!(
            response_tag(&payload, 0x00),
            0x0F,
            "eight bytes + 000 flag bits"
        );
        let mut body = [0u8; 9];
        body[..8].copy_from_slice(&payload);
        body[8] = 0x0F;
        assert!(verify5(&body));
    }

    /// Predicted, not yet observed: what a BM1370 should answer on the
    /// chip-id read. `AA 55 13 70 00 00 00 00 00 00 10`. First thing to
    /// check against a Gamma capture (stage B / HIL).
    #[test]
    fn response_tag_predicts_the_bm1370_chip_id_reply() {
        let payload = [0x13, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(response_tag(&payload, 0x00), 0x10);
    }

    #[test]
    fn verify5_rejects_empty() {
        assert!(!verify5(&[]));
    }

    /// Job frame template from `test_bitmain_job_packets.c`
    /// ("BM13xx work packets preserve complete header fields byte exact").
    /// 88 bytes: preamble(2) header(1) len(1) job_id(1) midstates(1)
    /// nonce(4) nbits(4) ntime(4) merkle(32) prev(32) version(4) crc16(2).
    /// The CRC covers bytes[2..86]. Job id and CRC differ per chip family
    /// because the job-id step differs (BM1366: 8, BM1368/70/73: 24).
    fn job_frame(job_id: u8, crc: [u8; 2]) -> Vec<u8> {
        let mut f = vec![0x55, 0xaa, 0x21, 0x56, job_id, 0x01];
        f.extend_from_slice(&[0x78, 0x56, 0x34, 0x12, 0x01, 0xdd, 0x05, 0x17]);
        f.extend_from_slice(&[0xd8, 0x8b, 0x65, 0x64]);
        f.extend((0x00u8..0x20).chain(0x20..0x40));
        f.extend_from_slice(&[0x04, 0x00, 0x00, 0x20]);
        f.extend_from_slice(&crc);
        assert_eq!(f.len(), 88);
        f
    }

    #[test]
    fn crc16_matches_the_bm1370_golden_job_frame() {
        let f = job_frame(0x18, [0x05, 0xd4]);
        assert_eq!(crc16(&f[2..86]), 0x05d4, "BM1368/BM1370/BM1373 job id 0x18");
    }

    #[test]
    fn crc16_matches_the_bm1366_golden_job_frame() {
        let f = job_frame(0x08, [0xd6, 0x21]);
        assert_eq!(crc16(&f[2..86]), 0xd621, "BM1366 job id 0x08");
    }

    /// CRC-16/CCITT-FALSE check value from the catalogue: "123456789" → 0x29B1.
    #[test]
    fn crc16_is_ccitt_false() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn crc5_of_nothing_is_the_init_value() {
        assert_eq!(crc5(&[]), 0x1F);
    }

    proptest::proptest! {
        /// A response assembled the way the chip does it always verifies,
        /// whatever the payload and flags. This is the contract between
        /// the simulator (which builds responses) and the decoder.
        #[test]
        fn a_response_built_with_response_tag_verifies(
            payload in proptest::array::uniform8(0u8..=255),
            flags in 0u8..=255,
        ) {
            let mut body = [0u8; 9];
            body[..8].copy_from_slice(&payload);
            body[8] = (flags & 0xE0) | response_tag(&payload, flags);
            proptest::prop_assert!(verify5(&body));
        }

        /// …and only the top three bits of `flags` matter.
        #[test]
        fn response_tag_ignores_the_low_flag_bits(
            payload in proptest::array::uniform8(0u8..=255),
            flags in 0u8..=255,
            junk in 0u8..32,
        ) {
            proptest::prop_assert_eq!(
                response_tag(&payload, flags),
                response_tag(&payload, (flags & 0xE0) | junk)
            );
        }

        /// A single flipped bit anywhere in the body must change the CRC.
        #[test]
        fn crc5_sees_every_bit(body in proptest::collection::vec(0u8..=255, 1..16),
                               idx in 0usize..128) {
            let idx = idx % (body.len() * 8);
            let mut flipped = body.clone();
            flipped[idx / 8] ^= 1 << (idx % 8);
            proptest::prop_assert_ne!(crc5(&body), crc5(&flipped));
        }

        #[test]
        fn crc16_sees_every_bit(body in proptest::collection::vec(0u8..=255, 1..96),
                                idx in 0usize..768) {
            let idx = idx % (body.len() * 8);
            let mut flipped = body.clone();
            flipped[idx / 8] ^= 1 << (idx % 8);
            proptest::prop_assert_ne!(crc16(&body), crc16(&flipped));
        }
    }
}
