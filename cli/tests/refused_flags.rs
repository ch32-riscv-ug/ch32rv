//! en: Flags a path cannot honour are refused (exit 24) before any device is opened, not ignored.
//! `oep://0123456789ab?via=tcp:127.0.0.1:1` is an OEP address that is known without connecting; `serial:NO-SUCH-PROBE`
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

const OEP: &str = "oep://0123456789ab?via=tcp:127.0.0.1:1";
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

#[test]
fn removed_address_forms_say_what_to_write() {
    for (gone, instead) in [
        ("port:/dev/ttyACM0", "/dev/ttyACM0"),
        (
            "tcp:127.0.0.1:4242",
            "oep://<unit_id>?via=tcp:127.0.0.1:4242",
        ),
        (
            "tcp://127.0.0.1:4242",
            "oep://<unit_id>?via=tcp:127.0.0.1:4242",
        ),
        ("tcp:0123456789ab", "oep://0123456789ab?via=net"),
        ("usb:3-1.4.2", "topology:3-1.4.2"),
        (
            "oep-net://0123456789ab/weact",
            "oep://0123456789ab/weact?via=net",
        ),
    ] {
        let (code, v) = run(&["target", "info", "--probe", gone]);
        assert_eq!(code, 2, "{gone}: {v}");
        let msg = v["error"]["msg"].as_str().unwrap();
        assert!(msg.contains(instead), "{gone}: {msg}");
    }
    // A query other than via, or a route that is none, is refused before anything is opened.
    for bad in [
        "oep://0123456789ab?via=wifi",
        "oep://0123456789ab?route=usb",
        "oep://0123456789ab?via=usb&x=1",
    ] {
        let (code, v) = run(&["target", "info", "--probe", bad]);
        assert_eq!(code, 2, "{bad}: {v}");
    }
}
