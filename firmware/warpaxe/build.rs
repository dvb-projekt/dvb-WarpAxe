use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git").args(args).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    embuild::espidf::sysenv::output();
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
