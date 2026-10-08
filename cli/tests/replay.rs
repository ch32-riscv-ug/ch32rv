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
fn hid_boot_flash_replays() {
    let out = Command::new(bin())
        .args([
            "boot",
            "hid",
            "flash",
            &input("runtest-ch32v003.bin"),
            "--replay",
            &fixture("hid-flash-v003.ndjson"),
            "--progress",
            "none",
        ])
        .output()
        .expect("run ch32rv");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit failure; stderr: {stderr}");
    assert!(stdout.contains("11 sector(s) changed"), "stdout: {stdout}");
    assert!(stdout.contains("verified"), "stdout: {stdout}");
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
fn run_semihosting_replays() {
    // The whole `run --exit-on semihosting` path replays offline from a CH32V203 capture: flash the
    // image, reset, resume, the dmdata output decode, and the semihosting host that recognises the
    // slli/ebreak/srai sequence, services SYS_WRITE0, and reports SYS_EXIT's code. Deterministic
    // because the recording ends on the target's own exit, not a timeout. The capture flashed, so
    // the replay must too (no --no-flash, or the recorded program transfers desync).
    let out = Command::new(bin())
        .args([
            "run",
            &input("semihosting.bin"),
            "--exit-on",
            "semihosting",
            "--replay",
            &fixture("run-semihosting-v203.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The fixture's program exits 42 via SYS_EXIT_EXTENDED: the tool says target-exit (60) and
    // names the target's code (docs/freeze-decisions.ja.md §6), not 42 as the process's code.
    assert_eq!(out.status.code(), Some(60), "stderr: {stderr}");
    assert!(stderr.contains("exited with code 42"), "stderr: {stderr}");
    // SYS_WRITE0 output is a runtime-output stream, so in human mode it lands on stdout.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello from semihosting"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !stderr.contains("diverged"),
        "unexpected divergence: {stderr}"
    );
}

#[test]
fn target_info_replays_across_families() {
    // Different chip families / probe variants all resolve their SKU offline from a recorded attach.
    for (fx, sku) in [
        ("target-info-v307.ndjson", "CH32V307VCT6"),
        ("target-info-v003.ndjson", "CH32V003F4P6"), // RV32EC part
        ("target-info-v103.ndjson", "CH32V103R8T6"), // via the CH549 Link
        ("target-info-v205.ndjson", "CH32V205RCT6"), // measured LinkE 2.22, family 0xce
        ("target-info-x315.ndjson", "CH32X315MCU6"), // measured LinkE 2.22, family 0xe6
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

#[test]
fn unregistered_target_still_reports_its_signature_and_connection() {
    for json in [false, true] {
        let mut cmd = Command::new(bin());
        cmd.args([
            "target",
            "info",
            "--replay",
            &fixture("target-info-unknown.ndjson"),
        ]);
        if json {
            cmd.arg("--json");
        }
        let out = cmd.output().expect("run ch32rv");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{stderr}");
        assert!(!stderr.contains("diverged"), "{stderr}");
        if json {
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(v["result"]["connection"], "connected");
            assert_eq!(v["result"]["identification"], "unregistered");
            assert_eq!(v["result"]["family_byte"], "0x77");
            assert_eq!(v["target"]["chip_id"], "0xdeadbeef");
            assert_eq!(v["target"]["flash_bytes"], 262144);
            assert_eq!(v["target"]["uid"], "0fb8abcd77d2bc4d");
            assert!(v["target"]["sku"].is_null());
            assert_eq!(v["warnings"][0]["code"], "family-unknown");
            assert_eq!(v["warnings"][1]["code"], "sku-unknown");
        } else {
            for s in [
                "connected (unregistered)",
                "0x77",
                "0xdeadbeef",
                "256 KiB",
                "0fb8abcd77d2bc4d",
            ] {
                assert!(stdout.contains(s), "missing {s}: {stdout}");
            }
            assert!(stderr.contains("not a connection failure"), "{stderr}");
        }
    }
}

#[test]
fn v205_registration_exposes_delivered_and_measured_evidence() {
    let out = Command::new(bin())
        .args([
            "target",
            "info",
            "--json",
            "--replay",
            &fixture("target-info-v205.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["target"]["family"], "CH32V205");
    assert_eq!(v["target"]["sku"], "CH32V205RCT6");
    assert_eq!(v["target"]["verified"], true);
    assert_eq!(v["target"]["provisional"], false);
    assert_eq!(v["result"]["identification"], "identified");
    assert_eq!(v["result"]["family_byte"], "0xce");
    assert_eq!(v["result"]["silicon_revision"], 1);
    assert_eq!(v["result"]["sram_bytes"], 32768);
    assert_eq!(v["result"]["debug_wiring"]["swdio"], "PA13");
    assert!(
        v["warnings"]
            .as_array()
            .is_none_or(|warnings| warnings.iter().all(|w| w["code"] != "sku-provisional"))
    );
}

#[test]
fn x315_erased_capacity_uses_db_and_retains_raw_response() {
    let out = Command::new(bin())
        .args([
            "target",
            "info",
            "--json",
            "--replay",
            &fixture("target-info-x315.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["target"]["family"], "CH32X315");
    assert_eq!(v["target"]["sku"], "CH32X315MCU6");
    assert_eq!(v["target"]["verified"], true);
    assert_eq!(v["target"]["provisional"], false);
    assert_eq!(v["target"]["flash_bytes"], 196608);
    assert_eq!(v["target"]["uid"], "36a0abcd9eb5bc48");
    assert_eq!(v["result"]["family_byte"], "0xe6");
    assert_eq!(v["result"]["sram_bytes"], 65536);
    assert_eq!(v["result"]["flash_capacity_raw"], "0xe339");
    assert_eq!(v["result"]["flash_capacity_source"], "db");
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "flash-capacity-unavailable")
    );

    let out = Command::new(bin())
        .args([
            "target",
            "info",
            "--replay",
            &fixture("target-info-x315.ndjson"),
        ])
        .output()
        .expect("run ch32rv");
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("192 KiB (DB; probe capacity unavailable)")
    );
}
