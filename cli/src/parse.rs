//! en: Shared argument parsing: debug speed and numeric addresses/ranges.
//! ja: 共通の引数パース: debug 速度と数値アドレス・範囲。

use ch32rv_contract::Warning;
use ch32rv_wchlink::Speed;

/// en: Parse --speed (low|medium|high|<kHz>), warning when a kHz value is rounded to a step.
/// ja: --speed をパースする。kHz 指定は段階へ丸め、丸めた事実を warning にする。
pub fn speed(s: &str) -> Result<(Speed, Vec<Warning>), String> {
    let mut warnings = Vec::new();
    let speed = match s {
        "low" => Speed::Low,
        "medium" => Speed::Medium,
        "high" => Speed::High,
        other => {
            let khz: u32 = other
                .parse()
                .map_err(|_| format!("invalid --speed `{other}` (low|medium|high|<kHz>)"))?;
            let (speed, actual) = if khz >= 6000 {
                (Speed::High, 6000)
            } else if khz >= 4000 {
                (Speed::Medium, 4000)
            } else if khz >= 400 {
                (Speed::Low, 400)
            } else {
                return Err(format!(
                    "--speed {khz} kHz is below the minimum step (400 kHz)"
                ));
            };
            if actual != khz {
                warnings.push(Warning {
                    code: "speed-rounded".to_owned(),
                    msg: format!("requested {khz} kHz rounded to the {actual} kHz step"),
                });
            }
            speed
        }
    };
    Ok((speed, warnings))
}

/// en: Parse a u32 in hex (`0x...`) or decimal.
/// ja: u32 を 16 進(`0x...`)または 10 進でパースする。
pub fn u32_addr(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let v = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
    } else {
        t.parse::<u32>()
    };
    v.map_err(|_| format!("invalid address/number `{s}` (use 0x-hex or decimal)"))
}

/// en: Parse a byte length allowing a `k`/`m` (KiB/MiB) suffix.
/// ja: `k`/`m`(KiB/MiB)接尾辞を許すバイト長をパースする。
pub fn byte_len(s: &str) -> Result<u32, String> {
    let t = s.trim().to_ascii_lowercase();
    let (num, mult) = if let Some(n) = t.strip_suffix('k') {
        (n, 1024)
    } else if let Some(n) = t.strip_suffix('m') {
        (n, 1024 * 1024)
    } else {
        (t.as_str(), 1)
    };
    let base = u32_addr(num)?;
    base.checked_mul(mult)
        .ok_or_else(|| format!("length `{s}` overflows u32"))
}

/// en: Base address of a named memory region (the single source of truth, shared by
/// `read` / `erase` / `write`). `code`/`flash` = flash base, `ram` = SRAM base, `option` = the
/// option-byte window. `system`/`eeprom` are family-specific and have no fixed base here.
/// ja: 名前付き領域の base 番地(read/erase/write 共有の唯一の出所)。`code`/`flash`=flash 先頭、
/// `ram`=SRAM 先頭、`option`=option byte 窓。`system`/`eeprom` は family 依存で固定 base 無し。
pub fn region_base(name: &str) -> Result<u32, String> {
    match name {
        "code" | "flash" => Ok(0x0800_0000),
        "ram" => Ok(0x2000_0000),
        "option" => Ok(0x1FFF_F800),
        "system" | "eeprom" => Err(format!(
            "region `{name}` is family-specific; use --range <addr>+<len>"
        )),
        other => Err(format!(
            "unknown region `{other}` (code|ram|option; system/eeprom need --range)"
        )),
    }
}

/// en: Resolve `--region <name>[+off[+len]]` to `(start, len)`. The default length is the whole
/// region: the probe's `flash_bytes` for `code`, the DB `sram_bytes` for `ram`, 16 for `option`.
/// ja: `--region <名前>[+off[+len]]` を `(start, len)` に解決。既定長は領域全体(code=probe 報告
/// flash、ram=DB SRAM、option=16)。
pub fn resolve_region(spec: &str, flash_bytes: u32, sram_bytes: u32) -> Result<(u32, u32), String> {
    let mut it = spec.split('+');
    let name = it.next().unwrap_or("");
    let base = region_base(name)?;
    let default_len = match name {
        "code" | "flash" => flash_bytes,
        "ram" => sram_bytes,
        "option" => 16,
        _ => 0,
    };
    let off = match it.next() {
        Some(s) => u32_addr(s)?,
        None => 0,
    };
    let len = match it.next() {
        Some(s) => byte_len(s)?,
        None => default_len.saturating_sub(off),
    };
    if len == 0 {
        return Err(format!(
            "region `{name}` size is unknown for this target; specify a length: --region {name}+0+<len>"
        ));
    }
    Ok((base.saturating_add(off), len))
}

/// en: Resolve a `<name>[+off]` region spec (or a raw `0x..`/decimal address) to a single address,
/// for commands that write to a point (`write --at`). Length is not implied.
/// ja: `<名前>[+off]`(または生番地)を単一番地に解決。length は含まない(`write --at` 用)。
pub fn region_or_addr(spec: &str) -> Result<u32, String> {
    // A bare number is an address; otherwise it is `<region>[+off]`.
    if let Ok(addr) = u32_addr(spec) {
        return Ok(addr);
    }
    let mut it = spec.split('+');
    let base = region_base(it.next().unwrap_or(""))?;
    let off = match it.next() {
        Some(s) => u32_addr(s)?,
        None => 0,
    };
    Ok(base.saturating_add(off))
}

/// en: Parse a range `<addr>[+len|..end]`. Returns (start, len).
/// ja: 範囲 `<addr>[+len|..end]` をパースする。(start, len) を返す。
pub fn range(s: &str) -> Result<(u32, u32), String> {
    if let Some((a, l)) = s.split_once('+') {
        let start = u32_addr(a)?;
        let len = byte_len(l)?;
        Ok((start, len))
    } else if let Some((a, b)) = s.split_once("..") {
        let start = u32_addr(a)?;
        let end = u32_addr(b)?;
        if end < start {
            return Err(format!("range end {end:#x} is before start {start:#x}"));
        }
        Ok((start, end - start))
    } else {
        Err(format!(
            "invalid range `{s}` (use <addr>+<len> or <addr>..<end>)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_named_and_rounded() {
        assert!(matches!(speed("low"), Ok((Speed::Low, w)) if w.is_empty()));
        assert!(matches!(speed("medium"), Ok((Speed::Medium, w)) if w.is_empty()));
        assert!(matches!(speed("high"), Ok((Speed::High, w)) if w.is_empty()));
        // Exact step values do not warn.
        assert!(matches!(speed("6000"), Ok((Speed::High, w)) if w.is_empty()));
        assert!(matches!(speed("4000"), Ok((Speed::Medium, w)) if w.is_empty()));
        assert!(matches!(speed("400"), Ok((Speed::Low, w)) if w.is_empty()));
        // In-between values round down to a step and warn.
        assert!(matches!(speed("5000"), Ok((Speed::Medium, w)) if w.len() == 1));
        assert!(matches!(speed("9000"), Ok((Speed::High, w)) if w.len() == 1));
        // Below the minimum step and non-numeric are errors.
        assert!(speed("399").is_err());
        assert!(speed("fast").is_err());
    }

    #[test]
    fn u32_hex_and_decimal() {
        assert_eq!(
            u32_addr("0x0800_0000".replace('_', "").as_str()),
            Ok(0x0800_0000)
        );
        assert_eq!(u32_addr("0X10"), Ok(0x10));
        assert_eq!(u32_addr("4096"), Ok(4096));
        assert_eq!(u32_addr("  0x20  "), Ok(0x20)); // trimmed
        assert!(u32_addr("nope").is_err());
        assert!(u32_addr("0xZZ").is_err());
    }

    #[test]
    fn byte_len_suffixes() {
        assert_eq!(byte_len("256"), Ok(256));
        assert_eq!(byte_len("4k"), Ok(4096));
        assert_eq!(byte_len("1m"), Ok(1024 * 1024));
        assert_eq!(byte_len("0x100"), Ok(256));
        assert_eq!(byte_len("2K"), Ok(2048)); // case-insensitive
        assert!(byte_len("5m").is_ok());
        assert!(byte_len("0xffffffffk").is_err()); // overflow
    }

    #[test]
    fn range_plus_and_dotdot() {
        assert_eq!(range("0x0+16"), Ok((0, 16)));
        assert_eq!(range("0x1000+4k"), Ok((0x1000, 4096)));
        assert_eq!(range("0x10..0x20"), Ok((0x10, 0x10)));
        assert_eq!(range("0x08000000..0x08000100"), Ok((0x0800_0000, 0x100)));
        // end before start, and missing delimiter, are errors.
        assert!(range("0x20..0x10").is_err());
        assert!(range("0x100").is_err());
    }

    #[test]
    fn region_resolution() {
        // code: flash base, default length = the probe's flash size.
        assert_eq!(
            resolve_region("code", 0x4_8000, 0x1_0000),
            Ok((0x0800_0000, 0x4_8000))
        );
        assert_eq!(resolve_region("flash", 1024, 0), Ok((0x0800_0000, 1024)));
        // ram: SRAM base + explicit offset/length.
        assert_eq!(
            resolve_region("ram+0+64", 0, 0x1_0000),
            Ok((0x2000_0000, 64))
        );
        assert_eq!(
            resolve_region("ram+0x100", 0, 0x8000),
            Ok((0x2000_0100, 0x8000 - 0x100))
        );
        // option: fixed 16 bytes.
        assert_eq!(resolve_region("option", 0, 0), Ok((0x1FFF_F800, 16)));
        // family-specific / unknown are rejected; code with no size and no length errors.
        assert!(resolve_region("system", 0x1000, 0x1000).is_err());
        assert!(resolve_region("bogus", 0x1000, 0x1000).is_err());
        assert!(resolve_region("code", 0, 0).is_err());
    }

    #[test]
    fn region_or_addr_forms() {
        assert_eq!(
            region_or_addr("0x2000_0010".replace('_', "").as_str()),
            Ok(0x2000_0010)
        );
        assert_eq!(region_or_addr("code+0x100"), Ok(0x0800_0100));
        assert_eq!(region_or_addr("ram+16"), Ok(0x2000_0010));
        assert_eq!(region_or_addr("option"), Ok(0x1FFF_F800));
        assert!(region_or_addr("system").is_err());
    }
}
