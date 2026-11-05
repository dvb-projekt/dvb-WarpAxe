//! Pinning the pool's public key for a certificate no public authority
//! issued (a self-signed one, as WarpPool makes on Umbrel).
//!
//! The pin is the SHA-256 of the leaf certificate's DER
//! SubjectPublicKeyInfo, 32 bytes, shown as upper-case hex with colons
//! (`AB:CD:…:EF`), the value `openssl x509 -pubkey -noout | openssl pkey
//! -pubin -outform der | openssl dgst -sha256 -c` prints (in lower case).
//! WarpPool shows the same value as `connect.tls_spki_sha256`. A renewed
//! certificate with the same key keeps the pin; a new key does not.
//!
//! The rule, run by the firmware's verify callback for the pool
//! connection only, on the leaf certificate (depth 0) after mbedTLS has
//! done its own checks:
//!
//! - A leaf the bundle trusts (no "not trusted" flag) is not this
//!   module's business: the public PKI decided, flags stay as they are.
//!   So is a leaf whose issuer name is a root of the bundle (the firmware
//!   checks `Bundle::has_subject` first): expired or on the wrong name, it
//!   is the public PKI's verdict, never offered for a pin.
//! - A leaf mbedTLS could not anchor, with nothing else wrong than its
//!   name, its dates or a weak hash in its own signature, is compared
//!   with the pin. The same key: accepted, flags cleared. The hostname
//!   is not checked then, so an IP address works as the pool address. No
//!   pin, or another key: refused, and the key seen is reported so the
//!   owner can confirm it with a click.
//! - Any other fault (a weak or unsupported key, a revoked certificate,
//!   a wrong key usage) is never overridden, pin or no pin.
//!
//! Dates: a self-signed certificate's validity was written by the holder
//! of the pinned key, so an expired or not-yet-valid one says nothing a
//! public PKI would. Refusing it would only make the connection depend
//! on the miner's clock (a boot before NTP starts in 1970, which is before
//! any certificate's `notBefore`) with no gain in safety: whoever holds
//! the key can issue one with any dates. So with a matching pin the dates
//! are ignored, as `is_sent_root` ignores them for a trust anchor.

use core::fmt;

use sha2::{Digest, Sha256};

/// mbedTLS `MBEDTLS_X509_BADCERT_CN_MISMATCH`.
pub const BADCERT_CN_MISMATCH: u32 = 0x04;

/// The pin: SHA-256 of the DER SubjectPublicKeyInfo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pin(pub [u8; 32]);

impl Pin {
    /// The pin of a public key, as mbedTLS hands the key over (`pk_raw`,
    /// the whole SubjectPublicKeyInfo element).
    pub fn of_spki(spki_der: &[u8]) -> Pin {
        Pin(Sha256::digest(spki_der).into())
    }

    /// The pin as the API and the UI write it: `AB:CD:…:EF`, upper case,
    /// no line breaks; 95 characters.
    pub fn text(&self) -> Text {
        let mut out = [b':'; 95];
        for (i, b) in self.0.iter().enumerate() {
            out[i * 3] = HEX[usize::from(b >> 4)];
            out[i * 3 + 1] = HEX[usize::from(b & 0x0F)];
        }
        Text(out)
    }

    /// Parse the written form: the hex pairs with or without colons,
    /// any case, spaces and line breaks ignored; `None` for anything
    /// that is not exactly 32 bytes.
    pub fn parse(s: &str) -> Option<Pin> {
        let mut out = [0u8; 32];
        let mut n = 0usize;
        let mut hi: Option<u8> = None;
        for c in s.bytes() {
            let v = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                b':' | b' ' | b'\n' | b'\r' | b'\t' => {
                    if hi.is_some() {
                        return None;
                    }
                    continue;
                }
                _ => return None,
            };
            match hi.take() {
                None => hi = Some(v),
                Some(h) => {
                    if n == 32 {
                        return None;
                    }
                    out[n] = (h << 4) | v;
                    n += 1;
                }
            }
        }
        (n == 32 && hi.is_none()).then_some(Pin(out))
    }

    /// The same key, written any way `parse` takes.
    pub fn matches_text(&self, s: &str) -> bool {
        Pin::parse(s).is_some_and(|p| p == *self)
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// The written pin, `AB:CD:…:EF`.
#[derive(Clone, Copy)]
pub struct Text([u8; 95]);

impl Text {
    /// As a string slice.
    pub fn as_str(&self) -> &str {
        // Only ASCII hex digits and colons are ever written.
        core::str::from_utf8(&self.0).unwrap_or("")
    }

    /// The short form the UI prints in bold: the first and the last four
    /// bytes, `AB:CD:EF:01 … 23:45:67:89`.
    pub fn short(&self) -> Short {
        Short(*self)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The short form of a written pin.
#[derive(Clone, Copy)]
pub struct Short(Text);

impl fmt::Display for Short {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.0.as_str();
        write!(f, "{} … {}", &s[..11], &s[s.len() - 11..])
    }
}

/// What the verify callback does with the leaf certificate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Not this rule's case: a leaf the public PKI trusts, or one with a
    /// fault a pin never covers. The flags stay as mbedTLS set them.
    Untouched,
    /// The pinned key: clear the flags, the connection goes on.
    Pinned,
    /// No pin stored, and the leaf is one a pin could cover: refuse, and
    /// offer this key for confirmation.
    Unconfirmed(Pin),
    /// A pin is stored, but the leaf carries another key: refuse, say so,
    /// and offer the new key for its own confirmation.
    Changed(Pin),
}

/// Faults a pin covers besides "not trusted": the name (an IP address or
/// another name than the certificate's), the dates and a weak hash in
/// the self-signature (see the module notes).
const COVERED: u32 =
    BADCERT_CN_MISMATCH | crate::BADCERT_EXPIRED | crate::BADCERT_FUTURE | crate::BADCERT_BAD_MD;

/// The verdict on the leaf certificate (`depth` 0) with mbedTLS's
/// `flags` and its public key (`pk_raw`), against the stored pin.
pub fn verdict(depth: i32, flags: u32, pin: Option<&Pin>, spki: &[u8]) -> Verdict {
    if depth != 0
        || flags & crate::BADCERT_NOT_TRUSTED == 0
        || flags & !(crate::BADCERT_NOT_TRUSTED | COVERED) != 0
        || spki.is_empty()
    {
        return Verdict::Untouched;
    }
    let seen = Pin::of_spki(spki);
    match pin {
        Some(p) if *p == seen => Verdict::Pinned,
        Some(_) => Verdict::Changed(seen),
        None => Verdict::Unconfirmed(seen),
    }
}
