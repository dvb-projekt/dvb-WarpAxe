//! What a display shows about the block being mined, read from the job:
//! the height (BIP34, first push of the coinbase script), the network
//! difficulty (from `nbits`) and the pool's tag in the coinbase script.

/// Bytes before the script length in a non-witness coinbase: version,
/// input count, the null prevout (hash + index).
const SCRIPT_LEN_AT: usize = 4 + 1 + 32 + 4;

fn varint(b: &[u8]) -> Option<(usize, usize)> {
    match *b.first()? {
        n @ 0..=0xfc => Some((n as usize, 1)),
        0xfd => Some((u16::from_le_bytes([*b.get(1)?, *b.get(2)?]) as usize, 3)),
        _ => None, // a coinbase script is at most 100 bytes
    }
}

/// The coinbase script as far as `coinb1` carries it, and its full length.
fn script_head(coinb1: &[u8]) -> Option<(&[u8], usize)> {
    let (len, n) = varint(coinb1.get(SCRIPT_LEN_AT..)?)?;
    Some((coinb1.get(SCRIPT_LEN_AT + n..)?, len))
}

/// Block height from the BIP34 push at the start of the coinbase script.
pub fn block_height(coinb1: &[u8]) -> Option<u64> {
    let (script, _) = script_head(coinb1)?;
    let op = *script.first()?;
    match op {
        // OP_1..OP_16 (heights 1-16 on test chains)
        0x51..=0x60 => Some(u64::from(op - 0x50)),
        1..=8 => {
            let bytes = script.get(1..1 + op as usize)?;
            Some(
                bytes
                    .iter()
                    .rev()
                    .fold(0u64, |acc, &b| (acc << 8) | u64::from(b)),
            )
        }
        _ => None,
    }
}

/// The readable text in the coinbase script (the pool's tag), taken from
/// both halves around the extranonce: runs of three or more printable
/// characters, joined by a space.
pub fn script_text(coinb1: &[u8], coinb2: &[u8], extranonce_len: usize) -> String {
    let Some((head, len)) = script_head(coinb1) else {
        return String::new();
    };
    // Skip the height push.
    let skip = match head.first() {
        Some(&n @ 1..=8) => 1 + n as usize,
        Some(0x51..=0x60) => 1,
        _ => 0,
    };
    let tail_len = len.saturating_sub(head.len() + extranonce_len);
    let tail = &coinb2[..tail_len.min(coinb2.len())];
    let mut out = String::new();
    for part in [head.get(skip..).unwrap_or(&[]), tail] {
        for run in part.split(|b| !(0x20..0x7f).contains(b)) {
            if run.len() >= 3 {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.extend(run.iter().map(|&b| b as char));
            }
        }
    }
    out.trim().to_string()
}

/// Network difficulty for a compact target (difficulty 1 = `0x1d00ffff`).
pub fn nbits_difficulty(nbits: u32) -> f64 {
    let exp = (nbits >> 24) as i32;
    let mant = f64::from(nbits & 0x00ff_ffff);
    if mant == 0.0 {
        return 0.0;
    }
    (f64::from(0xffffu32) / mant) * 256f64.powi(0x1d - exp)
}

#[cfg(test)]
mod tests {
    use super::*;

    // WarpPool job of 2026-09-23 (tests/vectors/warppool-2026-09-23.json).
    const COINB1: &str = "02000000010000000000000000000000000000000000000000000000000000000000000000ffffffff230357c60e64766270726f6a656b742d57617270506f6f6c";
    const COINB2: &str = "feffffff020ee4c212000000001600145a37f1fb0ef3ac9ff4c151c52118891cd8606a9e";

    #[test]
    fn height_is_the_bip34_push() {
        assert_eq!(block_height(&hex::decode(COINB1).unwrap()), Some(968_279));
    }

    #[test]
    fn script_text_is_the_pool_tag() {
        let (a, b) = (hex::decode(COINB1).unwrap(), hex::decode(COINB2).unwrap());
        // extranonce1 8 bytes + extranonce2 4 bytes fill the script exactly.
        assert_eq!(script_text(&a, &b, 12), "dvbprojekt-WarpPool");
    }

    #[test]
    fn text_after_the_extranonce_is_read_from_coinb2() {
        // Script: height push, "ab", 4 bytes extranonce, "tail!" .
        let mut c1 = hex::decode(&COINB1[..SCRIPT_LEN_AT * 2]).unwrap();
        c1.push(4 + 2 + 4 + 5);
        c1.extend_from_slice(&[3, 1, 2, 3, b'a', b'b']);
        let mut c2 = b"tail!".to_vec();
        c2.extend_from_slice(&[0xff; 4]);
        assert_eq!(script_text(&c1, &c2, 4), "tail!");
        assert_eq!(block_height(&c1), Some(0x030201));
    }

    #[test]
    fn difficulty_from_nbits() {
        assert_eq!(nbits_difficulty(0x1d00ffff), 1.0);
        let d = nbits_difficulty(0x17021ec5);
        assert!((d / 1e12 - 133.0).abs() < 1.5, "{d}");
    }

    #[test]
    fn garbage_gives_nothing() {
        assert_eq!(block_height(&[0; 10]), None);
        assert_eq!(script_text(&[0; 10], &[], 0), "");
    }
}
