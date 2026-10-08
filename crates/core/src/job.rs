//! The work packet: what goes into a job frame's payload.
//!
//! On the BM1366/68/70/73 a job is 82 bytes, little-endian integers,
//! hashes as-is:
//!
//! ```text
//! off  len  field
//!  0    1   job_id
//!  1    1   num_midstates   (1: the chip computes its own midstate)
//!  2    4   starting_nonce  LE
//!  6    4   nbits           LE
//! 10    4   ntime           LE
//! 14   32   merkle_root
//! 46   32   prev_block_hash
//! 78    4   version         LE
//! ```
//!
//! Wrapped by [`crate::frame::encode_job`] this is the 88-byte frame the
//! reference driver sends. Job ids advance in steps of 24 modulo 128 on
//! these chips (8 on the BM1366); the chip echoes the id back in bits
//! 7..4 of a result's id byte, which is why [`JobIds`] exists rather
//! than "just a counter".
//!
//! Source: ESP-Miner `bm1370.h` (`BM1370_job`), `bm1370.c`
//! (`BM1370_send_work`), `test_bitmain_job_packets.c` (the golden frame).
//!
//! The BM1397 takes a different packet ([`MidstateJob`]): no header
//! fields to hash from scratch, but the SHA-256 midstate of the header's
//! first block, up to four of them (one per version the host rolled),
//! plus the header tail the chip still needs:
//!
//! ```text
//! off  len  field
//!  0    1   job_id          multiple of 4 below 128
//!  1    1   num_midstates   1 or 4
//!  2    4   starting_nonce  LE
//!  6    4   nbits           LE
//! 10    4   ntime           LE
//! 14    4   merkle_root[28..32]  (the tail of the merkle root)
//! 18   32   midstate 0      words 7..0, each big-endian
//! 50   32   midstate 1      (only with num_midstates 4)
//! 82   32   midstate 2
//! 114  32   midstate 3
//! ```
//!
//! Source: ESP-Miner `bm1397.h` (`job_packet`), `bm_job_packet.c`
//! (`bm1397_build_job_packet`), the packet vector in
//! `test_bm_job_packets.c`; WantClue NerdNOS `bm1397.cpp`
//! (`BM1397_send_work`, `construct_bm_job`), which builds the same bytes.

/// Length of the job payload on the BM1366/68/70/73.
pub const JOB_LEN: usize = 82;
/// Length of a BM1397 job payload with one midstate.
pub const MIDSTATE_JOB_LEN_1: usize = 50;
/// Length of a BM1397 job payload with four midstates.
pub const MIDSTATE_JOB_LEN_4: usize = 146;
/// Midstates a BM1397 job can carry at most.
pub const MAX_MIDSTATES: usize = 4;

/// One unit of work for the chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Job {
    /// Slot id, 0..128, multiple of the chip's step. See [`JobIds`].
    pub job_id: u8,
    /// First nonce to try. The chip rolls from here.
    pub starting_nonce: u32,
    /// Compact target from the block header.
    pub nbits: u32,
    /// Block time.
    pub ntime: u32,
    /// Merkle root, header byte order.
    pub merkle_root: [u8; 32],
    /// Previous block hash, header byte order.
    pub prev_block_hash: [u8; 32],
    /// Block version, before rolling.
    pub version: u32,
}

impl Job {
    /// Serialise into the 82-byte payload.
    pub fn write(&self, out: &mut [u8; JOB_LEN]) {
        out[0] = self.job_id;
        out[1] = 1; // num_midstates: hardware midstate
        out[2..6].copy_from_slice(&self.starting_nonce.to_le_bytes());
        out[6..10].copy_from_slice(&self.nbits.to_le_bytes());
        out[10..14].copy_from_slice(&self.ntime.to_le_bytes());
        out[14..46].copy_from_slice(&self.merkle_root);
        out[46..78].copy_from_slice(&self.prev_block_hash);
        out[78..82].copy_from_slice(&self.version.to_le_bytes());
    }

    /// The payload as an array.
    pub fn to_bytes(&self) -> [u8; JOB_LEN] {
        let mut b = [0u8; JOB_LEN];
        self.write(&mut b);
        b
    }

    /// Parse a payload back. Used by the simulator, which receives what
    /// the firmware sends.
    pub fn from_bytes(b: &[u8; JOB_LEN]) -> Self {
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let mut merkle_root = [0u8; 32];
        merkle_root.copy_from_slice(&b[14..46]);
        let mut prev_block_hash = [0u8; 32];
        prev_block_hash.copy_from_slice(&b[46..78]);
        Job {
            job_id: b[0],
            starting_nonce: u32_at(2),
            nbits: u32_at(6),
            ntime: u32_at(10),
            merkle_root,
            prev_block_hash,
            version: u32_at(78),
        }
    }
}

/// Reverse the order of the eight 32-bit words of a hash, bytes within
/// a word untouched (ESP-Miner `reverse_32bit_words`). Its own inverse.
/// The BM1370 job carries the previous hash and the merkle root in this
/// order; the BM1397 midstate needs them back in header order.
pub fn reverse_words(h: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[(7 - i) * 4..(7 - i) * 4 + 4]);
    }
    out
}

/// The next version in a rolling sequence: the bits under `mask` count
/// up by one, bits outside it are kept, and a carry out of the mask
/// lands in the bit above the one that overflowed (ESP-Miner
/// `increment_bitmask`, `utils.c`, transcribed as is; WantClue has the
/// same function). With the BIP 320 mask `0x1fffe000` that is
/// `version + 0x2000`.
// `isolate_lowest_one` is newer than the firmware's toolchain.
#[allow(clippy::manual_isolate_lowest_one)]
pub fn increment_bitmask(value: u32, mask: u32) -> u32 {
    if mask == 0 {
        return value;
    }
    let carry = (value & mask).wrapping_add(mask & mask.wrapping_neg());
    let overflow = carry & !mask;
    let new_value = (value & !mask) | (carry & mask);
    if overflow > 0 {
        increment_bitmask(new_value, overflow << 1)
    } else {
        new_value
    }
}

/// `increment_bitmask` applied `times` times: the version midstate
/// number `times` hashed under.
pub fn rolled_version(base: u32, mask: u32, times: u8) -> u32 {
    let mut v = base;
    for _ in 0..times {
        v = increment_bitmask(v, mask);
    }
    v
}

/// A BM1397 job: midstates instead of header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidstateJob {
    /// Slot id, a multiple of 4 below 128 (the chip echoes bits 7..2 and
    /// puts the midstate number in bits 1..0).
    pub job_id: u8,
    /// Midstates sent: 1 or 4.
    pub num_midstates: u8,
    /// First nonce to try.
    pub starting_nonce: u32,
    /// Compact target.
    pub nbits: u32,
    /// Block time.
    pub ntime: u32,
    /// Bytes 28..32 of the merkle root, header order.
    pub merkle_tail: [u8; 4],
    /// The midstates as they go on the wire (words 7..0, each
    /// big-endian); entries past `num_midstates` are zero.
    pub midstates: [[u8; 32]; MAX_MIDSTATES],
}

impl MidstateJob {
    /// Build the job for a header: `prev_hash` and `merkle_root` in
    /// header order, `version` the base version, midstate `k` hashed
    /// under the version rolled `k` times by `version_mask`.
    /// `num_midstates` is clamped to 1..=4; with a zero mask only one
    /// midstate is sent (ESP-Miner `bm1397_build_job_packet`).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        job_id: u8,
        num_midstates: u8,
        starting_nonce: u32,
        nbits: u32,
        ntime: u32,
        prev_hash: &[u8; 32],
        merkle_root: &[u8; 32],
        version: u32,
        version_mask: u32,
    ) -> Self {
        let n = if version_mask == 0 {
            1
        } else {
            num_midstates.clamp(1, MAX_MIDSTATES as u8)
        };
        let mut block = [0u8; 64];
        block[4..36].copy_from_slice(prev_hash);
        block[36..64].copy_from_slice(&merkle_root[..28]);
        let mut midstates = [[0u8; 32]; MAX_MIDSTATES];
        let mut v = version;
        for (k, m) in midstates.iter_mut().enumerate().take(usize::from(n)) {
            if k > 0 {
                v = increment_bitmask(v, version_mask);
            }
            block[..4].copy_from_slice(&v.to_le_bytes());
            let state = crate::sha256::midstate(&block);
            // Words 7..0, each big-endian: `reverse_bytes` of the C
            // state as `memcpy` lays it out (little-endian words), or
            // `reverse_32bit_words` of its big-endian serialisation —
            // the same bytes either way.
            for (i, w) in state.iter().rev().enumerate() {
                m[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
            }
        }
        let mut merkle_tail = [0u8; 4];
        merkle_tail.copy_from_slice(&merkle_root[28..32]);
        MidstateJob {
            job_id,
            num_midstates: n,
            starting_nonce,
            nbits,
            ntime,
            merkle_tail,
            midstates,
        }
    }

    /// Payload length on the wire: 50 with one midstate, 146 with four
    /// (2 or 3 midstates are padded to four, as the reference does).
    pub const fn payload_len(&self) -> usize {
        if self.num_midstates <= 1 {
            MIDSTATE_JOB_LEN_1
        } else {
            MIDSTATE_JOB_LEN_4
        }
    }

    /// Serialise into `out`; returns the payload length.
    pub fn write(&self, out: &mut [u8; MIDSTATE_JOB_LEN_4]) -> usize {
        out.fill(0);
        out[0] = self.job_id;
        out[1] = if self.num_midstates <= 1 {
            1
        } else {
            MAX_MIDSTATES as u8
        };
        out[2..6].copy_from_slice(&self.starting_nonce.to_le_bytes());
        out[6..10].copy_from_slice(&self.nbits.to_le_bytes());
        out[10..14].copy_from_slice(&self.ntime.to_le_bytes());
        out[14..18].copy_from_slice(&self.merkle_tail);
        let n = usize::from(out[1]);
        for k in 0..n {
            out[18 + 32 * k..50 + 32 * k].copy_from_slice(&self.midstates[k]);
        }
        self.payload_len()
    }

    /// Parse a payload of 50 or 146 bytes (the simulator's side).
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != MIDSTATE_JOB_LEN_1 && b.len() != MIDSTATE_JOB_LEN_4 {
            return None;
        }
        let n = b[1];
        let want = if n <= 1 {
            MIDSTATE_JOB_LEN_1
        } else {
            MIDSTATE_JOB_LEN_4
        };
        if b.len() != want || n > MAX_MIDSTATES as u8 {
            return None;
        }
        let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
        let mut merkle_tail = [0u8; 4];
        merkle_tail.copy_from_slice(&b[14..18]);
        let mut midstates = [[0u8; 32]; MAX_MIDSTATES];
        for (k, m) in midstates.iter_mut().enumerate().take(usize::from(n.max(1))) {
            m.copy_from_slice(&b[18 + 32 * k..50 + 32 * k]);
        }
        Some(MidstateJob {
            job_id: b[0],
            num_midstates: n.max(1),
            starting_nonce: u32_at(2),
            nbits: u32_at(6),
            ntime: u32_at(10),
            merkle_tail,
            midstates,
        })
    }
}

/// The job-id sequence: `id = (id + step) % 128`, starting so that the
/// first id handed out is `step` — exactly what the reference driver's
/// static counter does on its first call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobIds {
    last: u8,
    step: u8,
}

impl JobIds {
    /// The BM1368/BM1370/BM1373 sequence: 24, 48, 72, 96, 120, 16, …
    pub const fn bm1370() -> Self {
        JobIds::with_step(24)
    }

    /// The BM1366 sequence: 8, 16, 24, …
    pub const fn bm1366() -> Self {
        JobIds::with_step(8)
    }

    /// A sequence with an arbitrary step.
    pub const fn with_step(step: u8) -> Self {
        JobIds { last: 0, step }
    }

    /// The step between ids.
    pub const fn step(&self) -> u8 {
        self.step
    }

    /// Advance and return the next id in the sequence.
    pub fn advance(&mut self) -> u8 {
        self.last = (self.last.wrapping_add(self.step)) % 128;
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode_job, MAX_FRAME_LEN};
    use proptest::strategy::Strategy;

    /// `make_job()` from `test_bitmain_job_packets.c`.
    fn packet_job(job_id: u8) -> Job {
        let mut merkle_root = [0u8; 32];
        let mut prev_block_hash = [0u8; 32];
        for i in 0..32u8 {
            merkle_root[i as usize] = i;
            prev_block_hash[i as usize] = 0x20 + i;
        }
        Job {
            job_id,
            starting_nonce: 0x12345678,
            nbits: 0x1705dd01,
            ntime: 0x64658bd8,
            merkle_root,
            prev_block_hash,
            version: 0x20000004,
        }
    }

    /// The golden 88-byte frame, BM1370 variant (first job id 0x18).
    #[test]
    fn job_payload_wrapped_in_a_frame_is_the_golden_packet() {
        let mut ids = JobIds::bm1370();
        let job = packet_job(ids.advance());
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = encode_job(&mut buf, &job.to_bytes()).unwrap();
        assert_eq!(n, 88);
        let mut want = vec![0x55, 0xaa, 0x21, 0x56, 0x18, 0x01];
        want.extend_from_slice(&[
            0x78, 0x56, 0x34, 0x12, 0x01, 0xdd, 0x05, 0x17, 0xd8, 0x8b, 0x65, 0x64,
        ]);
        want.extend(0x00u8..0x20);
        want.extend(0x20u8..0x40);
        want.extend_from_slice(&[0x04, 0x00, 0x00, 0x20, 0x05, 0xd4]);
        assert_eq!(&buf[..n], &want[..]);
    }

    #[test]
    fn bm1366_variant_starts_at_8_with_its_own_crc() {
        let mut ids = JobIds::bm1366();
        let job = packet_job(ids.advance());
        let mut buf = [0u8; MAX_FRAME_LEN];
        let n = encode_job(&mut buf, &job.to_bytes()).unwrap();
        assert_eq!(buf[4], 0x08);
        assert_eq!(&buf[n - 2..n], &[0xd6, 0x21]);
    }

    /// "Bitmain occupied work slots transfer ownership to replacements":
    /// the second id is the first plus the step, mod 128.
    #[test]
    fn job_ids_advance_by_the_chip_step_mod_128() {
        let mut ids = JobIds::bm1370();
        let seq: Vec<u8> = (0..8).map(|_| ids.advance()).collect();
        assert_eq!(seq, [24, 48, 72, 96, 120, 16, 40, 64]);
        let mut ids = JobIds::bm1366();
        let seq: Vec<u8> = (0..17).map(|_| ids.advance()).collect();
        assert_eq!(seq[0], 8);
        assert_eq!(seq[15], 0, "16 × 8 = 128 wraps to 0");
        assert_eq!(seq[16], 8);
        assert!(seq.iter().all(|&i| i < 128));
    }

    /// ESP-Miner `test_mining.c` "Validate version mask incrementing".
    #[test]
    fn increment_bitmask_counts_under_the_mask() {
        let mut v = 0x2000_0004;
        for want in [0x2000_0104, 0x2000_0204, 0x2000_0304, 0x2000_0404] {
            v = increment_bitmask(v, 0x00ff_ff00);
            assert_eq!(v, want);
        }
        // The BIP 320 mask: + 0x2000 per step, as the BM1397 test
        // harness expects (`expected_versions`).
        assert_eq!(
            [0, 1, 2, 3].map(|k| rolled_version(0x2000_0004, 0x1fff_e000, k)),
            [0x2000_0004, 0x2000_2004, 0x2000_4004, 0x2000_6004]
        );
        assert_eq!(increment_bitmask(0x1234_5678, 0), 0x1234_5678);
        // A carry out of a split mask lands in the bit above the
        // overflow (bit 2 here), as the C does; the BIP 320 mask is
        // contiguous, so this never comes up.
        assert_eq!(increment_bitmask(0x0000_0001, 0x0000_0101), 0x0000_0004);
    }

    #[test]
    fn reverse_words_is_an_involution_and_moves_whole_words() {
        let mut h = [0u8; 32];
        for (i, b) in h.iter_mut().enumerate() {
            *b = i as u8;
        }
        let r = reverse_words(&h);
        assert_eq!(&r[..4], &[28, 29, 30, 31]);
        assert_eq!(&r[28..], &[0, 1, 2, 3]);
        assert_eq!(reverse_words(&r), h);
    }

    /// ESP-Miner `test_bm_job_packets.c` "BM1397 work packet and returned
    /// midstate preserve version mapping": the 152-byte frame for the
    /// harness job (version 0x20000004, mask 0x1fffe000, nonce
    /// 0x12345678, the merkle root and previous hash of the BM13xx
    /// golden packet in header order), midstates "from OpenSSL
    /// SHA256_Update over each first header block".
    #[test]
    fn bm1397_packet_matches_the_esp_miner_vector() {
        use crate::frame::{encode_job, MAX_FRAME_LEN};
        let mut merkle = [0u8; 32];
        let mut prev = [0u8; 32];
        for i in 0..32u8 {
            merkle[i as usize] = i;
            prev[i as usize] = 0x20 + i;
        }
        // The harness stores them word-reversed from the BM13xx packet.
        let merkle = reverse_words(&merkle);
        let prev = reverse_words(&prev);
        assert_eq!(&merkle[..4], &[0x1c, 0x1d, 0x1e, 0x1f]);
        let job = MidstateJob::build(
            4,
            4,
            0x1234_5678,
            0x1705_dd01,
            0x6465_8bd8,
            &prev,
            &merkle,
            0x2000_0004,
            0x1fff_e000,
        );
        let mut payload = [0u8; MIDSTATE_JOB_LEN_4];
        let n = job.write(&mut payload);
        assert_eq!(n, 146);
        let mut buf = [0u8; MAX_FRAME_LEN];
        let k = encode_job(&mut buf, &payload[..n]).unwrap();
        assert_eq!(k, 152);
        #[rustfmt::skip]
        let want: [u8; 152] = [
            0x55, 0xaa, 0x21, 0x96, 0x04, 0x04, 0x78, 0x56,
            0x34, 0x12, 0x01, 0xdd, 0x05, 0x17, 0xd8, 0x8b,
            0x65, 0x64, 0x00, 0x01, 0x02, 0x03, 0x41, 0x70,
            0x41, 0x61, 0x2e, 0x05, 0x0d, 0xa2, 0x43, 0xd9,
            0x74, 0x92, 0x1b, 0xca, 0xaf, 0xe6, 0x98, 0x24,
            0x48, 0xd8, 0xb4, 0xe9, 0x69, 0xdd, 0xfd, 0xaf,
            0x43, 0x29, 0x34, 0x6f, 0x12, 0x2f, 0xb4, 0x45,
            0xa6, 0x48, 0x0f, 0x3b, 0xb4, 0x5f, 0x36, 0xc5,
            0x30, 0x09, 0xbb, 0x49, 0x95, 0x44, 0xe2, 0xb2,
            0x01, 0xa8, 0x6a, 0xe6, 0x53, 0x13, 0xc0, 0x4e,
            0x91, 0x04, 0x29, 0x90, 0x0e, 0x32, 0x94, 0xeb,
            0xcf, 0x87, 0xd2, 0x4d, 0x3f, 0x67, 0x9d, 0x0e,
            0x88, 0xf3, 0x82, 0x0c, 0xae, 0x49, 0xd5, 0xa4,
            0x0a, 0x8f, 0x37, 0x1b, 0x52, 0x3c, 0x9a, 0xfa,
            0x28, 0xc4, 0xed, 0xfc, 0xe7, 0x16, 0x58, 0xcf,
            0xd5, 0x38, 0xa5, 0x9c, 0x04, 0xf0, 0xdd, 0xe5,
            0x94, 0xe5, 0x75, 0x6b, 0x74, 0x3f, 0x94, 0x8d,
            0x51, 0x7e, 0xd9, 0x2b, 0x8b, 0xb2, 0x8a, 0x69,
            0x1d, 0xb9, 0x30, 0x0a, 0x1b, 0x40, 0x44, 0xc3,
        ];
        assert_eq!(&buf[..k], &want[..]);
        let back = MidstateJob::from_bytes(&payload[..n]).unwrap();
        assert_eq!(back, job);
    }

    /// The hardware-tested packet of ESP-Miner's `test_job_command.c`
    /// (job 0x18, four midstates, nonce 9B 04 4C 0A found for midstate 2)
    /// has the layout this module writes: tail fields where `write`
    /// puts them, and `from_bytes` reads them back.
    #[test]
    fn bm1397_packet_layout_from_the_hardware_test() {
        let mut p = [0u8; MIDSTATE_JOB_LEN_4];
        p[..18].copy_from_slice(&[
            0x18, 0x04, 0x9B, 0x04, 0x4C, 0x0A, 0x3A, 0xAE, 0x05, 0x17, 0xA0, 0x84, 0x73, 0x64,
            0x50, 0xE3, 0x71, 0x61,
        ]);
        let j = MidstateJob::from_bytes(&p).unwrap();
        assert_eq!(j.job_id, 0x18);
        assert_eq!(j.num_midstates, 4);
        assert_eq!(j.starting_nonce, 0x0a4c_049b);
        assert_eq!(j.nbits, 0x1705_ae3a);
        assert_eq!(j.ntime, 0x6473_84a0);
        assert_eq!(j.merkle_tail, [0x50, 0xE3, 0x71, 0x61]);
    }

    #[test]
    fn one_midstate_is_a_50_byte_payload_and_a_zero_mask_forces_one() {
        let job = MidstateJob::build(8, 4, 0, 1, 2, &[0; 32], &[0; 32], 0x2000_0000, 0);
        assert_eq!(job.num_midstates, 1);
        assert_eq!(job.payload_len(), 50);
        let mut p = [0u8; MIDSTATE_JOB_LEN_4];
        assert_eq!(job.write(&mut p), 50);
        assert_eq!(p[1], 1);
        assert_eq!(MidstateJob::from_bytes(&p[..50]).unwrap(), job);
        assert_eq!(MidstateJob::from_bytes(&p[..60]), None);
    }

    proptest::proptest! {
        #[test]
        fn midstate_jobs_round_trip(
            job_id in (0u8..32).prop_map(|k| k * 4),
            n in 1u8..=4,
            starting_nonce in proptest::num::u32::ANY,
            nbits in proptest::num::u32::ANY,
            ntime in proptest::num::u32::ANY,
            merkle in proptest::array::uniform32(0u8..=255),
            prev in proptest::array::uniform32(0u8..=255),
            version in proptest::num::u32::ANY,
        ) {
            let job = MidstateJob::build(job_id, n, starting_nonce, nbits, ntime, &prev, &merkle, version, 0x1fff_e000);
            let mut p = [0u8; MIDSTATE_JOB_LEN_4];
            let len = job.write(&mut p);
            let back = MidstateJob::from_bytes(&p[..len]).unwrap();
            // 2 and 3 midstates go out as 4 (the C packet has four slots).
            let sent = if n == 1 { 1 } else { 4 };
            proptest::prop_assert_eq!(back.num_midstates, sent);
            proptest::prop_assert_eq!(back.job_id, job.job_id);
            proptest::prop_assert_eq!(back.merkle_tail, job.merkle_tail);
            for k in 0..usize::from(n) {
                proptest::prop_assert_eq!(back.midstates[k], job.midstates[k]);
            }
        }

        #[test]
        fn job_round_trips(
            job_id in 0u8..128,
            starting_nonce in proptest::num::u32::ANY,
            nbits in proptest::num::u32::ANY,
            ntime in proptest::num::u32::ANY,
            merkle_root in proptest::array::uniform32(0u8..=255),
            prev_block_hash in proptest::array::uniform32(0u8..=255),
            version in proptest::num::u32::ANY,
        ) {
            let job = Job { job_id, starting_nonce, nbits, ntime, merkle_root, prev_block_hash, version };
            let bytes = job.to_bytes();
            proptest::prop_assert_eq!(bytes[1], 1);
            proptest::prop_assert_eq!(Job::from_bytes(&bytes), job);
        }

        /// Every id is below 128 and a multiple of gcd(step, 128) — not
        /// of the step itself: with step 24, 144 wraps to 16. The slots
        /// the sequence can ever visit are 128 / gcd of them; for the
        /// BM1370's 24 that is 16.
        #[test]
        fn job_ids_stay_in_range(step in 1u8..=64, n in 1usize..300) {
            fn gcd(a: u32, b: u32) -> u32 { if b == 0 { a } else { gcd(b, a % b) } }
            let g = gcd(u32::from(step), 128);
            let mut ids = JobIds::with_step(step);
            let mut seen = [false; 128];
            for _ in 0..n {
                let id = ids.advance();
                proptest::prop_assert!(id < 128);
                proptest::prop_assert_eq!(u32::from(id) % g, 0);
                seen[id as usize] = true;
            }
            if step == 24 && n >= 16 {
                proptest::prop_assert_eq!(seen.iter().filter(|&&s| s).count(), 16);
            }
        }
    }
}
