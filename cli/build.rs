//! en: Build script: capture the git revision and flash-stub hashes so `version --json` can
//! report reproducibility info (docs/architecture.ja.md §3). Kept dependency-free.
//! ja: build script: git revision と flash stub の hash を埋め、`version --json` が再現性
//! 情報を出せるようにする。依存無しで済ませる。

use std::process::Command;

fn main() {
    // Git short revision (best-effort; "unknown" when not a git checkout).
    let git_rev = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=CH32RV_GIT_REV={git_rev}");

    // Re-run if HEAD moves: HEAD itself names the branch (`ref: refs/heads/main`) and does not
    // change on a commit, so the branch's ref (loose or packed) is watched too.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    if let Ok(head) = std::fs::read_to_string("../.git/HEAD")
        && let Some(r) = head.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed=../.git/{r}");
        println!("cargo:rerun-if-changed=../.git/packed-refs");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
