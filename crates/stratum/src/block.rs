//! A found block: the exact test, its hash, and what became of it.
//!
//! A nonce is a block when its header hash is at or below the target in
//! the header's own `nbits`, compared as 256-bit numbers ([`meets_target`]);
//! the difficulty the dashboard shows is a rounded figure and belongs to
//! whatever job is current, not to the one the nonce was found on.
//!
//! What became of the block shows in the work the pool sends next
//! ([`judge`]): a job whose previous hash is the block's hash builds on
//! it (confirmed); a job for the next height that names another previous
//! hash says the network built on a different block (orphaned). Until
//! one of the two arrives the block is pending; when the first work
//! that says anything is already for a height further on (the miner was
//! off or without a pool in between), it can no longer be told and the
//! block is unknown. ForgeOS v1.8 tracks a found block the same way.
//!
//! Hashes here are in the header's byte order, as SHA-256d returns them
//! and as [`crate::Job::prev_hash`] holds them; [`hash_hex`] is the
//! reversed form block explorers show.

/// The 256-bit target of a compact `nbits`, big-endian; `None` for a
/// value Bitcoin rejects (the sign bit set, or a target that does not
/// fit 256 bits).
pub fn target(nbits: u32) -> Option<[u8; 32]> {
    let exponent = (nbits >> 24) as usize;
    let mantissa = nbits & 0x007f_ffff;
    if nbits & 0x0080_0000 != 0 {
        return None;
    }
    let mut t = [0u8; 32];
    if mantissa == 0 {
        return Some(t);
    }
    let m = mantissa.to_be_bytes();
    // The mantissa's three bytes end `exponent` bytes above the number's
    // low end: byte k of the mantissa (0 = most significant of the three)
    // has the value 256^(exponent - 1 - k).
    for (k, &byte) in m[1..].iter().enumerate() {
        if byte == 0 {
            continue;
        }
        // Below the low end (an exponent under 3): shifted out.
        let Some(pos) = (exponent).checked_sub(1 + k) else {
            continue;
        };
        if pos >= 32 {
            return None;
        }
        t[31 - pos] = byte;
    }
    Some(t)
}

/// The header hash (header byte order) is at or below the target of
/// `nbits`: the header is a block.
pub fn meets_target(hash: &[u8; 32], nbits: u32) -> bool {
    let Some(t) = target(nbits) else {
        return false;
    };
    // The hash is little-endian as a number: compare from its last byte.
    hash.iter().rev().cmp(t.iter()) != core::cmp::Ordering::Greater
}

/// The hash as block explorers show it: reversed, hex.
pub fn hash_hex(hash: &[u8; 32]) -> String {
    let mut be = *hash;
    be.reverse();
    hex::encode(be)
}

/// Back from [`hash_hex`] to the header's byte order.
pub fn hash_from_hex(s: &str) -> Option<[u8; 32]> {
    let mut h: [u8; 32] = hex::decode(s).ok()?.try_into().ok()?;
    h.reverse();
    Some(h)
}

/// What became of a found block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Found; no later work has said anything about it yet.
    Pending,
    /// The pool's next work builds on it.
    Confirmed,
    /// Work for the next height builds on another block.
    Orphaned,
    /// Still pending when work for a height two or more further on
    /// came: the work for the next height was never seen, so neither of
    /// the two can be told any more.
    Unknown,
}

impl Status {
    /// The API's and the stored record's word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Confirmed => "confirmed",
            Status::Orphaned => "orphaned",
            Status::Unknown => "unknown",
        }
    }

    /// From [`Status::as_str`].
    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "pending" => Status::Pending,
            "confirmed" => Status::Confirmed,
            "orphaned" => Status::Orphaned,
            "unknown" => Status::Unknown,
            _ => return None,
        })
    }
}

/// What a job says about a block found at `height` with `hash` whose
/// status is `now`: the job's previous hash and the height it builds
/// (0 = not readable). `None` when it says nothing new: work for the
/// block's own height (the pool has not moved on), or work further on
/// for a block that already has its verdict.
pub fn judge(
    hash: &[u8; 32],
    height: u64,
    now: Status,
    job_prev: &[u8; 32],
    job_height: u64,
) -> Option<Status> {
    let next = if job_prev == hash {
        Status::Confirmed
    } else if height > 0 && job_height == height + 1 {
        Status::Orphaned
    } else if height > 0 && job_height > height + 1 && now == Status::Pending {
        // Its parent is a later block's whichever chain won.
        Status::Unknown
    } else {
        return None;
    };
    (next != now).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256d;

    /// An 80-byte header from its fields as explorers show them.
    fn header(
        version: u32,
        prev: &str,
        merkle: &str,
        time: u32,
        bits: u32,
        nonce: u32,
    ) -> [u8; 80] {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&version.to_le_bytes());
        h[4..36].copy_from_slice(&hash_from_hex(prev).unwrap());
        h[36..68].copy_from_slice(&hash_from_hex(merkle).unwrap());
        h[68..72].copy_from_slice(&time.to_le_bytes());
        h[72..76].copy_from_slice(&bits.to_le_bytes());
        h[76..80].copy_from_slice(&nonce.to_le_bytes());
        h
    }

    const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const GENESIS: &str = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
    const BLOCK_1: &str = "00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048";
    const BLOCK_125552: &str = "00000000000000001e8d6829a8a21adc5d38d0a473b144b6765798e61f98bd1d";

    /// Bitcoin's genesis block.
    fn genesis() -> [u8; 80] {
        header(
            1,
            ZERO,
            "4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b",
            1_231_006_505,
            0x1d00_ffff,
            2_083_236_893,
        )
    }

    /// Bitcoin block 1.
    fn block_1() -> [u8; 80] {
        header(
            1,
            GENESIS,
            "0e3e2357e806b6cdb1f70b54c3a3a17b6714ee1f0e68bebb44a74b1efd512098",
            1_231_469_665,
            0x1d00_ffff,
            2_573_394_689,
        )
    }

    /// Bitcoin block 125552 (May 2011, difficulty 244,112).
    fn block_125552() -> [u8; 80] {
        header(
            1,
            "00000000000008a3a41b85b8b29ad444def299fee21793cd8b9e567eab02cd81",
            "2b12fcf1b09288fcaff797d71e950e71ae42b91e8bdb2304758dfcffc2b620e3",
            1_305_998_791,
            0x1a44_b9f2,
            2_504_433_986,
        )
    }

    #[test]
    fn real_headers_hash_to_their_block_hashes() {
        assert_eq!(hash_hex(&sha256d(&genesis())), GENESIS);
        assert_eq!(hash_hex(&sha256d(&block_1())), BLOCK_1);
        assert_eq!(hash_hex(&sha256d(&block_125552())), BLOCK_125552);
        let h = sha256d(&block_1());
        assert_eq!(hash_from_hex(&hash_hex(&h)), Some(h));
        assert_eq!(hash_from_hex("00"), None);
        assert_eq!(hash_from_hex("zz"), None);
    }

    #[test]
    fn the_compact_target_expands_as_bitcoin_does_it() {
        assert_eq!(
            hex::encode(target(0x1d00_ffff).unwrap()),
            "00000000ffff0000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex::encode(target(0x1a44_b9f2).unwrap()),
            "00000000000044b9f20000000000000000000000000000000000000000000000"
        );
        // Mainnet, block 840000.
        assert_eq!(
            hex::encode(target(0x1703_4219).unwrap()),
            "0000000000000000000342190000000000000000000000000000000000000000"
        );
        // Exponents at and under three: the mantissa's low bytes fall off.
        assert_eq!(target(0x0312_3456).unwrap()[29..], [0x12, 0x34, 0x56]);
        assert_eq!(target(0x0212_3456).unwrap()[29..], [0x00, 0x12, 0x34]);
        assert_eq!(target(0x0112_3456).unwrap()[29..], [0x00, 0x00, 0x12]);
        assert_eq!(target(0x0012_3456).unwrap(), [0u8; 32]);
        // Regtest's 0x207fffff: the top of the range.
        assert_eq!(target(0x207f_ffff).unwrap()[..3], [0x7f, 0xff, 0xff]);
        // The sign bit, and targets past 256 bits: not a target.
        assert_eq!(target(0x1d80_ffff), None);
        assert_eq!(target(0x2100_ffff).unwrap()[..3], [0xff, 0xff, 0x00]);
        assert_eq!(target(0x2200_ffff), None);
        assert_eq!(target(0x2101_0000), None);
        assert_eq!(target(0xff12_3456), None);
        // A mantissa of zero is the target zero, whatever the exponent.
        assert_eq!(target(0x2100_0000), Some([0u8; 32]));
    }

    #[test]
    fn a_block_meets_its_own_target_and_its_neighbours_do_not() {
        for (h, bits) in [
            (genesis(), 0x1d00_ffff),
            (block_1(), 0x1d00_ffff),
            (block_125552(), 0x1a44_b9f2),
        ] {
            assert!(meets_target(&sha256d(&h), bits));
            // The next nonce on the same header: not a block.
            let mut other = h;
            let n = u32::from_le_bytes(other[76..80].try_into().unwrap()).wrapping_add(1);
            other[76..80].copy_from_slice(&n.to_le_bytes());
            assert!(!meets_target(&sha256d(&other), bits));
        }
        // The genesis hash is a share at difficulty 1, not a block at the
        // difficulty of May 2011 (which a rounded float could not tell
        // apart only at the edge; here it is far).
        assert!(!meets_target(&sha256d(&genesis()), 0x1a44_b9f2));
        // Exactly the target counts, one above does not.
        let t = target(0x1a44_b9f2).unwrap();
        let mut at = t;
        at.reverse();
        assert!(meets_target(&at, 0x1a44_b9f2));
        let mut above = t;
        above[31] = 1;
        above.reverse();
        assert!(!meets_target(&above, 0x1a44_b9f2));
        let mut below = t;
        below[8] -= 1;
        below[31] = 0xff;
        below.reverse();
        assert!(meets_target(&below, 0x1a44_b9f2));
        // An invalid target is met by nothing.
        assert!(!meets_target(&[0u8; 32], 0x1d80_ffff));
    }

    #[test]
    fn the_next_real_header_confirms_the_block() {
        // Found: the genesis block, height 0 is "unknown" here, so take
        // block 1 as found at height 1 and the chain's real block 2.
        let found = sha256d(&block_1());
        // Work for height 1 on the same parent: says nothing.
        let genesis_hash = sha256d(&genesis());
        assert_eq!(judge(&found, 1, Status::Pending, &genesis_hash, 1), None);
        // Block 2's header names block 1 as its parent.
        let block_2 = header(
            1,
            BLOCK_1,
            "9b0fc92260312ce44e74ef369f5c66bbb85848f2eddd5a7a1cde251e54ccfdd5",
            1_231_469_744,
            0x1d00_ffff,
            1_639_830_024,
        );
        assert_eq!(
            hash_hex(&sha256d(&block_2)),
            "000000006a625f06636b8bb6ac7b960a8d03705d1ace08b1a19da3fdcc99ddbd"
        );
        let prev: [u8; 32] = block_2[4..36].try_into().unwrap();
        assert_eq!(
            judge(&found, 1, Status::Pending, &prev, 2),
            Some(Status::Confirmed)
        );
        // After the block had been written off, too; and said once.
        assert_eq!(
            judge(&found, 1, Status::Unknown, &prev, 2),
            Some(Status::Confirmed)
        );
        assert_eq!(judge(&found, 1, Status::Confirmed, &prev, 2), None);
        // Also when the job's height could not be read.
        assert_eq!(
            judge(&found, 1, Status::Pending, &prev, 0),
            Some(Status::Confirmed)
        );
    }

    #[test]
    fn work_for_the_next_height_on_another_block_orphans_it() {
        let found = sha256d(&block_125552());
        // A competing block at 125552 won: the work for 125553 names it.
        let rival = hash_from_hex(BLOCK_1).unwrap();
        let p = Status::Pending;
        assert_eq!(
            judge(&found, 125_552, p, &rival, 125_553),
            Some(Status::Orphaned)
        );
        assert_eq!(
            judge(&found, 125_552, Status::Orphaned, &rival, 125_553),
            None
        );
        // A reorg at the next height turns a confirmed block around.
        assert_eq!(
            judge(&found, 125_552, Status::Confirmed, &rival, 125_553),
            Some(Status::Orphaned)
        );
        // Work on the same height, or of unreadable height, says nothing.
        assert_eq!(judge(&found, 125_552, p, &rival, 125_552), None);
        assert_eq!(judge(&found, 125_552, p, &rival, 0), None);
        // A block whose own height is unknown is never called orphaned.
        assert_eq!(judge(&found, 0, p, &rival, 1), None);
    }

    #[test]
    fn work_two_heights_on_leaves_a_pending_block_unknown() {
        let found = sha256d(&block_125552());
        let later = hash_from_hex(BLOCK_1).unwrap();
        // The miner was off while block 125553 came: the first work it
        // sees is for 125554 (or further), on a parent it cannot place.
        for h in [125_554, 125_555, 900_000] {
            assert_eq!(
                judge(&found, 125_552, Status::Pending, &later, h),
                Some(Status::Unknown)
            );
        }
        // Said once, and a block with its verdict keeps it.
        for now in [Status::Unknown, Status::Confirmed, Status::Orphaned] {
            assert_eq!(judge(&found, 125_552, now, &later, 125_554), None);
        }
        // Without the block's own height there is nothing to count from.
        assert_eq!(judge(&found, 0, Status::Pending, &later, 125_554), None);
    }

    #[test]
    fn the_status_words_round_trip() {
        for s in [
            Status::Pending,
            Status::Confirmed,
            Status::Orphaned,
            Status::Unknown,
        ] {
            assert_eq!(Status::parse(s.as_str()), Some(s));
        }
        assert_eq!(Status::parse(""), None);
        assert_eq!(Status::parse("Confirmed"), None);
    }
}
