//! en: `run` (docs/cli.ja.md §4.1) - the HIL runner: flash an image (unless `--no-flash`), reset to
//! run, stream the target's runtime output, and end on either a timeout or a semihosting exit whose
//! code is propagated. Self-contained over the Debug Module (dmdata output + a semihosting host).
//! ja: `run`。HIL 用ランナー。書込→reset 実行→runtime 出力を流し、timeout か semihosting の
//! exit(コードを伝搬)で終わる。DM 上で自己完結(dmdata 出力 + semihosting ホスト)。

use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ch32rv_contract::policy::ImageFormat;
use ch32rv_contract::{ErrorKind, Warning};
use ch32rv_dmi::RegName;
use ch32rv_flash::params_for_family;
use ch32rv_wchlink::FlashParams as WlFlashParams;

use crate::args::{Cli, RunArgs};
use crate::cmd_probe::{fail, select_entry};
use crate::parse;
use crate::session::Session;

/// RISC-V semihosting call sequence: `slli x0,x0,0x1f; ebreak; srai x0,x0,7`.
const SEMI_SLLI: u32 = 0x01f0_1013;
const SEMI_EBREAK: u32 = 0x0010_0073;
const SEMI_SRAI: u32 = 0x4070_5013;
/// Semihosting operations we service.
const SYS_WRITEC: u32 = 0x03;
const SYS_WRITE0: u32 = 0x04;
const SYS_EXIT: u32 = 0x18;
const SYS_EXIT_EXTENDED: u32 = 0x20;
/// `ADP_Stopped_ApplicationExit` - a plain SYS_EXIT reason that means "exited normally".
const ADP_APPLICATION_EXIT: u32 = 0x2_0026;

enum ExitMode {
    /// Stream output until this deadline (None = until Ctrl-C), then exit 0.
    Timeout(Option<Duration>),
    /// Service semihosting; propagate the target's SYS_EXIT code (bounded by `cap`).
    Semihosting { cap: Duration },
}

/// Is `(before, at, after)` the RISC-V semihosting `slli/ebreak/srai` sequence?
fn is_semihosting_seq(before: u32, at: u32, after: u32) -> bool {
    before == SEMI_SLLI && at == SEMI_EBREAK && after == SEMI_SRAI
}

pub fn run(cli: &Cli, args: &RunArgs) -> ExitCode {
    const CMD: &str = "run";
    let exit_mode = match args.exit_on.as_deref() {
        None => ExitMode::Timeout(cli.timeout.map(Duration::from_secs)),
        Some("semihosting") => ExitMode::Semihosting {
            cap: Duration::from_secs(cli.timeout.unwrap_or(60)),
        },
        Some(s) if s.starts_with("timeout=") => match s["timeout=".len()..].parse::<u64>() {
            Ok(secs) => ExitMode::Timeout(Some(Duration::from_secs(secs))),
            Err(_) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "invalid --exit-on timeout value",
                    None,
                );
            }
        },
        Some(_) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                "--exit-on must be `semihosting` or `timeout=<seconds>`",
                None,
            );
        }
    };

    // Only dmdata streaming is wired into run so far (probe-agnostic; no CDC needed).
    if let Some(src) = args.source
        && !matches!(src, ch32rv_contract::policy::MonitorSource::Dmdata)
    {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "run currently streams via --source dmdata; uart/sdi/rtt are a follow-up",
            None,
        );
    }

    let bytes = if args.no_flash {
        Vec::new()
    } else {
        match std::fs::read(&args.elf) {
            Ok(d) => d,
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!("read {}: {e}", args.elf.display()),
                    None,
                );
            }
        }
    };

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "run requires a probe in RISC-V mode",
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
        Err(_) => return fail(cli, CMD, ErrorKind::AttachFailed, "attach failed", None),
    };

    // Program the image (unless --no-flash).
    if !args.no_flash {
        let family = session.attach.family_byte;
        let Some(fp) = params_for_family(family) else {
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                format!("flashing family 0x{family:02x} is not supported"),
                None,
            );
        };
        let image = match crate::cmd_flash::parse_image(
            &bytes,
            ImageFormat::Auto,
            &args.elf,
            None,
            fp.code_flash_start,
        ) {
            Ok(i) => i,
            Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
        };
        let wl = WlFlashParams {
            stub: fp.stub,
            data_packet_size: fp.data_packet_size,
            write_pack_size: fp.write_pack_size,
            supports_protect: fp.supports_protect,
            supports_special_erase: fp.supports_special_erase,
        };
        for seg in &image.segments {
            if let Err(e) = session.link().write_flash(&seg.data, seg.addr, &wl, |_| {}) {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransportTimeout,
                    format!("program failed at {:#010x}: {e}", seg.addr),
                    None,
                );
            }
        }
    }

    // Reset to run the freshly programmed image. For semihosting the ebreak-debug CSR is set
    // *after* the reset (in run_semihosting) so a core reset cannot clear it.
    let _ = session.link().soft_reset();
    if !cli.json {
        eprintln!(
            "run: {} (Ctrl-C to stop)",
            entry.dev.serial().unwrap_or("?")
        );
    }

    match exit_mode {
        ExitMode::Timeout(dur) => run_stream(cli, CMD, &mut session, dur, warnings),
        ExitMode::Semihosting { cap } => run_semihosting(cli, CMD, &mut session, cap, warnings),
    }
}

/// Stream dmdata output until the deadline (or forever), then exit 0.
fn run_stream(
    cli: &Cli,
    cmd: &str,
    session: &mut Session,
    dur: Option<Duration>,
    warnings: Vec<Warning>,
) -> ExitCode {
    let deadline = dur.map(|d| Instant::now() + d);
    let mut dm = session.dm();
    let _ = dm.resume();
    let mut out = std::io::stdout().lock();
    loop {
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            break;
        }
        match dm.dmdata_poll(&[]) {
            Ok(Some(bytes)) if !bytes.is_empty() => {
                let _ = out.write_all(&bytes);
                let _ = out.flush();
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    if cli.json {
        let mut env = ch32rv_contract::ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({ "exit": 0, "reason": "timeout" }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        ExitCode::SUCCESS
    }
}

/// Run, servicing semihosting calls, until SYS_EXIT (propagate the code) or the safety cap.
fn run_semihosting(
    cli: &Cli,
    cmd: &str,
    session: &mut Session,
    cap: Duration,
    warnings: Vec<Warning>,
) -> ExitCode {
    let deadline = Instant::now() + cap;
    let mut out = std::io::stdout().lock();
    let mut dm = session.dm();
    // ebreak must trap to debug mode (halt) rather than the target's own handler. Setting the CSR
    // requires the hart halted; do it now (post-reset), then resume into the application.
    let _ = dm.halt();
    let _ = dm.enable_ebreak_debug();
    let _ = dm.resume();
    loop {
        if Instant::now() >= deadline {
            let msg = "run: timed out waiting for a semihosting exit";
            if cli.json {
                let env =
                    ch32rv_contract::ResultEnvelope::failure(cmd, ErrorKind::TransportTimeout, msg);
                return crate::print_envelope(&env);
            }
            eprintln!("{msg}");
            return ErrorKind::TransportTimeout.exit_code().into();
        }
        // Also drain any dmdata output while running.
        if let Ok(Some(b)) = dm.dmdata_poll(&[])
            && !b.is_empty()
        {
            let _ = out.write_all(&b);
            let _ = out.flush();
        }
        match dm.is_halted() {
            Ok(false) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Ok(true) => {}
            Err(_) => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        }
        // Halted: is it a semihosting call?
        let Ok(dpc) = dm.read_reg(RegName::Pc) else {
            let _ = dm.resume();
            continue;
        };
        let at = dm.read_mem(dpc, 4).ok();
        let before = dm.read_mem(dpc.wrapping_sub(4), 4).ok();
        let after = dm.read_mem(dpc.wrapping_add(4), 4).ok();
        let w = |o: &Option<Vec<u8>>| {
            o.as_ref()
                .filter(|v| v.len() == 4)
                .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
        };
        match (w(&before), w(&at), w(&after)) {
            (Some(b), Some(a), Some(af)) if is_semihosting_seq(b, a, af) => {
                let op = dm.read_reg(RegName::Gpr(10)).unwrap_or(0); // a0
                let arg = dm.read_reg(RegName::Gpr(11)).unwrap_or(0); // a1
                match op {
                    SYS_EXIT | SYS_EXIT_EXTENDED => {
                        let code = semihosting_exit_code(&mut dm, op, arg);
                        if cli.json {
                            let mut env = ch32rv_contract::ResultEnvelope::success(cmd);
                            env.result = Some(serde_json::json!({ "exit": code }));
                            env.warnings = warnings;
                            return crate::print_envelope(&env);
                        }
                        return ExitCode::from(code as u8);
                    }
                    SYS_WRITE0 => {
                        // a1 -> NUL-terminated string.
                        let mut addr = arg;
                        'outer: for _ in 0..1024 {
                            let Ok(chunk) = dm.read_mem(addr, 16) else {
                                break;
                            };
                            for &byte in &chunk {
                                if byte == 0 {
                                    break 'outer;
                                }
                                let _ = out.write_all(&[byte]);
                            }
                            addr = addr.wrapping_add(16);
                        }
                        let _ = out.flush();
                        // Skip the ebreak: resume from the srai (dpc + 4).
                        let _ = dm.write_reg(RegName::Pc, dpc.wrapping_add(4));
                        let _ = dm.resume();
                    }
                    SYS_WRITEC => {
                        if let Ok(c) = dm.read_mem(arg, 1)
                            && let Some(&byte) = c.first()
                        {
                            let _ = out.write_all(&[byte]);
                            let _ = out.flush();
                        }
                        let _ = dm.write_reg(RegName::Pc, dpc.wrapping_add(4));
                        let _ = dm.resume();
                    }
                    _ => {
                        // Unsupported call: skip it and continue.
                        let _ = dm.write_reg(RegName::Pc, dpc.wrapping_add(4));
                        let _ = dm.resume();
                    }
                }
            }
            _ => {
                // A non-semihosting halt (a real breakpoint/trap): report and stop.
                let msg = format!("run: target halted at {dpc:#010x} (not a semihosting call)");
                if cli.json {
                    let env =
                        ch32rv_contract::ResultEnvelope::failure(cmd, ErrorKind::AttachFailed, msg);
                    return crate::print_envelope(&env);
                }
                eprintln!("{msg}");
                return ErrorKind::AttachFailed.exit_code().into();
            }
        }
    }
}

/// Derive the process exit code from a semihosting SYS_EXIT / SYS_EXIT_EXTENDED call.
fn semihosting_exit_code(
    dm: &mut ch32rv_dmi::DebugModule<'_, ch32rv_wchlink::WchLink>,
    op: u32,
    arg: u32,
) -> u32 {
    if op == SYS_EXIT_EXTENDED {
        // a1 -> [reason(u32), exit_code(u32)].
        if let Ok(block) = dm.read_mem(arg, 8)
            && block.len() == 8
        {
            return u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        }
        return 1;
    }
    // Plain SYS_EXIT (RV32): a1 is the reason code directly.
    if arg == ADP_APPLICATION_EXIT { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn detects_semihosting_sequence() {
        assert!(is_semihosting_seq(SEMI_SLLI, SEMI_EBREAK, SEMI_SRAI));
        assert!(!is_semihosting_seq(SEMI_SLLI, 0x0000_0013, SEMI_SRAI)); // plain nop, not ebreak
        assert!(!is_semihosting_seq(0, SEMI_EBREAK, 0)); // bare ebreak, no magic brackets
    }
}
