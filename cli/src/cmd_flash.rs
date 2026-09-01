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
use ch32rv_flash::{Image, params_for_family};
use ch32rv_wchlink::FlashParams as WlFlashParams;

use std::path::Path;

use crate::args::{Cli, FlashArgs, RecoverArgs};
use crate::cmd_probe::{fail, mode_str, select_entry};
use crate::parse;
use crate::session::{Session, SessionError};

/// en: Parse an image, treating a magic-less `.bin`/extensionless file as raw bin under
/// `--format auto` (ELF/HEX/UF2 are still detected by magic; anything else still errors).
/// ja: `--format auto` で magic の無い `.bin`/拡張子無しは raw bin 扱いにする。
fn parse_image(
    bytes: &[u8],
    format: ch32rv_contract::policy::ImageFormat,
    path: &Path,
    bin_offset: Option<u32>,
    code_flash_start: u32,
) -> Result<Image, ch32rv_flash::ImageError> {
    use ch32rv_contract::policy::ImageFormat;
    match Image::parse(bytes, format, bin_offset, code_flash_start) {
        Err(ch32rv_flash::ImageError::UnknownFormat) if format == ImageFormat::Auto => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("bin") | None) {
                Image::parse(bytes, ImageFormat::Bin, bin_offset, code_flash_start)
            } else {
                Err(ch32rv_flash::ImageError::UnknownFormat)
            }
        }
        other => other,
    }
}

pub fn flash(cli: &Cli, args: &FlashArgs) -> ExitCode {
    const CMD: &str = "flash";
    let bytes = match std::fs::read(&args.file) {
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

    // Parse the input into flash segments (ELF / Intel HEX / UF2 / raw bin).
    let bin_offset = match &args.offset {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => Some(a),
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => None,
    };
    let image = match parse_image(
        &bytes,
        args.format,
        &args.file,
        bin_offset,
        fp.code_flash_start,
    ) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
    };
    // The probe reports flash size; use it to reject out-of-range segments.
    let flash_size = session
        .chip
        .as_ref()
        .map(|c| u32::from(c.flash_kb) * 1024)
        .unwrap_or(0);
    if flash_size > 0
        && let Err(e) = image.check_within_flash(fp.code_flash_start, flash_size)
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            e.to_string(),
            Some("wrong --chip or --offset?"),
        );
    }
    let total = image.total_len() as u64;

    let wl = WlFlashParams {
        stub: fp.stub,
        data_packet_size: fp.data_packet_size,
        write_pack_size: fp.write_pack_size,
        supports_protect: fp.supports_protect,
        supports_special_erase: fp.supports_special_erase,
    };

    let sink = crate::progress::sink(cli);

    // Erase (once for the whole chip).
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

    // Program each segment.
    sink.event(&Event::Phase {
        name: "program".into(),
        total: Some(total),
    });
    {
        let s = &sink;
        let mut base = 0u64;
        for seg in &image.segments {
            let seg_len = seg.data.len() as u64;
            if let Err(e) = session
                .link()
                .write_flash(&seg.data, seg.addr, &wl, |done| {
                    s.event(&Event::Progress {
                        phase: "program".into(),
                        done: base + done,
                        total: Some(total),
                    });
                })
            {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransportTimeout,
                    format!("program failed at {:#010x}: {e}", seg.addr),
                    None,
                );
            }
            base += seg_len;
        }
    }

    // Verify (readback), unless disabled.
    let mut verified = None;
    if !matches!(args.verify, VerifyMode::None) {
        sink.event(&Event::Phase {
            name: "verify".into(),
            total: Some(total),
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
        for seg in &image.segments {
            match dm.read_mem(seg.addr, seg.data.len() as u32) {
                Ok(readback) => {
                    if readback != seg.data {
                        let at = readback
                            .iter()
                            .zip(&seg.data)
                            .position(|(a, b)| a != b)
                            .unwrap_or(0);
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::VerifyMismatch,
                            format!("verify mismatch at {:#010x}", seg.addr as usize + at),
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
        verified = Some(true);
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
                        total,
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
        total,
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
    total_bytes: u64,
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
            "bytes": total_bytes,
            "family": session.family(),
            "verify": verified,
            "running": running,
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        if ok {
            println!("flashed {total_bytes} bytes to {}", session.family());
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

/// en: Attach helper shared by erase/reset/verify: select a RISC-V probe and attach.
/// ja: erase/reset/verify 共通の attach。RISC-V probe を選んで attach する。
fn attach_for(cli: &Cli, cmd: &str) -> Result<Session, ExitCode> {
    let entry = select_entry(cli, cmd)?;
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return Err(fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; this needs RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        ));
    }
    let (speed, mut warnings) =
        parse::speed(&cli.speed).map_err(|m| fail(cli, cmd, ErrorKind::Usage, m, None))?;
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    Session::attach(&entry, speed, timeout, &mut warnings).map_err(|e| session_error(cli, cmd, e))
}

pub fn erase(cli: &Cli, args: &crate::args::EraseArgs) -> ExitCode {
    const CMD: &str = "erase";
    if args.region.is_some() || args.range.is_some() {
        return erase_range(cli, args);
    }
    // --all: whole-chip erase.
    let mut session = match attach_for(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if !cli.yes && !cli.non_interactive && !confirm("Erase the entire chip flash?") {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    if let Err(e) = session.link().erase_flash() {
        return fail(
            cli,
            CMD,
            ErrorKind::TransportTimeout,
            format!("erase failed: {e}"),
            None,
        );
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "scope": "chip", "family": session.family() }));
        crate::print_envelope(&env)
    } else {
        println!("erased entire chip flash ({})", session.family());
        ExitCode::SUCCESS
    }
}

/// en: `erase --range <a+len|a..b>` / `--region code[+off+len]`: page-granular erase via the
/// direct FLASH controller (docs/cli.ja.md §4.1). Requires page alignment (fail-closed) since
/// erase is per-page. `code` resolves to the probe-reported code-flash window.
/// ja: `erase --range/--region` を FLASH controller の page 単位消去で実装。page 境界必須。
fn erase_range(cli: &Cli, args: &crate::args::EraseArgs) -> ExitCode {
    const CMD: &str = "erase";
    let mut session = match attach_for(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let family = session.attach.family_byte;
    let Some(profile) = ch32rv_flash::flash_controller_profile(family) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "range/region erase is not yet supported for {} (0x{family:02x})",
                session.family()
            ),
            Some(
                "verified so far: V20x/V30x, V003/CH641, X035/CH643, L103 (CH32V103 is a follow-up)",
            ),
        );
    };
    let page = profile.page_size;

    // Resolve (start, len) from --range or --region.
    let (start, len) = if let Some(r) = &args.range {
        match parse::range(r) {
            Ok(v) => v,
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        }
    } else if let Some(region) = &args.region {
        match resolve_region(region, &session) {
            Ok(v) => v,
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        }
    } else {
        return fail(cli, CMD, ErrorKind::Usage, "no --range or --region", None);
    };

    if len == 0 {
        return fail(cli, CMD, ErrorKind::Usage, "empty range", None);
    }
    // Erase is per-page: demand page alignment so we never silently wipe neighbours.
    if start % page != 0 || len % page != 0 {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("range 0x{start:08x}+0x{len:x} is not aligned to the {page}-byte flash page"),
            Some("align both the start and the length to the page size"),
        );
    }
    let pages = len / page;
    let end = start.saturating_add(len);

    if !cli.yes
        && !cli.non_interactive
        && !confirm(&format!(
            "Erase {len} bytes ({pages} page(s)) at 0x{start:08x}..0x{end:08x}?"
        ))
    {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }

    if let Err(e) = session.dm().halt() {
        return fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }
    let mode = profile.mode;
    let mut dm = session.dm();
    for i in 0..pages {
        let addr = start + i * page;
        if let Err(e) = dm.flash_page_erase(addr, mode) {
            return fail(
                cli,
                CMD,
                ErrorKind::TransportTimeout,
                format!("page erase failed at 0x{addr:08x}: {e}"),
                None,
            );
        }
    }

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "scope": "range",
            "start": format!("0x{start:08x}"),
            "len": len,
            "pages": pages,
            "page_size": page,
            "family": session.family(),
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "erased {pages} page(s) ({len} bytes) at 0x{start:08x}..0x{end:08x} ({})",
            session.family()
        );
        ExitCode::SUCCESS
    }
}

/// en: Resolve a `--region` spec into (start, len). Supports `code[+off[+len]]` for now; the
/// bare name means the whole code-flash window (probe-reported size). Other named regions
/// arrive with the generated target DB.
/// ja: `--region` を (start, len) に解決。今は `code[+off[+len]]` に対応(bare は code flash 全体)。
fn resolve_region(spec: &str, session: &Session) -> Result<(u32, u32), String> {
    let mut it = spec.split('+');
    let name = it.next().unwrap_or("");
    if name != "code" {
        return Err(format!(
            "region `{name}` is not supported yet (only `code`); use --range for an explicit range"
        ));
    }
    let base = 0x0800_0000u32;
    let Some(chip) = &session.chip else {
        return Err("code region needs the flash size, which the probe did not report".to_owned());
    };
    let flash_len = u32::from(chip.flash_kb) * 1024;
    let off = match it.next() {
        Some(s) => parse::byte_len(s)?,
        None => 0,
    };
    let len = match it.next() {
        Some(s) => parse::byte_len(s)?,
        None => flash_len.saturating_sub(off),
    };
    if off.saturating_add(len) > flash_len {
        return Err(format!(
            "region extends past the {flash_len}-byte code flash"
        ));
    }
    Ok((base + off, len))
}

pub fn reset(cli: &Cli, args: &crate::args::ResetArgs) -> ExitCode {
    const CMD: &str = "reset";
    let mut session = match attach_for(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if args.dm {
        // Reset the debug module only (no target reset).
        // Best-effort: DMCONTROL dmactive toggle is inside the DM layer's halt path; here we
        // just detach/re-attach which re-initializes the DM.
        let _ = session.link().detach_chip();
        let _ = session.link().attach_chip();
    } else if args.halt {
        // Reset and halt: soft reset, then halt immediately.
        if let Err(e) = session.link().soft_reset() {
            return fail(
                cli,
                CMD,
                ErrorKind::TransportTimeout,
                format!("reset failed: {e}"),
                None,
            );
        }
        let mut dm = session.dm();
        if let Err(e) = dm.halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt after reset failed: {e}"),
                None,
            );
        }
    } else if let Err(e) = session.link().soft_reset() {
        return fail(
            cli,
            CMD,
            ErrorKind::TransportTimeout,
            format!("reset failed: {e}"),
            None,
        );
    }

    let mut running = None;
    if let Some(mode) = args.confirm_run
        && !args.halt
        && !args.dm
    {
        std::thread::sleep(Duration::from_millis(200));
        running = Some(confirm_run(&mut session, mode));
    }

    if cli.json {
        let mode = if args.dm {
            "dm"
        } else if args.halt {
            "halt"
        } else {
            "run"
        };
        let mut env = if running == Some(false) {
            ResultEnvelope::failure(
                CMD,
                ErrorKind::NotRunningAfterWrite,
                "target not running after reset",
            )
        } else {
            ResultEnvelope::success(CMD)
        };
        env.result = Some(serde_json::json!({ "mode": mode, "running": running }));
        crate::print_envelope(&env)
    } else {
        let what = if args.dm {
            "debug module reset"
        } else if args.halt {
            "reset and halted"
        } else {
            "reset, running"
        };
        println!("{what}");
        if running == Some(false) {
            eprintln!("ch32rv: error[not-running-after-write]: target not running after reset");
            return ErrorKind::NotRunningAfterWrite.exit_code().into();
        }
        ExitCode::SUCCESS
    }
}

pub fn verify(cli: &Cli, args: &crate::args::VerifyArgs) -> ExitCode {
    const CMD: &str = "verify";
    let bytes = match std::fs::read(&args.file) {
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
    let bin_offset = match &args.offset {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => Some(a),
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => None,
    };
    let mut session = match attach_for(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let Some(fp) = params_for_family(session.attach.family_byte) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "family not supported for verify",
            None,
        );
    };
    let image = match parse_image(
        &bytes,
        args.format,
        &args.file,
        bin_offset,
        fp.code_flash_start,
    ) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
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
    for seg in &image.segments {
        match dm.read_mem(seg.addr, seg.data.len() as u32) {
            Ok(readback) => {
                if readback != seg.data {
                    let at = readback
                        .iter()
                        .zip(&seg.data)
                        .position(|(a, b)| a != b)
                        .unwrap_or(0);
                    if cli.json {
                        let env = ResultEnvelope::failure(
                            CMD,
                            ErrorKind::VerifyMismatch,
                            format!("mismatch at {:#010x}", seg.addr as usize + at),
                        );
                        return crate::print_envelope(&env);
                    }
                    eprintln!("verify: MISMATCH at {:#010x}", seg.addr as usize + at);
                    return ErrorKind::VerifyMismatch.exit_code().into();
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
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "bytes": image.total_len(), "match": true }));
        crate::print_envelope(&env)
    } else {
        println!("verify: OK ({} bytes match)", image.total_len());
        ExitCode::SUCCESS
    }
}

/// Simple y/N confirmation on stderr/stdin.
fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes")
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
