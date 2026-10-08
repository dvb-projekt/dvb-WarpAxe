//! What the eight payload bytes of a chip response mean.
//!
//! Two shapes, told apart by the frame's job flag ([`crate::frame::Response::is_job`]):
//!
//! ```text
//! job result     nonce[4] | midstate_num | id | version[2]
//! register read  value[4] | asic_address | register | pad[2]
//! ```
//!
//! The nonce is used two ways at once. Read little-endian it is the
//! nonce the pool wants (`0x0a4c049b` for wire bytes `9b 04 4c 0a`).
//! Read big-endian, bits 24..17 carry the address of the chip that
//! found it and bits 31..25 the core — the chip stamps its identity into
//! the nonce space it was given, which is also why the driver splits the
//! nonce space by chip count. The `id` byte carries the job id in its
//! high nibble (shifted, so `(id & 0xF0) >> 1` recovers a multiple of
//! 16… of the step) and the small-core index in its low nibble. The two
//! version bytes are the rolled bits, to be shifted left 13 and OR-ed
//! into the job's version.
//!
//! Chip addresses come back raw here. Dividing by the chain's address
//! interval (`256 / chip_count`) to get a chip index is the caller's
//! business — this module does not know how many chips there are.
//!
//! Source: ESP-Miner `bm1370.c` (`bm1370_asic_result_job_t`,
//! `bm1370_asic_result_cmd_t`, `BM1370_process_work`), vectors from
//! `test_version_rolling.c` and `test_bitmain_job_packets.c`. The BM1373
//! uses the same layout.
//!
//! The BM1397 answers with six payload bytes ([`JobResult::parse_bm1397`]):
//!
//! ```text
//! job result     nonce[4] | midstate_num | id
//! register read  value[4] | asic_address | register
//! ```
//!
//! No version bytes: the chip does not roll. The `id` byte carries the
//! job id in bits 7..2 (a multiple of 4, as sent) and in bits 1..0 the
//! number of the midstate that produced the nonce, which is the number
//! of times the host rolled the version for it. Source: ESP-Miner
//! `bm1397.c` (`bm1397_asic_result_job_t`, `BM1397_process_work`),
//! WantClue `bm1397.cpp` (`BM1397_proccess_work`), the response vectors
//! in `test_asic_result_job.c` (id 0x13 → job 0x10, midstate 3).

/// A nonce the chip found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobResult {
    /// The job it belongs to, as handed out by [`crate::job::JobIds`].
    pub job_id: u8,
    /// The nonce, little-endian from the wire — what goes to the pool.
    pub nonce: u32,
    /// Raw chain address of the chip that found it (bits 24..17 of the
    /// big-endian nonce). Divide by the address interval for an index.
    pub asic_address: u8,
    /// Core within the chip (bits 31..25 of the big-endian nonce), 0..128.
    pub core_id: u8,
    /// Small core within the core (low nibble of the id byte), 0..16.
    pub small_core_id: u8,
    /// The rolled version bits, already shifted into place: OR into the
    /// job's version to get the header version that hashed.
    pub version_bits: u32,
    /// The midstate number byte, verbatim. Always 0 with hardware
    /// midstates; kept for completeness.
    pub midstate_num: u8,
}

impl JobResult {
    /// Interpret a job-result payload.
    pub fn parse(p: &[u8; 8]) -> Self {
        let wire = [p[0], p[1], p[2], p[3]];
        let nonce_be = u32::from_be_bytes(wire);
        JobResult {
            job_id: (p[5] & 0xF0) >> 1,
            nonce: u32::from_le_bytes(wire),
            asic_address: ((nonce_be >> 17) & 0xFF) as u8,
            core_id: ((nonce_be >> 25) & 0x7F) as u8,
            small_core_id: p[5] & 0x0F,
            version_bits: u32::from(u16::from_be_bytes([p[6], p[7]])) << 13,
            midstate_num: p[4],
        }
    }

    /// Interpret a BM1397 job-result payload (six bytes, zero-padded to
    /// eight by the decoder). `version_bits` is 0: the version is the
    /// job's, rolled `midstate_num` times by the host
    /// ([`crate::job::rolled_version`]). The small-core id is not in
    /// this response (ESP-Miner reads the id byte's low nibble, which
    /// is the midstate number); 0 here.
    pub fn parse_bm1397(p: &[u8; 8]) -> Self {
        let wire = [p[0], p[1], p[2], p[3]];
        let nonce_be = u32::from_be_bytes(wire);
        JobResult {
            job_id: p[5] & 0xFC,
            nonce: u32::from_le_bytes(wire),
            asic_address: ((nonce_be >> 17) & 0xFF) as u8,
            core_id: ((nonce_be >> 25) & 0x7F) as u8,
            small_core_id: 0,
            version_bits: 0,
            midstate_num: p[5] & 0x03,
        }
    }

    /// The header version that produced this nonce, given the job's base
    /// version. The reference driver ORs; bits the base already has set
    /// stay set.
    pub const fn rolled_version(&self, base_version: u32) -> u32 {
        base_version | self.version_bits
    }
}

/// Build a BM1397 job-result payload (six bytes of it; the simulator's
/// side of [`JobResult::parse_bm1397`]): the chip's address and core in
/// the nonce's high bits, the job id and midstate number in the id byte.
pub fn bm1397_job_result_payload(
    job_id: u8,
    nonce_low17: u32,
    asic_address: u8,
    core_id: u8,
    midstate_num: u8,
) -> [u8; 6] {
    let nonce_be = (u32::from(core_id & 0x7F) << 25)
        | (u32::from(asic_address) << 17)
        | (nonce_low17 & 0x1_FFFF);
    let be = nonce_be.to_be_bytes();
    [
        be[0],
        be[1],
        be[2],
        be[3],
        0x00,
        (job_id & 0xFC) | (midstate_num & 0x03),
    ]
}

/// A register value the chip reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterResult {
    /// The register's value, big-endian from the wire.
    pub value: u32,
    /// Raw chain address of the answering chip.
    pub asic_address: u8,
    /// Register address that was read.
    pub register: u8,
}

impl RegisterResult {
    /// Interpret a register-read payload.
    pub fn parse(p: &[u8; 8]) -> Self {
        RegisterResult {
            value: u32::from_be_bytes([p[0], p[1], p[2], p[3]]),
            asic_address: p[4],
            register: p[5],
        }
    }
}

/// Build a job-result payload — the simulator's side of [`JobResult::parse`].
///
/// `asic_address` and `core_id` are stamped into the nonce's high bits
/// the way the chip does it, so the low 17 bits of `nonce_be` are what
/// the caller controls freely; the rest is overwritten.
pub fn job_result_payload(
    job_id: u8,
    nonce_low17: u32,
    asic_address: u8,
    core_id: u8,
    small_core_id: u8,
    version_bits: u16,
) -> [u8; 8] {
    let nonce_be = (u32::from(core_id & 0x7F) << 25)
        | (u32::from(asic_address) << 17)
        | (nonce_low17 & 0x1_FFFF);
    let be = nonce_be.to_be_bytes();
    let id = ((job_id << 1) & 0xF0) | (small_core_id & 0x0F);
    let v = version_bits.to_be_bytes();
    [be[0], be[1], be[2], be[3], 0x00, id, v[0], v[1]]
}

/// Build a register-read payload — the simulator's side of [`RegisterResult::parse`].
pub fn register_result_payload(value: u32, asic_address: u8, register: u8) -> [u8; 8] {
    let v = value.to_be_bytes();
    [v[0], v[1], v[2], v[3], asic_address, register, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::strategy::Strategy;

    /// `test_version_rolling.c`, `queue_job_response`: wire
    /// `9b 04 4c 0a 00 | id=0x23 | vh vl`, asserted as nonce 0x0a4c049b,
    /// job id 0x10, asic 1 (of 2, interval 128), core 77, small core 3.
    #[test]
    fn parses_the_version_rolling_test_response() {
        let p = [0x9b, 0x04, 0x4c, 0x0a, 0x00, 0x23, 0x00, 0x01];
        let r = JobResult::parse(&p);
        assert_eq!(r.nonce, 0x0a4c049b);
        assert_eq!(r.job_id, 0x10);
        assert_eq!(r.asic_address / 128, 1, "raw address {}", r.asic_address);
        assert_eq!(r.core_id, 77);
        assert_eq!(r.small_core_id, 3);
        assert_eq!(r.version_bits, 0x2000);
        assert_eq!(r.rolled_version(0x20000004), 0x20002004);
    }

    /// The version-bit cases from the same test, verbatim.
    #[test]
    fn reconstructs_rolled_versions_like_the_reference_driver() {
        #[rustfmt::skip]
        let cases: &[(u32, [u8; 2], u32)] = &[
            (0x20000004, [0x00, 0x00], 0x20000004),
            (0x20000004, [0x00, 0x01], 0x20002004),
            (0x20000004, [0x00, 0x03], 0x20006004),
            (0x20000004, [0x01, 0x00], 0x20200004),
            (0x20000004, [0x80, 0x00], 0x30000004),
            (0x20000004, [0xff, 0xff], 0x3fffe004),
            // OR semantics: bits already set in the base stay.
            (0x21002004, [0x00, 0x01], 0x21002004),
            (0x21002004, [0x00, 0x02], 0x21006004),
        ];
        for &(base, wire, want) in cases {
            let p = [0x9b, 0x04, 0x4c, 0x0a, 0x00, 0x23, wire[0], wire[1]];
            assert_eq!(
                JobResult::parse(&p).rolled_version(base),
                want,
                "{wire:02x?}"
            );
        }
    }

    /// `test_bitmain_job_packets.c`, "BM1397 register results …" — the
    /// register shape is shared: `12 34 56 78 | addr 0x80 | reg 0x4c`.
    #[test]
    fn parses_a_register_result() {
        let p = [0x12, 0x34, 0x56, 0x78, 0x80, 0x4c, 0x00, 0x00];
        let r = RegisterResult::parse(&p);
        assert_eq!(r.value, 0x12345678);
        assert_eq!(r.asic_address, 0x80);
        assert_eq!(r.register, 0x4c);
    }

    #[test]
    fn simulator_payload_reproduces_the_test_vector() {
        // nonce 0x9b044c0a BE: core 77 (0x4d) in bits 31..25, address 130
        // (0x82) in 24..17, low 17 bits 0x00c0a.
        let p = job_result_payload(0x10, 0x9b044c0a & 0x1_FFFF, 0x82, 77, 3, 0x0001);
        assert_eq!(p, [0x9b, 0x04, 0x4c, 0x0a, 0x00, 0x23, 0x00, 0x01]);
    }

    /// `test_asic_result_job.c`: wire id 0x13 is job 0x10, midstate 3,
    /// which rolls 0x20000004 to 0x20006004 under the BIP 320 mask; the
    /// nonce goes out as it came (memcpy, little-endian).
    #[test]
    fn parses_a_bm1397_result() {
        let nonce: u32 = 0x6100_0002;
        let le = nonce.to_le_bytes();
        let p = [le[0], le[1], le[2], le[3], 0x00, 0x13, 0x00, 0x00];
        let r = JobResult::parse_bm1397(&p);
        assert_eq!(r.job_id, 0x10);
        assert_eq!(r.midstate_num, 3);
        assert_eq!(r.nonce, nonce);
        assert_eq!(r.version_bits, 0);
        assert_eq!(
            crate::job::rolled_version(0x2000_0004, 0x1fff_e000, r.midstate_num),
            0x2000_6004
        );
        // The hardware test in `test_job_command.c`: nonce 9B 04 4C 0A for
        // job 0x18, midstate 2.
        let r = JobResult::parse_bm1397(&[0x9B, 0x04, 0x4C, 0x0A, 0x00, 0x18 | 2, 0, 0]);
        assert_eq!((r.job_id, r.midstate_num, r.nonce), (0x18, 2, 0x0a4c_049b));
    }

    proptest::proptest! {
        #[test]
        fn bm1397_results_round_trip(
            job_id in (0u8..32).prop_map(|k| k * 4),
            low17 in 0u32..(1 << 17),
            addr in 0u8..=255,
            core in 0u8..128,
            k in 0u8..4,
        ) {
            let six = bm1397_job_result_payload(job_id, low17, addr, core, k);
            let mut p = [0u8; 8];
            p[..6].copy_from_slice(&six);
            let r = JobResult::parse_bm1397(&p);
            proptest::prop_assert_eq!(r.job_id, job_id);
            proptest::prop_assert_eq!(r.midstate_num, k);
            proptest::prop_assert_eq!(r.asic_address, addr);
            proptest::prop_assert_eq!(r.core_id, core);
            proptest::prop_assert_eq!(u32::from_be_bytes(r.nonce.to_le_bytes()) & 0x1_FFFF, low17);
        }

        #[test]
        fn job_result_round_trips(
            job_id in (0u8..8).prop_map(|k| k * 16),
            low17 in 0u32..(1 << 17),
            addr in 0u8..=255,
            core in 0u8..128,
            small in 0u8..16,
            vbits in proptest::num::u16::ANY,
        ) {
            let p = job_result_payload(job_id, low17, addr, core, small, vbits);
            let r = JobResult::parse(&p);
            proptest::prop_assert_eq!(r.job_id, job_id);
            proptest::prop_assert_eq!(r.asic_address, addr);
            proptest::prop_assert_eq!(r.core_id, core);
            proptest::prop_assert_eq!(r.small_core_id, small);
            proptest::prop_assert_eq!(r.version_bits, u32::from(vbits) << 13);
            proptest::prop_assert_eq!(u32::from_be_bytes(r.nonce.to_le_bytes()) & 0x1_FFFF, low17);
        }

        #[test]
        fn register_result_round_trips(value in proptest::num::u32::ANY, addr in 0u8..=255, reg in 0u8..=255) {
            let r = RegisterResult::parse(&register_result_payload(value, addr, reg));
            proptest::prop_assert_eq!((r.value, r.asic_address, r.register), (value, addr, reg));
        }
    }
}
