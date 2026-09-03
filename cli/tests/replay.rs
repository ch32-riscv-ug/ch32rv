//! en: Offline replay integration test. Runs the CLI against committed capture fixtures (no
//! hardware) and checks it reproduces the recorded exchange, so the wchlink/dmi protocol layers
//! and the replay engine are regression-tested in CI without a probe. Regenerate a fixture with
//! `ch32rv <cmd> --capture <file>` on real hardware (docs/cli.ja.md §3.7).
//! ja: offline replay 統合テスト。committed の capture fixture に対して CLI を動かし(HW 無し)、
//! 記録された交換を再現できるか確認する。probe 無しで CI 回帰できる。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ch32rv")
}

fn fixture(name: &str) -> String {
    format!(
        "{}/../tests/fixtures/replay/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn target_info_replays_identically() {
    let out = Command::new(bin())
        .args([
            "target",
            "info",
            "--json",
            "--replay",
            &fixture("target-info-v307.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit failure; stderr: {stderr}");
    // The recorded V307 exchange resolves to the exact chip / SKU / flash size, offline.
    assert!(
        stdout.contains(r#""chip_id":"0x30700528""#),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(r#""sku":"CH32V307VCT6""#),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains(r#""flash_bytes":294912"#),
        "stdout: {stdout}"
    );
    // A faithful replay must not warn about divergence.
    assert!(
        !stderr.contains("diverged"),
        "unexpected divergence: {stderr}"
    );
}

#[test]
fn probe_info_replays() {
    let out = Command::new(bin())
        .args([
            "probe",
            "info",
            "--json",
            "--replay",
            &fixture("probe-info-v307.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("38EF8F06BDC2"), "stdout: {stdout}");
}

#[test]
fn replay_without_device_line_errors() {
    let tmp = std::env::temp_dir().join("ch32rv_replay_nodev.ndjson");
    std::fs::write(&tmp, "{\"_meta\":{\"format\":1}}\n").unwrap();
    let out = Command::new(bin())
        .args(["target", "info", "--replay"])
        .arg(&tmp)
        .output()
        .expect("run ch32rv");
    assert_eq!(out.status.code(), Some(2)); // usage error
    assert!(String::from_utf8_lossy(&out.stderr).contains("_device"));
    let _ = std::fs::remove_file(&tmp);
}
