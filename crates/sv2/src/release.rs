//! Release signatures for the online update: BIP-340 Schnorr on
//! secp256k1, the same curve and library the Sv2 handshake uses, so the
//! firmware carries no second signature scheme.
//!
//! The message is the SHA-256 of the image (what `warpaxe.bin.sha256`
//! already holds). The signature travels as `warpaxe.bin.sig`: 64 bytes
//! as 128 hex characters. The release key pair is made once on the
//! maintainer's machine; the x-only public key is compiled into the
//! firmware, the private key never leaves that machine.

use crate::Error;
use secp256k1::{schnorr::Signature, Keypair, Message, Secp256k1, SecretKey, XOnlyPublicKey};
use sha2::{Digest, Sha256};

/// The digest the signature covers.
pub fn digest(image: &[u8]) -> [u8; 32] {
    Sha256::digest(image).into()
}

/// A release key pair from 32 random bytes: (private key, x-only public
/// key). Fails when the bytes are not a valid scalar (one chance in
/// 2^128, try again).
pub fn keypair(secret: &[u8; 32]) -> Result<([u8; 32], [u8; 32]), Error> {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(secret).map_err(|_| Error::BadKey)?;
    let kp = Keypair::from_secret_key(&secp, &sk);
    Ok((kp.secret_bytes(), kp.x_only_public_key().0.serialize()))
}

/// The x-only public key of a private key.
pub fn public_key(secret: &[u8; 32]) -> Result<[u8; 32], Error> {
    keypair(secret).map(|(_, p)| p)
}

/// Sign an image digest.
pub fn sign(secret: &[u8; 32], digest: &[u8; 32]) -> Result<[u8; 64], Error> {
    let secp = Secp256k1::new();
    let sk = SecretKey::from_slice(secret).map_err(|_| Error::BadKey)?;
    let kp = Keypair::from_secret_key(&secp, &sk);
    let msg = Message::from_digest(*digest);
    // Deterministic (no auxiliary randomness): the same image signs the
    // same way twice, which makes a release reproducible to check.
    Ok(*secp.sign_schnorr_no_aux_rand(&msg, &kp).as_ref())
}

/// Check an image digest against a signature and the release key.
pub fn verify(public: &[u8; 32], digest: &[u8; 32], signature: &[u8; 64]) -> bool {
    let secp = Secp256k1::verification_only();
    let Ok(pk) = XOnlyPublicKey::from_slice(public) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(signature) else {
        return false;
    };
    secp.verify_schnorr(&sig, &Message::from_digest(*digest), &pk)
        .is_ok()
}

/// Lower-case hex of bytes.
pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Bytes of a hex string of exactly `N` bytes; surrounding whitespace
/// is ignored.
pub fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ([u8; 32], [u8; 32]) {
        keypair(&core::array::from_fn(|i| {
            (i as u8).wrapping_mul(37).wrapping_add(11)
        }))
        .unwrap()
    }

    #[test]
    fn a_signed_image_verifies_and_a_changed_one_does_not() {
        let (sk, pk) = key();
        let image = b"\xE9 firmware image".to_vec();
        let d = digest(&image);
        let sig = sign(&sk, &d).unwrap();
        assert!(verify(&pk, &d, &sig));
        assert_eq!(sign(&sk, &d).unwrap(), sig, "deterministic");
        let mut other = image.clone();
        other.push(0);
        assert!(!verify(&pk, &digest(&other), &sig));
        let mut bad = sig;
        bad[10] ^= 1;
        assert!(!verify(&pk, &d, &bad));
        let (_, other_pk) = keypair(&[7u8; 32]).unwrap();
        assert!(!verify(&other_pk, &d, &sig));
        assert!(!verify(&[0u8; 32], &d, &sig));
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        let (sk, pk) = key();
        let sig = sign(&sk, &digest(b"x")).unwrap();
        let h = to_hex(&sig);
        assert_eq!(h.len(), 128);
        assert_eq!(from_hex::<64>(&format!(" {h}\n")).unwrap(), sig);
        assert_eq!(from_hex::<32>(&to_hex(&pk)).unwrap(), pk);
        assert!(from_hex::<64>(&h[..126]).is_none());
        assert!(from_hex::<64>(&h.replace('a', "z")).is_none());
        assert_eq!(public_key(&sk).unwrap(), pk);
        assert!(keypair(&[0u8; 32]).is_err());
    }
}
