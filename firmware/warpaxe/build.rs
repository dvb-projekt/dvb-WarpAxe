use std::process::Command;

fn main() {
    embuild::espidf::sysenv::output();
    // The commit the image was built from, for /api/system/info: a
    // device that reports one build's numbers while the tree says
    // another is otherwise hard to tell apart (BitForge Nano, 2026-09-29).
    let out = Command::new("git")
        .args(["rev-parse", "--short=9", "HEAD"])
        .output();
    let mut hash = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    };
    let dirty = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            "../../crates",
            "src",
            "Cargo.toml",
        ])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    if dirty && hash != "unknown" {
        hash.push_str("-dirty");
    }
    println!("cargo:rustc-env=WARPAXE_GIT={hash}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    println!("cargo:rerun-if-changed=../../.git/index");
}
