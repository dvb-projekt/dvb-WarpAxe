//! The boot screen's packed pictures unpack to the raw ones, byte for
//! byte: every `NAME.z` in firmware/warpaxe/src/splash against `NAME`
//! (tools/make-splash.py writes both; the firmware includes only the
//! packed file).

use std::path::PathBuf;
use warpaxe_inflate::{inflate, Error};

fn splash_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../firmware/warpaxe/src/splash")
}

/// `(packed path, raw bytes)` of every packed picture.
fn pictures() -> Vec<(PathBuf, Vec<u8>)> {
    let mut all: Vec<_> = std::fs::read_dir(splash_dir())
        .expect("the splash directory")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "z"))
        .map(|p| {
            let raw = std::fs::read(p.with_extension(""))
                .unwrap_or_else(|e| panic!("{}: no raw file next to it: {e}", p.display()));
            (p, raw)
        })
        .collect();
    all.sort();
    all
}

/// Width × height × bytes per pixel, from a name like
/// `mark_nerdqx_86x38.rgb565a.z`.
fn size_from_name(name: &str) -> usize {
    let stem = name.trim_end_matches(".z");
    let (stem, kind) = stem.rsplit_once('.').unwrap();
    let (w, h) = stem.rsplit_once('_').unwrap().1.split_once('x').unwrap();
    let bpp = match kind {
        "rgb565" => 2,
        "rgb565a" => 3,
        other => panic!("{name}: unknown kind {other}"),
    };
    w.parse::<usize>().unwrap() * h.parse::<usize>().unwrap() * bpp
}

#[test]
fn the_lcd_frame_and_all_nine_marks_are_packed() {
    let names: Vec<String> = pictures()
        .iter()
        .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 10, "{names:?}");
    assert!(names.contains(&"lcd_320x170.rgb565.z".to_string()));
    assert_eq!(names.iter().filter(|n| n.starts_with("mark_")).count(), 9);
}

#[test]
fn every_packed_picture_unpacks_to_its_raw_file() {
    for (path, raw) in pictures() {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(raw.len(), size_from_name(&name), "{name}: size");
        let packed = std::fs::read(&path).unwrap();
        // Into a buffer of exactly the picture's size, as the firmware does.
        let mut out = vec![0x5A; raw.len()];
        assert_eq!(inflate(&packed, &mut out), Ok(raw.len()), "{name}");
        assert!(
            out == raw,
            "{name}: unpacked picture differs from the raw file"
        );
        // And worth it: a packed picture is well below half its size.
        assert!(
            packed.len() * 2 < raw.len(),
            "{name}: {} bytes",
            packed.len()
        );
    }
}

#[test]
fn a_packed_picture_never_overruns_a_smaller_buffer() {
    for (path, raw) in pictures() {
        let packed = std::fs::read(&path).unwrap();
        let mut out = vec![0; raw.len() - 1];
        assert_eq!(inflate(&packed, &mut out), Err(Error::Overflow));
    }
}

#[test]
fn a_packed_picture_cut_short_is_an_error() {
    for (path, raw) in pictures() {
        let packed = std::fs::read(&path).unwrap();
        let mut out = vec![0; raw.len()];
        for cut in (0..packed.len()).step_by(61) {
            assert_eq!(inflate(&packed[..cut], &mut out), Err(Error::Truncated));
        }
    }
}
