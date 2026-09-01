//! en: `target info` (docs/cli.ja.md §4.3): attach, read the chip signature and factory
//! UUID/flash size, and detach. Read-only: nothing is written to the target, and the core is
//! always released (detach) on every path — including errors. The LinkE corrupted-readback
//! bug is detected and recovered via RedetectChip (board-identify, measured).
//! ja: `target info`。attach → chip 署名と工場 UUID・flash 容量の読み取り → detach。
//! 読み取り専用で、どの経路でも必ず detach して core を解放する。LinkE の壊れ読み値バグは
//! RedetectChip で検出・復旧する(board-identify 実測)。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ProbeMode, ProbeReport, ResultEnvelope, TargetReport, Warning};
use ch32rv_wchlink::{
    AttachInfo, ChipInfo, ChipInfoStatus, Speed, WchLink, WchLinkError, family_name,
};

use crate::args::Cli;
use crate::cmd_probe::{
    Entry, apply_probe_info, base_report, fail, mode_str, print_probe_human, select_entry,
};

const CMD: &str = "target.info";

pub fn info(cli: &Cli) -> ExitCode {
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(code) => return code,
    };
    if entry.mode != ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; attaching to a target requires RISC-V mode",
                mode_str(entry.mode)
            ),
            Some(
                "switch modes with WCH-LinkUtility (`ch32rv probe mode set` is not implemented yet)",
            ),
        );
    }
    let (speed, mut warnings) = match parse_speed(&cli.speed) {
        Ok(v) => v,
        Err(msg) => return fail(cli, CMD, ErrorKind::Usage, msg, None),
    };

    let mut link = match open_with_retry(&entry) {
        Ok(l) => l,
        Err(e) => {
            let kind = match &e {
                WchLinkError::Usb(ch32rv_usb::UsbError::AccessDenied(_)) => {
                    ErrorKind::DeviceOpenFailed
                }
                WchLinkError::Usb(ch32rv_usb::UsbError::Busy(_)) => ErrorKind::DeviceBusy,
                _ => ErrorKind::DeviceOpenFailed,
            };
            return fail(
                cli,
                CMD,
                kind,
                e.to_string(),
                Some("check permissions/driver binding, or whether another tool holds the probe"),
            );
        }
    };
    // en: Attach probing takes longer than an info query; board-identify uses 3 s.
    // ja: attach は info 系より時間がかかる。board-identify は 3 秒を使う。
    link.set_timeout(Duration::from_millis(
        cli.timeout.map(|s| s * 1000).unwrap_or(3000),
    ));

    // en: Clear leftover probe state first (a previous session may still hold the target).
    // ja: 前セッションの残り状態を先にクリアする。
    let _ = link.detach_chip();

    let mut probe_report = base_report(&entry);
    match link.probe_info() {
        Ok(pi) => apply_probe_info(&mut probe_report, &pi, &mut warnings),
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::DeviceOpenFailed,
                format!("probe did not answer GetProbeInfo: {e}"),
                None,
            );
        }
    }

    // en: Attach and read; always release the core afterwards, on every path.
    // ja: attach して読み、どの経路でも必ず解放する。
    let outcome = attach_and_read(&mut link, speed, &mut warnings);
    let _ = link.detach_chip();
    drop(link);

    let (attach, chip) = match outcome {
        Ok(v) => v,
        Err(msg) => {
            return fail_with_probe(
                cli,
                probe_report,
                ErrorKind::AttachFailed,
                msg,
                Some(
                    "check target wiring/power/BOOT; for a protected or bricked target see `ch32rv recover`",
                ),
            );
        }
    };

    let family = family_name(attach.family_byte)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("unknown(0x{:02x})", attach.family_byte));
    if family.starts_with("unknown") {
        warnings.push(Warning {
            code: "family-unknown".to_owned(),
            msg: format!(
                "family byte 0x{:02x} is not in the known table (possibly a gap series) - worth recording for data request 0001",
                attach.family_byte
            ),
        });
    }
    warnings.push(Warning {
        code: "db-empty".to_owned(),
        msg: "SKU resolution unavailable: the target DB is not generated yet (data request 0001 pending)"
            .to_owned(),
    });

    let target = TargetReport {
        sku: None,
        family: Some(family),
        chip_id: Some(format!("0x{:08x}", attach.chip_id)),
        uid: chip.as_ref().map(|c| hex(&c.uuid)),
        verified: None,
        provisional: None,
        protected: None,
        flash_kb: chip.as_ref().map(|c| u32::from(c.flash_kb)),
    };

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.probe = Some(probe_report);
        if let Some(c) = &chip {
            env.result = Some(serde_json::json!({
                "protection_raw": hex(&c.protection_raw),
                "chip_id_echo": format!("0x{:08x}", c.chip_id_echo),
            }));
        }
        env.target = Some(target);
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        print_probe_human(&probe_report);
        println!("---");
        println!("family:   {}", target.family.as_deref().unwrap_or("-"));
        println!(
            "chip id:  {}  (bits [7:4] = silicon revision)",
            target.chip_id.as_deref().unwrap_or("-")
        );
        println!("uid:      {}", target.uid.as_deref().unwrap_or("-"));
        match target.flash_kb {
            Some(kb) => println!("flash:    {kb} KiB"),
            None => println!("flash:    -"),
        }
        println!("sku:      - (target DB not generated yet)");
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

/// en: SetSpeed + AttachChip + ChipInfo, with one recovery round for the LinkE
/// corrupted-readback state (RedetectChip + detach + re-attach).
/// ja: SetSpeed + AttachChip + ChipInfo。LinkE の壊れ読み値は 1 回だけ復旧を試みる。
fn attach_and_read(
    link: &mut WchLink,
    speed: Speed,
    warnings: &mut Vec<Warning>,
) -> Result<(AttachInfo, Option<ChipInfo>), String> {
    let (attach, status) = attach_once(link, speed)?;
    match status {
        ChipInfoStatus::Ok(ci) => Ok((attach, Some(ci))),
        ChipInfoStatus::NoAnswer => {
            warnings.push(Warning {
                code: "uuid-unavailable".to_owned(),
                msg: "the target did not answer the UUID query (protected, or unsupported by this family)"
                    .to_owned(),
            });
            Ok((attach, None))
        }
        ChipInfoStatus::CorruptedReadback => {
            warnings.push(Warning {
                code: "probe-readback-corrupted".to_owned(),
                msg: "the probe held a corrupted target readback (known LinkE state); recovering via re-detect"
                    .to_owned(),
            });
            let _ = link.redetect_chip();
            let _ = link.detach_chip();
            let (attach2, status2) = attach_once(link, speed)?;
            match status2 {
                ChipInfoStatus::Ok(ci) => Ok((attach2, Some(ci))),
                _ => {
                    warnings.push(Warning {
                        code: "probe-readback-corrupted".to_owned(),
                        msg: "recovery did not produce a clean readback; replug the probe if values look wrong"
                            .to_owned(),
                    });
                    Ok((attach2, None))
                }
            }
        }
    }
}

fn attach_once(link: &mut WchLink, speed: Speed) -> Result<(AttachInfo, ChipInfoStatus), String> {
    link.set_speed(0x01, speed)
        .map_err(|e| format!("SetSpeed failed: {e}"))?;
    let attach = link.attach_chip().map_err(|e| match e {
        WchLinkError::Protocol { reason: 0x55, .. } | WchLinkError::UnexpectedResponse(_) => {
            "no target detected on the debug pins".to_owned()
        }
        other => format!("attach failed: {other}"),
    })?;
    let status = link
        .chip_info()
        .map_err(|e| format!("ChipInfo failed: {e}"))?;
    Ok((attach, status))
}

fn open_with_retry(entry: &Entry) -> Result<WchLink, WchLinkError> {
    let mut last = WchLink::open(&entry.dev);
    for _ in 0..2 {
        match &last {
            Err(WchLinkError::Usb(ch32rv_usb::UsbError::AccessDenied(_))) | Ok(_) => break,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(1));
                last = WchLink::open(&entry.dev);
            }
        }
    }
    last
}

/// en: Parse --speed (low|medium|high|<kHz>), warning when a kHz value is rounded to a step.
/// ja: --speed をパースする。kHz 指定は段階へ丸め、丸めた事実を warning にする。
fn parse_speed(s: &str) -> Result<(Speed, Vec<Warning>), String> {
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Failure envelope that still carries the probe identification.
fn fail_with_probe(
    cli: &Cli,
    probe: ProbeReport,
    kind: ErrorKind,
    msg: impl Into<String>,
    hint: Option<&str>,
) -> ExitCode {
    let msg = msg.into();
    if cli.json {
        let mut env = ResultEnvelope::failure(CMD, kind, msg);
        env.probe = Some(probe);
        if let Some(e) = env.error.as_mut() {
            e.hint = hint.map(str::to_owned);
        }
        let _ = crate::print_envelope(&env);
    } else {
        eprintln!("ch32rv: error[{}]: {msg}", kind.as_str());
        if let Some(h) = hint {
            eprintln!("  hint: {h}");
        }
    }
    kind.exit_code().into()
}
