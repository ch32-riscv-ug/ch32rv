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
