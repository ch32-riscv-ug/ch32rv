//! en: Real `--json` output checked against docs/contract/*.schema.json (docs/contract/README.ja.md
//! rule 5), with no device: `version`, `db list`, an error envelope, and replays of recorded
//! sessions (flash through the WCH stub and through a HID bootloader), plus the NDJSON events a
//! replayed flash streams. The validator covers the keywords these schemas use (type, required,
//! properties, const, enum, items, if / then, oneOf, minimum); a keyword it does not know fails
//! the test, so a schema that grows one cannot be checked by mistake.
//! ja: 実際の `--json` の出力を docs/contract の schema と照合する(device なし)。照合は schema が使う
//! keyword だけを扱い、知らない keyword があれば試験を落とす(照合したつもりにならないように)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_ch32rv")
}

fn root(rel: &str) -> String {
    format!("{}/../{rel}", env!("CARGO_MANIFEST_DIR"))
}

fn schema(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root(&format!("docs/contract/{name}"))).unwrap())
        .unwrap()
}

const KNOWN: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "type",
    "required",
    "properties",
    "const",
    "enum",
    "items",
    "if",
    "then",
    "oneOf",
    "minimum",
];

/// Errors of `v` against `s` at `path` (empty = valid).
fn check(s: &Value, v: &Value, path: &str) -> Vec<String> {
    let Some(obj) = s.as_object() else {
        return Vec::new();
    };
    let mut errs = Vec::new();
    for k in obj.keys() {
        assert!(
            KNOWN.contains(&k.as_str()),
            "schema keyword `{k}` at {path} is not checked here"
        );
    }
    if let Some(t) = obj.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let ok = types.iter().any(|t| match *t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "null" => v.is_null(),
            _ => false,
        });
        if !ok {
            errs.push(format!("{path}: {v} is not {types:?}"));
            return errs;
        }
    }
    if let Some(c) = obj.get("const")
        && v != c
    {
        errs.push(format!("{path}: {v} is not the const {c}"));
    }
    if let Some(Value::Array(e)) = obj.get("enum")
        && !e.contains(v)
    {
        errs.push(format!("{path}: {v} is not one of {e:?}"));
    }
    if let (Some(m), Some(n)) = (obj.get("minimum").and_then(Value::as_f64), v.as_f64())
        && n < m
    {
        errs.push(format!("{path}: {n} < minimum {m}"));
    }
    if let (Some(Value::Array(req)), Some(o)) = (obj.get("required"), v.as_object()) {
        for r in req.iter().filter_map(Value::as_str) {
            if !o.contains_key(r) {
                errs.push(format!("{path}: missing `{r}`"));
            }
        }
    }
    if let (Some(Value::Object(props)), Some(o)) = (obj.get("properties"), v.as_object()) {
        for (k, ps) in props {
            if let Some(x) = o.get(k) {
                errs.extend(check(ps, x, &format!("{path}.{k}")));
            }
        }
    }
    if let (Some(items), Some(a)) = (obj.get("items"), v.as_array()) {
        for (i, x) in a.iter().enumerate() {
            errs.extend(check(items, x, &format!("{path}[{i}]")));
        }
    }
    if let Some(cond) = obj.get("if")
        && check(cond, v, path).is_empty()
        && let Some(then) = obj.get("then")
    {
        errs.extend(check(then, v, path));
    }
    if let Some(Value::Array(alts)) = obj.get("oneOf") {
        let passing = alts.iter().filter(|a| check(a, v, path).is_empty()).count();
        if passing != 1 {
            errs.push(format!("{path}: {passing} of the oneOf branches match {v}"));
        }
    }
    errs
}

fn stdout_json(args: &[&str]) -> (Value, String) {
    let out = Command::new(bin()).args(args).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let v =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}"));
    (v, String::from_utf8_lossy(&out.stderr).into_owned())
}

fn assert_valid(s: &Value, v: &Value, what: &str) {
    let errs = check(s, v, "$");
    assert!(errs.is_empty(), "{what}:\n{}\n{v}", errs.join("\n"));
}

#[test]
fn result_envelopes_match_the_schema() {
    let s = schema("result.schema.json");
    let flash_v307 = root("tests/fixtures/runtest-ch32v307.bin");
    let flash_rec = root("tests/fixtures/replay/flash-v307.ndjson");
    let hid_img = root("tests/fixtures/runtest-ch32v003.bin");
    let hid_rec = root("tests/fixtures/replay/hid-flash-v003.ndjson");
    let cases: Vec<Vec<&str>> = vec![
        vec!["version", "--json"],
        vec!["db", "list", "--json"],
        vec![
            "target",
            "info",
            "--probe",
            "serial:NO-SUCH-PROBE",
            "--json",
        ],
        vec!["erase", "--all", "--dry-run", "--yes", "--json"],
        vec![
            "flash",
            &flash_v307,
            "--replay",
            &flash_rec,
            "--json",
            "--progress",
            "none",
        ],
        vec![
            "boot",
            "hid",
            "flash",
            &hid_img,
            "--replay",
            &hid_rec,
            "--json",
            "--progress",
            "none",
        ],
    ];
    for args in &cases {
        let (v, _) = stdout_json(args);
        assert_valid(&s, &v, &format!("{args:?}"));
        assert_eq!(v["contract"], ch32rv_contract_version(), "{args:?}");
    }
}

#[test]
fn flash_results_share_one_shape() {
    // docs/freeze-decisions.ja.md §7: result.flash, whatever the writer.
    let flash_v307 = root("tests/fixtures/runtest-ch32v307.bin");
    let flash_rec = root("tests/fixtures/replay/flash-v307.ndjson");
    let hid_img = root("tests/fixtures/runtest-ch32v003.bin");
    let hid_rec = root("tests/fixtures/replay/hid-flash-v003.ndjson");
    for (args, programmer) in [
        (
            vec![
                "flash",
                &flash_v307,
                "--replay",
                &flash_rec,
                "--json",
                "--progress",
                "none",
            ],
            "stub",
        ),
        (
            vec![
                "boot",
                "hid",
                "flash",
                &hid_img,
                "--replay",
                &hid_rec,
                "--json",
                "--progress",
                "none",
            ],
            "hid",
        ),
    ] {
        let (v, _) = stdout_json(&args);
        let f = &v["result"]["flash"];
        assert_eq!(f["programmer"], programmer, "{v}");
        for key in ["bytes", "family", "skipped", "scope", "verified", "running"] {
            assert!(
                f.get(key).is_some() || key == "family",
                "{programmer}: no `{key}` in {v}"
            );
        }
        assert!(f["verified"].is_boolean() || f["verified"].is_null(), "{v}");
    }
}

#[test]
fn progress_events_match_the_schema() {
    let s = schema("events.schema.json");
    let flash_v307 = root("tests/fixtures/runtest-ch32v307.bin");
    let flash_rec = root("tests/fixtures/replay/flash-v307.ndjson");
    let (_, stderr) = stdout_json(&[
        "flash",
        &flash_v307,
        "--replay",
        &flash_rec,
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
        assert_valid(&s, e, "event");
    }
}

fn ch32rv_contract_version() -> &'static str {
    "4"
}

#[test]
fn the_validator_catches_a_wrong_envelope() {
    // Guards the checker itself: an old contract version and a missing `ok` must both be found.
    let s = schema("result.schema.json");
    let bad = serde_json::json!({"contract": "3", "cmd": "flash"});
    let errs = check(&s, &bad, "$");
    assert!(errs.iter().any(|e| e.contains("const")), "{errs:?}");
    assert!(errs.iter().any(|e| e.contains("missing `ok`")), "{errs:?}");
}
