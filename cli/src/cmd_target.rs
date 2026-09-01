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
        Err(e) => return session_error(cli, CMD, e),
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

/// en: `target option get` (docs/cli.ja.md §4.3): read the option bytes (0x1FFF_F800, 16 bytes)
/// over DMI and decode the common fields (read protection, the USER byte's IWDG/STOP/STANDBY
/// bits, the Data0/Data1 user bytes, and the WRP write-protect mask). Read-only. Family-specific
/// USER bits need the generated target DB, so the raw bytes are always shown and the structured
/// decode is marked interim.
/// ja: `target option get`。option bytes(0x1FFF_F800、16 byte)を DMI で読み、共通フィールド
/// (読み出し保護・USER の IWDG/STOP/STANDBY・Data0/Data1・WRP)を復号。読み取り専用。family 固有の
/// USER ビットは DB 生成後。生バイトは常に表示し、構造化復号は暫定扱い。
pub fn option_get(cli: &Cli) -> ExitCode {
    const CMD: &str = "target.option.get";
    const OPTION_BASE: u32 = 0x1FFF_F800;
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
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(msg) => return fail(cli, CMD, ErrorKind::Usage, msg, None),
    };
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let mut session = match Session::attach(&entry, speed, timeout, &mut warnings) {
        Ok(s) => s,
        Err(e) => return session_error(cli, CMD, e),
    };

    let family = session.family();
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
    let raw = match dm.read_mem(OPTION_BASE, 16) {
        Ok(v) => v,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::TransportTimeout,
                format!("reading option bytes failed: {e}"),
                None,
            );
        }
    };

    // Layout (STM32F1-style, shared by CH32V0/V1/V2/V3/X0): each logical byte is stored with its
    // complement. [0]=RDPR [2]=USER [4]=Data0 [6]=Data1 [8/10/12/14]=WRPR0..3.
    let rdpr = raw[0];
    let user = raw[2];
    let data0 = raw[4];
    let data1 = raw[6];
    let wrpr = u32::from(raw[8])
        | u32::from(raw[10]) << 8
        | u32::from(raw[12]) << 16
        | u32::from(raw[14]) << 24;
    // RDPR == 0xA5 means read-out protection disabled (the factory/unprotected value).
    let unprotected = rdpr == 0xA5;
    // Common USER bits (family-specific bits above bit2 need the DB).
    let iwdg_sw = user & 0x01 != 0; // 1 = software IWDG, 0 = hardware (starts at reset)
    let nrst_stop = user & 0x02 != 0; // 0 = reset generated on entering STOP
    let nrst_stdby = user & 0x04 != 0; // 0 = reset generated on entering STANDBY

    warnings.push(Warning {
        code: "option-decode-interim".to_owned(),
        msg: "structured decode is interim: only the common RDP/USER/Data/WRP fields are decoded; family-specific USER bits arrive with the target DB (data request 0003)".to_owned(),
    });

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "family": family,
            "raw": hex(&raw),
            "read_protection": if unprotected { "off" } else { "on" },
            "rdpr": format!("0x{rdpr:02x}"),
            "user": format!("0x{user:02x}"),
            "iwdg": if iwdg_sw { "software" } else { "hardware" },
            "nrst_stop": nrst_stop,
            "nrst_standby": nrst_stdby,
            "data0": format!("0x{data0:02x}"),
            "data1": format!("0x{data1:02x}"),
            "wrpr": format!("0x{wrpr:08x}"),
            "write_protected": wrpr != 0xFFFF_FFFF,
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        println!("family:          {family}");
        println!("raw:             {}", hex(&raw));
        println!(
            "read protection: {}  (RDPR=0x{rdpr:02x}{})",
            if unprotected { "off" } else { "ON" },
            if unprotected {
                ", 0xA5=unprotected"
            } else {
                ""
            }
        );
        println!(
            "user (0x{user:02x}):     IWDG={}  nRST_STOP={}  nRST_STDBY={}",
            if iwdg_sw { "software" } else { "hardware" },
            u8::from(nrst_stop),
            u8::from(nrst_stdby),
        );
        println!("data0/data1:     0x{data0:02x} / 0x{data1:02x}");
        println!(
            "write protect:   0x{wrpr:08x}  ({})",
            if wrpr == 0xFFFF_FFFF {
                "none (all pages writable)"
            } else {
                "some pages write-protected"
            }
        );
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
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
            fail(
                cli,
                cmd,
                kind,
                s,
                Some("check permissions/driver binding, or whether another tool holds the probe"),
            )
        }
        SessionError::Attach(msg) => fail(
            cli,
            cmd,
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
