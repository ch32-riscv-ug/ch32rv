//! en: Real `--json` output checked against docs/contract/*.schema.json (docs/contract/README.ja.md
//! rule 5), with no device: `version`, `db`, error envelopes, and replays of every recorded session
//! (flash through the WCH stub, a HID bootloader and the factory ISP bootloader, target / probe /
//! isp info, read, run, dbg on the second H417 core), plus the NDJSON events a replayed flash
//! streams. Every command that emits a `result` has its schema (commands.schema.json); the
//! validator is in common/mod.rs.
//! ja: 実際の `--json` の出力を docs/contract の schema と照合する(device なし)。`result` を出す
//! コマンドはすべて schema を持つ(commands.schema.json)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeSet;
use std::process::Command;

use common::{CONTRACT, check, root, schema};
use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ch32rv")
}

fn replay(name: &str) -> String {
    root(&format!("tests/fixtures/replay/{name}"))
}

fn input(name: &str) -> String {
    root(&format!("tests/fixtures/{name}"))
}

fn stdout_json(args: &[&str]) -> (Value, String) {
    let out = Command::new(bin()).args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let v =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}"));
    (v, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Every device-free `--json` run: (args, the `cmd` it answers with, whether it succeeds).
fn cases() -> Vec<(Vec<String>, &'static str, bool)> {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    let mut c = vec![
        (s(&["version", "--json"]), "version", true),
        (s(&["db", "list", "--json"]), "db.list", true),
        (
            s(&["db", "info", "CH32V003F4P6", "--json"]),
            "db.info",
            true,
        ),
        (
            s(&[
                "target",
                "info",
                "--probe",
                "serial:NO-SUCH-PROBE",
                "--json",
            ]),
            "target.info",
            false,
        ),
        (
            s(&["erase", "--all", "--dry-run", "--yes", "--json"]),
            "erase",
            false,
        ),
        // No broker for a path nothing listens on: every field null.
        (
            s(&[
                "broker",
                "endpoint",
                "--probe",
                "port:/dev/ch32rv-no-such-port",
                "--json",
            ]),
            "broker.endpoint",
            true,
        ),
        (
            s(&[
                "flash",
                &input("runtest-ch32v307.bin"),
                "--confirm-run",
                "pc",
                "--replay",
                &replay("flash-v307.ndjson"),
                "--json",
                "--progress",
                "none",
            ]),
            "flash",
            true,
        ),
        (
            s(&[
                "boot",
                "hid",
                "flash",
                &input("runtest-ch32v003.bin"),
                "--replay",
                &replay("hid-flash-v003.ndjson"),
                "--json",
                "--progress",
                "none",
            ]),
            "boot.hid.flash",
            true,
        ),
        (
            s(&[
                "--json",
                "--replay",
                &replay("isp-flash-x035.ndjson"),
                "isp",
                "flash",
                &input("pattern-4k.bin"),
            ]),
            "isp.flash",
            true,
        ),
        (
            s(&[
                "--json",
                "--replay",
                &replay("isp-info-x035.ndjson"),
                "isp",
                "info",
            ]),
            "isp.info",
            true,
        ),
        (
            s(&[
                "--json",
                "--replay",
                &replay("isp-config-set-x035.ndjson"),
                "isp",
                "config",
                "set",
                "data0=0x5a",
                "data1=7",
            ]),
            "isp.config.set",
            true,
        ),
        (
            s(&[
                "probe",
                "info",
                "--json",
                "--replay",
                &replay("probe-info-v307.ndjson"),
            ]),
            "probe.info",
            true,
        ),
        (
            s(&[
                "read",
                "--range",
                "0x08000000+64",
                "--json",
                "--replay",
                &replay("read-v307.ndjson"),
            ]),
            "read",
            true,
        ),
        // The program exits 42: target-exit (60) with `result.exit`.
        (
            s(&[
                "run",
                &input("semihosting.bin"),
                "--exit-on",
                "semihosting",
                "--json",
                "--replay",
                &replay("run-semihosting-v203.ndjson"),
            ]),
            "run",
            false,
        ),
        (
            s(&[
                "--json",
                "--core",
                "1",
                "--speed",
                "low",
                "--replay",
                &replay("dbg-h417-core1-mhartid.ndjson"),
                "dbg",
                "reg",
                "read",
                "csr:0xf14",
            ]),
            "dbg.reg",
            true,
        ),
    ];
    for fx in ["unknown", "v003", "v103", "v205", "v307", "x315"] {
        c.push((
            s(&[
                "target",
                "info",
                "--json",
                "--replay",
                &replay(&format!("target-info-{fx}.ndjson")),
            ]),
            "target.info",
            true,
        ));
    }
    for (op, cmd) in [
        ("halt", "dbg.halt"),
        ("resume", "dbg.resume"),
        ("step", "dbg.step"),
    ] {
        let mut a = s(&[
            "--json",
            "--core",
            "1",
            "--speed",
            "low",
            "--replay",
            &replay(&format!("dbg-h417-core1-{op}.ndjson")),
            "dbg",
            op,
        ]);
        if op == "step" {
            a.push("1".to_owned());
        }
        c.push((a, cmd, true));
    }
    c
}

#[test]
fn every_device_free_output_matches_the_schema() {
    for (args, cmd, ok) in cases() {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let (v, stderr) = stdout_json(&a);
        common::assert_envelope(&v);
        assert_eq!(v["cmd"], cmd, "{args:?}");
        assert_eq!(v["ok"], ok, "{args:?}: {v}\n{stderr}");
        assert!(!stderr.contains("diverged"), "{args:?}: {stderr}");
    }
}

/// The `cmd` names `canonical_name` (cli/src/main.rs) gives.
fn command_names() -> BTreeSet<String> {
    let src = std::fs::read_to_string(root("cli/src/main.rs")).unwrap();
    let body = src
        .split("pub(crate) fn canonical_name")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    body.lines()
        .filter_map(|l| l.split("=> \"").nth(1))
        .map(|r| r.split('"').next().unwrap().to_owned())
        .collect()
}

#[test]
fn every_command_with_a_result_has_a_schema() {
    // Commands that print no `result`: gdb / broker serve / the arduino tools serve a protocol,
    // complete prints a script, isp reset only says ok.
    const NO_RESULT: &[&str] = &[
        "arduino.discovery",
        "arduino.monitor",
        "broker.serve",
        "complete",
        "gdb",
        "isp.reset",
    ];
    let doc = schema("commands.schema.json");
    let dispatched: BTreeSet<String> = doc["allOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let c = a["if"]["properties"]["cmd"]["const"].as_str().unwrap();
            assert_eq!(
                a["then"]["properties"]["result"]["$ref"],
                format!("#/$defs/{c}"),
                "{c}"
            );
            assert!(doc["$defs"].get(c).is_some(), "no $defs entry for {c}");
            c.to_owned()
        })
        .collect();
    let want: BTreeSet<String> = command_names()
        .into_iter()
        .filter(|c| !NO_RESULT.contains(&c.as_str()))
        .collect();
    assert_eq!(dispatched, want);
    assert_eq!(dispatched.len(), 50);
    // The envelope schema reaches the per-command one.
    let env = schema("result.schema.json");
    assert!(
        env["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["$ref"] == "commands.schema.json")
    );
}

#[test]
fn flash_results_share_one_shape() {
    // docs/freeze-decisions.ja.md §7: result.flash, whatever the writer.
    for (args, cmd, _) in cases() {
        if !cmd.ends_with("flash") {
            continue;
        }
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let (v, _) = stdout_json(&a);
        let f = &v["result"]["flash"];
        for key in [
            "bytes",
            "family",
            "programmer",
            "skipped",
            "scope",
            "verified",
            "running",
        ] {
            assert!(f.get(key).is_some(), "{cmd}: no `{key}` in {v}");
        }
        assert!(f["verified"].is_boolean() || f["verified"].is_null(), "{v}");
        let programmer = match cmd {
            "flash" => "stub",
            "boot.hid.flash" => "hid",
            "isp.flash" => "isp",
            other => panic!("{other}"),
        };
        assert_eq!(f["programmer"], programmer, "{v}");
    }
}

#[test]
fn progress_events_match_the_schema() {
    let (_, stderr) = stdout_json(&[
        "flash",
        &input("runtest-ch32v307.bin"),
        "--replay",
        &replay("flash-v307.ndjson"),
        "--json",
        "--progress",
        "ndjson",
    ]);
    let events: Vec<Value> = stderr
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    assert!(!events.is_empty(), "no events on stderr: {stderr}");
    for e in &events {
        common::assert_event(e);
    }
    // `ev` is open: a reader skips a kind it does not know, and the schema lets it through.
    common::assert_event(&serde_json::json!({"ev": "something-new", "x": 1}));
    let bad = serde_json::json!({"ev": "progress", "done": 1});
    assert!(!check("events.schema.json", &bad).is_empty());
}

#[test]
fn probe_list_entries_match_the_schema() {
    // `probe list` opens the probes it finds, so the test does not run it (a bench may be
    // attached): these are its entries as printed on the bench, 2026-10-01.
    let good = serde_json::json!({"contract": CONTRACT, "ok": true, "cmd": "probe.list", "result": {"probes": [
        {"kind": "wchlink", "serial": "497F8F06CE2F", "usb": "1a86:8010", "topology": "3-4",
         "ports": ["/dev/ttyACM12"], "model": "WCH-LinkE", "variant": "linke", "mode": "riscv",
         "firmware": {"raw": "0216", "norm": "2.22", "wch": "v42", "known_bad": false, "mode": "riscv"}},
        {"kind": "oep", "serial": "30eda0e31108", "usb": "303a:0002", "topology": "3-6",
         "ports": ["/dev/ttyACM13"], "model": "OEP probe (ESP32-P4)", "firmware": "0.0.40+2c4b396"}
    ], "network": []}});
    common::assert_envelope(&good);
    let mut bad = good.clone();
    bad["result"]["probes"][1]["kind"] = serde_json::json!("dap");
    let errs = check("result.schema.json", &bad);
    assert!(errs.iter().any(|e| e.contains("kind")), "{errs:?}");
    // monitor list has `probes` of its own shape: the probe.list rule does not reach it.
    let other = serde_json::json!({"contract": CONTRACT, "ok": true, "cmd": "monitor.list",
        "result": {"probes": [{"probe": "497F8F06CE2F", "mode": "riscv", "cdc_ports": []}]}});
    common::assert_envelope(&other);
}

#[test]
fn error_codes_kinds_and_warnings_are_open() {
    // B-3 / B-4: an exit code, error kind or warning code added later still validates.
    let v = serde_json::json!({"contract": CONTRACT, "ok": false, "cmd": "flash",
        "error": {"code": 63, "kind": "something-new", "msg": "x"},
        "warnings": [{"code": "not-yet-named", "msg": "y"}]});
    common::assert_envelope(&v);
    for code in [60, 61, 62] {
        let mut e = v.clone();
        e["error"]["code"] = serde_json::json!(code);
        common::assert_envelope(&e);
    }
}

#[test]
fn the_validator_catches_a_wrong_envelope() {
    // Guards the checker itself: an old contract version, a missing `ok` and a flash result
    // without `verified` must all be found.
    let bad = serde_json::json!({"contract": "4", "cmd": "flash",
        "result": {"flash": {"bytes": 1, "family": null, "programmer": "stub", "running": null,
                             "skipped": false, "scope": "chip"}}});
    let errs = check("result.schema.json", &bad);
    assert!(errs.iter().any(|e| e.contains("const")), "{errs:?}");
    assert!(errs.iter().any(|e| e.contains("missing `ok`")), "{errs:?}");
    assert!(
        errs.iter().any(|e| e.contains("missing `verified`")),
        "{errs:?}"
    );
}

#[test]
fn a_warning_goes_into_the_envelope_with_json() {
    // B-4: a warning raised outside a command's own list (here a config file that does not
    // parse) is an entry of `warnings[]` under --json, and `warning[code]: msg` on stderr without.
    let dir = std::env::temp_dir().join(format!("ch32rv-contract-warn-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ch32rv.toml"), "this is = = not toml").unwrap();
    let run = |json: bool| {
        let mut c = Command::new(bin());
        c.current_dir(&dir).arg("version");
        if json {
            c.arg("--json");
        }
        c.output().unwrap()
    };
    let out = run(true);
    let v = common::envelope(&out.stdout);
    assert_eq!(v["ok"], true, "{v}");
    assert!(
        v["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|w| w["code"] == "config-parse")),
        "{v}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("config-parse"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(false);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("warning[config-parse]"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
