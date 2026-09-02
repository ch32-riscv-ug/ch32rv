//! en: `db list` / `db info` (docs/cli.ja.md §4.7): inspect the generated target DB. No device is
//! needed - these read the built-in DB embedded from `crates/target/generated/` at compile time.
//! ja: `db list` / `db info`。生成済み target DB を見る。デバイス不要(コンパイル時埋め込みの内蔵 DB
//! を読むだけ)。

use std::process::ExitCode;

use ch32rv_contract::{ErrorKind, ResultEnvelope};

use crate::args::Cli;
use crate::cmd_probe::fail;

/// `db list [--family <f>] [--verified-only]`: enumerate SKUs in the built-in DB.
pub fn list(cli: &Cli, family: Option<&str>, verified_only: bool) -> ExitCode {
    let db = ch32rv_target::Db::builtin();
    let mut skus: Vec<_> = db
        .skus()
        .iter()
        .filter(|s| {
            family
                .map(|f| {
                    s.family.eq_ignore_ascii_case(f)
                        || s.sku
                            .to_ascii_uppercase()
                            .starts_with(&f.to_ascii_uppercase())
                })
                .unwrap_or(true)
        })
        .filter(|s| !verified_only || s.verified)
        .collect();
    skus.sort_by(|a, b| a.sku.cmp(&b.sku));

    if cli.json {
        let rows: Vec<_> = skus
            .iter()
            .map(|s| {
                serde_json::json!({
                    "sku": s.sku,
                    "family": s.family,
                    "device_id": s.device_id.map(|d| format!("0x{d:08x}")),
                    "flash_bytes": s.flash_bytes,
                    "sram_bytes": s.sram_bytes,
                    "verified": s.verified,
                })
            })
            .collect();
        let mut env = ResultEnvelope::success("db.list");
        env.result = Some(serde_json::json!({ "count": rows.len(), "skus": rows }));
        crate::print_envelope(&env)
    } else {
        println!(
            "{:<16} {:<10} {:<12} {:>6} {:>6}  VERIFIED",
            "SKU", "FAMILY", "DEVICE_ID", "FLASH", "SRAM"
        );
        for s in &skus {
            println!(
                "{:<16} {:<10} {:<12} {:>5}K {:>5}K  {}",
                s.sku,
                s.family,
                s.device_id
                    .map(|d| format!("0x{d:08x}"))
                    .unwrap_or_else(|| "-".to_owned()),
                s.flash_bytes / 1024,
                s.sram_bytes / 1024,
                if s.verified { "yes" } else { "" }
            );
        }
        println!("({} SKUs)", skus.len());
        ExitCode::SUCCESS
    }
}

/// `db info <SKU>`: show one SKU's DB record.
pub fn info(cli: &Cli, sku_name: &str) -> ExitCode {
    const CMD: &str = "db.info";
    let db = ch32rv_target::Db::builtin();
    let Some(s) = db
        .skus()
        .iter()
        .find(|s| s.sku.eq_ignore_ascii_case(sku_name))
    else {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("unknown SKU {sku_name:?} (not in the generated DB)"),
            Some("list known SKUs with `ch32rv db list`"),
        );
    };
    let wiring = ch32rv_target::debug_wiring(&s.series);
    let geo = ch32rv_target::flash_geometry(&s.family);
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "sku": s.sku,
            "family": s.family,
            "series": s.series,
            "device_id": s.device_id.map(|d| format!("0x{d:08x}")),
            "flash_bytes": s.flash_bytes,
            "sram_bytes": s.sram_bytes,
            "verified": s.verified,
            "provisional": s.provisional,
            "debug_wiring": wiring.as_ref().map(|w| serde_json::json!({
                "wire": w.wire, "swdio": w.swdio, "swclk": w.swclk,
            })),
            "flash_geometry": geo.map(|g| serde_json::json!({
                "page_erase": g.page_erase, "fast_erase": g.fast_erase,
                "fast_program": g.fast_program, "block_erase": g.block_erase,
            })),
        }));
        crate::print_envelope(&env)
    } else {
        println!("sku:        {}", s.sku);
        println!("family:     {}", s.family);
        println!("series:     {}", s.series);
        println!(
            "device_id:  {}  (bits [7:4] = silicon revision, masked when matching)",
            s.device_id
                .map(|d| format!("0x{d:08x}"))
                .unwrap_or_else(|| "-".to_owned())
        );
        println!("flash:      {} KiB", s.flash_bytes / 1024);
        println!("sram:       {} KiB", s.sram_bytes / 1024);
        if let Some(g) = &geo {
            let blk = if g.block_erase == 0 {
                "-".to_owned()
            } else {
                format!("{}B", g.block_erase)
            };
            println!(
                "flash geom: page-erase={}B  fast-erase={}B  fast-program={}B  block-erase={blk}",
                g.page_erase, g.fast_erase, g.fast_program
            );
        }
        if let Some(w) = &wiring {
            println!(
                "debug:      {} (SWDIO/DAT={}{})",
                w.wire,
                w.swdio,
                if w.swclk == "-" {
                    String::new()
                } else {
                    format!(", SWCLK={}", w.swclk)
                }
            );
        }
        println!(
            "verified:   {}",
            if s.verified {
                "yes (device_id confirmed on real silicon)"
            } else {
                "no (generated from datasheet/reference data)"
            }
        );
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    /// The hard-coded flash-controller page size (the FTER fast-erase granularity) must equal the
    /// DB's `fast_erase_bytes` for every family we drive - catches a divergence between the manual
    /// `flash_controller_profile` table and the RM/EVT-sourced geometry.
    #[test]
    fn controller_page_size_matches_db_fast_erase() {
        // (family_byte, DB family string)
        let cases = [
            (0x05u8, "CH32V20x"),
            (0x06, "CH32V307"),
            (0x09, "CH32V003"),
            (0x0D, "CH32X035"),
            (0x0E, "CH32L103"),
            (0x01, "CH32V103"),
        ];
        for (fb, fam) in cases {
            let profile =
                ch32rv_flash::flash_controller_profile(fb).expect("controller profile for family");
            let geo = ch32rv_target::flash_geometry(fam).expect("flash geometry for family");
            assert_eq!(
                profile.page_size, geo.fast_erase,
                "family_byte 0x{fb:02x} / {fam}: hard-coded page_size != DB fast_erase"
            );
        }
    }
}
