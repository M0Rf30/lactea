//! Captures `git describe --always --dirty` as `LACTEA_VCS_VERSION` (`"unknown"` outside a git checkout of this
//! crate, e.g. a published crate, or without git).

use std::path::PathBuf;
use std::process::Command;

fn git(dir: &PathBuf, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn describe(dir: &PathBuf) -> Option<String> {
    // a crate unpacked inside somebody else's repository is not tracked by it: its describe would be theirs
    git(dir, &["ls-files", "--error-unmatch", "Cargo.toml"])?;
    git(dir, &["describe", "--always", "--dirty"])
}

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let version = describe(&dir).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=LACTEA_VCS_VERSION={version}");
    println!("cargo:rerun-if-changed=build.rs");
    // a new commit moves HEAD (or the ref it points to); `--dirty` follows the index
    if let Some(git_dir) = git(&dir, &["rev-parse", "--git-dir"]) {
        let git_dir = dir.join(git_dir);
        for f in ["HEAD", "index"] {
            println!("cargo:rerun-if-changed={}", git_dir.join(f).display());
        }
    }
}
