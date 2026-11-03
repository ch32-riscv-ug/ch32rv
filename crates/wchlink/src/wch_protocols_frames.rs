//! en: What ch32rv knows of the WCH-Link protocol, checked against wch-protocols' tables: the
//! request frames it sends against `protocols/data/wchlink-commands.csv` (the source of the
//! command tables in `protocols/pc-to-link.ja.md#commands`), and its AttachChip family names
//! against `protocols/data/families.csv` (§9). The tables are read from ch32rv's own copy,
//! `crates/wchlink/wch-protocols/` (the commit in `WCH_PROTOCOLS_COMMIT`, refreshed by
//! `cargo xtask wch-protocols-sync`), so the tests never depend on a neighbour checkout.
//!
//! ja: ch32rv が送る request frame と family の名前を、wch-protocols の表の写し
//! (`crates/wchlink/wch-protocols/`。元の commit は `WCH_PROTOCOLS_COMMIT`、`cargo xtask
//! wch-protocols-sync` で更新)と突き合わせる。隣の checkout には頼らない。

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use crate::probe::{Request, Speed};

/// `(csv id or id#name, the request, the CSV's variable fields)`.
type Case = (&'static str, Request, Vec<(&'static str, u32)>);

/// The copy of wch-protocols' tables, and the commit it is.
const COMMANDS: &str = include_str!("../wch-protocols/wchlink-commands.csv");
const FAMILIES: &str = include_str!("../wch-protocols/families.csv");
const COMMIT: &str = include_str!("../wch-protocols/WCH_PROTOCOLS_COMMIT");

/// en: Minimal RFC 4180 reader: quoted fields may hold commas, `""` and newlines.
/// ja: RFC 4180 の最小限の読み取り(引用符の中の `,`・`""`・改行に対応)。
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// en: Fill a `request_frame` template: hex bytes as they are, `{name}` = one byte and
/// `{name:be32}` = four bytes big-endian, both taken from `vars`.
/// ja: `request_frame` の雛形を埋める。hex はそのまま、`{name}` は 1 byte、`{name:be32}` は BE の 4 byte。
fn fill(template: &str, vars: &BTreeMap<&str, u32>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for tok in template.split_whitespace() {
        if let Some(inner) = tok.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            let (name, ty) = inner.split_once(':').unwrap_or((inner, "u8"));
            let v = *vars
                .get(name)
                .ok_or_else(|| format!("no value for field `{name}` in `{template}`"))?;
            match ty {
                "u8" => out.push(u8::try_from(v).map_err(|e| e.to_string())?),
                "be32" => out.extend_from_slice(&v.to_be_bytes()),
                other => return Err(format!("unknown field type `{other}` in `{template}`")),
            }
        } else {
            out.push(u8::from_str_radix(tok, 16).map_err(|e| format!("`{tok}`: {e}"))?);
        }
    }
    Ok(out)
}

/// en: Each frame ch32rv sends, with the CSV `id` it must match (`id#name` where one id has rows
/// for different frames, such as on / off) and the values of the CSV's variable fields.
/// ja: ch32rv が送る frame と、一致すべき CSV の `id`(on / off のように 1 つの id に別の frame の行が
/// あるときは `id#name`)、可変 field の値。
fn cases() -> Vec<Case> {
    let mut v = vec![
        ("cmd-get-probe-info", Request::probe_info(), vec![]),
        ("cmd-attach-chip", Request::attach_chip(), vec![]),
        ("cmd-detach-chip", Request::detach_chip(), vec![]),
        ("cmd-redetect-chip", Request::redetect_chip(), vec![]),
        ("cmd-chip-info", Request::chip_info(), vec![]),
        ("cmd-rst-out#RstOut on", Request::rst_out(true), vec![]),
        ("cmd-rst-out#RstOut off", Request::rst_out(false), vec![]),
        (
            "cmd-set-power-3v3#SetPower 3V3 on",
            Request::set_power(false, true),
            vec![],
        ),
        (
            "cmd-set-power-3v3#SetPower 3V3 off",
            Request::set_power(false, false),
            vec![],
        ),
        (
            "cmd-set-power-5v#SetPower 5V on",
            Request::set_power(true, true),
            vec![],
        ),
        (
            "cmd-set-power-5v#SetPower 5V off",
            Request::set_power(true, false),
            vec![],
        ),
        ("cmd-soft-reset", Request::soft_reset(), vec![]),
        (
            "cmd-check-read-protect",
            Request::check_read_protect(),
            vec![],
        ),
        ("cmd-unprotect", Request::unprotect(), vec![]),
        ("cmd-erase-flash", Request::erase_flash(), vec![]),
        (
            "cmd-set-write-memory-region",
            Request::set_write_memory_region(0x0800_0000, 0x1234),
            vec![("addr", 0x0800_0000), ("len", 0x1234)],
        ),
        ("cmd-write-flash-op", Request::write_flash_op(), vec![]),
        (
            "cmd-write-flash-confirm",
            Request::write_flash_confirm(),
            vec![],
        ),
        ("cmd-write-flash", Request::write_flash(), vec![]),
        ("cmd-write-flash-end", Request::write_flash_end(), vec![]),
        (
            "cmd-set-read-memory-region",
            Request::set_read_memory_region(0x2000_0010, 0x4000),
            vec![("addr", 0x2000_0010), ("len", 0x4000)],
        ),
        ("cmd-read-memory", Request::read_memory(), vec![]),
        (
            "cmd-dmi-op",
            Request::dmi_op(0x10, 0x8000_0001, 2),
            vec![("addr", 0x10), ("data", 0x8000_0001), ("op", 2)],
        ),
        ("cmd-mode-to-dap", Request::mode_to_dap(), vec![]),
        ("cmd-mode-to-riscv", Request::mode_to_riscv(), vec![]),
        ("cmd-iap-entry", Request::iap_entry(), vec![]),
        (
            "cmd-erase-code-flash-power-off",
            Request::erase_code_flash_by_power_off(0x0d),
            vec![("family", 0x0d)],
        ),
        (
            "cmd-erase-code-flash-rst",
            Request::erase_code_flash_by_rst(0x0d),
            vec![("family", 0x0d)],
        ),
        ("cmd-sdi-print-enable", Request::sdi_print(true), vec![]),
        ("cmd-sdi-print-disable", Request::sdi_print(false), vec![]),
    ];
    // en: The CSV writes the speed code itself (high 01 / medium 02 / low 03).
    // ja: CSV は速度の符号そのもの(high 01 / medium 02 / low 03)を書く。
    for (speed, code) in [(Speed::High, 1), (Speed::Medium, 2), (Speed::Low, 3)] {
        v.push((
            "cmd-set-speed",
            Request::set_speed(0x0e, speed),
            vec![("family", 0x0e), ("speed", code)],
        ));
    }
    v
}

#[test]
fn request_frames_match_wch_protocols_csv() {
    let rows = parse_csv(COMMANDS);
    let header = &rows[0];
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("CSV has no `{name}` column"))
    };
    let (c_id, c_name, c_frame) = (col("id"), col("name"), col("request_frame"));

    let mut mismatches = Vec::new();
    for (key, req, vars) in cases() {
        let (id, name) = key
            .split_once('#')
            .map_or((key, None), |(i, n)| (i, Some(n)));
        let vars: BTreeMap<&str, u32> = vars.into_iter().collect();
        let ours = req.frame();
        let templates: Vec<&str> = rows[1..]
            .iter()
            .filter(|r| r.get(c_id).map(String::as_str) == Some(id))
            .filter(|r| name.is_none() || r.get(c_name).map(String::as_str) == name)
            .filter_map(|r| r.get(c_frame).map(String::as_str))
            .filter(|f| !f.is_empty())
            .collect();
        if templates.is_empty() {
            mismatches.push(format!("{key}: no request_frame in the CSV"));
            continue;
        }
        let mut expected = Vec::new();
        for t in &templates {
            match fill(t, &vars) {
                Ok(bytes) => expected.push(bytes),
                Err(e) => mismatches.push(format!("{key}: {e}")),
            }
        }
        if expected.contains(&ours) {
            eprintln!("match: {key} {ours:02x?}");
        } else {
            mismatches.push(format!(
                "{key}: ch32rv sends {ours:02x?}, CSV has {templates:?}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "frames that differ from wch-protocols {}:\n{}",
        COMMIT.trim(),
        mismatches.join("\n")
    );
}

/// en: `family_name` and wch-protocols' family table name the same bytes the same way.
/// ja: `family_name` と wch-protocols の family の表は、同じ byte に同じ名前を付ける。
#[test]
fn family_names_match_wch_protocols_csv() {
    let rows = parse_csv(FAMILIES);
    assert_eq!(rows[0], ["byte", "family", "core", "status", "note"]);
    let mut listed = Vec::new();
    for r in &rows[1..] {
        let byte = u8::from_str_radix(r[0].trim_start_matches("0x"), 16).unwrap();
        listed.push(byte);
        assert_eq!(
            crate::probe::family_name(byte),
            Some(r[1].as_str()),
            "family 0x{byte:02X} (wch-protocols {})",
            COMMIT.trim()
        );
    }
    for byte in 0..=u8::MAX {
        assert!(
            crate::probe::family_name(byte).is_none() || listed.contains(&byte),
            "family 0x{byte:02X} is named in ch32rv but not in wch-protocols {}",
            COMMIT.trim()
        );
    }
}

#[test]
fn fill_reads_the_csv_field_forms() {
    let vars = BTreeMap::from([("family", 0x0d), ("addr", 0x0800_0000)]);
    assert_eq!(
        fill("81 0d 02 0f {family}", &vars).unwrap(),
        [0x81, 0x0d, 0x02, 0x0f, 0x0d]
    );
    assert_eq!(
        fill("81 01 04 {addr:be32}", &vars).unwrap(),
        [0x81, 0x01, 0x04, 0x08, 0x00, 0x00, 0x00]
    );
    assert!(fill("81 {missing}", &vars).is_err());
    let rows = parse_csv("a,b\n\"x, \"\"y\"\"\nz\",2\n");
    assert_eq!(rows, [vec!["a", "b"], vec!["x, \"y\"\nz", "2"]]);
}
