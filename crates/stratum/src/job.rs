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

impl Session {
    /// From the `mining.subscribe` result.
    pub fn new(extranonce1_hex: &str, extranonce2_size: usize) -> Option<Self> {
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
    /// The full coinbase transaction.
    pub coinbase: Vec<u8>,
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
}

/// Reverse the bytes within each 32-bit word: how the BM13xx wants
/// hashes handed over (ESP-Miner `reverse_32bit_words`).
pub fn reverse_32bit_words(h: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, w) in h.chunks_exact(4).enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&[w[3], w[2], w[1], w[0]]);
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

impl Job {
    /// Assemble the coinbase, hash the merkle path, order the header fields.
    pub fn build(session: &Session, n: &Notify, extranonce2: &[u8]) -> Option<Job> {
        if extranonce2.len() != session.extranonce2_size {
            return None;
        }
        let mut coinbase = hex::decode(&n.coinb1).ok()?;
        coinbase.extend_from_slice(&session.extranonce1);
        coinbase.extend_from_slice(extranonce2);
        coinbase.extend_from_slice(&hex::decode(&n.coinb2).ok()?);

        // Merkle root: coinbase txid (natural order) folded with each branch.
        let mut h = sha256d(&coinbase);
        for b in &n.merkle_branches {
            let branch = hex32(b)?;
            let mut both = [0u8; 64];
            both[..32].copy_from_slice(&h);
            both[32..].copy_from_slice(&branch);
            h = sha256d(&both);
        }
        // Stratum sends prevhash with every 32-bit word byte-swapped
        // relative to the header; undo that.
        let prev_stratum = hex32(&n.prevhash)?;
        Some(Job {
            job_id: n.job_id.clone(),
            extranonce2: extranonce2.to_vec(),
            coinbase,
            merkle_root: h,
            prev_hash: reverse_32bit_words(&prev_stratum),
            version: hex_u32(&n.version)?,
            nbits: hex_u32(&n.nbits)?,
            ntime: hex_u32(&n.ntime)?,
            clean_jobs: n.clean_jobs,
        })
    }

    /// The chip's view. ESP-Miner hands the BM13xx the previous hash in
    /// header order and the merkle root word-swapped; we do the same,
    /// because that is what the chip was proven to hash correctly.
    pub fn work(&self) -> Work {
        Work {
            starting_nonce: 0,
            nbits: self.nbits,
            ntime: self.ntime,
            merkle_root: reverse_32bit_words(&self.merkle_root),
            prev_block_hash: self.prev_hash,
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
