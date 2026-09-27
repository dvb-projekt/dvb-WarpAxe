//! The pool's authority key as pasted into the pool settings: Base58Check
//! of `version u16 LE (1) | x-only key (32)`, the format `key-utils`
//! prints and every Sv2 pool publishes.

use crate::Error;
use sha2::{Digest, Sha256};

const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Plain Base58 decode.
fn base58(s: &str) -> Result<Vec<u8>, Error> {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    for c in s.bytes() {
        let digit = ALPHABET.iter().position(|&a| a == c).ok_or(Error::BadKey)? as u32;
        let mut carry = digit;
        for b in out.iter_mut() {
            let v = u32::from(*b) * 58 + carry;
            *b = (v & 0xFF) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            out.push((carry & 0xFF) as u8);
            carry >>= 8;
        }
    }
    // Leading '1's are leading zero bytes.
    let zeros = s.bytes().take_while(|&c| c == b'1').count();
    out.extend(std::iter::repeat_n(0u8, zeros));
    out.reverse();
    Ok(out)
}

/// The 32-byte x-only authority key from its Base58Check string.
pub fn authority_key_from_b58(s: &str) -> Result<[u8; 32], Error> {
    let raw = base58(s.trim())?;
    if raw.len() != 2 + 32 + 4 {
        return Err(Error::BadKey);
    }
    let (body, check) = raw.split_at(34);
    let h = Sha256::digest(Sha256::digest(body));
    if h[..4] != check[..] {
        return Err(Error::BadKey);
    }
    if u16::from_le_bytes([body[0], body[1]]) != 1 {
        return Err(Error::BadKey);
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&body[2..]);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Base58Check encode, the inverse, for the tests only.
    fn encode(body: &[u8]) -> String {
        let h = Sha256::digest(Sha256::digest(body));
        let mut raw = body.to_vec();
        raw.extend_from_slice(&h[..4]);
        let mut digits: Vec<u8> = Vec::new();
        for &b in &raw {
            let mut carry = u32::from(b);
            for d in digits.iter_mut() {
                let v = u32::from(*d) * 256 + carry;
                *d = (v % 58) as u8;
                carry = v / 58;
            }
            while carry > 0 {
                digits.push((carry % 58) as u8);
                carry /= 58;
            }
        }
        let zeros = raw.iter().take_while(|&&b| b == 0).count();
        let mut s: String = std::iter::repeat_n('1', zeros).collect();
        for &d in digits.iter().rev() {
            s.push(ALPHABET[d as usize] as char);
        }
        s
    }

    #[test]
    fn round_trips_a_versioned_key() {
        let key: [u8; 32] = core::array::from_fn(|i| (i * 7 + 3) as u8);
        let mut body = vec![1u8, 0];
        body.extend_from_slice(&key);
        let s = encode(&body);
        assert_eq!(authority_key_from_b58(&s).unwrap(), key);
        assert_eq!(authority_key_from_b58(&format!("  {s}\n")).unwrap(), key);
    }

    #[test]
    fn rejects_bad_checksum_version_length_and_alphabet() {
        let key = [0x11u8; 32];
        let mut body = vec![1u8, 0];
        body.extend_from_slice(&key);
        let s = encode(&body);
        let mut broken = s.clone();
        broken.pop();
        broken.push(if s.ends_with('2') { '3' } else { '2' });
        assert_eq!(authority_key_from_b58(&broken), Err(Error::BadKey));
        let mut body2 = vec![2u8, 0];
        body2.extend_from_slice(&key);
        assert_eq!(authority_key_from_b58(&encode(&body2)), Err(Error::BadKey));
        assert_eq!(
            authority_key_from_b58(&encode(&body[..20])),
            Err(Error::BadKey)
        );
        assert_eq!(authority_key_from_b58("0OIl"), Err(Error::BadKey));
        assert_eq!(authority_key_from_b58(""), Err(Error::BadKey));
    }

    #[test]
    fn leading_zero_bytes_survive() {
        let mut body = vec![1u8, 0];
        body.extend_from_slice(&[0u8; 32]);
        let s = encode(&body);
        assert_eq!(authority_key_from_b58(&s).unwrap(), [0u8; 32]);
    }
}
