//! en: `ch32rv flash` end to end on an OEP probe: the spec side's fake probe (oep-client-python
//! `fake_serve`) with ch32rv's loader played by `crates/oep/tests/fake/loader_hook.py`, over TCP
//! (`--probe tcp:`) and over its pty (`--probe port:<pty>`, the serial path with the single-serial
//! lock rule). Skipped when uv or the client checkout is missing.
//! ja: OEP の probe での `ch32rv flash` を端から端まで(偽の probe、TCP と pty)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../crates/oep/tests/fake/uv.rs"]
mod uv;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

struct Fake {
    child: Child,
    at: String,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn fake(args: &[&str]) -> Option<Fake> {
    let dir = std::env::var_os("OEP_CLIENT_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("../../dev_oep/oep-client-python"));
    if !dir.join("src/oep_client/fake_serve.py").exists() {
        eprintln!(
            "skip: no oep-client-python with fake_serve at {}",
            dir.display()
        );
        return None;
    }
    let hook = root().join("crates/oep/tests/fake/loader_hook.py:loader");
    let mut child = uv::uv_run(&dir)
        .args(["python", "-m", "oep_client.fake_serve"])
        .args(args)
        .arg("--run-hook")
        .arg(&hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    Some(Fake {
        child,
        at: line.trim().to_owned(),
    })
}

fn flash(probe: &str) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .arg("flash")
        .arg(root().join("tests/fixtures/runtest-ch32v203.bin"))
        .args([
            "--probe",
            probe,
            "--json",
            "--non-interactive",
            "--progress",
            "none",
        ])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    assert!(out.status.success(), "{v}");
    v
}

// The fake reports a CH32V203's chip id, so the DB resolves the family (not a family byte).
const V203: &str = "0x20310500";

#[test]
fn flash_over_tcp() {
    let Some(f) = fake(&[
        "--tcp",
        "0",
        "--once",
        "--framing",
        "length",
        "--target-id",
        V203,
    ]) else {
        return;
    };
    let port = f.at.strip_prefix("PORT ").unwrap();
    let v = flash(&format!("tcp:127.0.0.1:{port}"));
    let fl = &v["result"]["flash"];
    assert_eq!(fl["programmer"], "oep-loader");
    assert_eq!(fl["family"], "CH32V20x");
    assert_eq!(fl["chip_id"], V203);
    assert_eq!(fl["bytes"], 852);
    assert_eq!(fl["rewritten"], 0);
}

#[test]
fn flash_over_the_serial_path() {
    // A UART-bridge probe (no OEP USB device of its own, so its serial port is where it is flashed).
    let Some(f) = fake(&["--pty", "--profile", "esp32-v003", "--target-id", V203]) else {
        return;
    };
    let pty = f.at.strip_prefix("PTY ").unwrap();
    let v = flash(&format!("port:{pty}"));
    assert_eq!(v["result"]["flash"]["family"], "CH32V20x");
}

fn flash_raw(probe: &str, chip: Option<&str>) -> (bool, serde_json::Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ch32rv"));
    cmd.arg("flash")
        .arg(root().join("tests/fixtures/runtest-ch32v203.bin"))
        .args([
            "--probe",
            probe,
            "--json",
            "--non-interactive",
            "--progress",
            "none",
        ]);
    if let Some(c) = chip {
        cmd.args(["--chip", c]);
    }
    let out = cmd.output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    (out.status.success(), v)
}

#[test]
fn the_slot_of_the_boards_family_is_chosen() {
    // Two slots; the second has no target. The board's family picks the first.
    let Some(f) = fake(&[
        "--tcp",
        "0",
        "--framing",
        "length",
        "--profile",
        "p4-bench",
        "--slot",
        "bench-a",
        "--slot",
        "bench-b",
        "--absent",
        "1",
        "--target-id",
        V203,
    ]) else {
        return;
    };
    let port = f.at.strip_prefix("PORT ").unwrap();
    let (ok, v) = flash_raw(&format!("tcp:127.0.0.1:{port}"), Some("CH32V203C8T6"));
    assert!(ok, "{v}");
    assert_eq!(v["result"]["flash"]["family"], "CH32V20x");
}

#[test]
fn two_slots_of_the_family_stop_with_the_list() {
    let Some(f) = fake(&[
        "--tcp",
        "0",
        "--framing",
        "length",
        "--profile",
        "p4-bench",
        "--slot",
        "bench-a",
        "--slot",
        "bench-b",
        "--target-id",
        V203,
    ]) else {
        return;
    };
    let port = f.at.strip_prefix("PORT ").unwrap();
    let (ok, v) = flash_raw(&format!("tcp:127.0.0.1:{port}"), Some("CH32V20x"));
    assert!(!ok);
    let msg = v["error"]["msg"].as_str().unwrap_or_default();
    assert!(
        msg.contains("2 slot(s)") && msg.contains("bench-a") && msg.contains("bench-b"),
        "{v}"
    );
}

fn run(args: &[&str]) -> (bool, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args(args)
        .args(["--json", "--non-interactive"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    (out.status.success(), v)
}

#[test]
fn verify_read_reset_and_target_info_on_an_oep_probe() {
    // A UART-bridge probe (no OEP USB device of its own, so its serial port is where it is flashed).
    let Some(f) = fake(&["--pty", "--profile", "esp32-v003", "--target-id", V203]) else {
        return;
    };
    let probe = format!("port:{}", f.at.strip_prefix("PTY ").unwrap());
    let img = root().join("tests/fixtures/runtest-ch32v203.bin");
    let img = img.to_str().unwrap();
    let (ok, v) = run(&["flash", img, "--probe", &probe, "--progress", "none"]);
    assert!(ok, "{v}");

    let (ok, v) = run(&["verify", img, "--probe", &probe]);
    assert!(ok, "{v}");
    assert_eq!(v["result"]["verified"], true);
    // Another image does not match what is on the part.
    let other = root().join("tests/fixtures/runtest-ch32v307.bin");
    let (ok, v) = run(&["verify", other.to_str().unwrap(), "--probe", &probe]);
    assert!(!ok);
    assert_eq!(v["error"]["kind"], "verify-mismatch", "{v}");

    let (ok, v) = run(&["read", "--range", "0x08000000+16", "--probe", &probe]);
    assert!(ok, "{v}");
    let first16: String = std::fs::read(img).unwrap()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert!(v.to_string().contains(&first16), "{v}");

    let (ok, v) = run(&["reset", "--probe", &probe, "--confirm-run"]);
    assert!(ok, "{v}");
    assert_eq!(v["result"]["running"], true);

    let (ok, v) = run(&["target", "info", "--probe", &probe]);
    assert!(ok, "{v}");
    assert_eq!(v["result"]["target"]["family"], "CH32V20x");
    assert_eq!(v["result"]["target"]["chip_id"], V203);
}

#[test]
fn a_raw_upload_to_a_probe_that_announces_its_oep_device_is_refused() {
    // The P4 profile says oep_pid = 1: its slots are the IDE's ports, not this serial port.
    let Some(f) = fake(&["--pty", "--target-id", V203]) else {
        return;
    };
    let pty = f.at.strip_prefix("PTY ").unwrap();
    let (ok, v) = flash_raw(&format!("port:{pty}"), None);
    assert!(!ok);
    assert!(
        v["error"]["msg"]
            .as_str()
            .unwrap_or_default()
            .contains("slots"),
        "{v}"
    );
}

#[test]
fn flash_where_the_host_picks_the_pins() {
    // A probe whose wire takes its pins from the host (role_channels) and has no slot: ch32rv
    // scans for the pair with a target (the fake's is the first, GP0 / GP1) and attaches there.
    let dir = std::env::var_os("OEP_CLIENT_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("../../dev_oep/oep-client-python"));
    let has_profile = std::fs::read_to_string(dir.join("src/oep_client/fake.py"))
        .is_ok_and(|t| t.contains("rp2350-pins"));
    if !has_profile {
        eprintln!("skip: the fake has no rp2350-pins profile");
        return;
    }
    let Some(f) = fake(&["--pty", "--profile", "rp2350-pins", "--target-id", V203]) else {
        return;
    };
    let probe = format!("port:{}", f.at.strip_prefix("PTY ").unwrap());
    let v = flash(&probe);
    let fl = &v["result"]["flash"];
    assert_eq!(fl["family"], "CH32V20x");
    assert_eq!(fl["bytes"], 852);
}
