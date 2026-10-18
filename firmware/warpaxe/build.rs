use std::io::{Read, Write};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The web UI, gzip-compressed into OUT_DIR for `include_bytes!`: the
/// page is served with `Content-Encoding: gzip` and never kept
/// uncompressed in the image (290 KB → 90 KB). Deterministic: no mtime,
/// no file name in the header, so the same source gives the same image.
/// The result is decompressed again here and compared byte for byte
/// with the source; a mismatch fails the build.
fn gzip_ui() {
    let src = "src/ui/index.html";
    println!("cargo:rerun-if-changed={src}");
    let html = std::fs::read(src).expect("read src/ui/index.html");
    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(Vec::new(), flate2::Compression::best());
    gz.write_all(&html).expect("gzip the web UI");
    let packed = gz.finish().expect("finish the gzip");
    let mut back = Vec::new();
    flate2::read::GzDecoder::new(packed.as_slice())
        .read_to_end(&mut back)
        .expect("decompress the web UI again");
    assert_eq!(
        back, html,
        "the gzipped web UI does not decompress to its source"
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("index.html.gz"), &packed).expect("write index.html.gz");
}

fn main() {
    embuild::espidf::sysenv::output();
    gzip_ui();
    // The commit the image was built from, for /api/system/info: a
    // device that reports one build's numbers while the tree says
    // another is otherwise hard to tell apart (BitForge Nano, 2026-09-29).
    let mut hash = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&[
        "status",
        "--porcelain",
        "--untracked-files=no",
        "--",
        "../../crates",
        "src",
        "Cargo.toml",
    ])
    .is_some();
    if dirty && hash != "unknown" {
        hash.push_str("-dirty");
    }
    println!("cargo:rustc-env=WARPAXE_GIT={hash}");
    // Rerun on a new commit: a commit on the same branch rewrites only
    // the ref file, not HEAD and not the refs directory's mtime. Resolve
    // the files git actually writes (worktree-aware).
    for p in [
        git(&["rev-parse", "--git-path", "HEAD"]),
        git(&["rev-parse", "--git-path", "index"]),
        git(&["rev-parse", "--git-path", "packed-refs"]),
        git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r])),
    ]
    .into_iter()
    .flatten()
    {
        println!("cargo:rerun-if-changed={p}");
    }
}
