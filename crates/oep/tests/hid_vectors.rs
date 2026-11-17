//! en: HID reports (transports §1) against oep-spec's tests/vectors/hid_reports.json: packing the
//! messages' frames gives the reports byte for byte, and reading the reports (as a reader just
//! opened) gives the messages, lost and broken reports included. Skipped, with a note, when the
//! oep-spec checkout is not beside this one ($OEP_SPEC, default `../../dev_oep/oep-spec`).
//! ja: HID の report を oep-spec の共有ベクタで確かめる(詰め方と、欠け・壊れを含む読み方)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use ch32rv_oep::hid::{HidReader, HidWriter, ReportShape};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn list(v: &serde_json::Value) -> Vec<Vec<u8>> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| hex(x.as_str().unwrap()))
        .collect()
}

#[test]
fn hid_report_vectors() {
    let spec = std::env::var_os("OEP_SPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../dev_oep/oep-spec")
        });
    let Ok(text) = std::fs::read_to_string(spec.join("tests/vectors/hid_reports.json")) else {
        eprintln!("skip: no oep-spec hid_reports.json at {}", spec.display());
        return;
    };
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["hid_no_frame_start"].as_u64().unwrap(),
        u64::from(ch32rv_oep::registry::usb::HID_NO_FRAME_START)
    );
    let room = v["room"].as_u64().unwrap() as usize;
    let shape = ReportShape {
        id: None,
        input: room,
        output: room,
    };
    for c in v["pack"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let stream: Vec<u8> = list(&c["messages_hex"])
            .iter()
            .flat_map(|m| {
                let mut f = (m.len() as u16).to_le_bytes().to_vec();
                f.extend_from_slice(m);
                f
            })
            .collect();
        // hidapi's write takes the ID byte first (0 without IDs); the vectors start after it.
        let got: Vec<Vec<u8>> = HidWriter::new(shape)
            .pack(&stream)
            .into_iter()
            .map(|r| r[1..].to_vec())
            .collect();
        assert_eq!(got, list(&c["reports_hex"]), "{name}");
    }
    for c in v["read"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let mut r = HidReader::new(shape);
        let got: Vec<Vec<u8>> = list(&c["reports_hex"])
            .iter()
            .flat_map(|rep| r.push(rep))
            .map(|f| f[2..].to_vec())
            .collect();
        assert_eq!(got, list(&c["messages_hex"]), "{name}");
    }
}
