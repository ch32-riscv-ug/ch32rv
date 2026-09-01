//! en: `target info` (docs/cli.ja.md §4.3): attach, read the chip signature and factory
//! UUID/flash size, and detach. Read-only: nothing is written to the target, and the core is
//! always released (detach) on drop of the session, on every path. The LinkE corrupted-readback
//! bug is detected and recovered inside the shared session (board-identify, measured).
//! ja: `target info`。attach → chip 署名と工場 UUID・flash 容量の読み取り → detach。
//! 読み取り専用で、session の drop 時に必ず detach する。LinkE の壊れ読み値バグは共通 session で復旧。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ProbeMode, ResultEnvelope, TargetReport, Warning};

use crate::args::{Cli, SwitchState};
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

const OPTION_BASE: u32 = 0x1FFF_F800;

/// en: Attach a RISC-V session for an option-byte command (shared boilerplate). ja: option 系の
/// 共通 attach。
fn option_session(cli: &Cli, cmd: &str) -> Result<Session, ExitCode> {
    let entry = select_entry(cli, cmd)?;
    if entry.mode != ProbeMode::Riscv {
        return Err(fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; attaching to a target requires RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        ));
    }
    let (speed, mut warnings) =
        parse::speed(&cli.speed).map_err(|msg| fail(cli, cmd, ErrorKind::Usage, msg, None))?;
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    Session::attach(&entry, speed, timeout, &mut warnings).map_err(|e| session_error(cli, cmd, e))
}

/// Confirm a destructive option-byte write: `--yes` skips it, `--non-interactive` without `--yes`
/// refuses, otherwise prompt on the terminal.
fn ob_confirm(cli: &Cli, prompt: &str) -> bool {
    use std::io::Write;
    if cli.yes {
        return true;
    }
    if cli.non_interactive {
        return false;
    }
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    matches!(s.trim(), "y" | "Y" | "yes" | "YES")
}

/// Parse exactly 16 hex bytes (optionally space/`:`-separated) for `option write-raw`.
fn parse_hex16(s: &str) -> Result<[u8; 16], String> {
    let clean: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '_')
        .collect();
    if clean.len() != 32 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "expected 16 hex bytes (32 hex digits), got {} digit(s)",
            clean.len()
        ));
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// en: Read the 16 option bytes (attach, halt, read, detach). ja: 16 byte の option bytes を読む。
fn read_option_bytes(cli: &Cli, cmd: &str) -> Result<[u8; 16], ExitCode> {
    let mut session = option_session(cli, cmd)?;
    let mut dm = session.dm();
    dm.halt().map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        )
    })?;
    let v = dm.read_mem(OPTION_BASE, 16).map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::TransportTimeout,
            format!("reading option bytes failed: {e}"),
            None,
        )
    })?;
    let mut a = [0u8; 16];
    a.copy_from_slice(&v[..16]);
    Ok(a)
}

/// en: Erase + program the 16 option bytes to `new` (value+complement pairs, as `option get`
/// returns), then read back and verify. `new[0]` (RDPR) is programmed first so read protection is
/// re-established immediately. The bytes take effect after a system reset. ja: option bytes を
/// `new` へ erase+program し read-back で検証。RDPR を最初に書く。反映は system reset 後。
fn program_option(cli: &Cli, cmd: &str, new: &[u8; 16]) -> ExitCode {
    let mut session = match option_session(cli, cmd) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let family = session.family();
    let mut dm = session.dm();
    if let Err(e) = dm.halt() {
        return fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }
    let before = match dm.read_mem(OPTION_BASE, 16) {
        Ok(v) => v,
        Err(e) => {
            return fail(
                cli,
                cmd,
                ErrorKind::TransportTimeout,
                format!("reading option bytes failed: {e}"),
                None,
            );
        }
    };
    if let Err(e) = dm.flash_program_option_bytes(new) {
        return fail(
            cli,
            cmd,
            ErrorKind::TransportTimeout,
            format!(
                "programming option bytes failed: {e} - the target may be left with erased (read-protected) option bytes; recover with `ch32rv recover`"
            ),
            None,
        );
    }
    let after = match dm.read_mem(OPTION_BASE, 16) {
        Ok(v) => v,
        Err(e) => {
            return fail(
                cli,
                cmd,
                ErrorKind::TransportTimeout,
                format!("verify read failed: {e}"),
                None,
            );
        }
    };
    // Verify the value bytes (even indices) we asked for actually landed.
    let mismatch = (0..16).step_by(2).find(|&i| after[i] != new[i]);
    if cli.json {
        let mut env = ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({
            "family": family,
            "before": hex(&before),
            "after": hex(&after),
            "verified": mismatch.is_none(),
            "note": "option bytes take effect after a power-on / system reset",
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "option bytes: {} -> {} ({family})",
            hex(&before),
            hex(&after)
        );
        if let Some(i) = mismatch {
            eprintln!(
                "warning[option-verify]: value byte {i} reads back 0x{:02x}, not the requested 0x{:02x}",
                after[i], new[i]
            );
        }
        println!("note: option bytes take effect after a power-on / system reset");
        ExitCode::SUCCESS
    }
}

/// `target option write-raw <hex>`: overwrite the 16 option bytes with a raw value (expert).
pub fn option_write_raw(cli: &Cli, hexstr: &str) -> ExitCode {
    const CMD: &str = "target.option.write-raw";
    let bytes = match parse_hex16(hexstr) {
        Ok(b) => b,
        Err(m) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                m,
                Some("e.g. a55aff00ff00ff00ff00ff00ff00ff00 (16 bytes: RDPR nRDPR USER nUSER ...)"),
            );
        }
    };
    if bytes[0] != 0xA5
        && !ob_confirm(
            cli,
            &format!(
                "RDPR byte is 0x{:02x} (not 0xA5): this ENABLES read protection - flash becomes unreadable until you unprotect (which erases it). Continue?",
                bytes[0]
            ),
        )
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "aborted: RDPR would enable read protection (pass --yes to force)",
            None,
        );
    }
    if !ob_confirm(cli, "Overwrite the target's option bytes?") {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    program_option(cli, CMD, &bytes)
}

/// `target option reset`: restore factory-default option bytes (RDPR off, USER/Data/WRP cleared).
pub fn option_reset(cli: &Cli) -> ExitCode {
    const CMD: &str = "target.option.reset";
    // RDPR=0xA5 (unprotected), USER/Data0/Data1/WRPR0..3 = 0xff, each followed by its complement.
    let defaults: [u8; 16] = [
        0xA5, 0x5A, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
        0x00,
    ];
    if !ob_confirm(
        cli,
        "Restore factory-default option bytes (RDPR off, USER/Data/WRP cleared)?",
    ) {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    program_option(cli, CMD, &defaults)
}

/// `target protect on|off`: enable/disable flash read protection (RDPR). Turning it OFF triggers a
/// full mass erase of the target's flash.
pub fn protect(cli: &Cli, state: SwitchState) -> ExitCode {
    const CMD: &str = "target.protect";
    let mut ob = match read_option_bytes(cli, CMD) {
        Ok(b) => b,
        Err(c) => return c,
    };
    match state {
        SwitchState::On => {
            if ob[0] != 0xA5 {
                println!("read protection is already ON (RDPR=0x{:02x})", ob[0]);
                return ExitCode::SUCCESS;
            }
            if !ob_confirm(
                cli,
                "Enable read protection? The flash becomes unreadable/undebuggable until you turn it OFF (which ERASES all flash).",
            ) {
                return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
            }
            ob[0] = 0xFF;
            ob[1] = 0x00;
        }
        SwitchState::Off => {
            if ob[0] == 0xA5 {
                println!("read protection is already OFF (RDPR=0xA5)");
                return ExitCode::SUCCESS;
            }
            if !ob_confirm(
                cli,
                "Disable read protection? This ERASES ALL FLASH (mass erase) on the target.",
            ) {
                return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
            }
            ob[0] = 0xA5;
            ob[1] = 0x5A;
        }
    }
    program_option(cli, CMD, &ob)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::parse_hex16;

    #[test]
    fn parses_16_contiguous_bytes() {
        let b = parse_hex16("a55aff00ff00ff00ff00ff00ff00ff00").unwrap();
        assert_eq!(b[0], 0xA5);
        assert_eq!(b[1], 0x5A);
        assert_eq!(b[15], 0x00);
    }

    #[test]
    fn accepts_separators() {
        let a = parse_hex16("a5:5a:ff:00:ff:00:ff:00:ff:00:ff:00:ff:00:ff:00").unwrap();
        let b = parse_hex16("a5 5a ff 00 ff 00 ff 00 ff 00 ff 00 ff 00 ff 00").unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0], 0xA5);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(parse_hex16("abcd").is_err());
        assert!(parse_hex16("a55aff00ff00ff00ff00ff00ff00ff0000").is_err()); // 17 bytes
    }

    #[test]
    fn rejects_non_hex() {
        assert!(parse_hex16("zz5aff00ff00ff00ff00ff00ff00ff00").is_err());
    }
}
