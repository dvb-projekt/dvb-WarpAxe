//! The two raw fields of a DER certificate the rules here work on, as
//! mbedTLS hands them to the verify callback (`subject_raw`, `pk_raw`).
//! For the tests and the tools; the firmware takes them from mbedTLS.
//! A minimal DER walk, no validation beyond the lengths.

/// One DER element at `at`: (tag, start of content, end of element).
fn tlv(d: &[u8], at: usize) -> Option<(u8, usize, usize)> {
    let tag = *d.get(at)?;
    let l = *d.get(at + 1)?;
    let (len, head) = if l < 0x80 {
        (usize::from(l), 2)
    } else {
        let n = usize::from(l & 0x7F);
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for &b in d.get(at + 2..at + 2 + n)? {
            len = (len << 8) | usize::from(b);
        }
        (len, 2 + n)
    };
    let start = at.checked_add(head)?;
    let end = start.checked_add(len)?;
    (end <= d.len()).then_some((tag, start, end))
}

/// (subject, SubjectPublicKeyInfo) of a DER certificate, each the whole
/// element with its tag and length. `None` when the structure is not a
/// certificate's.
pub fn subject_and_spki(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    let (_, outer, _) = tlv(cert, 0)?;
    let (_, tbs_start, tbs_end) = tlv(cert, outer)?;
    // [0] version (explicit, optional), serial, signature, issuer,
    // validity, subject, subjectPublicKeyInfo.
    let mut at = tbs_start;
    let mut fields: [Option<(u8, usize, usize)>; 7] = [None; 7];
    for slot in fields.iter_mut() {
        if at >= tbs_end {
            break;
        }
        let (tag, _, end) = tlv(cert, at)?;
        *slot = Some((tag, at, end));
        at = end;
    }
    let skip = usize::from(fields[0]?.0 == 0xA0);
    let (_, s0, s1) = fields[skip + 4]?;
    let (_, k0, k1) = fields[skip + 5]?;
    Some((&cert[s0..s1], &cert[k0..k1]))
}
