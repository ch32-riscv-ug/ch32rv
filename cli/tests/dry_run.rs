//! en: `--dry-run` is refused by every command that does not honour it, before any device is
//! touched (it used to be ignored: `erase --all --dry-run --yes` erased).
//! ja: `--dry-run` に対応しないコマンドは、device に触れる前に断る(以前は無視して実行した)。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn run(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args(args)
        .args(["--json", "--probe", "serial:NO-SUCH-PROBE"])
        .output()
        .unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap();
    (out.status.code().unwrap(), v)
}

#[test]
fn commands_without_dry_run_refuse_it() {
    for args in [
        &["erase", "--all", "--dry-run", "--yes"][..],
        &["flash", "Cargo.toml", "--dry-run"][..],
        &["recover", "--dry-run", "--yes"][..],
    ] {
        let (code, v) = run(args);
        assert_eq!(code, 2, "{args:?}: {v}");
        assert_eq!(v["error"]["kind"], "usage", "{args:?}: {v}");
        assert!(
            v["error"]["msg"].as_str().unwrap().contains("--dry-run"),
            "{v}"
        );
    }
}

#[test]
fn chip_auto_is_no_chip_and_empty_is_a_usage_error() {
    // `--chip auto` (any case) detects and checks nothing, so it goes on to look for the probe;
    // an empty value names nothing and is refused (docs/freeze-decisions.ja.md §1).
    let (code, v) = run(&["target", "info", "--chip", "Auto"]);
    assert_eq!(code, 10, "{v}");
    assert_eq!(v["error"]["kind"], "device-not-found", "{v}");
    let (code, v) = run(&["target", "info", "--chip", ""]);
    assert_eq!(code, 2, "{v}");
}
