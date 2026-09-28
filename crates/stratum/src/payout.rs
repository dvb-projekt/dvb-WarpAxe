//! Who gets paid if this job finds a block: the coinbase outputs, read
//! from the job (Stratum V1 `coinb1` + extranonce + `coinb2`, Sv2 prefix +
//! extranonce + suffix), with their addresses. AxeOS shows the same as
//! `coinbaseOutputs`; a solo pool pays everything to the miner's address,
//! a pool that keeps a fee shows it here.

use sha2::{Digest, Sha256};

/// One coinbase output: its value and who can spend it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Satoshis.
    pub value: u64,
    /// The address, or `OP_RETURN <hex>` for a data output (the witness
    /// commitment), or `script <hex>` for anything else.
    pub address: String,
}

/// The payout split of a job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Payout {
    /// Every output, in the order of the transaction.
    pub outputs: Vec<Output>,
    /// Sum of all outputs, satoshis.
    pub total: u64,
    /// Sum of the outputs to the miner's own address, satoshis.
    pub user: u64,
}

fn varint(b: &[u8], at: &mut usize) -> Option<u64> {
    let first = *b.get(*at)?;
    *at += 1;
    let n = match first {
        0..=0xfc => return Some(u64::from(first)),
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
    };
    let bytes = b.get(*at..*at + n)?;
    *at += n;
    Some(
        bytes
            .iter()
            .rev()
            .fold(0u64, |acc, &x| (acc << 8) | u64::from(x)),
    )
}

fn take<'a>(b: &'a [u8], at: &mut usize, n: usize) -> Option<&'a [u8]> {
    let s = b.get(*at..at.checked_add(n)?)?;
    *at += n;
    Some(s)
}

/// The outputs of a coinbase transaction (legacy or segwit serialisation;
/// the witness is not needed). `None` if it does not parse.
pub fn outputs(tx: &[u8], hrp: &str) -> Option<Vec<Output>> {
    let mut at = 4; // version
    if tx.get(4..6) == Some(&[0x00, 0x01]) {
        at += 2; // segwit marker and flag
    }
    let inputs = varint(tx, &mut at)?;
    for _ in 0..inputs {
        take(tx, &mut at, 36)?; // prevout
        let len = varint(tx, &mut at)? as usize;
        take(tx, &mut at, len)?; // script
        take(tx, &mut at, 4)?; // sequence
    }
    let n = varint(tx, &mut at)?;
    if n == 0 || n > 64 {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let v = take(tx, &mut at, 8)?;
        let value = u64::from_le_bytes(v.try_into().ok()?);
        let len = varint(tx, &mut at)? as usize;
        let script = take(tx, &mut at, len)?;
        out.push(Output {
            value,
            address: address(script, hrp),
        });
    }
    Some(out)
}

/// The payout of a job: its outputs, their total and what goes to
/// `user` (the address part of the worker name, before the first `.`).
/// The extranonce bytes only fill space; zeros parse the same.
pub fn payout(head: &[u8], extranonce_len: usize, tail: &[u8], user: &str) -> Option<Payout> {
    let mut tx = Vec::with_capacity(head.len() + extranonce_len + tail.len());
    tx.extend_from_slice(head);
    tx.resize(head.len() + extranonce_len, 0);
    tx.extend_from_slice(tail);
    let addr = user.split('.').next().unwrap_or("");
    let outputs = outputs(&tx, hrp_for(addr))?;
    let total = outputs.iter().map(|o| o.value).sum();
    let user = outputs
        .iter()
        .filter(|o| !addr.is_empty() && o.address.eq_ignore_ascii_case(addr))
        .map(|o| o.value)
        .sum();
    Some(Payout {
        outputs,
        total,
        user,
    })
}

/// The bech32 prefix of the chain the miner's own address is on, so the
/// outputs read in the same form: `tb` (testnet, signet), `bcrt`
/// (regtest), else `bc`.
pub fn hrp_for(addr: &str) -> &'static str {
    let a = addr.to_ascii_lowercase();
    if a.starts_with("bcrt1") {
        "bcrt"
    } else if a.starts_with("tb1") || a.starts_with(['m', 'n', '2']) {
        "tb"
    } else {
        "bc"
    }
}

/// The address a standard output script pays to.
pub fn address(script: &[u8], hrp: &str) -> String {
    let main = hrp == "bc";
    match script {
        [0x76, 0xa9, 0x14, h @ .., 0x88, 0xac] if h.len() == 20 => {
            base58check(if main { 0x00 } else { 0x6f }, h)
        }
        [0xa9, 0x14, h @ .., 0x87] if h.len() == 20 => {
            base58check(if main { 0x05 } else { 0xc4 }, h)
        }
        [0x00, n, p @ ..] if (*n == 20 || *n == 32) && p.len() == *n as usize => segwit(hrp, 0, p),
        [v @ 0x51..=0x60, n, p @ ..] if (2..=40).contains(n) && p.len() == *n as usize => {
            segwit(hrp, v - 0x50, p)
        }
        [0x6a, rest @ ..] => {
            // Skip the push opcode in front of the data.
            let data = match rest {
                [n @ 1..=75, d @ ..] if d.len() >= *n as usize => &d[..*n as usize],
                [0x4c, n, d @ ..] if d.len() >= *n as usize => &d[..*n as usize],
                _ => rest,
            };
            format!("OP_RETURN {}", hex::encode(data))
        }
        _ => format!("script {}", hex::encode(script)),
    }
}

const B58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

fn base58check(version: u8, payload: &[u8]) -> String {
    let mut data = Vec::with_capacity(25);
    data.push(version);
    data.extend_from_slice(payload);
    let check = Sha256::digest(Sha256::digest(&data));
    data.extend_from_slice(&check[..4]);
    // Big-number division by 58.
    let mut digits: Vec<u8> = Vec::new();
    for &byte in &data {
        let mut carry = u32::from(byte);
        for d in digits.iter_mut() {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let zeros = data.iter().take_while(|&&b| b == 0).count();
    let mut s = String::with_capacity(zeros + digits.len());
    s.extend(std::iter::repeat_n('1', zeros));
    s.extend(digits.iter().rev().map(|&d| B58[d as usize] as char));
    s
}

const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut chk: u32 = 1;
    for &v in values {
        let top = chk >> 25;
        chk = ((chk & 0x1ff_ffff) << 5) ^ u32::from(v);
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

/// A segwit address: bech32 for version 0, bech32m from version 1 on
/// (BIP 173, BIP 350).
fn segwit(hrp: &str, version: u8, program: &[u8]) -> String {
    let mut data = vec![version];
    // 8-bit bytes to 5-bit groups.
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in program {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            data.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        data.push(((acc << (5 - bits)) & 31) as u8);
    }
    let mut values: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|c| c & 31));
    values.extend_from_slice(&data);
    values.extend_from_slice(&[0; 6]);
    let constant = if version == 0 { 1 } else { 0x2bc8_30a3 };
    let pm = polymod(&values) ^ constant;
    let mut s = String::with_capacity(hrp.len() + 1 + data.len() + 6);
    s.push_str(hrp);
    s.push('1');
    s.extend(data.iter().map(|&d| CHARSET[d as usize] as char));
    s.extend((0..6).map(|i| CHARSET[((pm >> (5 * (5 - i))) & 31) as usize] as char));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Vec<u8> {
        hex::decode(s).unwrap()
    }

    #[test]
    fn addresses_match_the_bip_vectors() {
        // BIP 173
        assert_eq!(
            address(&h("0014751e76e8199196d454941c45d1b3a323f1433bd6"), "bc"),
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4"
        );
        assert_eq!(
            address(
                &h("00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262"),
                "tb"
            ),
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7"
        );
        // BIP 350 (bech32m, taproot)
        assert_eq!(
            address(
                &h("512079be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"),
                "bc"
            ),
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0"
        );
        // The genesis block's P2PKH address.
        assert_eq!(
            address(
                &h("76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac"),
                "bc"
            ),
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"
        );
        // A P2SH output (hash160 of an empty redeem script is not needed:
        // the version byte and checksum are what is tested).
        let p2sh = address(&h("a914e9c3dd0c07aac76179ebc76a6c78d4d67c6c160a87"), "bc");
        assert!(p2sh.starts_with('3'), "{p2sh}");
    }

    #[test]
    fn data_and_odd_scripts_are_named_not_dropped() {
        let commitment = format!("6a24aa21a9ed{}", "11".repeat(32));
        assert_eq!(
            address(&h(&commitment), "bc"),
            format!("OP_RETURN aa21a9ed{}", "11".repeat(32))
        );
        assert_eq!(address(&h("51"), "bc"), "script 51");
    }

    /// A coinbase as a pool sends it, split around an 8-byte extranonce:
    /// 3.125 BTC to the miner's P2WPKH address, the witness commitment.
    fn job() -> (Vec<u8>, Vec<u8>) {
        let mut head = h("01000000").to_vec();
        head.extend(h("01"));
        head.extend([0u8; 32]);
        head.extend(h("ffffffff"));
        // script: height push (3 bytes), tag "WarpPool", then the extranonce.
        let tag = b"WarpPool";
        let script_len = 4 + tag.len() + 8;
        head.push(script_len as u8);
        head.extend(h("03b2c80e"));
        head.extend(tag);
        let mut tail = h("ffffffff").to_vec();
        tail.push(2);
        tail.extend(312_500_000u64.to_le_bytes());
        tail.extend(h("160014751e76e8199196d454941c45d1b3a323f1433bd6"));
        tail.extend(0u64.to_le_bytes());
        tail.extend(h("26"));
        tail.extend(h("6a24aa21a9ed"));
        tail.extend([0x11u8; 32]);
        tail.extend(h("00000000"));
        (head, tail)
    }

    #[test]
    fn a_solo_job_pays_everything_to_the_miner() {
        let (head, tail) = job();
        let p = payout(
            &head,
            8,
            &tail,
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.NerdOctaxe",
        )
        .unwrap();
        assert_eq!(p.outputs.len(), 2);
        assert_eq!(p.total, 312_500_000);
        assert_eq!(p.user, 312_500_000);
        assert!(p.outputs[1].address.starts_with("OP_RETURN aa21a9ed"));
    }

    #[test]
    fn someone_elses_address_gets_nothing_counted_as_ours() {
        let (head, tail) = job();
        let p = payout(&head, 8, &tail, "bc1qsomeoneelse.worker").unwrap();
        assert_eq!((p.total, p.user), (312_500_000, 0));
    }

    #[test]
    fn a_segwit_serialised_coinbase_parses_too() {
        let (head, tail) = job();
        let mut sw = head[..4].to_vec();
        sw.extend(h("0001"));
        sw.extend(&head[4..]);
        let p = payout(&sw, 8, &tail, "x").unwrap();
        assert_eq!(p.outputs.len(), 2);
    }

    #[test]
    fn garbage_does_not_parse() {
        assert!(payout(&[1, 2, 3], 4, &[5], "x").is_none());
        assert!(outputs(&[], "bc").is_none());
    }

    #[test]
    fn the_chain_follows_the_miners_address() {
        assert_eq!(hrp_for("bc1qxyz"), "bc");
        assert_eq!(hrp_for("tb1qxyz"), "tb");
        assert_eq!(hrp_for("bcrt1qxyz"), "bcrt");
        assert_eq!(hrp_for("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"), "bc");
    }
}
