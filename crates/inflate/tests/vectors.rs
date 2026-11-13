//! The decoder against streams packed by zlib (fixed vectors) and by
//! miniz_oxide (round trips), and against damaged input.

use std::io::Write;
use warpaxe_inflate::{inflate, Error};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn unpack(packed: &[u8], room: usize) -> Result<Vec<u8>, Error> {
    let mut out = vec![0xA5; room];
    let n = inflate(packed, &mut out)?;
    // What lies behind the unpacked bytes is not touched.
    assert!(out[n..].iter().all(|&b| b == 0xA5));
    out.truncate(n);
    Ok(out)
}

fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// xorshift: test data that is the same on every run.
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

// Packed with Python's zlib 1.2.12, wbits -15 (tools/make-splash.py
// packs the pictures the same way).
const FIXED: &str = "0b4f2c2a70ac48550847a393f2f34b148a0b72128b3374d0684500";
const FIXED_TEXT: &[u8] = b"WarpAxe WarpAxe WarpAxe boot splash, splash, splash!";
const FIXED_OVERLAP: &str = "4b4c1c05c482a464ea4200";
const STORED: &str = "010c00f3ff73746f72656420626c6f636b";
const EMPTY: &str = "0300";
// Two blocks with an empty stored block between them (a sync flush).
const MULTI: &str = "4acb2c2a2e5148cac94fced65100000000ffff2b4e4dcecf4b81f1d2105200";

#[test]
fn a_fixed_code_block() {
    assert_eq!(unpack(&hex(FIXED), 100).unwrap(), FIXED_TEXT);
}

#[test]
fn a_match_that_runs_into_itself() {
    let want = [vec![b'a'; 300], b"bc".repeat(40)].concat();
    assert_eq!(unpack(&hex(FIXED_OVERLAP), 400).unwrap(), want);
}

#[test]
fn a_stored_block() {
    assert_eq!(unpack(&hex(STORED), 12).unwrap(), b"stored block");
}

#[test]
fn an_empty_stream() {
    assert_eq!(unpack(&hex(EMPTY), 0).unwrap(), b"");
    assert_eq!(unpack(&hex(EMPTY), 8).unwrap(), b"");
}

#[test]
fn several_blocks_one_after_another() {
    assert_eq!(
        unpack(&hex(MULTI), 64).unwrap(),
        b"first block, second block, first block"
    );
}

fn samples() -> Vec<Vec<u8>> {
    let mut rng = Rng(0x2026_1113);
    let noise: Vec<u8> = (0..70_000).map(|_| rng.next() as u8).collect();
    // A picture-like sample: long runs, a few values, some noise.
    let runs: Vec<u8> = (0..120_000u32)
        .map(|i| match (i / 97) % 5 {
            0 => 0,
            1 => (i / 640) as u8,
            2 => 0xFF,
            3 => (rng.next() % 3) as u8,
            _ => (i % 251) as u8,
        })
        .collect();
    vec![
        Vec::new(),
        vec![0],
        b"a".to_vec(),
        b"abcabcabcabcabcabcabcabcabcabc".to_vec(),
        vec![0; 100_000],
        (0..=255u8).cycle().take(5000).collect(),
        noise,
        runs,
    ]
}

#[test]
fn round_trips_at_every_level() {
    for data in samples() {
        // Level 0 stores (several blocks above 64 KB), 1 and up use the
        // fixed and the dynamic codes.
        for level in 0..=9 {
            let packed = deflate(&data, level);
            assert_eq!(
                unpack(&packed, data.len()).unwrap(),
                data,
                "level {level}, {} bytes",
                data.len()
            );
            assert_eq!(unpack(&packed, data.len() + 77).unwrap(), data);
        }
    }
}

#[test]
fn more_than_the_output_takes_is_an_error() {
    for data in samples().into_iter().filter(|d| !d.is_empty()) {
        for level in [0, 1, 9] {
            let packed = deflate(&data, level);
            for room in [0, data.len() / 2, data.len() - 1] {
                let mut out = vec![0; room];
                assert_eq!(inflate(&packed, &mut out), Err(Error::Overflow));
            }
        }
    }
}

#[test]
fn a_stream_cut_short_is_an_error() {
    for (packed, room) in [
        (hex(FIXED), 100),
        (hex(FIXED_OVERLAP), 400),
        (hex(STORED), 12),
        (hex(MULTI), 64),
        (deflate(&samples()[7][..20_000], 9), 20_000),
        (deflate(&samples()[6][..3_000], 0), 3_000),
    ] {
        for cut in 0..packed.len() {
            let mut out = vec![0; room];
            assert_eq!(
                inflate(&packed[..cut], &mut out),
                Err(Error::Truncated),
                "cut at {cut} of {}",
                packed.len()
            );
        }
    }
}

#[test]
fn bad_block_types_and_lengths_are_errors() {
    // Block type 3.
    assert_eq!(inflate(&[0x07], &mut [0; 8]), Err(Error::Corrupt));
    // A stored block whose length and complement disagree.
    assert_eq!(
        inflate(&hex("010c00f3fe73746f72656420626c6f636b"), &mut [0; 12]),
        Err(Error::Corrupt)
    );
    // A match that reaches back before the first byte: fixed code,
    // length 3 at distance 1 with nothing written yet.
    assert_eq!(inflate(&hex("03020000"), &mut [0; 8]), Err(Error::Corrupt));
}

/// Damaged input must end in an error or in some output, never in a
/// panic (which the test run would report) and never outside `out`.
#[test]
fn damaged_streams_never_panic() {
    let mut rng = Rng(0x0BAD_5EED);
    let data = &samples()[7][..30_000];
    for level in [1, 6, 9] {
        let packed = deflate(data, level);
        // Every single bit of the first bytes (the block header and the
        // code lengths), then bits all over the stream.
        let head = (0..packed.len().min(160) * 8).collect::<Vec<_>>();
        let rest = (0..4000).map(|_| rng.next() as usize % (packed.len() * 8));
        for bit in head.into_iter().chain(rest) {
            let mut bad = packed.clone();
            bad[bit / 8] ^= 1 << (bit % 8);
            let mut out = vec![0; data.len()];
            if let Ok(n) = inflate(&bad, &mut out) {
                assert!(n <= out.len());
            }
        }
    }
    // Noise as a stream.
    for _ in 0..3000 {
        let len = rng.next() as usize % 200;
        let junk: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let mut out = vec![0; 4096];
        if let Ok(n) = inflate(&junk, &mut out) {
            assert!(n <= out.len());
        }
    }
}
