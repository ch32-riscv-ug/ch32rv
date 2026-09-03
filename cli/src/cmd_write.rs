//! en: `write` (docs/cli.ja.md §4.1, advanced): a raw memory/flash write. RAM/peripheral addresses
//! go straight through the Debug Module (`write_mem`); a flash address does a page read-modify-write
//! via the direct FLASH controller. Always verifies by reading back. This is the expert escape
//! hatch - `flash` is the image-oriented path.
//! ja: `write`(上級)。raw なメモリ/flash 書き込み。RAM/peripheral は DM 直書き、flash は直接 FLASH
//! controller で page read-modify-write。書込後 readback で検証。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ResultEnvelope};
use ch32rv_flash::Segment;

use crate::args::{Cli, WriteArgs, WriteErase};
use crate::cmd_flash::{covered_pages, overlay_page};
use crate::cmd_probe::{fail, mode_str, select_entry};
use crate::parse;
use crate::session::Session;

const FLASH_BASE: u32 = 0x0800_0000;
const FLASH_END: u32 = 0x0810_0000;

pub fn write(cli: &Cli, args: &WriteArgs) -> ExitCode {
    const CMD: &str = "write";
    let bytes = match parse_source(&args.source) {
        Ok(b) => b,
        Err(m) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                m,
                Some("source: <file> | hex:<bytes> | word:<u32>"),
            );
        }
    };
    if bytes.is_empty() {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "nothing to write (empty source)".to_owned(),
            None,
        );
    }
    let addr = match parse_at(&args.at) {
        Ok(a) => a,
        Err(m) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                m,
                Some("--at <addr> | code[+off] | ram[+off]"),
            );
        }
    };
    let len = bytes.len() as u32;

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; writing needs RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let mut session = match Session::attach(
        &entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(e) => return crate::cmd_probe::session_error(cli, CMD, e),
    };

    let is_flash = (FLASH_BASE..FLASH_END).contains(&addr);
    if !is_flash && addr < 0x0010_0000 {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!(
                "0x{addr:08x} is the low flash alias; write to the physical flash address 0x{:08x} instead",
                FLASH_BASE + addr
            ),
            None,
        );
    }

    let family_byte = session.attach.family_byte;
    let mut dm = session.dm();
    if let Err(e) = dm.halt() {
        return fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }

    let result = if is_flash {
        write_flash(&mut dm, family_byte, addr, &bytes, args.erase)
    } else {
        // RAM / peripheral: straight DM write. --erase has no meaning here.
        dm.write_mem(addr, &bytes).map_err(|e| e.to_string())
    };
    if let Err(e) = result {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("write failed: {e}"),
            None,
        );
    }

    // Verify by reading back.
    let verified = match dm.read_mem(addr, len) {
        Ok(rb) => rb == bytes,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("verify read failed: {e}"),
                None,
            );
        }
    };
    if !verified {
        return fail(
            cli,
            CMD,
            ErrorKind::VerifyMismatch,
            format!("write to 0x{addr:08x} did not verify (readback differs)"),
            if is_flash && matches!(args.erase, WriteErase::None) {
                Some("flash needs erased cells: retry with --erase auto")
            } else {
                None
            },
        );
    }

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "addr": format!("0x{addr:08x}"),
            "len": len,
            "region": if is_flash { "flash" } else { "memory" },
            "verified": true,
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        println!(
            "wrote {len} byte(s) to 0x{addr:08x} ({}) - verified",
            if is_flash { "flash" } else { "memory" }
        );
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

/// Page read-modify-write into flash via the direct controller.
fn write_flash(
    dm: &mut ch32rv_dmi::DebugModule<'_, ch32rv_wchlink::WchLink>,
    family_byte: u8,
    addr: u32,
    bytes: &[u8],
    erase: WriteErase,
) -> Result<(), String> {
    let profile = ch32rv_flash::flash_controller_profile(family_byte)
        .ok_or_else(|| format!("no FLASH-controller profile for family 0x{family_byte:02x}"))?;
    let page = profile.page_size;
    let pages = covered_pages([(addr, bytes.len() as u32)], page);
    let seg = [Segment {
        addr,
        data: bytes.to_vec(),
    }];
    for pg in pages {
        // Read the current page, overlay the new bytes, (erase,) reprogram the whole page.
        let mut buf = dm.read_mem(pg, page).map_err(|e| e.to_string())?;
        buf.resize(page as usize, 0xff);
        overlay_page(pg, &mut buf, &seg);
        if matches!(erase, WriteErase::Auto) {
            dm.flash_page_erase(pg, profile.mode)
                .map_err(|e| e.to_string())?;
        }
        dm.flash_program_page(pg, &buf, profile.mode)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Parse the `<file> | hex:<bytes> | word:<u32>` source into bytes.
fn parse_source(s: &str) -> Result<Vec<u8>, String> {
    if let Some(w) = s.strip_prefix("word:") {
        let v = parse_u32(w).ok_or_else(|| format!("bad word {w:?}"))?;
        return Ok(v.to_le_bytes().to_vec());
    }
    if let Some(h) = s.strip_prefix("hex:") {
        let clean: String = h
            .chars()
            .filter(|c| !c.is_whitespace() && *c != ':' && *c != '_')
            .collect();
        if !clean.len().is_multiple_of(2) || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("bad hex {h:?} (need an even count of hex digits)"));
        }
        return (0..clean.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| e.to_string()))
            .collect();
    }
    std::fs::read(s).map_err(|e| format!("read {s}: {e}"))
}

/// Parse `--at`: a raw address, or `code[+off]` / `ram[+off]`.
fn parse_at(s: &str) -> Result<u32, String> {
    let (base, off) = match s.split_once('+') {
        Some((b, o)) => (b, Some(o)),
        None => (s, None),
    };
    let base_addr = match base.trim() {
        "code" | "flash" => FLASH_BASE,
        "ram" => 0x2000_0000,
        "option" => 0x1FFF_F800,
        other => parse_u32(other).ok_or_else(|| format!("bad address {other:?}"))?,
    };
    let off = match off {
        Some(o) => parse_u32(o).ok_or_else(|| format!("bad offset {o:?}"))?,
        None => 0,
    };
    Ok(base_addr.wrapping_add(off))
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parses_word_and_hex_and_at() {
        assert_eq!(parse_source("word:0x04030201").unwrap(), vec![1, 2, 3, 4]);
        assert_eq!(
            parse_source("hex:a5 5a ff").unwrap(),
            vec![0xA5, 0x5A, 0xFF]
        );
        assert!(parse_source("hex:xyz").is_err());
        assert_eq!(parse_at("0x20000010").unwrap(), 0x2000_0010);
        assert_eq!(parse_at("code+0x100").unwrap(), 0x0800_0100);
        assert_eq!(parse_at("ram+16").unwrap(), 0x2000_0010);
    }
}
