//! en: `dbg regs` / `dbg reg read` and `read` (docs/cli.ja.md §4.4-§4.1). These halt the
//! target and read registers/memory; they never write to flash. `dbg halt` leaves the core
//! halted, `dbg resume` restarts it.
//! ja: `dbg regs` / `dbg reg read` と `read`。target を halt してレジスタ/メモリを読む。
//! flash への書き込みはしない。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ResultEnvelope, Warning};
use ch32rv_dmi::RegName;

use crate::args::{Cli, ReadArgs, ReadFormat};
use crate::cmd_probe::{Entry, fail, select_entry};
use crate::parse;
use crate::session::{Session, SessionError};

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
    Session::attach(entry, speed, timeout, warnings).map_err(|e| match e {
        SessionError::Open(err) => {
            let kind = if err.to_string().contains("access denied") {
                ErrorKind::DeviceOpenFailed
            } else if err.to_string().contains("busy") {
                ErrorKind::DeviceBusy
            } else {
                ErrorKind::DeviceOpenFailed
            };
            fail(cli, cmd, kind, err.to_string(), None)
        }
        SessionError::ProbeInfo(err) => {
            fail(cli, cmd, ErrorKind::DeviceOpenFailed, err.to_string(), None)
        }
        SessionError::Attach(msg) => fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            msg,
            Some("check target wiring/power/BOOT; a protected target needs `ch32rv recover`"),
        ),
    })
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
    let mut gprs = Vec::with_capacity(32);
    for i in 0..32u8 {
        match dm.read_reg(RegName::Gpr(i)) {
            Ok(v) => gprs.push(v),
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransportTimeout,
                    format!("read x{i} failed: {e}"),
                    None,
                );
            }
        }
    }
    let pc = dm.read_reg(RegName::Pc).ok();

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        let regs: serde_json::Map<String, serde_json::Value> = (0..32)
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
        for i in 0..32 {
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

pub fn read(cli: &Cli, args: &ReadArgs) -> ExitCode {
    const CMD: &str = "read";
    // The clap ArgGroup guarantees exactly one of --range / --region is present.
    let (start, len) = match resolve_range(args) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
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
    let data = match dm.read_mem(start, len) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::TransportTimeout,
                format!("read failed: {e}"),
                None,
            );
        }
    };

    if args.blank_check {
        let blank = data.iter().all(|&b| b == 0xff);
        if cli.json {
            let mut env = if blank {
                ResultEnvelope::success(CMD)
            } else {
                ResultEnvelope::failure(CMD, ErrorKind::BlankCheckFailed, "region is not blank")
            };
            env.result = Some(serde_json::json!({
                "start": format!("0x{start:08x}"), "len": len, "blank": blank,
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

fn resolve_range(args: &ReadArgs) -> Result<(u32, u32), String> {
    if let Some(r) = &args.range {
        parse::range(r)
    } else if let Some(_region) = &args.region {
        // en: Named regions need the target DB (region base addresses); not generated yet.
        // ja: 領域名は target DB(領域ベース番地)が要る。DB 未生成のため当面 --range を使う。
        Err(
            "--region is not available until the target DB is generated; use --range for now"
                .to_owned(),
        )
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
            ReadFormat::Hex => hex_dump(start, data).into_bytes(),
            ReadFormat::Ihex => match ihex_dump(start, data) {
                Ok(s) => s.into_bytes(),
                Err(m) => return fail(cli, cmd, ErrorKind::Internal, m, None),
            },
        };
        if let Err(e) = std::fs::write(path, &bytes) {
            return fail(
                cli,
                cmd,
                ErrorKind::Internal,
                format!("write {}: {e}", path.display()),
                None,
            );
        }
        if cli.json {
            let mut env = ResultEnvelope::success(cmd);
            env.result = Some(serde_json::json!({
                "start": format!("0x{start:08x}"), "len": data.len(),
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
            "start": format!("0x{start:08x}"), "len": data.len(),
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
