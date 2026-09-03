//! en: `dbg regs` / `dbg reg read` and `read` (docs/cli.ja.md §4.4-§4.1). These halt the
//! target and read registers/memory; they never write to flash. `dbg halt` leaves the core
//! halted, `dbg resume` restarts it.
//! ja: `dbg regs` / `dbg reg read` と `read`。target を halt してレジスタ/メモリを読む。
//! flash への書き込みはしない。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ResultEnvelope, Warning};
use ch32rv_dmi::{DtmAccess, RegName};

use crate::args::{Cli, DmiCmd, ReadArgs, ReadFormat, RegCmd};
use crate::cmd_probe::{Entry, fail, select_entry};
use crate::parse;
use crate::session::Session;

const GPR_ABI: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

fn prepare(
    cli: &Cli,
    cmd: &str,
    warnings: &mut Vec<Warning>,
) -> Result<(Entry, ch32rv_wchlink::Speed), ExitCode> {
    let entry = select_entry(cli, cmd)?;
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return Err(fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            "attaching to a target requires a probe in RISC-V mode",
            None,
        ));
    }
    let (speed, w) =
        parse::speed(&cli.speed).map_err(|m| fail(cli, cmd, ErrorKind::Usage, m, None))?;
    warnings.extend(w);
    Ok((entry, speed))
}

fn open_session(
    cli: &Cli,
    cmd: &str,
    entry: &Entry,
    speed: ch32rv_wchlink::Speed,
    warnings: &mut Vec<Warning>,
) -> Result<Session, ExitCode> {
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    Session::attach(
        entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        warnings,
    )
    .map_err(|e| crate::cmd_probe::session_error(cli, cmd, e))
}

pub fn regs(cli: &Cli) -> ExitCode {
    const CMD: &str = "dbg.regs";
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
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
    // RV32E cores (misa.E, e.g. CH32V003) expose only x0..x15; reading x16.. raises cmderr.
    let gpr_count: u8 = match dm.read_reg(RegName::Csr(0x301)) {
        Ok(misa) if misa & (1 << 4) != 0 => 16,
        _ => 32,
    };
    let mut gprs = [0u32; 32];
    for i in 0..gpr_count {
        match dm.read_reg(RegName::Gpr(i)) {
            Ok(v) => gprs[i as usize] = v,
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("read x{i} failed: {e}"),
                    None,
                );
            }
        }
    }
    let pc = dm.read_reg(RegName::Pc).ok();
    if gpr_count == 16 {
        warnings.push(Warning {
            code: "rv32e".to_owned(),
            msg: "RV32E core (misa.E): only x0-x15 exist; x16-x31 omitted".to_owned(),
        });
    }
    let n = gpr_count as usize;

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        let regs: serde_json::Map<String, serde_json::Value> = (0..n)
            .map(|i| {
                (
                    format!("x{i}"),
                    serde_json::json!(format!("0x{:08x}", gprs[i])),
                )
            })
            .collect();
        env.result = Some(serde_json::json!({
            "gpr": regs,
            "pc": pc.map(|v| format!("0x{v:08x}")),
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        for i in 0..n {
            println!("x{i:<2} {:<4} 0x{:08x}", GPR_ABI[i], gprs[i]);
        }
        match pc {
            Some(v) => println!("pc       0x{v:08x}"),
            None => println!("pc       (unavailable)"),
        }
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

pub fn halt(cli: &Cli, reset: bool) -> ExitCode {
    const CMD: &str = "dbg.halt";
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if reset && let Err(e) = session.link().soft_reset() {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("reset failed: {e}"),
            None,
        );
    }
    let mut dm = session.dm();
    match dm.halt() {
        Ok(()) => {
            let pc = dm.read_reg(RegName::Pc).ok();
            simple_ok(
                cli,
                CMD,
                serde_json::json!({ "halted": true, "pc": pc.map(|v| format!("0x{v:08x}")) }),
                &format!(
                    "halted{}",
                    pc.map(|v| format!(" at 0x{v:08x}")).unwrap_or_default()
                ),
                warnings,
            )
        }
        Err(e) => fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        ),
    }
}

pub fn resume(cli: &Cli) -> ExitCode {
    const CMD: &str = "dbg.resume";
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let mut dm = session.dm();
    match dm.resume() {
        Ok(()) => simple_ok(
            cli,
            CMD,
            serde_json::json!({ "resumed": true }),
            "resumed",
            warnings,
        ),
        Err(e) => fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("resume failed: {e}"),
            None,
        ),
    }
}

pub fn step(cli: &Cli, n: Option<u32>) -> ExitCode {
    const CMD: &str = "dbg.step";
    let count = n.unwrap_or(1).max(1);
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
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
    for i in 0..count {
        if let Err(e) = dm.step() {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("step {} failed: {e}", i + 1),
                None,
            );
        }
    }
    let pc = dm.read_reg(RegName::Pc).ok();
    simple_ok(
        cli,
        CMD,
        serde_json::json!({ "stepped": count, "pc": pc.map(|v| format!("0x{v:08x}")) }),
        &format!(
            "stepped {count}{}",
            pc.map(|v| format!(", pc 0x{v:08x}")).unwrap_or_default()
        ),
        warnings,
    )
}

fn simple_ok(
    cli: &Cli,
    cmd: &str,
    result: serde_json::Value,
    human: &str,
    warnings: Vec<Warning>,
) -> ExitCode {
    if cli.json {
        let mut env = ResultEnvelope::success(cmd);
        env.result = Some(result);
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        println!("{human}");
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

pub fn read(cli: &Cli, args: &ReadArgs) -> ExitCode {
    const CMD: &str = "read";
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    // The clap ArgGroup guarantees exactly one of --range / --region. A named region needs the
    // target's sizes, so resolve it after attach: flash size from the probe (ChipInfo), SRAM from DB.
    let (start, len) = {
        let flash_bytes = session.chip.as_ref().map(|c| c.flash_bytes).unwrap_or(0);
        let sram_bytes =
            match ch32rv_target::Db::builtin().resolve_by_chip_id(session.attach.chip_id) {
                ch32rv_target::Resolution::Sku(s) => s.sram_bytes,
                _ => 0,
            };
        match resolve_range(args, flash_bytes, sram_bytes) {
            Ok(v) => v,
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        }
    };
    if let Err(e) = session.dm().halt() {
        return fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }
    // en: Fast bulk read via the WCH-Link (SetReadMemoryRegion + ReadMemory + data endpoint),
    // chunked so a large read (e.g. a full-flash backup) stays under the probe's per-transfer limit
    // and timeout; each chunk falls back to word-by-word DMI reads if the probe rejects it.
    // ja: WCH-Link の高速バルク read を chunk 単位で行い(全 flash backup 等の大読みが probe の
    // 1 転送上限・timeout を超えないように)、chunk ごとに弾かれたら DMI word 読みへ fallback。
    const READ_CHUNK: u32 = 32 * 1024;
    let mut data = Vec::with_capacity(len as usize);
    let mut off = 0u32;
    while off < len {
        let want = READ_CHUNK.min(len - off);
        let chunk = match session.link().read_mem(start + off, want) {
            Ok(d) => d,
            Err(_) => match session.dm().read_mem(start + off, want) {
                Ok(d) => d,
                Err(e) => {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TransferFailed,
                        format!("read failed at {:#010x}: {e}", start + off),
                        None,
                    );
                }
            },
        };
        data.extend_from_slice(&chunk);
        off += want;
    }

    if args.blank_check {
        let blank = data.iter().all(|&b| b == 0xff);
        if cli.json {
            let mut env = if blank {
                ResultEnvelope::success(CMD)
            } else {
                ResultEnvelope::failure(CMD, ErrorKind::BlankCheckFailed, "region is not blank")
            };
            env.result = Some(serde_json::json!({
                "addr": format!("0x{start:08x}"), "len": len, "blank": blank,
            }));
            env.warnings = warnings;
            return crate::print_envelope(&env);
        } else {
            println!(
                "blank check 0x{start:08x}+{len}: {}",
                if blank { "BLANK" } else { "NOT BLANK" }
            );
            return if blank {
                ExitCode::SUCCESS
            } else {
                ErrorKind::BlankCheckFailed.exit_code().into()
            };
        }
    }

    output_data(cli, CMD, args, start, &data, warnings)
}

fn resolve_range(args: &ReadArgs, flash_bytes: u32, sram_bytes: u32) -> Result<(u32, u32), String> {
    if let Some(r) = &args.range {
        parse::range(r)
    } else if let Some(region) = &args.region {
        parse::resolve_region(region, flash_bytes, sram_bytes)
    } else {
        Err("read needs --range or --region".to_owned())
    }
}

fn output_data(
    cli: &Cli,
    cmd: &str,
    args: &ReadArgs,
    start: u32,
    data: &[u8],
    warnings: Vec<Warning>,
) -> ExitCode {
    // en: `-o -` or no `-o` in JSON mode: bytes go into the envelope as hex; otherwise to file
    // or stdout in the requested format.
    // ja: `-o -` または JSON 時は envelope に hex で載せる。それ以外は file/stdout へ。
    if let Some(path) = &args.out
        && path.as_os_str() != "-"
    {
        let bytes = match args.format {
            ReadFormat::Bin => data.to_vec(),
            ReadFormat::HexDump => hex_dump(start, data).into_bytes(),
            ReadFormat::Ihex => match ihex_dump(start, data) {
                Ok(s) => s.into_bytes(),
                Err(m) => return fail(cli, cmd, ErrorKind::Usage, m, None),
            },
        };
        if let Err(e) = std::fs::write(path, &bytes) {
            // A filesystem error on the user's output path is a usage error, not a bug (exit 70).
            return fail(
                cli,
                cmd,
                ErrorKind::Usage,
                format!("write {}: {e}", path.display()),
                None,
            );
        }
        if cli.json {
            let mut env = ResultEnvelope::success(cmd);
            env.result = Some(serde_json::json!({
                "addr": format!("0x{start:08x}"), "len": data.len(),
                "out": path.display().to_string(),
            }));
            env.warnings = warnings;
            return crate::print_envelope(&env);
        }
        println!("wrote {} bytes to {}", data.len(), path.display());
        return ExitCode::SUCCESS;
    }

    if cli.json {
        let mut env = ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({
            "addr": format!("0x{start:08x}"), "len": data.len(),
            "hex": data.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        print!("{}", hex_dump(start, data));
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

fn hex_dump(start: u32, data: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = start.wrapping_add((i * 16) as u32);
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!("{addr:08x}  {:<48}  {ascii}\n", hex.join(" ")));
    }
    out
}

/// en: Minimal Intel HEX writer (record types 00 data / 04 extended linear / 01 EOF).
/// ja: 最小の Intel HEX 出力(record 00 data / 04 upper address / 01 EOF)。
fn ihex_dump(start: u32, data: &[u8]) -> Result<String, String> {
    let mut out = String::new();
    let mut upper = u16::MAX; // force an initial 04 record
    for (i, chunk) in data.chunks(16).enumerate() {
        let addr = start
            .checked_add((i * 16) as u32)
            .ok_or("address overflow")?;
        let hi = (addr >> 16) as u16;
        if hi != upper {
            out.push_str(&ihex_record(0, 0x04, &hi.to_be_bytes()));
            upper = hi;
        }
        out.push_str(&ihex_record((addr & 0xffff) as u16, 0x00, chunk));
    }
    out.push_str(&ihex_record(0, 0x01, &[]));
    Ok(out)
}

fn ihex_record(addr: u16, rtype: u8, data: &[u8]) -> String {
    let len = data.len() as u8;
    let mut bytes = vec![len, (addr >> 8) as u8, addr as u8, rtype];
    bytes.extend_from_slice(data);
    let sum = bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    let checksum = sum.wrapping_neg();
    let body: String = bytes
        .iter()
        .chain(std::iter::once(&checksum))
        .map(|b| format!("{b:02X}"))
        .collect();
    format!(":{body}\n")
}

/// en: Parse a register name for `dbg reg`: `x0`..`x31`, `pc`/`dpc`, `csr:<addr>`, or a few common
/// CSR aliases (misa/mstatus/mcause/mepc/mtval/dcsr/mvendorid/marchid).
/// ja: `dbg reg` のレジスタ名を解析。
fn parse_reg_name(name: &str) -> Option<RegName> {
    let n = name.trim().to_ascii_lowercase();
    if n == "pc" || n == "dpc" {
        return Some(RegName::Pc);
    }
    if let Some(x) = n.strip_prefix('x') {
        return x.parse::<u8>().ok().filter(|v| *v < 32).map(RegName::Gpr);
    }
    if let Some(c) = n.strip_prefix("csr:") {
        return parse_u32(c)
            .and_then(|v| u16::try_from(v).ok())
            .map(RegName::Csr);
    }
    let csr: u16 = match n.as_str() {
        "mstatus" => 0x300,
        "misa" => 0x301,
        "mepc" => 0x341,
        "mcause" => 0x342,
        "mtval" => 0x343,
        "dcsr" => 0x7b0,
        "mvendorid" => 0xf11,
        "marchid" => 0xf12,
        _ => return None,
    };
    Some(RegName::Csr(csr))
}

/// Parse a `0x`-hex or decimal u32.
fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// `dbg reg read|write <name> [value]`: read or write one core register (GPR / pc / CSR). The core
/// is halted for the access. Writing a register (expert) can disturb the running program.
pub fn reg(cli: &Cli, sub: &RegCmd) -> ExitCode {
    const CMD: &str = "dbg.reg";
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
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
    match sub {
        RegCmd::Read { name } => {
            let Some(reg) = parse_reg_name(name) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!("unknown register {name:?}"),
                    Some("use x0..x31, pc, csr:<addr>, or misa/mstatus/mcause/mepc/mtval/dcsr"),
                );
            };
            match dm.read_reg(reg) {
                Ok(v) => {
                    if cli.json {
                        let mut env = ResultEnvelope::success(CMD);
                        env.result = Some(serde_json::json!({
                            "reg": name, "value": format!("0x{v:08x}"),
                        }));
                        crate::print_envelope(&env)
                    } else {
                        println!("{name} = 0x{v:08x}");
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("read {name} failed: {e}"),
                    None,
                ),
            }
        }
        RegCmd::Write { name, value } => {
            let Some(reg) = parse_reg_name(name) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!("unknown register {name:?}"),
                    None,
                );
            };
            let Some(val) = parse_u32(value) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!("bad value {value:?} (use 0x.. or decimal)"),
                    None,
                );
            };
            match dm.write_reg(reg, val) {
                Ok(()) => {
                    if cli.json {
                        let mut env = ResultEnvelope::success(CMD);
                        env.result = Some(serde_json::json!({
                            "reg": name, "value": format!("0x{val:08x}"),
                        }));
                        crate::print_envelope(&env)
                    } else {
                        println!("{name} <- 0x{val:08x}");
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("write {name} failed: {e}"),
                    None,
                ),
            }
        }
    }
}

/// `dbg dmi read|write <addr> [value]`: raw Debug Module register access over DMI (expert). `addr`
/// is a DM register address (e.g. 0x11 = DMSTATUS, 0x10 = DMCONTROL).
pub fn dmi(cli: &Cli, sub: &DmiCmd) -> ExitCode {
    const CMD: &str = "dbg.dmi";
    let mut warnings = Vec::new();
    let (entry, speed) = match prepare(cli, CMD, &mut warnings) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let mut session = match open_session(cli, CMD, &entry, speed, &mut warnings) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let parse_addr = |a: &str| parse_u32(a).and_then(|v| u8::try_from(v).ok());
    match sub {
        DmiCmd::Read { addr } => {
            let Some(a) = parse_addr(addr) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!("bad DM address {addr:?} (0x00..0x7f)"),
                    None,
                );
            };
            match session.link().dmi_read(a) {
                Ok(v) => {
                    if cli.json {
                        let mut env = ResultEnvelope::success(CMD);
                        env.result = Some(serde_json::json!({
                            "addr": format!("0x{a:02x}"), "value": format!("0x{v:08x}"),
                        }));
                        crate::print_envelope(&env)
                    } else {
                        println!("dmi[0x{a:02x}] = 0x{v:08x}");
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("dmi read 0x{a:02x} failed: {e}"),
                    None,
                ),
            }
        }
        DmiCmd::Write { addr, value } => {
            let (Some(a), Some(v)) = (parse_addr(addr), parse_u32(value)) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "bad DM address or value".to_owned(),
                    None,
                );
            };
            match session.link().dmi_write(a, v) {
                Ok(()) => {
                    if cli.json {
                        let mut env = ResultEnvelope::success(CMD);
                        env.result = Some(serde_json::json!({
                            "addr": format!("0x{a:02x}"), "value": format!("0x{v:08x}"),
                        }));
                        crate::print_envelope(&env)
                    } else {
                        println!("dmi[0x{a:02x}] <- 0x{v:08x}");
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("dmi write 0x{a:02x} failed: {e}"),
                    None,
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_reg_name, parse_u32};
    use ch32rv_dmi::RegName;

    #[test]
    fn parses_register_names() {
        assert_eq!(parse_reg_name("pc"), Some(RegName::Pc));
        assert_eq!(parse_reg_name("x2"), Some(RegName::Gpr(2)));
        assert_eq!(parse_reg_name("X31"), Some(RegName::Gpr(31)));
        assert_eq!(parse_reg_name("x32"), None); // out of range
        assert_eq!(parse_reg_name("misa"), Some(RegName::Csr(0x301)));
        assert_eq!(parse_reg_name("csr:0x7b0"), Some(RegName::Csr(0x7b0)));
        assert_eq!(parse_reg_name("csr:769"), Some(RegName::Csr(769)));
        assert_eq!(parse_reg_name("nope"), None);
    }

    #[test]
    fn parses_u32_hex_and_decimal() {
        assert_eq!(parse_u32("0x40901105"), Some(0x4090_1105));
        assert_eq!(parse_u32("42"), Some(42));
        assert_eq!(parse_u32("zz"), None);
    }
}
