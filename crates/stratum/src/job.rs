//! From a `mining.notify` to what the chip hashes.

use warpaxe_chain::Work;

use crate::message::Notify;
use crate::sha256d;

/// What `mining.subscribe` gave us and what the pool set since.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Pool-assigned extranonce1.
    pub extranonce1: Vec<u8>,
    /// Bytes of extranonce2 we choose per job.
    pub extranonce2_size: usize,
    /// Share difficulty the pool wants; 1.0 until `mining.set_difficulty`.
    pub difficulty: f64,
    /// Version-rolling mask granted by `mining.configure` (0 = none).
    pub version_mask: u32,
}

/// The extranonce2 sizes a pool may ask for, in bytes. Pools use 4 to 8;
/// a size outside this range is a broken or hostile pool, and an
/// unchecked one would have the miner allocate whatever it says
/// (ForgeOS v1.7 checks it the same way).
pub const EXTRANONCE2_SIZES: core::ops::RangeInclusive<usize> = 1..=16;

impl Session {
    /// From the `mining.subscribe` result; `None` for an extranonce2 size
    /// outside [`EXTRANONCE2_SIZES`].
    pub fn new(extranonce1_hex: &str, extranonce2_size: usize) -> Option<Self> {
        if !EXTRANONCE2_SIZES.contains(&extranonce2_size) {
            return None;
        }
        Some(Session {
            extranonce1: hex::decode(extranonce1_hex).ok()?,
            extranonce2_size,
            difficulty: 1.0,
            version_mask: 0,
        })
    }
}

/// One unit of work derived from a notify with a chosen extranonce2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Pool's job id.
    pub job_id: String,
    /// The extranonce2 this job was built with.
    pub extranonce2: Vec<u8>,
    /// Merkle root in the block header's byte order.
    pub merkle_root: [u8; 32],
    /// Previous block hash in the block header's byte order.
    pub prev_hash: [u8; 32],
    /// Block version before rolling.
    pub version: u32,
    /// Compact target.
    pub nbits: u32,
    /// Block time.
    pub ntime: u32,
    /// The notify said to drop older jobs.
    pub clean_jobs: bool,
    /// The height of the block this job builds (BIP34, from the
    /// coinbase); 0 = not readable.
    pub height: u64,
}

/// Swap the bytes within each 32-bit word: how a stratum `prevhash`
/// becomes header order (ESP-Miner `reverse_endianness_per_word`).
pub fn reverse_endianness_per_word(h: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let (words, _) = h.as_chunks::<4>();
    for (i, w) in words.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&[w[3], w[2], w[1], w[0]]);
    }
    out
}

/// Reverse the order of the eight 32-bit words, bytes within a word
/// untouched: how the BM13xx wants the previous hash and the merkle
/// root handed over, and how the reference rebuilds the header from
/// what it sent (ESP-Miner `reverse_32bit_words`: `d[0] = s[7] …`). A
/// Gamma's nonces only verified once this was a word reversal and not,
/// as first transcribed, a byte swap within the words.
pub fn reverse_32bit_words(h: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[i * 4..i * 4 + 4].copy_from_slice(&h[(7 - i) * 4..(7 - i) * 4 + 4]);
    }
    out
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s).ok()?;
    v.try_into().ok()
}

fn hex_u32(s: &str) -> Option<u32> {
    u32::from_str_radix(s, 16).ok()
}

/// A `mining.notify` decoded once: the coinbase halves as bytes and the
/// merkle branches as hashes. Each job then only hashes them with its own
/// extranonce2; the coinbase is never assembled nor kept per job. A
/// coinbase that pays many outputs (10 KB and more; ESP-Miner#2081) would
/// otherwise be decoded for every job and copied into every job the
/// miner keeps (up to 64), in pieces below the 16 KB the ESP-IDF heap
/// puts into PSRAM, so into the scarce internal RAM.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    /// The notify as the pool sent it.
    pub notify: Notify,
    coinb1: Vec<u8>,
    coinb2: Vec<u8>,
    branches: Vec<[u8; 32]>,
    prev_hash: [u8; 32],
    version: u32,
    nbits: u32,
    ntime: u32,
    height: u64,
}

impl Prepared {
    /// `None` if a field is not the hex it should be.
    pub fn new(n: Notify) -> Option<Self> {
        let branches = n
            .merkle_branches
            .iter()
            .map(|b| hex32(b))
            .collect::<Option<Vec<_>>>()?;
        // Stratum sends prevhash with every 32-bit word byte-swapped
        // relative to the header; undo that.
        let prev_hash = reverse_endianness_per_word(&hex32(&n.prevhash)?);
        let coinb1 = hex::decode(&n.coinb1).ok()?;
        Some(Prepared {
            height: crate::coinbase::block_height(&coinb1).unwrap_or(0),
            coinb1,
            coinb2: hex::decode(&n.coinb2).ok()?,
            branches,
            prev_hash,
            version: hex_u32(&n.version)?,
            nbits: hex_u32(&n.nbits)?,
            ntime: hex_u32(&n.ntime)?,
            notify: n,
        })
    }

    /// The previous block hash in the block header's byte order.
    pub fn prev_hash(&self) -> &[u8; 32] {
        &self.prev_hash
    }

    /// The height of the block the notify builds (BIP34); 0 = not
    /// readable.
    pub fn height(&self) -> u64 {
        self.height
    }

    /// The coinbase halves as bytes (`coinb1`, `coinb2`).
    pub fn coinbase_parts(&self) -> (&[u8], &[u8]) {
        (&self.coinb1, &self.coinb2)
    }

    /// The full coinbase transaction for `extranonce2`.
    pub fn coinbase(&self, session: &Session, extranonce2: &[u8]) -> Vec<u8> {
        let mut c = Vec::with_capacity(
            self.coinb1.len() + session.extranonce1.len() + extranonce2.len() + self.coinb2.len(),
        );
        c.extend_from_slice(&self.coinb1);
        c.extend_from_slice(&session.extranonce1);
        c.extend_from_slice(extranonce2);
        c.extend_from_slice(&self.coinb2);
        c
    }

    /// The job for `extranonce2`; `None` if it has the wrong size.
    pub fn build(&self, session: &Session, extranonce2: &[u8]) -> Option<Job> {
        if extranonce2.len() != session.extranonce2_size {
            return None;
        }
        // Coinbase txid without assembling the coinbase.
        use sha2::{Digest, Sha256};
        let first = Sha256::new()
            .chain_update(&self.coinb1)
            .chain_update(&session.extranonce1)
            .chain_update(extranonce2)
            .chain_update(&self.coinb2)
            .finalize();
        let mut h: [u8; 32] = Sha256::digest(first).into();
        // Merkle root: coinbase txid (natural order) folded with each branch.
        for branch in &self.branches {
            let mut both = [0u8; 64];
            both[..32].copy_from_slice(&h);
            both[32..].copy_from_slice(branch);
            h = sha256d(&both);
        }
        Some(Job {
            job_id: self.notify.job_id.clone(),
            extranonce2: extranonce2.to_vec(),
            merkle_root: h,
            prev_hash: self.prev_hash,
            version: self.version,
            nbits: self.nbits,
            ntime: self.ntime,
            clean_jobs: self.notify.clean_jobs,
            height: self.height,
        })
    }
}

impl Job {
    /// Assemble the coinbase, hash the merkle path, order the header
    /// fields: [`Prepared::build`] for a notify used once.
    pub fn build(session: &Session, n: &Notify, extranonce2: &[u8]) -> Option<Job> {
        Prepared::new(n.clone())?.build(session, extranonce2)
    }

    /// The chip's view. ESP-Miner hands the BM13xx the previous hash in
    /// header order and the merkle root word-swapped; we do the same,
    /// because that is what the chip was proven to hash correctly.
    pub fn work(&self) -> Work {
        Work {
            starting_nonce: 0,
            nbits: self.nbits,
            ntime: self.ntime,
            // Both hashes go to the chip with the word order reversed
            // (ESP-Miner reverse_32bit_words); the chip hashes them back in
            // header order.
            merkle_root: reverse_32bit_words(&self.merkle_root),
            prev_block_hash: reverse_32bit_words(&self.prev_hash),
            version: self.version,
        }
    }

    /// The 80-byte block header for a nonce and rolled version, for
    /// checking a share against a target on the host or on the board.
    pub fn header(&self, version: u32, nonce: u32) -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&version.to_le_bytes());
        h[4..36].copy_from_slice(&self.prev_hash);
        h[36..68].copy_from_slice(&self.merkle_root);
        h[68..72].copy_from_slice(&self.ntime.to_le_bytes());
        h[72..76].copy_from_slice(&self.nbits.to_le_bytes());
        h[76..80].copy_from_slice(&nonce.to_le_bytes());
        h
    }

    /// Extranonce2 as the pool wants it in `mining.submit`.
    pub fn extranonce2_hex(&self) -> String {
        hex::encode(&self.extranonce2)
    }
}

/// Difficulty of a header hash (natural byte order), as pools count it:
/// `diff1_target / hash`, computed with the hash's leading 64 bits.
pub fn hash_difficulty(hash: &[u8; 32]) -> f64 {
    // The hash is little-endian: the most significant bytes are at the end.
    let mut be = *hash;
    be.reverse();
    // diff-1 target: 0x00000000FFFF0000000000000000000000000000000000000000000000000000
    // Use the first 12 bytes as a 96-bit number for adequate precision.
    let mut num = 0f64;
    for b in &be[..12] {
        num = num * 256.0 + f64::from(*b);
    }
    if num == 0.0 {
        return f64::INFINITY;
    }
    // 0xFFFF << (96 - 32 - 16) as f64
    let diff1 = 65535.0 * 2f64.powi(96 - 48);
    diff1 / num
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_refuses_extranonce2_sizes_out_of_range() {
        assert!(Session::new("abcd", 0).is_none());
        assert!(Session::new("abcd", 17).is_none());
        assert!(Session::new("abcd", usize::MAX).is_none());
        assert_eq!(Session::new("abcd", 4).unwrap().extranonce2_size, 4);
        assert_eq!(Session::new("abcd", 16).unwrap().extranonce2_size, 16);
    }
}
