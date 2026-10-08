//! SHA-256 compression, for the midstate a BM1397 job carries.
//!
//! The BM1397 does not hash a block header from scratch: the host runs
//! the first SHA-256 block (the header's first 64 bytes: version,
//! previous hash, the first 28 bytes of the merkle root) and sends the
//! resulting state, the *midstate*; the chip continues with the tail
//! (last 4 merkle bytes, ntime, nbits, nonce). Since the chip does not
//! roll the version either, the host sends one midstate per version it
//! wants searched — four on the BM1397 ([`crate::job::MidstateJob`]).
//!
//! One function, no allocation, no state kept: the compression of one
//! 64-byte block from a given state. Constants and schedule are FIPS
//! 180-4; the test vectors are ESP-Miner's (`bm_job_midstate.c`,
//! `test_bm_job_packets.c`, `test_mining.c`) and the NIST "abc" digest.

/// The eight initial hash values.
pub const IV: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// Run one 64-byte block through the compression function, updating
/// `state` in place.
pub fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

/// The midstate of a block header: the state after its first 64 bytes.
pub fn midstate(header_first_block: &[u8; 64]) -> [u32; 8] {
    let mut state = IV;
    compress(&mut state, header_first_block);
    state
}

/// The state as SHA-256 serialises it: each word big-endian, in order.
pub fn to_bytes(state: &[u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, w) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FIPS 180-4 "abc": one padded block.
    #[test]
    fn abc_digest() {
        let mut block = [0u8; 64];
        block[..3].copy_from_slice(b"abc");
        block[3] = 0x80;
        block[62..].copy_from_slice(&(24u16).to_be_bytes());
        let mut st = IV;
        compress(&mut st, &block);
        assert_eq!(
            to_bytes(&st),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    /// ESP-Miner `test_mining.c` / `test_bm_job_packet_building.c`
    /// "Validate bm job construction": the midstate of version
    /// 0x20000004, prevhash `bf44fd35…` (stratum order, so each word is
    /// byte-swapped into the header) and merkle root `cd1be821…`; the
    /// expected hex is the midstate's words serialised little-endian
    /// (what `memcpy` of the C state gives).
    #[test]
    fn esp_miner_midstate_vector() {
        let prev_stratum: [u8; 32] =
            hex("bf44fd3513dc7b837d60e5c628b572b448d204a8000007490000000000000000");
        let mut prev = [0u8; 32];
        for i in 0..8 {
            let w = &prev_stratum[i * 4..i * 4 + 4];
            prev[i * 4..i * 4 + 4].copy_from_slice(&[w[3], w[2], w[1], w[0]]);
        }
        let merkle: [u8; 32] =
            hex("cd1be82132ef0d12053dcece1fa0247fcfdb61d4dbd3eb32ea9ef9b4c604a846");
        let mut block = [0u8; 64];
        block[..4].copy_from_slice(&0x2000_0004u32.to_le_bytes());
        block[4..36].copy_from_slice(&prev);
        block[36..].copy_from_slice(&merkle[..28]);
        let st = midstate(&block);
        let mut le = [0u8; 32];
        for (i, w) in st.iter().enumerate() {
            le[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        assert_eq!(
            le,
            hex::<32>("91DFEA528A9F73683D0D495DD6DD7415E1CA21CB411759E3E05D7D5FF285314D")
        );
    }

    pub(crate) fn hex<const N: usize>(s: &str) -> [u8; N] {
        let mut out = [0u8; N];
        let b = s.as_bytes();
        assert_eq!(b.len(), 2 * N);
        for i in 0..N {
            let v = |c: u8| match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("hex"),
            };
            out[i] = v(b[2 * i]) << 4 | v(b[2 * i + 1]);
        }
        out
    }
}
