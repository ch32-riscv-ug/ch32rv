//! en: `target info` (docs/cli.ja.md §4.3): attach, read the chip signature and factory
//! UUID/flash size, and detach. Read-only: nothing is written to the target, and the core is
//! always released (detach) on drop of the session, on every path. The LinkE corrupted-readback
//! bug is detected and recovered inside the shared session (board-identify, measured).
//! ja: `target info`。attach → chip 署名と工場 UUID・flash 容量の読み取り → detach。
//! 読み取り専用で、session の drop 時に必ず detach する。LinkE の壊れ読み値バグは共通 session で復旧。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ProbeMode, ResultEnvelope, TargetReport, Warning};

use crate::args::Cli;
use crate::cmd_probe::{
    apply_probe_info, base_report, fail, mode_str, print_probe_human, select_entry,
};
use crate::parse;
use crate::session::{Session, SessionError};

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
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(msg) => return fail(cli, CMD, ErrorKind::Usage, msg, None),
    };

    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let session = match Session::attach(&entry, speed, timeout, &mut warnings) {
        Ok(s) => s,
        Err(e) => return session_error(cli, e),
    };

    let mut probe_report = base_report(&entry);
    apply_probe_info(&mut probe_report, &session.probe_info, &mut warnings);

    let family = session.family();
    if family.starts_with("unknown") {
        warnings.push(Warning {
            code: "family-unknown".to_owned(),
            msg: format!(
                "family byte 0x{:02x} is not in the known table (possibly a gap series) - worth recording for data request 0001",
                session.attach.family_byte
            ),
        });
    }
    warnings.push(Warning {
        code: "db-empty".to_owned(),
        msg: "SKU resolution unavailable: the target DB is not generated yet (data request 0001 pending)".to_owned(),
    });

    let chip = session.chip;
    let target = TargetReport {
        sku: None,
        family: Some(family),
        chip_id: Some(format!("0x{:08x}", session.attach.chip_id)),
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

fn session_error(cli: &Cli, e: SessionError) -> ExitCode {
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
            fail(
                cli,
                CMD,
                kind,
                s,
                Some("check permissions/driver binding, or whether another tool holds the probe"),
            )
        }
        SessionError::Attach(msg) => fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            msg,
            Some(
                "check target wiring/power/BOOT; for a protected or bricked target see `ch32rv recover`",
            ),
        ),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
