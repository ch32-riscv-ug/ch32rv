#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

#[test]
fn unsupported_core_operations_fail_before_opening_a_probe() {
    for args in [
        vec!["--core", "2", "dbg", "regs"],
        vec!["--core", "1", "dbg", "halt", "--reset"],
        vec!["--core", "1", "erase", "--all", "--yes"],
        vec!["--core", "1", "reset"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
            .args(["--json", "--probe", "serial:absent-core-test"])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(24));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["error"]["kind"], "capability-unsupported");
    }
}

#[test]
fn h417_core1_register_read_replays_with_selection_preserved() {
    let fixture = format!(
        "{}/../tests/fixtures/replay/dbg-h417-core1-mhartid.ndjson",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args([
            "--json",
            "--core",
            "1",
            "--speed",
            "low",
            "--replay",
            &fixture,
            "dbg",
            "reg",
            "read",
            "csr:0xf14",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["ok"].as_bool().unwrap());
    assert_eq!(value["result"]["value"], "0x00000001");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("divergen"));
}

#[test]
fn h417_core1_run_control_replays() {
    for operation in ["step", "resume", "halt"] {
        let fixture = format!(
            "{}/../tests/fixtures/replay/dbg-h417-core1-{operation}.ndjson",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut args = vec![
            "--json", "--core", "1", "--speed", "low", "--replay", &fixture, "dbg", operation,
        ];
        if operation == "step" {
            args.push("1");
        }
        let output = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{operation}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("divergen"));
    }
}

#[test]
fn rejected_hart_readback_does_not_access_core0_registers() {
    let fixture = format!(
        "{}/../tests/fixtures/replay/dbg-h417-core1-rejected.ndjson",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args([
            "--json",
            "--core",
            "1",
            "--speed",
            "low",
            "--replay",
            &fixture,
            "dbg",
            "reg",
            "read",
            "csr:0xf14",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(24));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["kind"], "capability-unsupported");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("divergen"));
}
