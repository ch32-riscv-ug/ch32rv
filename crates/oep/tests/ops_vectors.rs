//! en: The describe `ops` encoding (core §7.4) against oep-spec's tests/vectors/ops_encoding.json:
//! every valid value decodes to its set, every invalid one is refused. Skipped, with a note, when
//! the oep-spec checkout is not beside this one ($OEP_SPEC, default `../../dev_oep/oep-spec`).
//! ja: describe の ops の符号を oep-spec の試験のベクタで確かめる。oep-spec が無ければ skip。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ch32rv_oep::session::decode_ops;

#[test]
fn ops_encoding_vectors() {
    let spec = std::env::var_os("OEP_SPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../dev_oep/oep-spec")
        });
    let Ok(text) = std::fs::read_to_string(spec.join("tests/vectors/ops_encoding.json")) else {
        eprintln!("skip: no oep-spec ops_encoding.json at {}", spec.display());
        return;
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let cases = v["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let hex = c["value_hex"].as_str().unwrap();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let got = decode_ops(&bytes);
        if c["valid"].as_bool().unwrap() {
            let want: Vec<u8> = c["ops"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_u64().unwrap() as u8)
                .collect();
            assert_eq!(got, Some(want), "{name}");
        } else {
            assert_eq!(got, None, "{name}");
        }
    }
}
