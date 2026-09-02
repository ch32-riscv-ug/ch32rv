//! en: In-repo tasks (`cargo xtask <task>`). `db-gen` generates `crates/target/generated/` from
//! the pinned `ch32-device-data` repo (docs/architecture.ja.md §3). The generated file is committed;
//! the target crate loads it with `include_str!`, so the build never depends on a neighbour repo.
//!
//! ja: repo 内タスク。`db-gen` は pinned な `ch32-device-data` から `crates/target/generated/` を
//! 生成する(§3)。生成物は commit し、target crate は `include_str!` で読むのでビルドは隣接 repo に
//! 依存しない。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("db-gen") => {
            // Data repo path: arg, else CH32_DEVICE_DATA env, else the sibling checkout.
            let data = args
                .next()
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("CH32_DEVICE_DATA").map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("../ch32-device-data"));
            match db_gen(&data) {
                Ok(msg) => {
                    println!("{msg}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("xtask db-gen: {e}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <task>\n\ntasks:\n  db-gen [DATA_DIR]   generate crates/target/generated/ from ch32-device-data\n                      (DATA_DIR default: $CH32_DEVICE_DATA or ../ch32-device-data)"
            );
            ExitCode::from(2)
        }
    }
}

/// One SKU's identity + geometry, joined from device_ids and parts.
struct Sku {
    family: String,
    series: String,
    device_id: u32,
    id_addr: u32,
    flash_bytes: u64,
    sram_bytes: u64,
}

fn db_gen(data: &Path) -> Result<String, String> {
    let ids_path = data.join("evidence/device_ids.csv");
    let parts_path = data.join("index/parts.csv");
    let ids = read_csv(&ids_path)?;
    let parts = read_csv(&parts_path)?;

    // Geometry by part_number (flash_bytes, sram_bytes).
    let mut geom: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for row in &parts {
        let (Some(pn), Some(flash), Some(sram)) = (row.first(), row.get(5), row.get(6)) else {
            continue;
        };
        let flash = flash.parse::<u64>().unwrap_or(0);
        let sram = sram.parse::<u64>().unwrap_or(0);
        geom.insert(pn.clone(), (flash, sram));
    }

    // Join device_ids with geometry. Skip blank / all-zero ids. Dedup identical parts.
    let mut skus: BTreeMap<String, Sku> = BTreeMap::new();
    for row in &ids {
        let (Some(pn), Some(did), Some(addr), Some(dc)) =
            (row.first(), row.get(1), row.get(2), row.get(3))
        else {
            continue;
        };
        let did = did.trim();
        if did.is_empty() || did.eq_ignore_ascii_case("0x00000000") {
            continue;
        }
        // Every delivered row uses don't-care bits [7:4]; the resolver hard-codes that mask, so
        // reject anything else rather than silently generating a record it cannot match.
        if dc.trim() != "[7:4]" {
            return Err(format!(
                "{pn}: unexpected dont_care_bits {dc:?} (expected [7:4])"
            ));
        }
        let device_id = parse_hex_u32(did).ok_or_else(|| format!("{pn}: bad device_id {did:?}"))?;
        let id_addr = parse_hex_u32(addr).ok_or_else(|| format!("{pn}: bad id_addr {addr:?}"))?;
        let (flash_bytes, sram_bytes) = geom.get(pn).copied().unwrap_or((0, 0));
        let part_row = parts.iter().find(|r| r.first() == Some(pn));
        // "family" (parts.csv col2) groups multiple series; "series" (col1) keys debug wiring and
        // option fields. Fall back to the part-number series prefix when parts.csv lacks the row.
        let family = part_row
            .and_then(|r| r.get(2).cloned())
            .unwrap_or_else(|| series_prefix(pn));
        let series = part_row
            .and_then(|r| r.get(1).cloned())
            .unwrap_or_else(|| series_prefix(pn));
        skus.insert(
            pn.clone(),
            Sku {
                family,
                series,
                device_id,
                id_addr,
                flash_bytes,
                sram_bytes,
            },
        );
    }

    if skus.is_empty() {
        return Err("no device_ids rows produced any SKU records".to_owned());
    }

    // Sanity: after masking [7:4], each device_id must map to exactly one SKU (fail-closed on a
    // real collision so we never generate an ambiguous auto-detect table).
    let mut by_masked: BTreeMap<u32, &String> = BTreeMap::new();
    for (pn, s) in &skus {
        let masked = s.device_id & DONT_CARE_MASK;
        if let Some(prev) = by_masked.insert(masked, pn)
            && prev != pn
        {
            return Err(format!(
                "masked device_id 0x{masked:08x} collides: {prev} and {pn}"
            ));
        }
    }

    let rev = git_rev(data).unwrap_or_else(|| "unknown".to_owned());
    let n = skus.len();
    let mut out = String::new();
    out.push_str(&format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} (evidence/device_ids.csv + index/parts.csv)\n# verified = this project confirmed the device_id on real silicon (docs/data-requests/measured/)\n# columns: sku,family,series,device_id,id_addr,flash_bytes,sram_bytes,verified\n"
    ));
    for (pn, s) in &skus {
        let verified = MEASURED.contains(&pn.as_str());
        out.push_str(&format!(
            "{pn},{},{},0x{:08x},0x{:08x},{},{},{}\n",
            s.family, s.series, s.device_id, s.id_addr, s.flash_bytes, s.sram_bytes, verified
        ));
    }

    let out_dir = Path::new("crates/target/generated");
    std::fs::create_dir_all(out_dir).map_err(|e| format!("create {out_dir:?}: {e}"))?;
    let out_path = out_dir.join("skus.csv");
    std::fs::write(&out_path, &out).map_err(|e| format!("write {out_path:?}: {e}"))?;

    let (opt_out, n_opt) = gen_option_fields(data, &rev)?;
    let opt_path = out_dir.join("option_fields.csv");
    std::fs::write(&opt_path, &opt_out).map_err(|e| format!("write {opt_path:?}: {e}"))?;

    let (wire_out, n_wire) = gen_debug_wiring(data, &rev)?;
    let wire_path = out_dir.join("debug_wiring.csv");
    std::fs::write(&wire_path, &wire_out).map_err(|e| format!("write {wire_path:?}: {e}"))?;

    let (geo_out, n_geo) = gen_flash_geometry(data, &rev)?;
    let geo_path = out_dir.join("flash_geometry.csv");
    std::fs::write(&geo_path, &geo_out).map_err(|e| format!("write {geo_path:?}: {e}"))?;

    Ok(format!(
        "wrote {} ({n} SKUs), {} ({n_opt} USER fields), {} ({n_wire} series), {} ({n_geo} families) from ch32-device-data@{rev}",
        out_path.display(),
        opt_path.display(),
        wire_path.display(),
        geo_path.display()
    ))
}

/// Generate the per-family flash geometry (erase/program granularities) from
/// `evidence/flash_geometry.csv`. `fast_erase_bytes` is the granularity `erase --range` / flash
/// software breakpoints use; a cli test cross-checks it against the hard-coded controller profile.
fn gen_flash_geometry(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let rows = read_csv(&data.join("evidence/flash_geometry.csv"))?;
    let mut out = String::new();
    out.push_str(&format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} (evidence/flash_geometry.csv)\n# columns: family,page_erase,fast_erase,fast_program,block_erase (0 = not applicable)\n"
    ));
    let mut n = 0;
    for row in &rows {
        // family,page_erase_bytes,fast_erase_bytes,fast_program_bytes,block_erase_bytes,...
        let Some(family) = row.first() else {
            continue;
        };
        let num = |i: usize| {
            row.get(i)
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0)
        };
        out.push_str(&format!(
            "{family},{},{},{},{}\n",
            num(1),
            num(2),
            num(3),
            num(4)
        ));
        n += 1;
    }
    Ok((out, n))
}

/// Generate the per-series debug-wiring table from `evidence/debug_wiring.csv`. `wire` is derived:
/// no SWCLK pad -> 1-wire (SWIO); dual_support=yes -> both; otherwise 2-wire (RVSWD).
fn gen_debug_wiring(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let rows = read_csv(&data.join("evidence/debug_wiring.csv"))?;
    let mut out = String::new();
    out.push_str(&format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} (evidence/debug_wiring.csv)\n# columns: series,wire,swdio,swclk\n"
    ));
    let mut n = 0;
    for row in &rows {
        // series,swdio_pad,swclk_pad,dual_support,...
        let (Some(series), Some(swdio), Some(swclk), dual) =
            (row.first(), row.get(1), row.get(2), row.get(3))
        else {
            continue;
        };
        let dual = dual.map(|d| d.trim() == "yes").unwrap_or(false);
        let wire = if swclk.trim().is_empty() {
            "1-wire"
        } else if dual {
            "1-or-2-wire"
        } else {
            "2-wire"
        };
        let swclk = if swclk.trim().is_empty() { "-" } else { swclk };
        out.push_str(&format!("{series},{wire},{swdio},{swclk}\n"));
        n += 1;
    }
    Ok((out, n))
}

/// Generate the per-family USER-byte option field table from `evidence/option_byte_fields.csv`.
/// Only the named USER bits are emitted (Reserved bits are skipped); these drive the structured
/// decode in `target option get`.
fn gen_option_fields(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let rows = read_csv(&data.join("evidence/option_byte_fields.csv"))?;
    let mut out = String::new();
    out.push_str(&format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} (evidence/option_byte_fields.csv, byte=USER)\n# columns: family,bit,field,default\n"
    ));
    let mut n = 0;
    for row in &rows {
        // family,byte,bits,field,default,...
        let (Some(family), Some(byte), Some(bits), Some(field), Some(default)) =
            (row.first(), row.get(1), row.get(2), row.get(3), row.get(4))
        else {
            continue;
        };
        if byte != "USER" || field.is_empty() || field == "Reserved" {
            continue;
        }
        // Only single-bit fields are decoded (a `[hi:lo]` multi-bit field is skipped for now).
        let Ok(bit) = bits.parse::<u8>() else {
            continue;
        };
        let def = default.parse::<u8>().unwrap_or(0);
        out.push_str(&format!("{family},{bit},{field},{def}\n"));
        n += 1;
    }
    Ok((out, n))
}

const DONT_CARE_MASK: u32 = 0xFFFF_FF0F; // bits [7:4] are silicon revision (don't-care)

/// SKUs whose device_id this project confirmed on real silicon (docs/data-requests/measured/).
/// These are marked `verified` in the generated DB; everything else is datasheet/reference data.
const MEASURED: &[&str] = &[
    "CH32V003F4P6",
    "CH32V103R8T6",
    "CH32V203C8T6",
    "CH32V307VCT6",
    "CH32L103C8T6",
    "CH32X035C8T6",
];

/// Minimal CSV reader: returns rows of fields, honouring double-quoted fields (which may contain
/// commas). Skips the header row and blank/`#`-comment lines.
fn read_csv(path: &Path) -> Result<Vec<Vec<String>>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        rows.push(split_csv_line(line));
    }
    Ok(rows)
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => fields.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}

fn parse_hex_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u32::from_str_radix(s, 16).ok()
}

/// `CH32V003F4P6` -> `CH32V003` (letters+digits up to the first package letter after the digits).
fn series_prefix(pn: &str) -> String {
    // Keep the leading "CH32" + series letter(s) + series digits.
    let bytes = pn.as_bytes();
    let mut end = 0;
    let mut seen_digit = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            seen_digit = true;
        } else if seen_digit && b.is_ascii_alphabetic() && i > 5 {
            // package letter after the series digits
            end = i;
            break;
        }
        end = i + 1;
    }
    pn[..end].to_string()
}

fn git_rev(repo: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
