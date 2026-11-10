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
fn bootloaders_refuse_erase_chip_and_none() {
    // HID and DFU erase only what they write: no whole-flash erase, no write without erase.
    for erase in ["chip", "none"] {
        refused(
            &["boot", "hid", "flash", "Cargo.toml", "--erase", erase],
            &format!("--erase {erase}"),
        );
        refused(
            &["boot", "dfu", "flash", "Cargo.toml", "--erase", erase],
            &format!("--erase {erase}"),
        );
    }
    // auto is gone everywhere.
    for args in [
        &["flash", "Cargo.toml", "--erase", "auto"][..],
        &["isp", "flash", "Cargo.toml", "--erase", "auto"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
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
        ("hid://3-2.4", "ch32rv-hid://3-2.4"),
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

fn usage(args: &[&str]) -> (String, String) {
    let (code, v) = run(args);
    assert_eq!(code, 2, "{args:?}: {v}");
    (
        v["error"]["msg"].as_str().unwrap_or("").to_owned(),
        v["error"]["hint"].as_str().unwrap_or("").to_owned(),
    )
}

fn image() -> String {
    format!(
        "{}/../tests/fixtures/runtest-ch32v003.bin",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn a_write_target_is_a_device_not_a_probe() {
    // --probe names a debug probe only; a bootloader's or application's address goes to --device.
    for a in [
        "ch32rv-hid://3-2.4",
        "ch32rv-app://C016ABCD283FBC5C",
        "ch32rv-isp://3-2.4",
    ] {
        let (msg, _) = usage(&["target", "info", "--probe", a]);
        assert!(msg.contains("--device"), "{a}: {msg}");
    }
    // boot dfu / boot hid write through no probe: --probe is refused with the --device to write.
    let img = image();
    for args in [
        vec![
            "boot",
            "hid",
            "flash",
            &img,
            "--probe",
            "ch32rv-hid://3-2.4",
        ],
        vec!["boot", "dfu", "flash", &img, "--probe", "/dev/ttyACM0"],
        vec!["boot", "dfu", "info", "--probe", "/dev/ttyACM0"],
    ] {
        let (msg, hint) = usage(&args);
        assert!(msg.contains("--probe"), "{args:?}: {msg}");
        assert!(
            hint.contains(&format!("--device {}", args[args.len() - 1])),
            "{args:?}: {hint}"
        );
    }
    // CH32RV_PROBE names the other commands' probe; it is not a mistake on boot.
    let out = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args(["boot", "hid", "flash", &img, "--dry-run"])
        .env("CH32RV_PROBE", "serial:NO-SUCH-PROBE")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    // isp --app-device is gone: --device takes the application too.
    let (msg, hint) = usage(&["isp", "info", "--app-device", "/dev/ttyACM0"]);
    assert!(msg.contains("--app-device"), "{msg}");
    assert!(hint.contains("--device /dev/ttyACM0"), "{hint}");
}

#[test]
fn device_forms_are_checked_before_anything_is_opened() {
    let img = image();
    // boot hid: a position only; the old scheme says what to write.
    let (_, hint) = usage(&["boot", "hid", "flash", &img, "--device", "hid://3-2.4"]);
    assert!(hint.contains("ch32rv-hid://3-2.4"), "{hint}");
    let (msg, _) = usage(&["boot", "hid", "flash", &img, "--device", "1209:b803"]);
    assert!(msg.contains("position"), "{msg}");
    // isp: removed forms and another route's bootloader are refused.
    let (msg, _) = usage(&["isp", "info", "--device", "usb:3-1.4"]);
    assert!(msg.contains("topology:3-1.4"), "{msg}");
    let (msg, _) = usage(&["isp", "info", "--device", "hid://3-2.4"]);
    assert!(msg.contains("ch32rv-hid://3-2.4"), "{msg}");
    let (_, hint) = usage(&["isp", "info", "--device", "ch32rv-hid://3-2.4"]);
    assert!(hint.contains("boot hid flash"), "{hint}");
    let (_, hint) = usage(&["boot", "dfu", "info", "--device", "ch32rv-isp://3-2.4"]);
    assert!(hint.contains("isp --device"), "{hint}");
}

/// The IDE's waiting port (`ch32rv-bootloader://WAIT_FOR_BOOT`, docs/freeze-decisions.ja.md §2):
/// a chip with no USB is refused before any wait, another word after the scheme is a usage
/// error, and it is a write target - not a probe, and not `boot dfu`'s.
#[test]
fn the_waiting_port_refuses_what_it_cannot_wait_for() {
    let img = image();
    let wait = "ch32rv-bootloader://WAIT_FOR_BOOT";
    for chip in ["CH32V003", "CH32V006"] {
        refused(
            &["--chip", chip, "isp", "flash", &img, "--device", wait],
            "has no USB peripheral",
        );
    }
    let (msg, _) = usage(&[
        "isp",
        "flash",
        &img,
        "--device",
        "ch32rv-bootloader://3-2.4",
    ]);
    assert!(msg.contains("WAIT_FOR_BOOT"), "{msg}");
    let (msg, _) = usage(&["target", "info", "--probe", wait]);
    assert!(msg.contains("--device"), "{msg}");
    let (_, hint) = usage(&["boot", "dfu", "info", "--device", wait]);
    assert!(hint.contains("isp flash"), "{hint}");
    let (msg, _) = usage(&[
        "isp",
        "flash",
        &img,
        "--transport",
        "uart",
        "--device",
        wait,
    ]);
    assert!(msg.contains("UART"), "{msg}");
}

/// Discovery lists the waiting port with nothing connected, so the IDE can keep it selected.
#[test]
fn discovery_always_lists_the_waiting_port() {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_ch32rv"))
        .args(["arduino", "discovery"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"HELLO 1 \"test\"\nLIST\nQUIT\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let list = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v["eventType"] == "list")
        .expect("a list event");
    let port = list["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["protocol"] == "ch32rv-bootloader")
        .expect("the waiting port");
    assert_eq!(port["address"], "ch32rv-bootloader://WAIT_FOR_BOOT");
}
