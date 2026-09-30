//! en: In-repo tasks (`cargo xtask <task>`). `db-gen` generates `crates/target/generated/` from
//! the pinned `ch32-device-data` repo (docs/architecture.ja.md §3). The generated file is committed;
//! the target crate loads it with `include_str!`, so the build never depends on a neighbour repo.
//!
//! ja: repo 内タスク。`db-gen` は pinned な `ch32-device-data` から `crates/target/generated/` を
//! 生成する(§3)。生成物は commit し、target crate は `include_str!` で読むのでビルドは隣接 repo に
//! 依存しない。

mod oep;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const OUT_DIR: &str = "crates/target/generated";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let task = args.next();
    // Data repo path: arg, else CH32_DEVICE_DATA env, else the sibling checkout.
    let data_arg = args.next().map(PathBuf::from);
    let data = data_arg
        .clone()
        .or_else(|| std::env::var_os("CH32_DEVICE_DATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("../ch32-device-data"));
    // oep-gen / oep-check take the oep-spec checkout: arg, else OEP_SPEC env, else the sibling.
    // `--worktree` (anywhere after the task) reads uncommitted registry edits instead of HEAD.
    let rest: Vec<String> = std::env::args().skip(2).collect();
    let source = if rest.iter().any(|a| a == "--worktree") {
        oep::Source::Worktree
    } else {
        oep::Source::Head
    };
    let spec = || {
        rest.iter()
            .find(|a| !a.starts_with("--"))
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("OEP_SPEC").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("../../dev_oep/oep-spec"))
    };
    match task.as_deref() {
        Some("oep-gen") => run(oep::write(&spec(), source)),
        Some("oep-check") => run(oep::check(&spec(), source)),
        Some("loader-gen") => run(oep::loader_write()),
        Some("loader-check") => run(oep::loader_check()),
        Some("db-gen") => run(db_write(&data)),
        Some("db-check") => run(db_check(&data)),
        _ => {
            eprintln!(
                "usage: cargo xtask <task> [DATA_DIR]\n\ntasks:\n  db-gen     generate crates/target/generated/ from ch32-device-data\n  db-check   verify the committed generated files match a fresh generation (CI)\n  oep-gen    generate crates/oep/src/registry.rs from oep-spec's committed HEAD (DIR default: $OEP_SPEC or\n             ../../dev_oep/oep-spec; --worktree reads uncommitted edits)\n  oep-check  verify the committed registry.rs matches the registry (CI)\n  loader-gen   assemble crates/flash/loader/ch32_loader.S ($CH32_GCC_BIN or ArduinoCore-CH32's xpack gcc)\n  loader-check verify the committed loader binary matches its source\n\n(DATA_DIR default: $CH32_DEVICE_DATA or ../ch32-device-data)"
            );
            ExitCode::from(2)
        }
    }
}

fn run(r: Result<String, String>) -> ExitCode {
    match r {
        Ok(msg) => {
            println!("{msg}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::from(1)
        }
    }
}

/// `db-gen`: generate and write the files.
fn db_write(data: &Path) -> Result<String, String> {
    let files = generate(data)?;
    let out_dir = Path::new(OUT_DIR);
    std::fs::create_dir_all(out_dir).map_err(|e| format!("create {out_dir:?}: {e}"))?;
    for (name, content) in &files {
        let path = out_dir.join(name);
        std::fs::write(&path, content).map_err(|e| format!("write {path:?}: {e}"))?;
    }
    Ok(format!(
        "wrote {} files to {OUT_DIR}: {}",
        files.len(),
        files.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
    ))
}

/// `db-check`: regenerate in memory and compare the DATA against the committed files (fail-closed on
/// drift). The `#`-comment provenance header (which carries the data-repo rev) is ignored, so an
/// unrelated bump of ch32-device-data's HEAD with identical data does not trip the check.
fn db_check(data: &Path) -> Result<String, String> {
    let files = generate(data)?;
    let out_dir = Path::new(OUT_DIR);
    let mut stale = Vec::new();
    for (name, content) in &files {
        let path = out_dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(on_disk) if data_rows(&on_disk) == data_rows(content) => {}
            Ok(_) => stale.push(format!("{name} (differs)")),
            Err(_) => stale.push(format!("{name} (missing)")),
        }
    }
    if stale.is_empty() {
        Ok(format!(
            "up to date: {} generated files' data match ch32-device-data",
            files.len()
        ))
    } else {
        Err(format!(
            "generated files are stale - run `cargo xtask db-gen`: {}",
            stale.join(", ")
        ))
    }
}

/// The data (non-comment, non-blank) lines of a generated file.
fn data_rows(s: &str) -> Vec<&str> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// en: One table of ch32-device-data's public surface, `index/<name>.csv` (its consumer contract,
/// index/README.md "Contract for consumers": only `index/` is read, columns by name, the table is
/// pinned by the repo commit plus its `index/manifest.csv` sha256, and `index/VERSION` moves
/// before a column is removed / renamed / reformatted).
/// ja: ch32-device-data の公開面の表 1 つ(`index/<name>.csv`)。読むのは `index/` だけ、列は名前で、
/// 固定は commit と manifest の sha256、`index/VERSION` は列の削除・改名・書き方の変更の前に上がる。
struct Table {
    name: &'static str,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    sha256: String,
}

/// The `index/VERSION` this generator is written against.
const INDEX_VERSION: &str = "1";

impl Table {
    fn open(data: &Path, name: &'static str) -> Result<Table, String> {
        let version_path = data.join("index/VERSION");
        let version = std::fs::read_to_string(&version_path)
            .map_err(|e| format!("read {version_path:?}: {e}"))?;
        if version.trim() != INDEX_VERSION {
            return Err(format!(
                "ch32-device-data index/VERSION is {} (db-gen reads version {INDEX_VERSION}): \
                 a column was removed, renamed or reformatted - update the generator",
                version.trim()
            ));
        }
        let path = data.join(format!("index/{name}.csv"));
        let manifest = read_csv(&data.join("index/manifest.csv"))?;
        let sha256 = manifest
            .iter()
            .find(|r| r.first().map(String::as_str) == Some(&format!("{name}.csv")))
            .and_then(|r| r.get(2).cloned())
            .unwrap_or_default();
        Ok(Table {
            name,
            header: csv_header(&path)?,
            rows: read_csv(&path)?,
            sha256,
        })
    }

    /// The column index of `col`, or an error naming the table.
    fn col(&self, col: &str) -> Result<usize, String> {
        self.header
            .iter()
            .position(|h| h == col)
            .ok_or_else(|| format!("index/{}.csv: no column `{col}`", self.name))
    }

    /// The cell of `row` at column index `i` (trimmed; empty when missing).
    fn cell(row: &[String], i: usize) -> &str {
        row.get(i).map(|s| s.trim()).unwrap_or("")
    }

    /// `index/<name>.csv sha256:<first 12>` for a generated file's source line.
    fn provenance(&self) -> String {
        format!(
            "index/{}.csv sha256:{}",
            self.name,
            &self.sha256[..self.sha256.len().min(12)]
        )
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

/// Build the generated files' contents (no writes): a list of `(filename, content)`.
fn generate(data: &Path) -> Result<Vec<(&'static str, String)>, String> {
    let ids = Table::open(data, "device_ids")?;
    let parts = Table::open(data, "parts")?;
    let (p_pn, p_series, p_family, p_flash, p_sram) = (
        parts.col("part_number")?,
        parts.col("series")?,
        parts.col("family")?,
        parts.col("flash_bytes")?,
        parts.col("sram_bytes")?,
    );
    let (i_pn, i_id, i_addr, i_dc) = (
        ids.col("part_number")?,
        ids.col("device_id")?,
        ids.col("id_addr")?,
        ids.col("dont_care_bits")?,
    );
    let cell = Table::cell;

    // Join device_ids with parts (series, family, geometry). Skip blank / all-zero ids.
    let mut skus: BTreeMap<String, Sku> = BTreeMap::new();
    for row in &ids.rows {
        let pn = cell(row, i_pn);
        let did = cell(row, i_id);
        if pn.is_empty() || did.is_empty() || did.eq_ignore_ascii_case("0x00000000") {
            continue;
        }
        // Every delivered row uses don't-care bits [7:4]; the resolver hard-codes that mask, so
        // reject anything else rather than silently generating a record it cannot match.
        let dc = cell(row, i_dc);
        if dc != "[7:4]" {
            return Err(format!(
                "{pn}: unexpected dont_care_bits {dc:?} (expected [7:4])"
            ));
        }
        let device_id = parse_hex_u32(did).ok_or_else(|| format!("{pn}: bad device_id {did:?}"))?;
        let addr = cell(row, i_addr);
        let id_addr = parse_hex_u32(addr).ok_or_else(|| format!("{pn}: bad id_addr {addr:?}"))?;
        let part_row = parts.rows.iter().find(|r| cell(r, p_pn) == pn);
        let num = |i: usize| {
            part_row
                .map(|r| cell(r, i))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        // "family" groups several series; "series" keys the debug wiring. Fall back to the
        // part-number series prefix when parts.csv lacks the row.
        let text = |i: usize| {
            part_row
                .map(|r| cell(r, i).to_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| series_prefix(pn))
        };
        skus.insert(
            pn.to_owned(),
            Sku {
                family: text(p_family),
                series: text(p_series),
                device_id,
                id_addr,
                flash_bytes: num(p_flash),
                sram_bytes: num(p_sram),
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
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({} + {})\n# verified = this project confirmed the device_id on real silicon (docs/data-requests/measured/)\n# columns: sku,family,series,device_id,id_addr,flash_bytes,sram_bytes,verified\n",
        ids.provenance(),
        parts.provenance()
    ));
    for (pn, s) in &skus {
        let verified = MEASURED.contains(&pn.as_str());
        out.push_str(&format!(
            "{pn},{},{},0x{:08x},0x{:08x},{},{},{}\n",
            s.family, s.series, s.device_id, s.id_addr, s.flash_bytes, s.sram_bytes, verified
        ));
    }

    let _ = n; // (SKU count is reflected in the file itself)
    Ok(vec![
        ("skus.csv", out),
        ("option_fields.csv", gen_option_fields(data, &rev)?.0),
        ("debug_wiring.csv", gen_debug_wiring(data, &rev)?.0),
        ("flash_geometry.csv", gen_flash_geometry(data, &rev)?.0),
        (
            "flash_program_method.csv",
            gen_flash_program_method(data, &rev)?.0,
        ),
        ("option_bytes.csv", gen_option_bytes(data, &rev)?.0),
    ])
}

/// Generate the per-family flash geometry (erase/program granularities + the erased-cell read
/// value) from `index/flash_geometry.csv`. `fast_erase_bytes` is the granularity `erase --range`
/// / flash software breakpoints use. `erased_word` is the value a blank word reads back as, taken
/// from `blank_check_word` (WCH's own IAP blank check, already normalised to 32 bits) and falling
/// back to the RM's `erased_read_word` (which group A states as the byte `0xFF`).
fn gen_flash_geometry(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "flash_geometry")?;
    let (family_i, page_i, fast_i, prog_i, block_i, blank_i, erased_i) = (
        t.col("family")?,
        t.col("page_erase_bytes")?,
        t.col("fast_erase_bytes")?,
        t.col("fast_program_bytes")?,
        t.col("block_erase_bytes")?,
        t.col("blank_check_word")?,
        t.col("erased_read_word")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({})\n# columns: family,page_erase,fast_erase,fast_program,block_erase,erased_word (0 = not applicable; erased_word empty = unknown)\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let family = Table::cell(row, family_i);
        if family.is_empty() {
            continue;
        }
        let num = |i: usize| Table::cell(row, i).parse::<u32>().unwrap_or(0);
        // `blank_check_word` is already a 32-bit value; `erased_read_word` follows the RM, which
        // writes group A's word as `0xFF` - widen that to the word it means.
        let erased = parse_hex_u32(Table::cell(row, blank_i))
            .or_else(|| match parse_hex_u32(Table::cell(row, erased_i)) {
                Some(0xFF) => Some(0xFFFF_FFFF),
                other => other,
            })
            .map(|w| format!("0x{w:08x}"))
            .unwrap_or_default();
        out.push_str(&format!(
            "{family},{},{},{},{},{erased}\n",
            num(page_i),
            num(fast_i),
            num(prog_i),
            num(block_i)
        ));
        n += 1;
    }
    Ok((out, n))
}

/// Generate the per-family FLASH-controller programming procedure from
/// `index/flash_program_method.csv` (R-32). The RM/EVT prose is reduced to what the controller
/// driver needs to branch on: `buffered` (FTPG + BUFRST/BUFLOAD, then STRT) vs `direct`
/// (FTPG, then PG_STRT), plus the width of one buffer load. Rows the data repo marks `conflict`
/// are emitted with that confidence so the consumer can fail closed.
fn gen_flash_program_method(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "flash_program_method")?;
    let (family_i, method_i, commit_i, bits_i, conf_i) = (
        t.col("family")?,
        t.col("program_method")?,
        t.col("program_commit")?,
        t.col("program_buffer_load_bits")?,
        t.col("confidence")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({})\n# columns: family,mode,commit,buffer_load_bits,confidence\n#   mode: buffered (FTPG + BUFRST/BUFLOAD, then STRT) | direct (FTPG, then PG_STRT)\n#   buffer_load_bits: width of one FLASH_BufLoad; a word-at-a-time DMI writer needs 32.\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let family = Table::cell(row, family_i);
        if family.is_empty() {
            continue;
        }
        let method = Table::cell(row, method_i);
        let mode = if method.contains("buffer writes") {
            "buffered"
        } else if method.contains("direct writes") {
            "direct"
        } else {
            "" // not classified (e.g. the H417 RM/EVT conflict) - the consumer fails closed
        };
        // `STRT (bit6)` / `PG_STRT (bit21)` -> the bit name alone.
        let commit = Table::cell(row, commit_i)
            .split_whitespace()
            .next()
            .unwrap_or("");
        out.push_str(&format!(
            "{family},{mode},{commit},{},{}\n",
            Table::cell(row, bits_i),
            Table::cell(row, conf_i)
        ));
        n += 1;
    }
    Ok((out, n))
}

/// en: Generate the per-series debug-wiring table from `index/debug_interfaces.csv`. `wire` comes
/// from `debug_if`: swio -> 1-wire, rvswd -> 2-wire, both -> 1-or-2-wire.
/// ja: series ごとの配線の表を `index/debug_interfaces.csv` から作る。`wire` は `debug_if` から。
fn gen_debug_wiring(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "debug_interfaces")?;
    let (series_i, if_i, dio_i, clk_i) = (
        t.col("series")?,
        t.col("debug_if")?,
        t.col("swdio_pads")?,
        t.col("swclk_pads")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({})\n# columns: series,wire,swdio,swclk\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let wire = match Table::cell(row, if_i) {
            "swio" => "1-wire",
            "rvswd" => "2-wire",
            "both" => "1-or-2-wire",
            _ => continue,
        };
        let clk = Table::cell(row, clk_i);
        let swclk = if clk.is_empty() { "-" } else { clk };
        out.push_str(&format!(
            "{},{wire},{},{swclk}\n",
            Table::cell(row, series_i),
            Table::cell(row, dio_i)
        ));
        n += 1;
    }
    Ok((out, n))
}

/// Generate the per-family option-byte block location from `index/option_bytes.csv` (R-30):
/// the block's base is the byte at offset 0 (RDPR), and the write unit says how it is written.
fn gen_option_bytes(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "option_bytes")?;
    let (family_i, addr_i, off_i, unit_i) = (
        t.col("family")?,
        t.col("address")?,
        t.col("offset")?,
        t.col("write_unit")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({}, offset 0x00 row)\n# columns: family,base,write_method\n#   write_method: obpg (half-word, FLASH_CTLR.OPTPG) | ftpg (fast page, 32-bit buffer writes)\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let family = Table::cell(row, family_i);
        if family.is_empty() || Table::cell(row, off_i) != "0x00" {
            continue;
        }
        let Some(base) = parse_hex_u32(Table::cell(row, addr_i)) else {
            return Err(format!(
                "{family}: option-byte row 0x00 has no usable address"
            ));
        };
        let unit = Table::cell(row, unit_i);
        let method = if unit.contains("OBPG") {
            "obpg"
        } else if unit.contains("FTPG") {
            "ftpg"
        } else {
            ""
        };
        out.push_str(&format!("{family},0x{base:08x},{method}\n"));
        n += 1;
    }
    Ok((out, n))
}

/// Generate the per-family USER-byte option field table from `index/option_byte_fields.csv`.
/// Only the named single-bit USER fields are emitted (Reserved bits are skipped); these drive the
/// structured decode in `target option get`.
fn gen_option_fields(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "option_byte_fields")?;
    let (family_i, byte_i, bits_i, field_i, default_i) = (
        t.col("family")?,
        t.col("byte")?,
        t.col("bits")?,
        t.col("field")?,
        t.col("default")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({}, byte=USER)\n# columns: family,bit,field,default\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let field = Table::cell(row, field_i);
        if Table::cell(row, byte_i) != "USER" || field.is_empty() || field == "Reserved" {
            continue;
        }
        // Only single-bit fields are decoded (a `[hi:lo]` multi-bit field is skipped for now).
        let Ok(bit) = Table::cell(row, bits_i).parse::<u8>() else {
            continue;
        };
        let def = Table::cell(row, default_i).parse::<u8>().unwrap_or(0);
        out.push_str(&format!(
            "{},{bit},{field},{def}\n",
            Table::cell(row, family_i)
        ));
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
    "CH32V006K8U6",
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

/// The header row's field names of a CSV (so a generator can look columns up by name and keep
/// working when the data repo adds columns).
fn csv_header(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path:?}: {e}"))?;
    let line = text
        .lines()
        .next()
        .ok_or_else(|| format!("{path:?}: empty file"))?;
    Ok(split_csv_line(line)
        .into_iter()
        .map(|f| f.trim().to_owned())
        .collect())
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
