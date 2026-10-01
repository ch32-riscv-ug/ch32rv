//! en: ArduinoCore-CH32RV's upload recipe (platform.txt `tools.ch32rv.upload.pattern`) as ch32rv
//! sees it, with no device: the argument set parses, and a board whose `build.ch32rv_chip` is a
//! series ch32rv has no name for stops with target-not-in-db (20) before any probe is opened
//! (docs/freeze-decisions.ja.md §1).
//! ja: ArduinoCore-CH32RV の upload の recipe を device なしで確かめる。ch32rv に名前の無い series の
//! 板は、probe を開く前に target-not-in-db(20)で止まる。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn upload(chip: &str) -> std::process::Output {
    let image = format!(
        "{}/../tests/fixtures/runtest-ch32v307.bin",
        env!("CARGO_MANIFEST_DIR")
    );
    // The recipe's line, with `--format elf` left out (the fixture is a bin) and an address
    // nothing answers on.
    Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args([
            "flash",
            &image,
            "--chip",
            chip,
            "--reset",
            "run",
            "--confirm-run",
            "--non-interactive",
            "--progress",
            "none",
            "--probe",
            "port:/dev/ch32rv-no-such-port",
        ])
        .output()
        .expect("run ch32rv")
}

#[test]
fn a_series_ch32rv_does_not_know_stops_before_the_probe() {
    let out = upload("CH32V205");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(20), "{stderr}");
    assert!(stderr.contains("target-not-in-db"), "{stderr}");
}

#[test]
fn a_known_name_goes_on_to_the_probe() {
    // A series, a family and a SKU ch32rv knows get past the name check; here the probe is
    // missing, so it stops at the open (11), not at the name.
    for chip in ["CH32V203", "CH32V20x", "CH32V203C8T6"] {
        let out = upload(chip);
        assert_eq!(
            out.status.code(),
            Some(11),
            "{chip}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
