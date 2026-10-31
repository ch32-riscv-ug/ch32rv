//! en: In-repo tasks (`cargo xtask <task>`). `db-gen` generates `crates/target/generated/` from
//! the pinned `ch32-device-data` repo (docs/architecture.ja.md §3). The generated file is committed;
//! the target crate loads it with `include_str!`, so the build never depends on a neighbour repo.
//!
//! ja: repo 内タスク。`db-gen` は pinned な `ch32-device-data` から `crates/target/generated/` を
//! 生成する(§3)。生成物は commit し、target crate は `include_str!` で読むのでビルドは隣接 repo に
//! 依存しない。

mod oep;
// The probe device -> variant ID mapping, shared with crates/target (defined once there).
#[path = "../../crates/target/src/link_variant.rs"]
mod link_variant;

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
                "usage: cargo xtask <task> [DATA_DIR]\n\ntasks:\n  db-gen     generate crates/target/generated/ from ch32-device-data\n  db-check   verify the committed generated files match a fresh generation (CI)\n  oep-gen    generate crates/oep/src/registry.rs from oep-spec's committed HEAD (DIR default: $OEP_SPEC or\n             ../../dev_oep/oep-spec; --worktree reads uncommitted edits)\n  oep-check  verify the committed registry.rs matches the registry (CI)\n  loader-gen   assemble crates/flash/loader/ch32_loader.S ($CH32_GCC_BIN or ArduinoCore-CH32RV's xpack gcc)\n  loader-check verify the committed loader binary matches its source\n\n(DATA_DIR default: $CH32_DEVICE_DATA or ../ch32-device-data)"
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

/// en: One SKU's identity + geometry, joined from device_ids and parts. A part that parts lists
/// but device_ids does not (no single official or measured id, e.g. CH32V203F6P6, whose EVT names
/// two) has no id: `--chip` still knows the name and checks its family, but the chip id never
/// resolves to it.
/// ja: SKU の識別と容量。parts にあって device_ids に無い型番(公式/実測の ID が 1 つに決まらない。
/// 例: EVT が 2 つ書く CH32V203F6P6)は ID 無し。`--chip` は名前を知り family を照合するが、ID から
/// その型番へは引かない。
struct Sku {
    family: String,
    series: String,
    device_id: Option<u32>,
    id_addr: Option<u32>,
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
                device_id: Some(device_id),
                id_addr: Some(id_addr),
                flash_bytes: num(p_flash),
                sram_bytes: num(p_sram),
            },
        );
    }

    if skus.is_empty() {
        return Err("no device_ids rows produced any SKU records".to_owned());
    }

    // Parts without an id: known by name (geometry, family), never resolved from a chip id.
    for r in &parts.rows {
        let pn = cell(r, p_pn);
        if pn.is_empty() || skus.contains_key(pn) {
            continue;
        }
        let num = |i: usize| cell(r, i).parse::<u64>().unwrap_or(0);
        let text = |i: usize| {
            Some(cell(r, i).to_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| series_prefix(pn))
        };
        skus.insert(
            pn.to_owned(),
            Sku {
                family: text(p_family),
                series: text(p_series),
                device_id: None,
                id_addr: None,
                flash_bytes: num(p_flash),
                sram_bytes: num(p_sram),
            },
        );
    }

    // Temperature grades share silicon IDs. Preserve both catalog names: the resolver
    // returns Family with candidates rather than guessing a grade. Reject other collisions.
    let mut by_masked: BTreeMap<u32, &String> = BTreeMap::new();
    for (pn, s) in &skus {
        let Some(id) = s.device_id else { continue };
        let masked = id & DONT_CARE_MASK;
        if let Some(prev) = by_masked.insert(masked, pn)
            && prev != pn
        {
            let previous = &skus[prev];
            let grade_siblings = pn.get(..pn.len() - 1) == prev.get(..prev.len() - 1)
                && pn.ends_with(['6', '7'])
                && prev.ends_with(['6', '7']);
            if grade_siblings
                && previous.family == s.family
                && previous.series == s.series
                && previous.id_addr == s.id_addr
                && previous.flash_bytes == s.flash_bytes
                && previous.sram_bytes == s.sram_bytes
            {
                continue;
            }
            return Err(format!(
                "masked device_id 0x{masked:08x} collides: {prev} and {pn}"
            ));
        }
    }

    let rev = git_rev(data).unwrap_or_else(|| "unknown".to_owned());
    let n = skus.len();
    let mut out = String::new();
    out.push_str(&format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({} + {})\n# verified = this project confirmed the device_id on real silicon (docs/data-requests/measured/)\n# an empty device_id = a part with no single known id: --chip knows it, a chip id never resolves to it\n# columns: sku,family,series,device_id,id_addr,flash_bytes,sram_bytes,verified\n",
        ids.provenance(),
        parts.provenance()
    ));
    for (pn, s) in &skus {
        let verified = MEASURED.contains(&pn.as_str());
        let hex = |v: Option<u32>| v.map(|v| format!("0x{v:08x}")).unwrap_or_default();
        out.push_str(&format!(
            "{pn},{},{},{},{},{},{},{}\n",
            s.family,
            s.series,
            hex(s.device_id),
            hex(s.id_addr),
            s.flash_bytes,
            s.sram_bytes,
            verified
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
        ("esig.csv", gen_esig(data, &rev)?.0),
        ("link_firmware.csv", gen_link_firmware(data, &rev)?.0),
        (
            "link_firmware_issues.csv",
            gen_link_firmware_issues(data, &rev)?.0,
        ),
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
    // The flash-mode columns (ch32-device-data c9ca0dd): the bit that picks the erase sizes, and
    // the sizes while it is set (CH32H417's FLASH_CFGR0.DBMODE). Empty for every other family.
    let (mode_i, page_dual_i, block_dual_i) = (
        t.col("flash_mode_bit")?,
        t.col("page_erase_bytes_dual")?,
        t.col("block_erase_bytes_dual")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({})\n# columns: family,page_erase,fast_erase,fast_program,block_erase,erased_word,mode_bit,page_erase_dual,block_erase_dual (0 = not applicable; erased_word / mode_bit empty = unknown / none; *_dual = the sizes while mode_bit is set)\n",
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
            "{family},{},{},{},{},{erased},{},{},{}\n",
            num(page_i),
            num(fast_i),
            num(prog_i),
            num(block_i),
            Table::cell(row, mode_i),
            num(page_dual_i),
            num(block_dual_i)
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
        // en: `program_method` and `program_commit` are prose columns (index/columns.csv
        // `spelling`: a rewording does not bump VERSION), so a value that does not read as
        // expected stops the generation instead of turning into a wrong mode. A row the data
        // repo marks `conflict` may stay unclassified: the consumer fails closed on it.
        // ja: 2 つとも prose の列(言い換えで VERSION は上がらない)。想定の形で読めなければ生成を
        // 止める。`conflict` の行だけは分類できなくてよい(使う側が fail-closed)。
        let conflict = Table::cell(row, conf_i) == "conflict";
        let method = Table::cell(row, method_i);
        let mode = match (
            method.contains("buffer writes"),
            method.contains("direct writes"),
        ) {
            (true, false) => "buffered",
            (false, true) => "direct",
            _ if conflict => "",
            _ => {
                return Err(format!(
                    "{family}: flash_program_method.program_method `{method}` reads as neither \
                     `buffer writes` nor `direct writes` (a prose column changed: ask \
                     ch32-device-data for a fixed column, or update db-gen)"
                ));
            }
        };
        // `STRT (bit6)` / `PG_STRT (bit21)` -> the bit name alone.
        let commit_text = Table::cell(row, commit_i);
        let commit = commit_text.split_whitespace().next().unwrap_or("");
        if !matches!(commit, "STRT" | "PG_STRT") && !conflict {
            return Err(format!(
                "{family}: flash_program_method.program_commit `{commit_text}` does not start \
                 with STRT or PG_STRT (a prose column changed)"
            ));
        }
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

/// en: Generate the per-family unique-ID address from `index/esig.csv`: the `UNIID1` row (the
/// lowest ID word; ch32rv reads UNIID1|UNIID2, the words that differ between chips). CH32M030 keeps
/// it at 0x1FFFF3A8, the others at 0x1FFFF7E8.
/// ja: family ごとの固有 ID の番地を `index/esig.csv` の `UNIID1` 行から作る。
fn gen_esig(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "esig")?;
    let (family_i, reg_i, addr_i) = (t.col("family")?, t.col("register")?, t.col("address")?);
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({}, UNIID1 rows)\n# columns: family,uid_addr\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let family = Table::cell(row, family_i);
        if family.is_empty() || Table::cell(row, reg_i) != "UNIID1" {
            continue;
        }
        let Some(addr) = parse_hex_u32(Table::cell(row, addr_i)) else {
            return Err(format!("{family}: esig UNIID1 has no usable address"));
        };
        out.push_str(&format!("{family},0x{addr:08x}\n"));
        n += 1;
    }
    Ok((out, n))
}

/// en: Generate the WCH probe firmware images WCH-LinkUtility ships from `index/link_firmware.csv`,
/// keyed by sha256, for `probe firmware update`'s image check. `device` is the probe product
/// (`WCH-LinkE`), `mode` the target architecture of a CH549 image (`RISC-V` / `ARM`, empty when the
/// probe has one firmware for both), `role` `offline` (`FIRMWARE_*.bin`, the application alone, the
/// file the IAP path writes) or `iap` (`*APP_IAP*.bin`, the IAP bootloader in front of it), and
/// `version` the `reported_version` the probe answers with. Generation stops on a role outside
/// those two, a device + mcu with no ch32rv probe variant (crates/target/src/link_variant.rs), or a sha256 that is not 64 hex digits.
/// ja: `index/link_firmware.csv` から WCH-LinkUtility 同梱の probe firmware を sha256 で引く表を作る
/// (`probe firmware update` の image 検査用)。role は `offline`(`FIRMWARE_*.bin`、application のみ。
/// IAP 経路で書くのはこれ)か `iap`(`*APP_IAP*.bin`、IAP bootloader 付き)。それ以外の role・variant に
/// 対応の無い device・64 桁の 16 進でない sha256 なら生成を止める。
fn gen_link_firmware(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "link_firmware")?;
    let (sha_i, device_i, mcu_i, mode_i, role_i, file_i, size_i, version_i) = (
        t.col("sha256")?,
        t.col("device")?,
        t.col("mcu")?,
        t.col("mode")?,
        t.col("role")?,
        t.col("file")?,
        t.col("size")?,
        t.col("reported_version")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({})\n# columns: sha256,device,mcu,mode,role,file,size,version\n#   mode: RISC-V | ARM (a CH549 WCH-Link image; empty = one firmware for both)\n#   role: offline (FIRMWARE_*.bin, the application alone - what the IAP path writes) | iap (*APP_IAP*.bin, IAP bootloader + application)\n#   version: the version the probe reports with this image\n",
        t.provenance()
    );
    let mut n = 0;
    for row in &t.rows {
        let c = |i: usize| Table::cell(row, i);
        let sha = c(sha_i).to_ascii_lowercase();
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!(
                "{}: link_firmware.sha256 `{sha}` is not a sha256",
                c(file_i)
            ));
        }
        if link_variant::variant_id(c(device_i), c(mcu_i)).is_none() {
            return Err(format!(
                "{}: link_firmware device `{}` / mcu `{}` has no ch32rv probe variant \
                 (add it to crates/target/src/link_variant.rs)",
                c(file_i),
                c(device_i),
                c(mcu_i)
            ));
        }
        let role = c(role_i);
        if !matches!(role, "offline" | "iap") {
            return Err(format!(
                "{}: link_firmware.role `{role}` is neither offline nor iap",
                c(file_i)
            ));
        }
        let mode = c(mode_i);
        if !matches!(mode, "" | "RISC-V" | "ARM") {
            return Err(format!(
                "{}: link_firmware.mode `{mode}` is neither RISC-V nor ARM",
                c(file_i)
            ));
        }
        out.push_str(&format!(
            "{sha},{},{},{mode},{role},{},{},{}\n",
            c(device_i),
            c(mcu_i),
            c(file_i),
            c(size_i),
            c(version_i)
        ));
        n += 1;
    }
    Ok((out, n))
}

/// en: Generate the probe firmware versions measured to fail from `index/link_firmware_issues.csv`
/// (`fw-known-bad`, `probe firmware info|check`, `doctor`). Each row is keyed by
/// `(device, mcu, mode, version)`; `variant` is the ch32rv probe variant ID of `device` + `mcu`
/// (crates/target/src/link_variant.rs). Generation stops on a device with no variant, a mode other
/// than `RISC-V` / `ARM`, a version that is not `major.minor`, a `wcfg_version` that is not
/// major*10 + minor, a `works_in` that is not empty or `major.minor`, an empty symptom, a basis that
/// is not `measured:...`, or a repeated key. Prose cells are CSV-quoted.
/// ja: `index/link_firmware_issues.csv` から実測で不良の probe firmware の版の表を作る。variant は
/// `device` + `mcu` の ch32rv の variant ID。対応の無い device、RISC-V / ARM 以外の mode、
/// `major.minor` でない版、版と合わない `wcfg_version`、空の symptom、`measured:` でない basis、
/// 鍵の重複で生成を止める。
fn gen_link_firmware_issues(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "link_firmware_issues")?;
    let (
        device_i,
        mcu_i,
        mode_i,
        version_i,
        wcfg_i,
        tool_i,
        target_i,
        symptom_i,
        works_i,
        conf_i,
        basis_i,
    ) = (
        t.col("device")?,
        t.col("mcu")?,
        t.col("mode")?,
        t.col("version")?,
        t.col("wcfg_version")?,
        t.col("tool")?,
        t.col("target")?,
        t.col("symptom")?,
        t.col("works_in")?,
        t.col("confidence")?,
        t.col("basis")?,
    );
    let parse_version = |v: &str| -> Option<(u8, u8)> {
        let (a, b) = v.split_once('.')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({})\n# Probe firmware versions measured to fail; ch32rv warns `fw-known-bad` and `probe firmware check` exits 12 on a match.\n# columns: variant,mode,version,wcfg_version,device,mcu,tool,target,works_in,confidence,basis,symptom\n#   variant: ch32rv probe variant ID of device + mcu (crates/target/src/link_variant.rs)\n#   mode: RISC-V | ARM, the firmware mode the failure was measured in\n#   version: the version the probe reports (major.minor); wcfg_version: the same in WCH's numbering (v<n>)\n#   tool / target: the host tool and the target part the failure was seen with\n#   works_in: the version measured to work on the same hardware (empty = none measured)\n#   basis: the measurement record (measured:<date>(<where it is recorded>)); symptom: what fails, as recorded\n",
        t.provenance()
    );
    let mut keys = std::collections::BTreeSet::new();
    let mut n = 0;
    for row in &t.rows {
        let c = |i: usize| Table::cell(row, i);
        let (device, mcu, mode, version) = (c(device_i), c(mcu_i), c(mode_i), c(version_i));
        let at = format!("link_firmware_issues {device}/{mcu}/{mode} {version}");
        let Some(variant) = link_variant::variant_id(device, mcu) else {
            return Err(format!(
                "{at}: device `{device}` / mcu `{mcu}` has no ch32rv probe variant \
                 (add it to crates/target/src/link_variant.rs)"
            ));
        };
        if !matches!(mode, "RISC-V" | "ARM") {
            return Err(format!("{at}: mode `{mode}` is neither RISC-V nor ARM"));
        }
        let Some((major, minor)) = parse_version(version) else {
            return Err(format!("{at}: version `{version}` is not major.minor"));
        };
        let wcfg = c(wcfg_i);
        if wcfg.parse::<u32>().ok() != Some(u32::from(major) * 10 + u32::from(minor)) {
            return Err(format!(
                "{at}: wcfg_version `{wcfg}` is not major*10 + minor of {version}"
            ));
        }
        let works_in = c(works_i);
        if !works_in.is_empty() && parse_version(works_in).is_none() {
            return Err(format!("{at}: works_in `{works_in}` is not major.minor"));
        }
        if c(symptom_i).is_empty() || c(tool_i).is_empty() || c(target_i).is_empty() {
            return Err(format!("{at}: symptom, tool or target is empty"));
        }
        if !c(basis_i).starts_with("measured:") {
            return Err(format!("{at}: basis `{}` is not measured:...", c(basis_i)));
        }
        if !keys.insert((device, mcu, mode, (major, minor))) {
            return Err(format!("{at}: the key appears twice"));
        }
        let fields = [
            variant,
            mode,
            version,
            wcfg,
            device,
            mcu,
            c(tool_i),
            c(target_i),
            works_in,
            c(conf_i),
            c(basis_i),
            c(symptom_i),
        ];
        let line: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
        n += 1;
    }
    Ok((out, n))
}

/// A CSV field, double-quoted when it holds a comma or a quote (quotes doubled).
fn csv_field(s: &str) -> String {
    if s.contains([',', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
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
        // A prose column (index/columns.csv `spelling`): stop on a value that names neither.
        let unit = Table::cell(row, unit_i);
        let method = match (unit.contains("OBPG"), unit.contains("FTPG")) {
            (true, false) => "obpg",
            (false, true) => "ftpg",
            _ => {
                return Err(format!(
                    "{family}: option_bytes.write_unit `{unit}` names neither OBPG nor FTPG \
                     (a prose column changed)"
                ));
            }
        };
        out.push_str(&format!("{family},0x{base:08x},{method}\n"));
        n += 1;
    }
    Ok((out, n))
}

/// en: Generate the per-family USER-byte option field table from `index/option_byte_fields.csv`.
/// The named USER fields, single-bit (`2`) and ranges (`[4:3]`), with their defaults read from
/// the as-printed spellings (`1`, `11b`, `0x3`), and the field's WCH EVT name: `option` (the key
/// `target option set` / `isp config set` take), `evt_argument` and `evt_values` as the data repo
/// writes them, plus `words`, the value words derived from the EVT constants by [`option_words`].
/// A field whose default the manual leaves open (an `x`, as Reserved's `xxxb`) or gives as prose
/// is listed only when it has an `option`, with an empty default (its bits are kept on a reset).
/// The WRPR rows are not emitted; their `option` must be `write-protect` with `1=protected` (the
/// sense `write-protect=<mask>` is written in), else generation stops. `confidence` is not read:
/// a `conflict` row (CH32M030 RST_MODE, CH32V003 / CH32V205 IWDG: RM CN and EN spell the field
/// name differently) is used like any other.
/// ja: `index/option_byte_fields.csv` から family ごとの USER の field 表を作る。field の EVT の名前
/// (`option`・`evt_argument`・`evt_values`)はそのまま、値の語(`words`)は EVT の定数から
/// [`option_words`] で作る。既定値が開いている・文で書かれた field は `option` があるときだけ
/// 既定値を空にして出す。WRPR の行は出さず、`write-protect` / `1=protected` であることだけ確かめる。
fn gen_option_fields(data: &Path, rev: &str) -> Result<(String, usize), String> {
    let t = Table::open(data, "option_byte_fields")?;
    let (family_i, byte_i, bits_i, field_i, default_i) = (
        t.col("family")?,
        t.col("byte")?,
        t.col("bits")?,
        t.col("field")?,
        t.col("default")?,
    );
    let (option_i, arg_i, values_i) = (
        t.col("option")?,
        t.col("evt_argument")?,
        t.col("evt_values")?,
    );
    let mut out = format!(
        "# GENERATED by `cargo xtask db-gen` - do not edit by hand.\n# source: ch32-device-data@{rev} ({}, byte=USER)\n# columns: family,bit,width,field,default,option,evt_argument,evt_values,words\n#   bit: the lowest bit of the field; width: its bits; default: its value (empty = the manual leaves it open; such a field is listed only when it has an option)\n#   option: the key `target option set` / `isp config set` take (empty = written as user.<field>); evt_values: <field value>=<EVT constant>;...\n#   words: <field value>=<value word>;... - the constant after the prefix every value shares, RST/EN = on, NoRST/NoEN = off, EN_DT<t> = <t>, lowercased, _ -> - (docs/cli.ja.md §4.3)\n",
        t.provenance()
    );
    let mut n = 0;
    let mut seen = std::collections::BTreeSet::new();
    for row in &t.rows {
        let family = Table::cell(row, family_i);
        let byte = Table::cell(row, byte_i);
        let option = Table::cell(row, option_i);
        if byte.starts_with("WRPR") {
            let values = Table::cell(row, values_i);
            if !option.is_empty() && (option != "write-protect" || values != "1=protected") {
                return Err(format!(
                    "option_byte_fields: {family} {byte}: option `{option}` / evt_values `{values}` - \
                     write-protect=<mask> is written as `1=protected` (FLASH_EnableWriteProtection)"
                ));
            }
            continue;
        }
        // RM CN/EN may print a range in the name (`RST_MODE[1:0]`); the bits are in `bits`.
        let raw_field = Table::cell(row, field_i);
        let field = raw_field.split_once('[').map_or(raw_field, |(f, _)| f);
        if byte != "USER" || field.is_empty() || field == "Reserved" {
            continue;
        }
        let bits = Table::cell(row, bits_i);
        let (lo, width) = match bits.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            Some(r) => {
                let Some((hi, lo)) = r.split_once(':') else {
                    return Err(format!("option_byte_fields: bits `{bits}` is not [hi:lo]"));
                };
                let (Ok(hi), Ok(lo)) = (hi.trim().parse::<u8>(), lo.trim().parse::<u8>()) else {
                    return Err(format!("option_byte_fields: bits `{bits}` is not [hi:lo]"));
                };
                if hi < lo || hi > 7 {
                    return Err(format!("option_byte_fields: bits `{bits}` out of the byte"));
                }
                (lo, hi - lo + 1)
            }
            None => match bits.parse::<u8>() {
                Ok(b) if b < 8 => (b, 1),
                _ => continue,
            },
        };
        let def = Table::cell(row, default_i);
        let value = if def.contains(['x', 'X']) && !def.starts_with("0x") {
            None // left open by the manual
        } else if let Some(h) = def.strip_prefix("0x") {
            u8::from_str_radix(h, 16).ok()
        } else if let Some(b) = def.strip_suffix('b') {
            u8::from_str_radix(b, 2).ok()
        } else {
            // A default the manual gives as prose (CH32V006 RST_MODE: one value per chip) is not
            // one number: treated like an open one, its bits are kept on a reset.
            def.parse::<u8>().ok()
        };
        let (arg, values, words) = if option.is_empty() {
            (String::new(), String::new(), String::new())
        } else {
            if !seen.insert((family.to_owned(), option.to_owned())) {
                return Err(format!(
                    "option_byte_fields: {family}: option `{option}` on two USER fields"
                ));
            }
            let arg = Table::cell(row, arg_i);
            let values = Table::cell(row, values_i);
            let words = option_words(width, values)
                .map_err(|e| format!("option_byte_fields: {family} {option}: {e}"))?;
            (arg.to_owned(), values.to_owned(), words)
        };
        if value.is_none() && option.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{family},{lo},{width},{field},{},{option},{arg},{values},{words}\n",
            value.map(|v| v.to_string()).unwrap_or_default()
        ));
        n += 1;
    }
    Ok((out, n))
}

/// en: The value words of one option, from its `evt_values` (`3=OB_RST_NoEN;2=OB_RST_EN_DT12ms;...`):
/// each constant after the longest `_`-ended prefix every value shares (`OB_RST_`), then `RST` /
/// `EN` -> `on`, `NoRST` / `NoEN` -> `off`, `EN_DT<t>` -> `<t>`, else lowercased with `_` -> `-`
/// (`OB_IWDG_SW` -> `sw`, `OB_PowerON_Start_Mode_BOOT` -> `boot`). Every field value must be named
/// once and the words must differ. Returned as `<field value>=<word>;...` in the data's order.
/// ja: 値の語を EVT の定数から作る(docs/cli.ja.md §4.3 の規則)。
fn option_words(width: u8, evt_values: &str) -> Result<String, String> {
    let mut pairs = Vec::new();
    for item in evt_values.split(';') {
        let Some((v, c)) = item.split_once('=') else {
            return Err(format!(
                "evt_values `{evt_values}` is not <value>=<constant>;..."
            ));
        };
        let v: u8 = v
            .trim()
            .parse()
            .map_err(|_| format!("evt_values `{evt_values}`: `{v}` is not a field value"))?;
        pairs.push((v, c.trim()));
    }
    let first = pairs.first().map(|p| p.1).unwrap_or_default();
    let mut prefix = first.len();
    for (_, c) in &pairs {
        prefix = prefix.min(
            first
                .bytes()
                .zip(c.bytes())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    let prefix = first[..prefix].rfind('_').map_or(0, |i| i + 1);
    let mut words: Vec<(u8, String)> = Vec::new();
    for (v, c) in &pairs {
        let rest = &c[prefix..];
        let word = match rest {
            "RST" | "EN" => "on".to_owned(),
            "NoRST" | "NoEN" => "off".to_owned(),
            _ => rest
                .strip_prefix("EN_DT")
                .unwrap_or(rest)
                .to_ascii_lowercase()
                .replace('_', "-"),
        };
        if word.is_empty() || !word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(format!("`{c}` gives no value word"));
        }
        if words.iter().any(|(w, s)| *w == *v || *s == word) {
            return Err(format!("`{c}`: value {v} or word `{word}` named twice"));
        }
        words.push((*v, word));
    }
    if words.len() != 1 << width || words.iter().any(|(v, _)| u16::from(*v) >= 1 << width) {
        return Err(format!(
            "evt_values `{evt_values}` does not name each of the {} field values once",
            1u16 << width
        ));
    }
    Ok(words
        .iter()
        .map(|(v, w)| format!("{v}={w}"))
        .collect::<Vec<_>>()
        .join(";"))
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
    "CH32V205RCT6",
    "CH32X315MCU6",
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
