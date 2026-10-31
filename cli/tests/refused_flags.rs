//! en: Flags a path cannot honour are refused (exit 24) before any device is opened, not ignored.
//! `tcp:127.0.0.1:1` is an OEP address that is known without connecting; `serial:NO-SUCH-PROBE`
//! is a WCH-Link selector that finds nothing.
//! ja: その経路で守れないフラグは、device を開く前に断る(exit 24)。無視しない。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

fn run(args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let v = common::envelope(&out.stdout);
    (out.status.code().unwrap(), v)
}

fn refused(args: &[&str], names: &str) {
    let (code, v) = run(args);
    assert_eq!(code, 24, "{args:?}: {v}");
    assert_eq!(
        v["error"]["kind"], "capability-unsupported",
        "{args:?}: {v}"
    );
    let msg = v["error"]["msg"].as_str().unwrap();
    assert!(msg.contains(names), "{args:?}: {msg}");
}

const OEP: &str = "tcp:127.0.0.1:1";
const WCH: &str = "serial:NO-SUCH-PROBE";

#[test]
fn an_oep_flash_refuses_what_its_loader_path_would_ignore() {
    for (extra, names) in [
        (&["--erase", "chip"][..], "--erase chip"),
        (&["--erase", "none"][..], "--erase none"),
        (&["--verify", "none"][..], "--verify none"),
        (&["--sdi", "on"][..], "--sdi"),
        (&["--monitor", "dmdata"][..], "--monitor"),
        (&["--reset", "halt"][..], "--reset halt"),
        (&["--repeat"][..], "--repeat"),
        (&["--connect-under-reset"][..], "--connect-under-reset"),
        (&["--attach-wait", "3"][..], "--attach-wait"),
    ] {
        let mut args = vec!["flash", "Cargo.toml", "--probe", OEP];
        args.extend_from_slice(extra);
        refused(&args, names);
    }
}

#[test]
fn pins_and_release_dm_are_refused_where_they_do_nothing() {
    refused(
        &[
            "target",
            "info",
            "--probe",
            WCH,
            "--pins",
            "swdio=0,swclk=1",
        ],
        "--pins",
    );
    refused(&["reset", "--probe", OEP, "--release-dm"], "--release-dm");
}

#[test]
fn recover_image_is_refused_outside_an_oep_nrst() {
    refused(
        &["recover", "--probe", WCH, "--image", "Cargo.toml"],
        "--image",
    );
    refused(
        &[
            "recover",
            "--probe",
            OEP,
            "--method",
            "unprotect",
            "--image",
            "Cargo.toml",
        ],
        "--image",
    );
    refused(
        &[
            "recover",
            "--probe",
            WCH,
            "--method",
            "nrst",
            "--image",
            "Cargo.toml",
        ],
        "--image",
    );
}

#[test]
fn values_no_path_implements_are_usage_errors() {
    for args in [
        &["flash", "Cargo.toml", "--verify", "crc"][..],
        &["flash", "Cargo.toml", "--region", "system"][..],
        &["verify", "Cargo.toml", "--region", "code"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}
