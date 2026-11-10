//! SHA-256 for an 80-byte block header, the way a miner computes it: the
//! first 64 bytes once (the midstate), then per nonce the 16-byte tail
//! padded to a block, and the 32-byte result padded to a block again.
//!
//! Two consumers: the software worker ([`sha256d_tail`]) and the S3's
//! SHA peripheral. The peripheral takes a message block as sixteen
//! 32-bit words read from memory in the CPU's (little-endian) order and
//! keeps its digest state the same way: the eight words read back from
//! its `H` registers, copied to memory, are the digest bytes
//! (ESP-IDF `esp_sha256.c`: `memcpy(output, ctx->state, 32)`). Hence
//! [`hw_state`] byte-swaps the native state words, and the two
//! `hw_block` functions build the blocks as the words the firmware
//! writes into the `TEXT` registers. The firmware's loop is NerdMiner
//! v2's (`src/mining.cpp`, `minerWorkerHw`), proven on this chip.

/// SHA-256 initial hash value.
pub const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Round constants (256 bytes; from the flash cache on the devkit, a
/// `link_section` into DRAM would need `unsafe_code`).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// One SHA-256 compression: `state` absorbs the 64-byte `block`.
#[inline(always)]
pub fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    let (words, _) = block.as_chunks::<4>();
    for (i, chunk) in words.iter().enumerate() {
        w[i] = u32::from_be_bytes(*chunk);
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

/// The digest bytes of a state (big-endian words).
pub fn digest_bytes(state: &[u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, w) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

/// The state after the header's first 64 bytes: version, previous hash
/// and the merkle root's first 28 bytes. Constant per job.
pub fn midstate(header: &[u8; 80]) -> [u32; 8] {
    let mut state = IV;
    let block: [u8; 64] = header[..64].try_into().expect("64 bytes");
    compress(&mut state, &block);
    state
}

/// The header bytes 64..76 that follow the midstate and precede the
/// nonce: the merkle root's last 4 bytes, ntime and nbits.
pub fn tail(header: &[u8; 80]) -> [u8; 12] {
    header[64..76].try_into().expect("12 bytes")
}

/// The second block of the first hash: the tail, the nonce, padding and
/// the length of 80 bytes (640 bits).
pub fn first_block(tail: &[u8; 12], nonce: u32) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..12].copy_from_slice(tail);
    b[12..16].copy_from_slice(&nonce.to_le_bytes());
    b[16] = 0x80;
    b[62] = 0x02;
    b[63] = 0x80;
    b
}

/// The one block of the second hash: the first hash, padding and the
/// length of 32 bytes (256 bits).
pub fn second_block(first: &[u8; 32]) -> [u8; 64] {
    let mut b = [0u8; 64];
    b[..32].copy_from_slice(first);
    b[32] = 0x80;
    b[62] = 0x01;
    b
}

/// Double SHA-256 of the header `midstate` + `tail` + `nonce`, in the
/// hash's natural byte order (the one `hash_difficulty` reads), both
/// compressions inlined. The devkit's software worker runs a copy of
/// this placed in internal RAM (firmware `cpuminer.rs`, what `IRAM_ATTR`
/// does for NerdMiner's `nerd_sha256d`): from the flash cache it made
/// 88 µs a double hash (11 kH/s), the `sha2` crate's unrolled
/// compressor 110 µs.
pub fn sha256d_tail(midstate: &[u32; 8], tail: &[u8; 12], nonce: u32) -> [u8; 32] {
    let mut state = *midstate;
    compress(&mut state, &first_block(tail, nonce));
    let first = digest_bytes(&state);
    let mut state = IV;
    compress(&mut state, &second_block(&first));
    digest_bytes(&state)
}

/// Double SHA-256 of a whole 80-byte header (reference, slow path).
pub fn sha256d_header(header: &[u8; 80]) -> [u8; 32] {
    let mid = midstate(header);
    let nonce = u32::from_le_bytes([header[76], header[77], header[78], header[79]]);
    sha256d_tail(&mid, &tail(header), nonce)
}

/// Quick pre-filter on a hash: its sixteen most significant bits are
/// zero, which about one hash in 65536 passes. Only those are worth a
/// difficulty computation; the peripheral's loop reads just the one
/// register word this needs before deciding to read the rest.
pub fn candidate(hash: &[u8; 32]) -> bool {
    hash[30] == 0 && hash[31] == 0
}

// ---- The S3 SHA peripheral's view -------------------------------------

/// The midstate as the eight words to write into the peripheral's `H`
/// registers: the digest bytes of the state read as little-endian words.
pub fn hw_state(midstate: &[u32; 8]) -> [u32; 8] {
    let mut out = [0u32; 8];
    for (o, w) in out.iter_mut().zip(midstate) {
        *o = w.swap_bytes();
    }
    out
}

/// The hash as the firmware assembles it from the eight `H` words read
/// after the second block: each word's bytes in memory order.
pub fn hw_hash(h: &[u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// The three words of the tail as written into `TEXT[0..3]` (the nonce
/// goes into `TEXT[3]`, as a plain `u32`: little-endian in memory is the
/// header's byte order).
pub fn hw_tail_words(tail: &[u8; 12]) -> [u32; 3] {
    let w = |i: usize| u32::from_le_bytes([tail[i], tail[i + 1], tail[i + 2], tail[i + 3]]);
    [w(0), w(4), w(8)]
}

/// `TEXT[4]` after the nonce: the 0x80 padding byte.
pub const HW_PAD: u32 = 0x0000_0080;
/// `TEXT[15]` of the first hash's second block: 640 bits, big-endian.
pub const HW_LEN_640: u32 = 0x8002_0000;
/// `TEXT[8]` of the second hash's block: the 0x80 padding byte.
pub const HW_PAD2: u32 = 0x0000_0080;
/// `TEXT[15]` of the second hash's block: 256 bits, big-endian.
pub const HW_LEN_256: u32 = 0x0001_0000;

/// The sixteen `TEXT` words of the first hash's second block.
pub fn hw_block1(tail_words: &[u32; 3], nonce: u32) -> [u32; 16] {
    let mut b = [0u32; 16];
    b[..3].copy_from_slice(tail_words);
    b[3] = nonce;
    b[4] = HW_PAD;
    b[15] = HW_LEN_640;
    b
}

/// The sixteen `TEXT` words of the second hash's block, from the eight
/// `H` words read after the first.
pub fn hw_block2(h: &[u32; 8]) -> [u32; 16] {
    let mut b = [0u32; 16];
    b[..8].copy_from_slice(h);
    b[8] = HW_PAD2;
    b[15] = HW_LEN_256;
    b
}

/// The firmware's test on the last `H` word before reading the rest:
/// [`candidate`] on the word alone.
pub fn hw_candidate(h7: u32) -> bool {
    h7 >> 16 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha256d(data: &[u8]) -> [u8; 32] {
        Sha256::digest(Sha256::digest(data)).into()
    }

    /// The peripheral as the firmware sees it: `TEXT` words in, `H`
    /// words out, both in memory order. The reference arithmetic
    /// underneath is `compress`, which the other tests pin to `sha2`.
    struct Hw {
        state: [u32; 8],
    }

    impl Hw {
        fn write_h(&mut self, h: &[u32; 8]) {
            for (s, w) in self.state.iter_mut().zip(h) {
                *s = w.swap_bytes();
            }
        }
        fn read_h(&self) -> [u32; 8] {
            hw_state(&self.state)
        }
        fn block_bytes(text: &[u32; 16]) -> [u8; 64] {
            let mut b = [0u8; 64];
            for (i, w) in text.iter().enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            b
        }
        fn start(&mut self, text: &[u32; 16]) {
            self.state = IV;
            compress(&mut self.state, &Self::block_bytes(text));
        }
        fn continue_(&mut self, text: &[u32; 16]) {
            compress(&mut self.state, &Self::block_bytes(text));
        }
    }

    fn pseudo_header(seed: u32) -> [u8; 80] {
        let mut h = [0u8; 80];
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
        for b in h.iter_mut() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = x as u8;
        }
        h
    }

    #[test]
    fn compress_matches_sha2_on_one_block() {
        let msg = b"abc";
        let mut block = [0u8; 64];
        block[..3].copy_from_slice(msg);
        block[3] = 0x80;
        block[63] = 24;
        let mut s = IV;
        compress(&mut s, &block);
        assert_eq!(digest_bytes(&s), <[u8; 32]>::from(Sha256::digest(msg)));
    }

    #[test]
    fn midstate_path_equals_full_double_hash() {
        for seed in 0..64u32 {
            let h = pseudo_header(seed);
            assert_eq!(sha256d_header(&h), sha256d(&h), "seed {seed}");
            let mid = midstate(&h);
            let t = tail(&h);
            for nonce in [0u32, 1, 0x7fff_ffff, 0xffff_ffff, seed.wrapping_mul(7919)] {
                let mut hh = h;
                hh[76..80].copy_from_slice(&nonce.to_le_bytes());
                assert_eq!(sha256d_tail(&mid, &t, nonce), sha256d(&hh));
            }
        }
    }

    #[test]
    fn hardware_block_layout_reproduces_the_double_hash() {
        for seed in 0..32u32 {
            let h = pseudo_header(seed);
            let mid = midstate(&h);
            let tw = hw_tail_words(&tail(&h));
            let nonce = seed.wrapping_mul(0x9e37_79b9);
            let mut hw = Hw { state: IV };
            // The firmware's per-nonce sequence.
            hw.write_h(&hw_state(&mid));
            hw.continue_(&hw_block1(&tw, nonce));
            let first = hw.read_h();
            hw.start(&hw_block2(&first));
            let out = hw.read_h();
            let mut hh = h;
            hh[76..80].copy_from_slice(&nonce.to_le_bytes());
            assert_eq!(hw_hash(&out), sha256d(&hh), "seed {seed}");
            assert_eq!(hw_candidate(out[7]), candidate(&hw_hash(&out)));
        }
    }

    /// The firmware's pipelined order (cpuminer.rs `hw_batch`): the
    /// `TEXT` register file keeps its words between blocks, so each
    /// block only rewrites what the other one changed. Words 9..15 are
    /// zeroed once per batch; the first block writes 0..9 and 15, the
    /// second copies the digest into 0..8 and writes 8 and 15. Modelled
    /// on a sixteen-word file across two nonces.
    #[test]
    fn pipelined_write_sets_leave_the_right_blocks() {
        let h = pseudo_header(7);
        let mid = midstate(&h);
        let tw = hw_tail_words(&tail(&h));
        let mut text = [0xffff_ffffu32; 16];
        for w in text[9..15].iter_mut() {
            *w = 0;
        }
        let block1 = |text: &mut [u32; 16], nonce: u32| {
            text[..3].copy_from_slice(&tw);
            text[3] = nonce;
            text[4] = HW_PAD;
            for w in text[5..9].iter_mut() {
                *w = 0;
            }
            text[15] = HW_LEN_640;
        };
        let mut hw = Hw { state: IV };
        block1(&mut text, 5);
        for nonce in 5..8u32 {
            assert_eq!(text, hw_block1(&tw, nonce));
            hw.write_h(&hw_state(&mid));
            hw.continue_(&text);
            // While the engine runs: the second block's padding.
            text[8] = HW_PAD2;
            text[15] = HW_LEN_256;
            let first = hw.read_h();
            text[..8].copy_from_slice(&first);
            assert_eq!(text, hw_block2(&first));
            hw.start(&text);
            // While the engine runs: the next nonce's first block.
            block1(&mut text, nonce + 1);
            let out = hw.read_h();
            let mut hh = h;
            hh[76..80].copy_from_slice(&nonce.to_le_bytes());
            assert_eq!(hw_hash(&out), sha256d(&hh), "nonce {nonce}");
        }
    }

    #[test]
    fn candidate_reads_the_most_significant_bytes() {
        let mut hash = [0xffu8; 32];
        assert!(!candidate(&hash));
        hash[30] = 0;
        hash[31] = 0;
        assert!(candidate(&hash));
        // In hw order the last word holds bytes 28..32; its upper half
        // is bytes 30 and 31.
        let h7 = u32::from_le_bytes([hash[28], hash[29], hash[30], hash[31]]);
        assert!(hw_candidate(h7));
        assert!(!hw_candidate(0x0001_0000));
    }

    /// The genesis block: header fields from the chain, hash
    /// 000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f.
    #[test]
    fn genesis_block_hashes_right() {
        let header = hex::decode(concat!(
            "01000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "3ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a",
            "29ab5f49",
            "ffff001d",
            "1dac2b7c"
        ))
        .unwrap();
        let header: [u8; 80] = header.try_into().unwrap();
        let hash = sha256d_header(&header);
        let mut shown = hash;
        shown.reverse();
        assert_eq!(
            hex::encode(shown),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
        let mid = midstate(&header);
        assert_eq!(sha256d_tail(&mid, &tail(&header), 0x7c2b_ac1d), hash);
        assert!(candidate(&hash));
        // Pools would count it as about 2536 times diff 1.
        let d = warpaxe_stratum::job::hash_difficulty(&hash);
        assert!((2500.0..2600.0).contains(&d), "{d}");
    }

    /// Block 125552, the Bitcoin wiki's "block hashing algorithm" vector:
    /// the stratum crate's `Job` assembles the same header from its
    /// fields, so a job built from a notify hashes like the chain does.
    #[test]
    fn block_125552_through_a_job() {
        use warpaxe_stratum::Job;
        let prev = hex::decode("81cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000")
            .unwrap();
        let merkle =
            hex::decode("e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122b")
                .unwrap();
        let job = Job {
            job_id: "1".into(),
            extranonce2: vec![0; 4],
            merkle_root: merkle.try_into().unwrap(),
            prev_hash: prev.try_into().unwrap(),
            version: 1,
            nbits: 0x1a44_b9f2,
            ntime: 0x4dd7_f5c7,
            clean_jobs: false,
            height: 125_552,
        };
        let header = job.header(1, 0x9546_a142);
        assert_eq!(
            hex::encode(header),
            "0100000081cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122bc7f5d74df2b9441a42a14695"
        );
        let hash = sha256d_header(&header);
        let mut shown = hash;
        shown.reverse();
        assert_eq!(
            hex::encode(shown),
            "00000000000000001e8d6829a8a21adc5d38d0a473b144b6765798e61f98bd1d"
        );
        // Above the network difficulty of its day (244 139).
        let d = warpaxe_stratum::job::hash_difficulty(&hash);
        assert!(d > 244_139.0, "{d}");
        // And the midstate path, as the workers run it.
        let mid = midstate(&header);
        assert_eq!(sha256d_tail(&mid, &tail(&header), 0x9546_a142), hash);
    }
}
