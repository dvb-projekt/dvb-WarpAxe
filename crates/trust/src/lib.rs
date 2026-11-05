//! Trust in a root the server sends along.
//!
//! The firmware checks TLS servers against ESP-IDF's compact CA bundle
//! (`esp_crt_bundle`): per root only its subject name and public key, no
//! certificate. ESP-IDF's verifier looks up the *issuer* of the topmost
//! certificate the server sends and checks that certificate's signature
//! with the bundle's key for that name. A server may send a root of the
//! bundle itself on top of its chain, cross-signed by an older root: since
//! the pinned bundle of 2026-10-20 (firmware/warpaxe/certificates) no longer
//! carries some of those older roots, such a chain failed although its root
//! is trusted. api.coingecko.com sends `GTS Root R4` signed by `GlobalSign
//! Root CA`; ESP-IDF v5.3's own lists still had GlobalSign Root CA, so
//! the change of bundle turned a working chain into a failing one. Found
//! in the ESP-Miner port of the same bundle (2026-10-27).
//!
//! The rule added here: a certificate whose subject name **and** public
//! key are those of a root in the bundle is that root. Its own signature
//! and dates are then beside the point, as for any trust anchor (the
//! bundle carries neither); the certificates below it were already checked
//! against its key by mbedTLS. Nothing else is accepted.
//!
//! No I/O and no unsafe code: the firmware hands in the bundle as embedded
//! and the raw subject and key of the certificate (mbedTLS `subject_raw`,
//! `pk_raw`), so the rule runs under `cargo test`.
//!
//! [`pin`] is the second rule, for the pool connection only: a pool
//! certificate no public authority issued is accepted when its public
//! key is the one the owner confirmed (SHA-256 of the SubjectPublicKeyInfo).

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

pub mod pin;
pub mod x509;

/// mbedTLS `MBEDTLS_X509_BADCERT_EXPIRED`.
pub const BADCERT_EXPIRED: u32 = 0x01;
/// mbedTLS `MBEDTLS_X509_BADCERT_NOT_TRUSTED`.
pub const BADCERT_NOT_TRUSTED: u32 = 0x08;
/// mbedTLS `MBEDTLS_X509_BADCERT_FUTURE`.
pub const BADCERT_FUTURE: u32 = 0x200;
/// mbedTLS `MBEDTLS_X509_BADCERT_BAD_MD`.
pub const BADCERT_BAD_MD: u32 = 0x4000;

/// ESP-IDF's compact bundle (`gen_crt_bundle.py`): a big-endian count,
/// then per root its name length, key length, DER subject name and DER
/// SubjectPublicKeyInfo.
#[derive(Debug, Clone, Copy)]
pub struct Bundle<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> Bundle<'a> {
    /// The bundle, or `None` if it does not parse to the count it states.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        let count = usize::from(u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]));
        let b = Bundle { bytes, count };
        (b.entries().count() == count).then_some(b)
    }

    /// Roots in the bundle.
    pub fn len(&self) -> usize {
        self.count
    }

    /// No roots at all.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Each root as (subject, key).
    pub fn entries(&self) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + '_ {
        let bytes = self.bytes;
        let mut at = 2usize;
        let mut left = self.count;
        core::iter::from_fn(move || {
            if left == 0 {
                return None;
            }
            let head = bytes.get(at..at + 4)?;
            let name = usize::from(u16::from_be_bytes([head[0], head[1]]));
            let key = usize::from(u16::from_be_bytes([head[2], head[3]]));
            let subject = bytes.get(at + 4..at + 4 + name)?;
            let spki = bytes.get(at + 4 + name..at + 4 + name + key)?;
            at += 4 + name + key;
            left -= 1;
            Some((subject, spki))
        })
    }

    /// A root with exactly this subject name and public key.
    pub fn contains(&self, subject: &[u8], spki: &[u8]) -> bool {
        !subject.is_empty()
            && !spki.is_empty()
            && self.entries().any(|(s, k)| s == subject && k == spki)
    }
}

/// Whether a certificate mbedTLS could not anchor (`flags`) is a root of
/// the bundle sent along by the server: it must be only "not trusted",
/// apart from what does not matter for a trust anchor (its dates, a weak
/// hash in its own signature), and its subject and key must be a root's.
pub fn is_sent_root(bundle: &Bundle<'_>, flags: u32, subject: &[u8], spki: &[u8]) -> bool {
    let ignorable = BADCERT_EXPIRED | BADCERT_FUTURE | BADCERT_BAD_MD;
    flags & BADCERT_NOT_TRUSTED != 0
        && flags & !(BADCERT_NOT_TRUSTED | ignorable) == 0
        && bundle.contains(subject, spki)
}
