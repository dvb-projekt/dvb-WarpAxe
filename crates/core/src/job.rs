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

/// Length of the job payload on the BM1366/68/70/73.
pub const JOB_LEN: usize = 82;

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

    proptest::proptest! {
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
