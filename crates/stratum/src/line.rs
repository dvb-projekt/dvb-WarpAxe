//! Reading one Stratum V1 line with an upper bound.
//!
//! A `mining.notify` carries the coinbase as hex, so its length grows with
//! the number of payout outputs: about 140 hex characters per output. A
//! pool that pays its miners in the coinbase (around 110 outputs gives a
//! line of ~20 KB; ESP-Miner issue bitaxeorg/ESP-Miner#2081) is fine; a
//! line that never ends is not. Without a bound `read_line` would read it
//! until the heap is gone and the miner restarts. With one, the session
//! ends with an error, the connection closes and the usual reconnect and
//! fallback rules apply.

use std::io::{self, BufRead};

/// The longest line a session accepts, in bytes. 512 KiB holds a notify
/// with over 3,000 payout outputs and sits in PSRAM (8 MB on every board).
pub const MAX_LINE: usize = 512 * 1024;

/// The error a line over the bound ends the session with.
pub fn too_long(max: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("pool line over {} KiB", max / 1024),
    )
}

/// Whether an error is the one [`read_line`] gives for a line over the bound.
pub fn is_too_long(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::InvalidData && e.to_string().starts_with("pool line over")
}

/// Appends one line (up to and including `\n`, or to the end of the
/// stream) to `buf` and returns its length, like [`BufRead::read_line`].
/// A line longer than `max` bytes fails with [`too_long`]; what was read of
/// it is dropped. Invalid UTF-8 fails as `read_line` does.
pub fn read_line<R: BufRead + ?Sized>(
    r: &mut R,
    buf: &mut String,
    max: usize,
) -> io::Result<usize> {
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let (done, used) = {
            let avail = match r.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if avail.is_empty() {
                (true, 0)
            } else {
                match avail.iter().position(|&b| b == b'\n') {
                    Some(i) => {
                        bytes.extend_from_slice(&avail[..=i]);
                        (true, i + 1)
                    }
                    None => {
                        bytes.extend_from_slice(avail);
                        (false, avail.len())
                    }
                }
            }
        };
        r.consume(used);
        if bytes.len() > max {
            return Err(too_long(max));
        }
        if done {
            break;
        }
    }
    let s = String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pool line is not UTF-8"))?;
    buf.push_str(&s);
    Ok(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Cursor};

    #[test]
    fn reads_lines_like_read_line() {
        let mut r = BufReader::with_capacity(4, Cursor::new(b"ab\ncdef\ngh".to_vec()));
        let mut s = String::new();
        assert_eq!(read_line(&mut r, &mut s, 100).unwrap(), 3);
        assert_eq!(s, "ab\n");
        s.clear();
        assert_eq!(read_line(&mut r, &mut s, 100).unwrap(), 5);
        assert_eq!(s, "cdef\n");
        s.clear();
        assert_eq!(read_line(&mut r, &mut s, 100).unwrap(), 2);
        assert_eq!(s, "gh");
        s.clear();
        assert_eq!(read_line(&mut r, &mut s, 100).unwrap(), 0);
    }

    #[test]
    fn a_line_at_the_bound_passes_one_over_fails() {
        let mut ok = vec![b'x'; 9];
        ok.push(b'\n');
        let mut r = BufReader::with_capacity(3, Cursor::new(ok));
        let mut s = String::new();
        assert_eq!(read_line(&mut r, &mut s, 10).unwrap(), 10);

        let mut over = vec![b'x'; 10];
        over.push(b'\n');
        let mut r = BufReader::with_capacity(3, Cursor::new(over));
        let e = read_line(&mut r, &mut String::new(), 10).unwrap_err();
        assert!(is_too_long(&e));
        assert_eq!(e.to_string(), "pool line over 0 KiB");
    }

    #[test]
    fn a_line_without_end_stops_at_the_bound() {
        // An endless stream: fails once the bound is passed, never reads on.
        let mut r = BufReader::with_capacity(1024, io::repeat(b'a'));
        let e = read_line(&mut r, &mut String::new(), 64 * 1024).unwrap_err();
        assert!(is_too_long(&e));
    }

    #[test]
    fn a_notify_with_thousands_of_outputs_fits() {
        // 3,000 P2WPKH outputs: 31 bytes each, 62 hex characters.
        let coinb2 = "a".repeat(3_000 * 62);
        let line = format!(
            "{{\"id\":null,\"method\":\"mining.notify\",\"params\":[\"1\",\"{}\",\"01\",\"{coinb2}\",[],\"20000000\",\"1702b3a8\",\"6745c1d2\",true]}}\n",
            "0".repeat(64)
        );
        let mut r = BufReader::new(Cursor::new(line.clone().into_bytes()));
        let mut s = String::new();
        assert_eq!(read_line(&mut r, &mut s, MAX_LINE).unwrap(), line.len());
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        let mut r = BufReader::new(Cursor::new(vec![0xFF, b'\n']));
        assert!(read_line(&mut r, &mut String::new(), 10).is_err());
    }
}
