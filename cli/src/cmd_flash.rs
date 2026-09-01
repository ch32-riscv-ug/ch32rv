//! en: `flash`, `erase`, `reset`, and `recover` (docs/cli.ja.md §4.1). These write to the
//! target. `flash` erases (per policy), programs via the flash loader stub, verifies by
//! readback, resets to run, and (with `--confirm-run`) checks the target is actually running.
//! `recover --method power-off|nrst` is the "Clear All Code Flash" recovery for a target whose
//! debug pins were repurposed.
//! ja: `flash` / `erase` / `reset` / `recover`。target へ書き込む。flash は消去→stub 経由で
//! 書き込み→readback verify→reset run→(--confirm-run で)走行確認。recover は debug ピンを
//! 他用途に使った target の「Clear All Code Flash」復旧。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::event::Event;
use ch32rv_contract::policy::{ConfirmRunMode, EraseMode, RecoverMethod, ResetPolicy, VerifyMode};
use ch32rv_contract::progress::ProgressSink;
use ch32rv_contract::{ErrorKind, ResultEnvelope, Warning};
use ch32rv_flash::params_for_family;
use ch32rv_wchlink::FlashParams as WlFlashParams;

use crate::args::{Cli, FlashArgs, RecoverArgs};
use crate::cmd_probe::{fail, mode_str, select_entry};
use crate::parse;
use crate::session::{Session, SessionError};

pub fn flash(cli: &Cli, args: &FlashArgs) -> ExitCode {
    const CMD: &str = "flash";
    // en: For the first milestone only raw bin is supported; ELF/HEX/UF2 come next.
    // ja: 第1段階は raw bin のみ対応。ELF/HEX/UF2 は次段階。
    if !matches!(
        args.format,
        ch32rv_contract::policy::ImageFormat::Auto | ch32rv_contract::policy::ImageFormat::Bin
    ) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!(
                "input format {:?} is not implemented yet (bin only for now)",
                args.format
            ),
            Some("convert to a raw .bin, or wait for ELF/HEX support"),
        );
    }
    // Guard against accidentally treating an ELF/HEX as bin.
    let data = match std::fs::read(&args.file) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("read {}: {e}", args.file.display()),
                None,
            );
        }
    };
    if args.format == ch32rv_contract::policy::ImageFormat::Auto
        && (data.starts_with(b"\x7fELF") || data.first() == Some(&b':'))
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "input looks like ELF/Intel-HEX; only raw bin is implemented so far",
            Some("pass a raw .bin (ELF/HEX support is coming)"),
        );
    }

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
                "probe is in {} mode; flashing requires RISC-V mode",
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
    let mut session = match Session::attach(&entry, speed, timeout, &mut warnings) {
        Ok(s) => s,
        Err(e) => return session_error(cli, CMD, e),
    };

    let family = session.attach.family_byte;
    let Some(fp) = params_for_family(family) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "flashing family 0x{family:02x} ({}) is not supported yet (no flash stub in the interim table)",
                session.family()
            ),
            Some("only the connected families have stubs so far; more come with the generated DB"),
        );
    };

    let load_addr = match &args.offset {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => a,
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => fp.code_flash_start,
    };

    let wl = WlFlashParams {
        stub: fp.stub,
        data_packet_size: fp.data_packet_size,
        write_pack_size: fp.write_pack_size,
        supports_protect: fp.supports_protect,
        supports_special_erase: fp.supports_special_erase,
    };

    let sink = crate::progress::sink(cli);

    // Erase.
    if !matches!(args.erase, EraseMode::None) {
        sink.event(&Event::Phase {
            name: "erase".into(),
            total: None,
        });
        if let Err(e) = session.link().erase_flash() {
            return fail(
                cli,
                CMD,
                ErrorKind::TransportTimeout,
                format!("erase failed: {e}"),
                None,
            );
        }
    }

    // Program.
    sink.event(&Event::Phase {
        name: "program".into(),
        total: Some(data.len() as u64),
    });
    {
        let s = &sink;
        if let Err(e) = session.link().write_flash(&data, load_addr, &wl, |done| {
            s.event(&Event::Progress {
                phase: "program".into(),
                done,
                total: Some(data.len() as u64),
            });
        }) {
            return fail(
                cli,
                CMD,
                ErrorKind::TransportTimeout,
                format!("program failed: {e}"),
                None,
            );
        }
    }

    // Verify (readback), unless disabled.
    let mut verified = None;
    if !matches!(args.verify, VerifyMode::None) {
        sink.event(&Event::Phase {
            name: "verify".into(),
            total: Some(data.len() as u64),
        });
        session.link().detach_chip().ok();
        let _ = session.link().attach_chip();
        let mut dm = session.dm();
        if let Err(e) = dm.halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt for verify failed: {e}"),
                None,
            );
        }
        match dm.read_mem(load_addr, data.len() as u32) {
            Ok(readback) => {
                if readback == data {
                    verified = Some(true);
                } else {
                    let at = readback
                        .iter()
                        .zip(&data)
                        .position(|(a, b)| a != b)
                        .unwrap_or(0);
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::VerifyMismatch,
                        format!(
                            "verify mismatch at offset {at} (0x{:08x})",
                            load_addr as usize + at
                        ),
                        None,
                    );
                }
            }
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransportTimeout,
                    format!("readback failed: {e}"),
                    None,
                );
            }
        }
    }

    // Reset policy.
    let mut running = None;
    match args.reset {
        ResetPolicy::Run => {
            if let Err(e) = session.link().soft_reset() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransportTimeout,
                    format!("reset failed: {e}"),
                    None,
                );
            }
            if let Some(mode) = args.confirm_run {
                std::thread::sleep(Duration::from_millis(200));
                running = Some(confirm_run(&mut session, mode));
                if running == Some(false) {
                    return finish(
                        cli,
                        CMD,
                        &mut session,
                        &data,
                        verified,
                        running,
                        warnings,
                        Some((
                            ErrorKind::NotRunningAfterWrite,
                            "programmed and verified, but the target is not running after reset"
                                .to_owned(),
                        )),
                    );
                }
            }
        }
        ResetPolicy::Halt => {
            let mut dm = session.dm();
            let _ = dm.halt();
        }
        ResetPolicy::None => {}
    }

    finish(
        cli,
        CMD,
        &mut session,
        &data,
        verified,
        running,
        warnings,
        None,
    )
}

/// en: confirm-run: sample whether the target is actually executing. `status` checks the
/// running bit; `pc` additionally halts, reads dpc, checks it lies in flash, and resumes.
/// ja: confirm-run。`status` は running bit を見る。`pc` はさらに halt→dpc→flash 判定→resume。
fn confirm_run(session: &mut Session, mode: ConfirmRunMode) -> bool {
    let mut dm = session.dm();
    match mode {
        ConfirmRunMode::Status => dm.is_running().unwrap_or(false),
        ConfirmRunMode::Pc => {
            if dm.halt().is_err() {
                return false;
            }
            let pc = dm.read_reg(ch32rv_dmi::RegName::Pc).ok();
            let _ = dm.resume();
            match pc {
                // Flash executes from the low alias (< 0x0002_0000, i.e. up to 128 KiB) or the
                // 0x0800_0000 window; SRAM (0x2000_0000) means it never left the reset stub.
                Some(p) => p < 0x0002_0000 || (0x0800_0000..0x0810_0000).contains(&p),
                None => false,
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn finish(
    cli: &Cli,
    cmd: &str,
    session: &mut Session,
    data: &[u8],
    verified: Option<bool>,
    running: Option<bool>,
    warnings: Vec<Warning>,
    error: Option<(ErrorKind, String)>,
) -> ExitCode {
    let ok = error.is_none();
    if cli.json {
        let mut env = if let Some((kind, msg)) = &error {
            ResultEnvelope::failure(cmd, *kind, msg.clone())
        } else {
            ResultEnvelope::success(cmd)
        };
        env.result = Some(serde_json::json!({
            "bytes": data.len(),
            "family": session.family(),
            "verify": verified,
            "running": running,
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        if ok {
            println!("flashed {} bytes to {}", data.len(), session.family());
            if let Some(v) = verified {
                println!(
                    "verify:  {}",
                    if v {
                        "OK (readback matches)"
                    } else {
                        "MISMATCH"
                    }
                );
            }
            if let Some(r) = running {
                println!("running: {}", if r { "yes" } else { "NO" });
            }
        }
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        if let Some((kind, msg)) = &error {
            eprintln!("ch32rv: error[{}]: {msg}", kind.as_str());
            return kind.exit_code().into();
        }
        ExitCode::SUCCESS
    }
}

pub fn recover(cli: &Cli, args: &RecoverArgs) -> ExitCode {
    const CMD: &str = "recover";
    match args.method {
        RecoverMethod::PowerOff | RecoverMethod::Nrst => recover_special_erase(cli, args.method),
        other => fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!("recover --method {} is not implemented yet", other.as_str()),
            Some("power-off and nrst are implemented; unprotect/unbrick come next"),
        ),
    }
}

/// en: "Clear All Code Flash - By Power off / RST pin". Does NOT attach the target first
/// (the point is to recover a target you cannot attach). Needs `--chip` to know the family
/// byte, since we cannot read it without attaching.
/// ja: 「Clear All Code Flash」。attach しない(attach できない target の復旧が目的)。
/// family byte を読めないため `--chip` が要る。
fn recover_special_erase(cli: &Cli, method: RecoverMethod) -> ExitCode {
    const CMD: &str = "recover";
    let Some(chip) = cli.chip.as_deref() else {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "special erase needs --chip <family> (the target cannot be probed for it)",
            Some("e.g. --chip CH32V203 or the family name; see `ch32rv db list`"),
        );
    };
    let Some(family_byte) = family_byte_from_name(chip) else {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("unknown --chip `{chip}` for special erase (family not recognized)"),
            None,
        );
    };
    if params_for_family(family_byte).is_none_or(|p| !p.supports_special_erase) {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!("family `{chip}` does not support special (power-off/RST) erase"),
            None,
        );
    }

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    // Only LinkE/LinkW can power-cycle the target.
    let mut link = match ch32rv_wchlink::WchLink::open(&entry.dev) {
        Ok(l) => l,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    link.set_timeout(Duration::from_millis(
        cli.timeout.map(|s| s * 1000).unwrap_or(5000),
    ));

    let variant = link.probe_info().ok().map(|i| i.variant);
    let power_capable = matches!(
        variant,
        Some(ch32rv_wchlink::Variant::LinkE) | Some(ch32rv_wchlink::Variant::LinkW)
    );
    if method == RecoverMethod::PowerOff && !power_capable {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "only WCH-LinkE / LinkW can power-cycle the target for power-off erase",
            Some("use --method nrst with the RST pin wired, or a LinkE/LinkW probe"),
        );
    }

    let res = match method {
        RecoverMethod::PowerOff => link.erase_code_flash_by_power_off(family_byte),
        RecoverMethod::Nrst => link.erase_code_flash_by_rst(family_byte),
        _ => unreachable!(),
    };
    if let Err(e) = res {
        return fail(
            cli,
            CMD,
            ErrorKind::TransportTimeout,
            format!("special erase failed: {e}"),
            None,
        );
    }

    // en: The power-cycle leaves the probe holding a stale (corrupted) readback of the target;
    // clear it so a follow-up attach/read is clean (board-identify's re-detect recovery).
    // ja: 電源再投入で probe が壊れ読み値を保持するため、redetect でクリアして次の attach を綺麗にする。
    let _ = link.redetect_chip();
    let _ = link.detach_chip();

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "method": method.as_str(), "chip": chip }));
        crate::print_envelope(&env)
    } else {
        println!("special erase ({}) issued for {chip}", method.as_str());
        println!("the target code flash is cleared; re-flash normally now");
        ExitCode::SUCCESS
    }
}

/// en: Map a --chip name to its AttachChip family byte (interim; the DB will replace this).
/// ja: --chip 名を AttachChip family byte へ(暫定。将来は DB)。
fn family_byte_from_name(name: &str) -> Option<u8> {
    let n = name.to_ascii_uppercase();
    let n = n.strip_prefix("CH32").unwrap_or(&n);
    Some(match n {
        s if s.starts_with("V103") => 0x01,
        s if s.starts_with("V20") || s.starts_with("V205") => 0x05,
        s if s.starts_with("V30") || s.starts_with("V317") => 0x06,
        s if s.starts_with("V003") => 0x09,
        s if s.starts_with("V00") => 0x4E,
        s if s.starts_with("X03") || s.starts_with("X035") => 0x0D,
        s if s.starts_with("L103") => 0x0E,
        s if s.starts_with("643") || s.starts_with("CH643") => 0x0C,
        s if s.starts_with("641") || s.starts_with("CH641") => 0x49,
        s if s.starts_with("H4") => 0xC6,
        _ => return None,
    })
}

fn session_error(cli: &Cli, cmd: &str, e: SessionError) -> ExitCode {
    match e {
        SessionError::Open(err) | SessionError::ProbeInfo(err) => {
            let s = err.to_string();
            let kind = if s.contains("access denied") {
                ErrorKind::DeviceOpenFailed
            } else if s.contains("busy") {
                ErrorKind::DeviceBusy
            } else {
                ErrorKind::DeviceOpenFailed
            };
            fail(cli, cmd, kind, s, None)
        }
        SessionError::Attach(msg) => fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            msg,
            Some(
                "check target wiring/power/BOOT; if debug pins were repurposed try `ch32rv recover --method power-off --chip <family>`",
            ),
        ),
    }
}
