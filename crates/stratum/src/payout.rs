//! Who gets paid if this job finds a block: the coinbase outputs, read
//! from the job (Stratum V1 `coinb1` + extranonce + `coinb2`, Sv2 prefix +
//! extranonce + suffix), with their addresses. AxeOS shows the same as
//! `coinbaseOutputs`; a solo pool pays everything to the miner's address,
//! a pool that keeps a fee shows it here.
//!
//! The miner's own address is decoded, not compared as text, so the
//! split is right on any SHA-256 coin: Bitcoin, its forks with their own
//! base58 or bech32 prefixes (FixedCoin, DigiByte), and the CashAddr
//! coins (Bitcoin Cash, eCash).

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
    /// The worker name starts with an address. If not, it is a pool
    /// account, and the pool, not the coinbase, pays the miner.
    pub own_address: bool,
    /// That address is a Bitcoin one (mainnet, testnet, signet, regtest),
    /// so the amounts are in BTC.
    pub bitcoin: bool,
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
/// the witness is not needed): value and output script. `None` if it does
/// not parse.
fn raw_outputs(tx: &[u8]) -> Option<Vec<(u64, &[u8])>> {
    let mut at = 4; // version
    if tx.get(4..6) == Some(&[0x00, 0x01]) {
        at += 2; // segwit marker and flag
    }
    let inputs = varint(tx, &mut at)?;
    for _ in 0..inputs {
        take(tx, &mut at, 36)?; // prevout
        let len = usize::try_from(varint(tx, &mut at)?).ok()?;
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
        let len = usize::try_from(varint(tx, &mut at)?).ok()?;
        out.push((value, take(tx, &mut at, len)?));
    }
    Some(out)
}

/// The outputs of a coinbase transaction with Bitcoin addresses for the
/// chain `hrp` names. `None` if it does not parse.
pub fn outputs(tx: &[u8], hrp: &str) -> Option<Vec<Output>> {
    Some(
        raw_outputs(tx)?
            .into_iter()
            .map(|(value, script)| Output {
                value,
                address: address(script, hrp),
            })
            .collect(),
    )
}

/// The payout of a job: its outputs, their total and what goes to
/// `user` (the address part of the worker name, before the first `.`).
/// The extranonce bytes only fill space; zeros parse the same.
pub fn payout(head: &[u8], extranonce_len: usize, tail: &[u8], user: &str) -> Option<Payout> {
    let mut tx = Vec::with_capacity(head.len() + extranonce_len + tail.len());
    tx.extend_from_slice(head);
    tx.resize(head.len() + extranonce_len, 0);
    tx.extend_from_slice(tail);
    let raw = raw_outputs(&tx)?;
    let wallet = Wallet::parse(user.split('.').next().unwrap_or(""));
    let mut p = Payout {
        own_address: wallet.is_some(),
        bitcoin: wallet.as_ref().is_none_or(|w| w.bitcoin().is_some()),
        ..Payout::default()
    };
    for (value, script) in raw {
        // The pool sends these numbers: more than all bitcoin there will
        // ever be is not a coinbase.
        p.total = p.total.checked_add(value).filter(|&t| t <= MAX_SAT)?;
        let address = match &wallet {
            Some(w) if w.pays(script) => {
                p.user += value; // ≤ total, checked above
                w.text.clone()
            }
            Some(w) => w.label(script),
            None => address(script, "bc"),
        };
        p.outputs.push(Output { value, address });
    }
    Some(p)
}

/// 21 million coins in satoshis; no SHA-256 coin's coinbase pays more.
const MAX_SAT: u64 = 21_000_000 * 100_000_000;

/// The miner's own address, decoded: what its outputs look like and how
/// the coin writes the other outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wallet {
    /// As the miner typed it.
    pub text: String,
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// Base58check: version byte and the 20-byte hash.
    Base58 { version: u8, hash: [u8; 20] },
    /// CashAddr (Bitcoin Cash, eCash): prefix, P2SH or P2PKH, hash.
    Cash {
        prefix: String,
        p2sh: bool,
        hash: [u8; 20],
    },
    /// Bech32/bech32m: prefix, witness version and program.
    Segwit {
        hrp: String,
        version: u8,
        program: Vec<u8>,
    },
}

/// CashAddr prefixes tried for an address typed without one.
const CASH_PREFIXES: [&str; 6] = [
    "bitcoincash",
    "ecash",
    "bchtest",
    "ectest",
    "bchreg",
    "ecregtest",
];

impl Wallet {
    /// Decode an address of any SHA-256 coin; `None` for anything else
    /// (a pool account name).
    pub fn parse(addr: &str) -> Option<Wallet> {
        let addr = addr.trim();
        let kind = decode_segwit(addr)
            .or_else(|| decode_cash(addr))
            .or_else(|| decode_base58(addr))?;
        Some(Wallet {
            text: addr.to_string(),
            kind,
        })
    }

    /// `Some(mainnet)` for a Bitcoin address, `None` for another coin.
    pub fn bitcoin(&self) -> Option<bool> {
        match &self.kind {
            Kind::Base58 { version, .. } => match version {
                0x00 | 0x05 => Some(true),
                0x6f | 0xc4 => Some(false),
                _ => None,
            },
            Kind::Segwit { hrp, .. } => match hrp.as_str() {
                "bc" => Some(true),
                "tb" | "bcrt" => Some(false),
                _ => None,
            },
            Kind::Cash { .. } => None,
        }
    }

    /// Whether an output script pays this address.
    pub fn pays(&self, script: &[u8]) -> bool {
        match &self.kind {
            Kind::Base58 { version, hash } => {
                let (pkh, sh) = match version {
                    // Bitcoin (and the legacy form of BCH), DigiByte.
                    0x00 | 0x6f | 0x1e => (true, false),
                    0x05 | 0xc4 | 0x3f => (false, true),
                    // Another coin's version byte: counted as a key hash
                    // only, so a script-hash output cannot pass for ours.
                    _ => (true, false),
                };
                (pkh && p2pkh(script) == Some(hash)) || (sh && p2sh(script) == Some(hash))
            }
            Kind::Cash { p2sh: sh, hash, .. } => {
                if *sh {
                    p2sh(script) == Some(hash)
                } else {
                    p2pkh(script) == Some(hash)
                }
            }
            Kind::Segwit {
                version, program, ..
            } => witness(script) == Some((*version, program.as_slice())),
        }
    }

    /// Another output, written the way this coin writes addresses; as the
    /// script itself where the coin's form is not known.
    pub fn label(&self, script: &[u8]) -> String {
        if let Some(main) = self.bitcoin() {
            let hrp = match &self.kind {
                Kind::Segwit { hrp, .. } => hrp.as_str(),
                _ if main => "bc",
                _ => "tb",
            };
            return address(script, hrp);
        }
        if script.first() == Some(&0x6a) {
            return address(script, "bc");
        }
        match &self.kind {
            Kind::Cash { prefix, .. } => {
                if let Some(h) = p2pkh(script) {
                    return cashaddr(prefix, false, h);
                }
                if let Some(h) = p2sh(script) {
                    return cashaddr(prefix, true, h);
                }
            }
            Kind::Segwit { hrp, .. } => {
                if let Some((v, prog)) = witness(script) {
                    return segwit(hrp, v, prog);
                }
            }
            Kind::Base58 { .. } => {}
        }
        format!("script {}", hex::encode(script))
    }
}

fn p2pkh(script: &[u8]) -> Option<&[u8; 20]> {
    match script {
        [0x76, 0xa9, 0x14, h @ .., 0x88, 0xac] => h.try_into().ok(),
        _ => None,
    }
}

fn p2sh(script: &[u8]) -> Option<&[u8; 20]> {
    match script {
        [0xa9, 0x14, h @ .., 0x87] => h.try_into().ok(),
        _ => None,
    }
}

fn witness(script: &[u8]) -> Option<(u8, &[u8])> {
    match script {
        [0x00, n, p @ ..] if (*n == 20 || *n == 32) && p.len() == *n as usize => Some((0, p)),
        [v @ 0x51..=0x60, n, p @ ..] if (2..=40).contains(n) && p.len() == *n as usize => {
            Some((v - 0x50, p))
        }
        _ => None,
    }
}

/// 5-bit groups to bytes; `None` if the padding is not clean.
fn from_5bit(data: &[u8]) -> Option<Vec<u8>> {
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut out = Vec::with_capacity(data.len() * 5 / 8);
    for &d in data {
        if d > 31 {
            return None;
        }
        acc = (acc << 5) | u32::from(d);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 5 || (acc & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

fn to_5bit(bytes: &[u8]) -> Vec<u8> {
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut out = Vec::with_capacity(bytes.len() * 8 / 5 + 1);
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        out.push(((acc << (5 - bits)) & 31) as u8);
    }
    out
}

/// The bech32 characters of a lower-case string as 5-bit values.
fn values(s: &str) -> Option<Vec<u8>> {
    s.bytes()
        .map(|c| CHARSET.iter().position(|&x| x == c).map(|i| i as u8))
        .collect()
}

fn decode_segwit(addr: &str) -> Option<Kind> {
    let lower = addr.to_ascii_lowercase();
    if addr != lower && addr != addr.to_ascii_uppercase() {
        return None;
    }
    let (hrp, data) = lower.rsplit_once('1')?;
    if hrp.is_empty() || data.len() < 7 || lower.len() > 90 || hrp.contains(':') {
        return None;
    }
    let data = values(data)?;
    let mut v: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
    v.push(0);
    v.extend(hrp.bytes().map(|c| c & 31));
    v.extend_from_slice(&data);
    let check = polymod(&v);
    let version = data[0];
    let program = from_5bit(&data[1..data.len() - 6])?;
    let ok = match version {
        0 => check == 1 && (program.len() == 20 || program.len() == 32),
        1..=16 => check == 0x2bc8_30a3 && (2..=40).contains(&program.len()),
        _ => false,
    };
    ok.then(|| Kind::Segwit {
        hrp: hrp.to_string(),
        version,
        program,
    })
}

fn cash_polymod(v: &[u8]) -> u64 {
    const GEN: [u64; 5] = [
        0x98_f2bc_8e61,
        0x79_b76d_99e2,
        0xf3_3e5f_b3c4,
        0xae_2eab_e2a8,
        0x1e_4f43_e470,
    ];
    let mut c: u64 = 1;
    for &d in v {
        let top = c >> 35;
        c = ((c & 0x07_ffff_ffff) << 5) ^ u64::from(d);
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                c ^= g;
            }
        }
    }
    c ^ 1
}

fn cash_prefix_values(prefix: &str) -> Vec<u8> {
    let mut v: Vec<u8> = prefix.bytes().map(|c| c & 31).collect();
    v.push(0);
    v
}

fn decode_cash(addr: &str) -> Option<Kind> {
    let lower = addr.to_ascii_lowercase();
    if addr != lower && addr != addr.to_ascii_uppercase() {
        return None;
    }
    let (prefixes, payload): (Vec<&str>, &str) = match lower.split_once(':') {
        Some((p, rest)) => (vec![p], rest),
        None => (CASH_PREFIXES.to_vec(), lower.as_str()),
    };
    let data = values(payload)?;
    if data.len() != 42 {
        return None; // 160-bit hashes only
    }
    let prefix = prefixes.into_iter().find(|p| {
        let mut v = cash_prefix_values(p);
        v.extend_from_slice(&data);
        cash_polymod(&v) == 0
    })?;
    let bytes = from_5bit(&data[..data.len() - 8])?;
    let (version, hash) = bytes.split_first()?;
    if version & 0x07 != 0 || version & 0x80 != 0 {
        return None;
    }
    let p2sh = match version >> 3 {
        0 => false,
        1 => true,
        _ => return None,
    };
    Some(Kind::Cash {
        prefix: prefix.to_string(),
        p2sh,
        hash: hash.try_into().ok()?,
    })
}

/// A CashAddr address with its prefix.
fn cashaddr(prefix: &str, p2sh: bool, hash: &[u8; 20]) -> String {
    let mut payload = vec![if p2sh { 0x08 } else { 0x00 }];
    payload.extend_from_slice(hash);
    let data = to_5bit(&payload);
    let mut v = cash_prefix_values(prefix);
    v.extend_from_slice(&data);
    v.extend_from_slice(&[0; 8]);
    let pm = cash_polymod(&v);
    let mut s = String::with_capacity(prefix.len() + 1 + data.len() + 8);
    s.push_str(prefix);
    s.push(':');
    s.extend(data.iter().map(|&d| CHARSET[d as usize] as char));
    s.extend((0..8).map(|i| CHARSET[((pm >> (5 * (7 - i))) & 31) as usize] as char));
    s
}

fn decode_base58(addr: &str) -> Option<Kind> {
    let mut bytes: Vec<u8> = Vec::with_capacity(25);
    for c in addr.bytes() {
        let mut carry = B58.iter().position(|&x| x == c)? as u32;
        for b in bytes.iter_mut() {
            carry += u32::from(*b) * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            bytes.push(carry as u8);
            carry >>= 8;
        }
    }
    let zeros = addr.bytes().take_while(|&c| c == b'1').count();
    bytes.extend(std::iter::repeat_n(0, zeros));
    bytes.reverse();
    if bytes.len() != 25 {
        return None;
    }
    let check = Sha256::digest(Sha256::digest(&bytes[..21]));
    if check[..4] != bytes[21..] {
        return None;
    }
    Some(Kind::Base58 {
        version: bytes[0],
        hash: bytes[1..21].try_into().ok()?,
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
    data.extend(to_5bit(program));
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

    /// The job's coinbase with its first output paying `script` instead.
    fn job_paying(script: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (head, mut tail) = job();
        let at = 4 + 1 + 8; // sequence, output count, value
        let old = 1 + tail[at] as usize;
        let mut s = vec![script.len() as u8];
        s.extend_from_slice(script);
        tail.splice(at..at + old, s);
        (head, tail)
    }

    const HASH: &str = "76a04053bda0a88bda5177b86a15c3b29f559873";

    fn pkh() -> Vec<u8> {
        h(&format!("76a914{HASH}88ac"))
    }

    #[test]
    fn cashaddr_matches_the_spec_vectors() {
        // The CashAddr specification's example: the same hash as
        // 1BpEi6DfDAUFd7GtittLSdBeYJvcoaVggu.
        let w = Wallet::parse("bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a").unwrap();
        assert!(w.pays(&pkh()));
        assert!(!w.pays(&h(&format!("a914{HASH}87"))), "P2SH is not ours");
        assert_eq!(w.bitcoin(), None);
        // Without its prefix, and upper case.
        let bare = Wallet::parse("QPM2QSZNHKS23Z7629MMS6S4CWEF74VCWVY22GDX6A").unwrap();
        assert!(bare.pays(&pkh()));
        let sh = Wallet::parse("bitcoincash:ppm2qsznhks23z7629mms6s4cwef74vcwvn0h829pq").unwrap();
        assert!(sh.pays(&h(&format!("a914{HASH}87"))));
        // A wrong checksum is not an address.
        assert!(Wallet::parse("bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6b").is_none());
        // Our encoder writes the spec's form back.
        let hash: [u8; 20] = h(HASH).try_into().unwrap();
        assert_eq!(
            cashaddr("bitcoincash", false, &hash),
            "bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6a"
        );
    }

    #[test]
    fn ecash_and_its_outputs_are_written_as_ecash() {
        let hash: [u8; 20] = h(HASH).try_into().unwrap();
        let addr = cashaddr("ecash", false, &hash);
        let (head, tail) = job_paying(&pkh());
        let typed = addr.trim_start_matches("ecash:").to_string();
        let p = payout(&head, 8, &tail, &format!("{typed}.bitaxe")).unwrap();
        assert_eq!(p.user, 312_500_000);
        assert!(p.own_address && !p.bitcoin);
        assert_eq!(p.outputs[0].address, typed, "ours as typed");
        let w = Wallet::parse(&typed).unwrap();
        let other = h("76a914000000000000000000000000000000000000000088ac");
        assert!(w.label(&other).starts_with("ecash:q"));
    }

    #[test]
    fn a_fork_with_its_own_base58_prefix_is_still_ours() {
        // DigiByte P2PKH (version 0x1e, "D…"); FixedCoin and other forks
        // work the same way whatever their version byte is.
        let hash: [u8; 20] = h(HASH).try_into().unwrap();
        let dgb = base58check(0x1e, &hash);
        assert!(dgb.starts_with('D'), "{dgb}");
        let (head, tail) = job_paying(&pkh());
        let p = payout(&head, 8, &tail, &format!("{dgb}.rig1")).unwrap();
        assert_eq!((p.total, p.user), (312_500_000, 312_500_000));
        assert!(p.own_address && !p.bitcoin);
        // The same hash as a Bitcoin P2PKH address also matches, but a
        // Bitcoin P2SH address with that hash does not.
        let btc_sh = base58check(0x05, &hash);
        let p = payout(&head, 8, &tail, &btc_sh).unwrap();
        assert_eq!(p.user, 0);
        assert!(p.bitcoin);
        // A broken checksum is not an address.
        let mut bad = dgb.clone().into_bytes();
        let last = bad.len() - 1;
        bad[last] = if bad[last] == b'2' { b'3' } else { b'2' };
        assert!(Wallet::parse(std::str::from_utf8(&bad).unwrap()).is_none());
    }

    #[test]
    fn a_fork_with_its_own_bech32_prefix_is_still_ours() {
        let prog = h("751e76e8199196d454941c45d1b3a323f1433bd6");
        let dgb = segwit("dgb", 0, &prog);
        let (head, tail) = job();
        let p = payout(&head, 8, &tail, &format!("{dgb}.x")).unwrap();
        assert_eq!(p.user, 312_500_000);
        assert!(!p.bitcoin);
        let w = Wallet::parse(&dgb).unwrap();
        let other = h(&format!("0014{}", "22".repeat(20)));
        assert!(w.label(&other).starts_with("dgb1q"));
        // The witness commitment keeps its name.
        assert!(p.outputs[1].address.starts_with("OP_RETURN"));
    }

    #[test]
    fn bitcoin_addresses_still_decode() {
        for a in [
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0",
            "1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa",
            "tb1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3q0sl5k7",
        ] {
            let w = Wallet::parse(a).unwrap_or_else(|| panic!("{a}"));
            assert!(w.bitcoin().is_some(), "{a}");
        }
        // bech32 with the bech32m constant (and the reverse) is invalid.
        assert!(Wallet::parse("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5").is_none());
    }

    #[test]
    fn an_unknown_fork_version_counts_only_key_hashes() {
        let hash: [u8; 20] = h(HASH).try_into().unwrap();
        let fork = base58check(0x23, &hash);
        let w = Wallet::parse(&fork).unwrap();
        assert!(w.pays(&pkh()));
        assert!(!w.pays(&h(&format!("a914{HASH}87"))));
        // DigiByte's script-hash version is known.
        let dgb_sh = Wallet::parse(&base58check(0x3f, &hash)).unwrap();
        assert!(dgb_sh.pays(&h(&format!("a914{HASH}87"))));
        assert!(!dgb_sh.pays(&pkh()));
    }

    #[test]
    fn impossible_amounts_do_not_parse() {
        let (head, mut tail) = job();
        let at = 4 + 1;
        tail[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(payout(&head, 8, &tail, "x").is_none());
    }

    #[test]
    fn a_pool_account_is_not_an_address() {
        let (head, tail) = job();
        let p = payout(&head, 8, &tail, "jY9cmgRLes.Bit-Four").unwrap();
        assert_eq!((p.total, p.user), (312_500_000, 0));
        assert!(!p.own_address);
        assert!(p.bitcoin, "unknown, so the amounts stay in BTC");
        assert!(p.outputs[0].address.starts_with("bc1q"));
    }
}
