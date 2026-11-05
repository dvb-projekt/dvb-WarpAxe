//! The pool pin against self-signed certificates: two made with openssl
//! from one key (`pool-self-signed.der`, then a renewal with other dates,
//! another serial, another name and SAN: `pool-self-signed-renewed.der`),
//! one from another key, and the one WarpPool on Oliver's Umbrel served
//! on 2026-11-05 (rcgen, CN "rcgen self signed cert"), whose value
//! 15:1A:…:47 was measured with openssl on the Mac the same day.

use warpaxe_trust::pin::{verdict, Pin, Verdict, BADCERT_CN_MISMATCH};
use warpaxe_trust::x509::subject_and_spki;
use warpaxe_trust::{BADCERT_EXPIRED, BADCERT_FUTURE, BADCERT_NOT_TRUSTED};

const SELF: &[u8] = include_bytes!("data/pool-self-signed.der");
const RENEWED: &[u8] = include_bytes!("data/pool-self-signed-renewed.der");
const OTHER_KEY: &[u8] = include_bytes!("data/pool-self-signed-other-key.der");
const UMBREL: &[u8] = include_bytes!("data/warppool-umbrel-2026-11-05.der");

/// `openssl x509 -pubkey -noout | openssl pkey -pubin -outform der |
/// openssl dgst -sha256 -c` on the test certificates.
const SELF_FP: &str = "7c:e5:16:ea:97:03:ec:61:21:d7:7b:90:a6:c5:5e:31:33:08:12:b9:02:c6:e6:8e:58:4c:f3:0b:79:a0:10:10";
const OTHER_FP: &str = "21:38:fd:4b:87:31:d4:93:eb:97:24:00:a0:87:16:55:49:e5:ea:d6:5d:12:14:42:61:17:ae:5a:cd:c3:d9:97";
/// The agreed value of the Umbrel's WarpPool (`connect.tls_spki_sha256`).
const UMBREL_FP: &str = "15:1A:63:B8:21:17:9C:A4:60:E8:E9:C5:BC:66:9F:F4:2F:00:C8:7B:EE:E3:0F:06:35:06:26:BD:09:9E:E0:47";

fn spki(cert: &[u8]) -> &[u8] {
    subject_and_spki(cert).expect("a certificate").1
}

#[test]
fn the_pin_is_what_openssl_prints() {
    let p = Pin::of_spki(spki(SELF));
    assert_eq!(p.text().as_str(), SELF_FP.to_uppercase());
    assert!(p.matches_text(SELF_FP));
    assert_eq!(
        Pin::of_spki(spki(OTHER_KEY)).text().as_str(),
        OTHER_FP.to_uppercase()
    );
}

#[test]
fn the_umbrels_warppool_has_the_agreed_value() {
    let (subject, key) = subject_and_spki(UMBREL).unwrap();
    // CN=rcgen self signed cert, as a DER Name.
    assert!(subject.windows(22).any(|w| w == b"rcgen self signed cert"));
    assert_eq!(Pin::of_spki(key).text().as_str(), UMBREL_FP);
    assert_eq!(
        Pin::of_spki(key).text().short().to_string(),
        "15:1A:63:B8 … 09:9E:E0:47"
    );
}

#[test]
fn a_renewed_certificate_with_the_same_key_keeps_the_pin() {
    assert_ne!(SELF, RENEWED);
    assert_ne!(
        subject_and_spki(SELF).unwrap().0,
        subject_and_spki(RENEWED).unwrap().0
    );
    assert_eq!(Pin::of_spki(spki(SELF)), Pin::of_spki(spki(RENEWED)));
    assert_ne!(Pin::of_spki(spki(SELF)), Pin::of_spki(spki(OTHER_KEY)));
}

#[test]
fn the_written_form_is_95_characters_upper_case_with_colons() {
    let t = Pin::of_spki(spki(SELF)).text();
    let s = t.as_str();
    assert_eq!(s.len(), 95);
    assert_eq!(s.matches(':').count(), 31);
    assert!(s
        .bytes()
        .all(|c| c == b':' || c.is_ascii_digit() || (b'A'..=b'F').contains(&c)));
    assert!(!s.contains('\n'));
    assert_eq!(format!("{t}"), s);
}

#[test]
fn parse_takes_any_case_with_or_without_colons_and_nothing_else() {
    let p = Pin::of_spki(spki(SELF));
    assert_eq!(Pin::parse(SELF_FP), Some(p));
    assert_eq!(Pin::parse(&SELF_FP.to_uppercase()), Some(p));
    assert_eq!(Pin::parse(&SELF_FP.replace(':', "")), Some(p));
    assert_eq!(
        Pin::parse(&format!("  {}\n", SELF_FP.replace(':', " "))),
        Some(p)
    );
    // 31 bytes, 33 bytes, an odd digit, a stray character, empty.
    assert_eq!(Pin::parse(&SELF_FP[..92]), None);
    assert_eq!(Pin::parse(&format!("{SELF_FP}:00")), None);
    assert_eq!(Pin::parse(&SELF_FP[..94]), None);
    assert_eq!(Pin::parse(&SELF_FP.replace('7', "g")), None);
    assert_eq!(Pin::parse("7:ce5"), None);
    assert_eq!(Pin::parse(""), None);
    assert!(!p.matches_text(OTHER_FP));
}

#[test]
fn a_self_signed_leaf_with_the_pinned_key_is_accepted() {
    let key = spki(SELF);
    let pin = Pin::of_spki(key);
    assert_eq!(
        verdict(0, BADCERT_NOT_TRUSTED, Some(&pin), key),
        Verdict::Pinned
    );
    // The name is not checked with a pin (an IP address as the pool).
    assert_eq!(
        verdict(
            0,
            BADCERT_NOT_TRUSTED | BADCERT_CN_MISMATCH,
            Some(&pin),
            key
        ),
        Verdict::Pinned
    );
    // Nor the dates: the clock before NTP, or a certificate the owner let run out.
    assert_eq!(
        verdict(0, BADCERT_NOT_TRUSTED | BADCERT_FUTURE, Some(&pin), key),
        Verdict::Pinned
    );
    assert_eq!(
        verdict(
            0,
            BADCERT_NOT_TRUSTED | BADCERT_EXPIRED | BADCERT_CN_MISMATCH,
            Some(&pin),
            key
        ),
        Verdict::Pinned
    );
    // The renewed certificate, same key.
    assert_eq!(
        verdict(
            0,
            BADCERT_NOT_TRUSTED | BADCERT_CN_MISMATCH,
            Some(&pin),
            spki(RENEWED)
        ),
        Verdict::Pinned
    );
}

#[test]
fn without_a_pin_the_key_is_offered_never_accepted() {
    let key = spki(SELF);
    assert_eq!(
        verdict(0, BADCERT_NOT_TRUSTED, None, key),
        Verdict::Unconfirmed(Pin::of_spki(key))
    );
    assert_eq!(
        verdict(0, BADCERT_NOT_TRUSTED | BADCERT_CN_MISMATCH, None, key),
        Verdict::Unconfirmed(Pin::of_spki(key))
    );
}

#[test]
fn another_key_than_the_pinned_one_never_connects() {
    let pin = Pin::of_spki(spki(SELF));
    let other = spki(OTHER_KEY);
    assert_eq!(
        verdict(0, BADCERT_NOT_TRUSTED, Some(&pin), other),
        Verdict::Changed(Pin::of_spki(other))
    );
    assert_eq!(
        verdict(
            0,
            BADCERT_NOT_TRUSTED | BADCERT_CN_MISMATCH,
            Some(&pin),
            other
        ),
        Verdict::Changed(Pin::of_spki(other))
    );
    // The pin of the Umbrel does not accept the test pool, nor the other way round.
    let umbrel = Pin::of_spki(spki(UMBREL));
    assert!(matches!(
        verdict(0, BADCERT_NOT_TRUSTED, Some(&umbrel), spki(SELF)),
        Verdict::Changed(_)
    ));
    assert!(matches!(
        verdict(0, BADCERT_NOT_TRUSTED, Some(&pin), spki(UMBREL)),
        Verdict::Changed(_)
    ));
}

#[test]
fn a_certificate_the_public_pki_trusts_is_not_touched() {
    let key = spki(SELF);
    let pin = Pin::of_spki(key);
    // Trusted: flags 0, whatever the pin says (a public pool with the
    // same key as a pin would be odd, but the PKI's verdict stands).
    assert_eq!(verdict(0, 0, Some(&pin), key), Verdict::Untouched);
    assert_eq!(verdict(0, 0, None, key), Verdict::Untouched);
    // Trusted but expired, or a wrong name: the public PKI's business,
    // never offered for a pin.
    assert_eq!(verdict(0, BADCERT_EXPIRED, None, key), Verdict::Untouched);
    assert_eq!(
        verdict(0, BADCERT_CN_MISMATCH, Some(&pin), key),
        Verdict::Untouched
    );
}

#[test]
fn faults_a_pin_does_not_cover_are_never_overridden() {
    let key = spki(SELF);
    let pin = Pin::of_spki(key);
    // MBEDTLS_X509_BADCERT_REVOKED 0x02, _KEY_USAGE 0x0800,
    // _EXT_KEY_USAGE 0x1000, _NS_CERT_TYPE 0x2000, _BAD_PK 0x8000,
    // _BAD_KEY 0x10000.
    for extra in [0x02u32, 0x0800, 0x1000, 0x2000, 0x8000, 0x10000] {
        assert_eq!(
            verdict(0, BADCERT_NOT_TRUSTED | extra, Some(&pin), key),
            Verdict::Untouched,
            "flag {extra:#x}"
        );
        assert_eq!(
            verdict(0, BADCERT_NOT_TRUSTED | extra, None, key),
            Verdict::Untouched,
            "flag {extra:#x}"
        );
    }
}

#[test]
fn only_the_leaf_is_pinned() {
    let key = spki(SELF);
    let pin = Pin::of_spki(key);
    // A private CA above the leaf is not covered: at depth 1 the rule
    // does nothing, so the chain still fails on the CA.
    assert_eq!(
        verdict(1, BADCERT_NOT_TRUSTED, Some(&pin), key),
        Verdict::Untouched
    );
    assert_eq!(
        verdict(2, BADCERT_NOT_TRUSTED, None, key),
        Verdict::Untouched
    );
    // No key: nothing to compare.
    assert_eq!(
        verdict(0, BADCERT_NOT_TRUSTED, Some(&pin), &[]),
        Verdict::Untouched
    );
}

#[test]
fn the_der_walk_reads_the_bundles_roots_too() {
    // The helper gives the same fields the sent-root test reads by hand.
    let cross = include_bytes!("data/gts-r4-cross-signed.der");
    let own = include_bytes!("data/gts-r4-self-signed.der");
    assert_eq!(
        subject_and_spki(cross).unwrap(),
        subject_and_spki(own).unwrap()
    );
    assert_eq!(subject_and_spki(&cross[..100]), None);
    assert_eq!(subject_and_spki(&[]), None);
    assert_eq!(
        subject_and_spki(&[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF]),
        None
    );
}
