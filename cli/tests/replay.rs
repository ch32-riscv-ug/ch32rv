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

/// An input firmware fixture (under tests/fixtures/, not tests/fixtures/replay/).
fn input(name: &str) -> String {
    format!("{}/../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
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

#[test]
fn flash_round_trip_replays() {
    // The whole flash protocol (stub upload, chip erase, program, readback verify, reset,
    // confirm-run) replays offline from a CH32V307 capture - no probe, no writes to hardware.
    let out = Command::new(bin())
        .args([
            "flash",
            &input("runtest-ch32v307.bin"),
            "--confirm-run",
            "pc",
            "--replay",
            &fixture("flash-v307.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit failure; stderr: {stderr}");
    assert!(stdout.contains("readback matches"), "stdout: {stdout}");
    assert!(stdout.contains("running: yes"), "stdout: {stdout}");
    assert!(
        !stderr.contains("diverged"),
        "unexpected divergence: {stderr}"
    );
}

#[test]
fn read_replays() {
    let out = Command::new(bin())
        .args([
            "read",
            "--range",
            "0x08000000+64",
            "--format",
            "hex-dump",
            "-o",
            "-",
            "--replay",
            &fixture("read-v307.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    assert!(out.status.success());
    // The hex dump starts at the flash base.
    assert!(String::from_utf8_lossy(&out.stdout).contains("08000000"));
}

#[test]
fn target_info_replays_across_families() {
    // Different chip families / probe variants all resolve their SKU offline from a recorded attach.
    for (fx, sku) in [
        ("target-info-v307.ndjson", "CH32V307VCT6"),
        ("target-info-v003.ndjson", "CH32V003F4P6"), // RV32EC part
        ("target-info-v103.ndjson", "CH32V103R8T6"), // via the CH549 Link
    ] {
        let out = Command::new(bin())
            .args(["target", "info", "--json", "--replay", &fixture(fx)])
            .output()
            .expect("run ch32rv");
        assert!(
            out.status.success(),
            "{fx}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(sku),
            "{fx}: expected {sku}"
        );
    }
}
