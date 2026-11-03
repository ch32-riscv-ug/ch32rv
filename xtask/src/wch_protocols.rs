//! en: The copy of wch-protocols' machine-readable tables that ch32rv's tests check against
//! (`crates/wchlink/wch-protocols/`): the command frames and the AttachChip family bytes.
//! `wch-protocols-sync` copies them from a wch-protocols checkout's committed HEAD and records that
//! commit in `WCH_PROTOCOLS_COMMIT`; the tests read the copy, so they never need the neighbour
//! repository. `wch-protocols-check` tells whether the copy is still what that checkout has.
//!
//! ja: ch32rv の試験が照合する wch-protocols の表の写し。`wch-protocols-sync` が checkout の HEAD から
//! 写して commit を記録する。試験は写しを読むので、隣の repo に頼らない。

use std::path::Path;

/// Where the copy lives (from the workspace root).
pub const DIR: &str = "crates/wchlink/wch-protocols";
/// The files copied: their path in wch-protocols, and their name in the copy.
const FILES: [(&str, &str); 2] = [
    (
        "protocols/data/wchlink-commands.csv",
        "wchlink-commands.csv",
    ),
    ("protocols/data/families.csv", "families.csv"),
];
const COMMIT_FILE: &str = "WCH_PROTOCOLS_COMMIT";

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("git output: {e}"))
}

/// Each copied file's name in the copy, with its text.
type Files = Vec<(&'static str, String)>;

/// The files at the checkout's HEAD, and that commit.
fn at_head(repo: &Path) -> Result<(Files, String), String> {
    let commit = git(repo, &["rev-parse", "HEAD"])?.trim().to_owned();
    let mut files = Vec::new();
    for (from, to) in FILES {
        files.push((to, git(repo, &["show", &format!("HEAD:{from}")])?));
    }
    Ok((files, commit))
}

/// `wch-protocols-sync`: write the copy and the commit it came from.
pub fn sync(repo: &Path) -> Result<String, String> {
    let (files, commit) = at_head(repo)?;
    let dir = Path::new(DIR);
    std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    for (name, text) in &files {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("write {path:?}: {e}"))?;
    }
    std::fs::write(dir.join(COMMIT_FILE), format!("{commit}\n"))
        .map_err(|e| format!("write {COMMIT_FILE}: {e}"))?;
    Ok(format!("copied wch-protocols {commit} into {DIR}"))
}

/// `wch-protocols-check`: whether the copy is the checkout's HEAD.
pub fn check(repo: &Path) -> Result<String, String> {
    let (files, commit) = at_head(repo)?;
    let dir = Path::new(DIR);
    let mut stale = Vec::new();
    for (name, text) in &files {
        if std::fs::read_to_string(dir.join(name)).ok().as_deref() != Some(text.as_str()) {
            stale.push((*name).to_owned());
        }
    }
    let recorded = std::fs::read_to_string(dir.join(COMMIT_FILE)).unwrap_or_default();
    if recorded.trim() != commit {
        stale.push(format!("{COMMIT_FILE} ({} != {commit})", recorded.trim()));
    }
    if stale.is_empty() {
        Ok(format!("up to date: {DIR} is wch-protocols {commit}"))
    } else {
        Err(format!(
            "{DIR} is not wch-protocols {commit}: {} - run `cargo xtask wch-protocols-sync`",
            stale.join(", ")
        ))
    }
}
