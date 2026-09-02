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
use ch32rv_contract::policy::{
    ConfirmRunMode, EraseMode, MonitorSource, RecoverMethod, ResetPolicy, VerifyMode,
};
use ch32rv_contract::progress::ProgressSink;
use ch32rv_contract::{ErrorKind, ResultEnvelope, Warning};
use ch32rv_flash::{Image, Segment, params_for_family};
use ch32rv_wchlink::FlashParams as WlFlashParams;

use std::path::Path;

use crate::args::{Cli, FlashArgs, RecoverArgs, SwitchState};
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

/// en: The set of flash pages (page-aligned start addresses) that the given `(addr, len)`
/// segments touch, for a `page`-byte page size. `--erase sector` erases exactly these pages, so
/// it never wipes flash outside the image. A segment that starts mid-page pulls in its whole
/// page; segments that share a page collapse to one entry.
/// ja: `(addr, len)` セグメント群が触れる flash page(page 境界の開始番地)の集合。`--erase sector`
/// はこの page だけを消すので image 外の flash を消さない。page 途中開始はその page 全体を含む。
pub(crate) fn covered_pages(
    segments: impl IntoIterator<Item = (u32, u32)>,
    page: u32,
) -> std::collections::BTreeSet<u32> {
    let mut pages = std::collections::BTreeSet::new();
    for (addr, len) in segments {
        let first = addr - (addr % page);
        let end = addr.saturating_add(len); // exclusive
        let mut p = first;
        while p < end {
            pages.insert(p);
            p = p.saturating_add(page);
        }
    }
    pages
}

/// en: Resolve the effective erase scope. `auto` becomes `chip` for a program loaded from the
/// flash base (a full flash - one fast whole-chip erase) and `sector` for a partial/offset image
/// (never wipe outside it); `--restore-unwritten` forces `sector` since it needs a page-granular
/// erase. Every other mode passes through unchanged.
/// ja: 実効 erase scope を決める。`auto` は flash 先頭から始まる program(=フル)なら `chip`、
/// 部分/offset image なら `sector`。`--restore-unwritten` は page 単位 erase が要るので `sector` に
/// 倒す。他のモードはそのまま。
fn resolve_erase(
    requested: EraseMode,
    base_addr: Option<u32>,
    code_flash_start: u32,
    restore_unwritten: bool,
) -> EraseMode {
    match requested {
        EraseMode::Auto => {
            if base_addr == Some(code_flash_start) && !restore_unwritten {
                EraseMode::Chip
            } else {
                EraseMode::Sector
            }
        }
        other => other,
    }
}

/// en: Overlay `segments` onto one page's pre-read `content` (the page starts at `page_addr` and
/// is `content.len()` bytes): bytes a segment covers take the segment's value, the rest keep their
/// original `content`. Used by `--restore-unwritten` so a page can be re-programmed whole without
/// losing bytes the image does not touch.
/// ja: `content`(page 先頭 `page_addr`、長さ=page サイズ)に `segments` を上書き合成する。segment
/// が覆う byte はその値、他は元の `content` のまま。`--restore-unwritten` で page 全体を再 program
/// するのに使う。
pub(crate) fn overlay_page(page_addr: u32, content: &mut [u8], segments: &[Segment]) {
    let page_end = page_addr + content.len() as u32;
    for seg in segments {
        let lo = seg.addr.max(page_addr);
        let hi = (seg.addr + seg.data.len() as u32).min(page_end);
        for a in lo..hi {
            content[(a - page_addr) as usize] = seg.data[(a - seg.addr) as usize];
        }
    }
}

pub fn flash(cli: &Cli, args: &FlashArgs) -> ExitCode {
    if args.repeat {
        flash_repeat(cli, args)
    } else {
        flash_once(cli, args)
    }
}

/// en: `--repeat` (production): program the current target, then wait for the operator to remove it
/// and insert the next one, and program that too - looping until interrupted (Ctrl-C). A failed
/// board is reported and the loop moves on to the next, matching a production line's flow.
/// ja: `--repeat`(量産): 今の target を焼き、operator が外して次を挿すのを待って焼く、を Ctrl-C まで
/// 繰り返す。失敗 board は報告して次へ進む(産線の流れに合わせる)。
fn flash_repeat(cli: &Cli, args: &FlashArgs) -> ExitCode {
    let mut count = 0u32;
    loop {
        count += 1;
        eprintln!("repeat: programming target #{count} (Ctrl-C to stop)");
        let _ = flash_once(cli, args);
        eprintln!("repeat: remove the programmed target ...");
        wait_for_chip(cli, false);
        eprintln!("repeat: insert the next target ...");
        wait_for_chip(cli, true);
    }
}

/// en: Poll the selected probe until a target chip is present (`want == true`) or absent
/// (`want == false`). The probe (WCH-Link) stays enumerated across a target swap, so this attaches
/// to the chip through it; only Ctrl-C (process signal) breaks the wait.
/// ja: 選択 probe に target chip が有る(want=true)/無い(want=false)になるまで poll。probe 自体は
/// target 交換で再列挙されないので、それ越しに chip へ attach を試す。抜けるのは Ctrl-C のみ。
fn wait_for_chip(cli: &Cli, want: bool) {
    loop {
        if chip_present(cli) == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Whether a target chip currently answers AttachChip on the selected probe.
fn chip_present(cli: &Cli) -> bool {
    let Ok(entry) = select_entry(cli, "flash") else {
        return false;
    };
    let Ok(mut link) = ch32rv_wchlink::WchLink::open(&entry.dev) else {
        return false;
    };
    let _ = link.probe_info();
    let present = link.attach_chip().is_ok();
    let _ = link.detach_chip();
    present
}

fn flash_once(cli: &Cli, args: &FlashArgs) -> ExitCode {
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
    let mut session =
        match Session::attach(&entry, speed, timeout, cli.chip.as_deref(), &mut warnings) {
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

    // --preverify: if the target already holds this exact image, skip erase+program entirely (saves
    // a flash cycle and its wear). Read the image region and compare before doing anything
    // destructive; on a mismatch, reset the link state and fall through to a normal flash.
    if args.preverify {
        sink.event(&Event::Phase {
            name: "preverify".into(),
            total: Some(total),
        });
        let already_matches = {
            let mut dm = session.dm();
            if let Err(e) = dm.halt() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::AttachFailed,
                    format!("halt for preverify failed: {e}"),
                    None,
                );
            }
            let mut all = true;
            for seg in &image.segments {
                match dm.read_mem(seg.addr, seg.data.len() as u32) {
                    Ok(readback) => {
                        if readback != seg.data {
                            all = false;
                            break;
                        }
                    }
                    Err(e) => {
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::TransportTimeout,
                            format!("preverify read failed at {:#010x}: {e}", seg.addr),
                            None,
                        );
                    }
                }
            }
            all
        };
        // We halted the core to read; reset the link/target state before whatever comes next.
        session.link().detach_chip().ok();
        let _ = session.link().attach_chip();
        if already_matches {
            return finish_flash(
                cli,
                CMD,
                session,
                total,
                "none",
                true,
                Some(true),
                warnings,
                args.reset,
                args.confirm_run,
                args.sdi,
                args.monitor,
            );
        }
    }

    // Erase per policy.
    //   chip   - one fast whole-chip erase (~100x faster per area than page erase).
    //   sector - erase only the flash pages the image covers, via the direct FLASH controller, so
    //            nothing outside the image is touched (a bootloader / calibration data in high
    //            flash survives). `sector` was previously a silent alias for chip erase - a
    //            data-loss footgun - and now erases surgically.
    //   auto   - chip for a program loaded from the flash base (a full flash, so the one fast
    //            erase is right), sector for a partial/offset image (never wipe outside it).
    //   none   - skip erase.
    // The chosen scope is reported (JSON `erase`, and a line of normal output) so `auto` is never
    // a mystery.
    let erase = resolve_erase(
        args.erase,
        image.base_addr(),
        fp.code_flash_start,
        args.restore_unwritten,
    );
    let erase_scope = match erase {
        EraseMode::None => "none",
        EraseMode::Chip => "chip",
        EraseMode::Sector => "sector",
        EraseMode::Auto => unreachable!("auto resolved above"),
    };
    // `--restore-unwritten` re-programs whole pages, so it needs a page-granular (sector) erase.
    if args.restore_unwritten && erase != EraseMode::Sector {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("--restore-unwritten needs page-granular erase, but --erase is {erase_scope}"),
            Some("use --erase sector (or --erase auto with a partial/offset image)"),
        );
    }
    // Full pages to program instead of the sparse image, populated only under --restore-unwritten
    // (each covered page read pre-erase, with the image overlaid, so unwritten bytes survive).
    let mut restored: Option<Vec<Segment>> = None;
    match erase {
        EraseMode::Auto => unreachable!("auto resolved above"),
        EraseMode::None => {}
        EraseMode::Chip => {
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
        EraseMode::Sector => {
            // Sector erase needs a verified FLASH-controller profile (page size + mechanism).
            let Some(cprofile) = ch32rv_flash::flash_controller_profile(family) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    format!(
                        "--erase sector is not supported for {} (0x{family:02x}) yet",
                        session.family()
                    ),
                    Some("use --erase chip to erase the whole chip"),
                );
            };
            // restore-unwritten needs a true 0xff readback for erased cells, so a blank byte in a
            // page is not confused with real data and re-programmed as a placeholder.
            if args.restore_unwritten && !cprofile.erased_reads_ff {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    format!(
                        "--restore-unwritten is not supported on {} (erased cells do not read back as 0xff)",
                        session.family()
                    ),
                    Some("omit --restore-unwritten; sector erase clears whole covered pages"),
                );
            }
            let page = cprofile.page_size;
            // Every flash page any segment touches (dedup + sorted, page-aligned). Erase them all
            // before programming so segments that share a page never wipe each other.
            let pages = covered_pages(
                image.segments.iter().map(|s| (s.addr, s.data.len() as u32)),
                page,
            );
            let total_pages = pages.len() as u64;
            sink.event(&Event::Phase {
                name: "erase".into(),
                total: Some(total_pages),
            });
            if let Err(e) = session.dm().halt() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::AttachFailed,
                    format!("halt for sector erase failed: {e}"),
                    None,
                );
            }
            // restore-unwritten: capture each page's current content (before erasing) and overlay
            // the image on it, so the program step rewrites whole pages and unwritten bytes survive.
            if args.restore_unwritten {
                let mut dm = session.dm();
                let mut merged = Vec::with_capacity(pages.len());
                for &pg in &pages {
                    let mut buf = match dm.read_mem(pg, page) {
                        Ok(b) => b,
                        Err(e) => {
                            return fail(
                                cli,
                                CMD,
                                ErrorKind::TransportTimeout,
                                format!("restore-unwritten read of page 0x{pg:08x} failed: {e}"),
                                None,
                            );
                        }
                    };
                    buf.resize(page as usize, 0xff);
                    overlay_page(pg, &mut buf, &image.segments);
                    merged.push(Segment {
                        addr: pg,
                        data: buf,
                    });
                }
                restored = Some(merged);
            }
            {
                let mode = cprofile.mode;
                let mut dm = session.dm();
                for (i, pg) in pages.iter().enumerate() {
                    if let Err(e) = dm.flash_page_erase(*pg, mode) {
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::TransportTimeout,
                            format!("sector erase failed at 0x{pg:08x}: {e}"),
                            None,
                        );
                    }
                    sink.event(&Event::Progress {
                        phase: "erase".into(),
                        done: (i + 1) as u64,
                        total: Some(total_pages),
                    });
                }
            }
            // Reset the link/target debug state so the stub loader programs from a clean slate
            // (mirrors the detach/reattach the verify step does below; verified to program
            // correctly into page-erased - not chip-erased - flash).
            session.link().detach_chip().ok();
            let _ = session.link().attach_chip();
        }
    }

    // Program each segment. Under --restore-unwritten we program the merged whole pages instead of
    // the sparse image (so unwritten bytes in a partially-filled page keep their original values).
    let program_segments: &[Segment] = restored.as_deref().unwrap_or(&image.segments);
    let program_total: u64 = program_segments.iter().map(|s| s.data.len() as u64).sum();
    sink.event(&Event::Phase {
        name: "program".into(),
        total: Some(program_total),
    });
    {
        let s = &sink;
        let mut base = 0u64;
        for seg in program_segments {
            let seg_len = seg.data.len() as u64;
            if let Err(e) = session
                .link()
                .write_flash(&seg.data, seg.addr, &wl, |done| {
                    s.event(&Event::Progress {
                        phase: "program".into(),
                        done: base + done,
                        total: Some(program_total),
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

    finish_flash(
        cli,
        CMD,
        session,
        total,
        erase_scope,
        false,
        verified,
        warnings,
        args.reset,
        args.confirm_run,
        args.sdi,
        args.monitor,
    )
}

/// en: Apply the reset policy (run/halt/none, with `--confirm-run`), optionally set the SDI print
/// state (`--sdi`) and hand off to a monitor session (`--monitor`), then print/emit the result.
/// Shared by the normal end of `flash` and the `--preverify` "already matches" skip path. Takes the
/// session by value so it can be dropped (releasing the probe's USB handle) before a monitor
/// session re-opens the probe.
/// ja: reset 方針を適用し、必要なら SDI print 状態を設定(`--sdi`)して monitor へ移行(`--monitor`)、
/// 結果を出す。`flash` の通常終了と `--preverify` スキップ経路で共有。monitor が probe を開き直せる
/// よう session を値で受け、drop で USB を解放してから渡す。
#[allow(clippy::too_many_arguments)]
fn finish_flash(
    cli: &Cli,
    cmd: &str,
    mut session: Session,
    total: u64,
    erase_scope: &str,
    skipped: bool,
    verified: Option<bool>,
    warnings: Vec<Warning>,
    reset: ResetPolicy,
    confirm: Option<ConfirmRunMode>,
    sdi: Option<SwitchState>,
    monitor: Option<MonitorSource>,
) -> ExitCode {
    let mut running = None;
    match reset {
        ResetPolicy::Run => {
            if let Err(e) = session.link().soft_reset() {
                return fail(
                    cli,
                    cmd,
                    ErrorKind::TransportTimeout,
                    format!("reset failed: {e}"),
                    None,
                );
            }
            if let Some(mode) = confirm {
                std::thread::sleep(Duration::from_millis(200));
                running = Some(confirm_run(&mut session, mode));
                if running == Some(false) {
                    return finish(
                        cli,
                        cmd,
                        &mut session,
                        total,
                        erase_scope,
                        skipped,
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

    // --sdi: set the probe's SDI-print forwarding after the target is (re)started. A failure here
    // must not fail the flash (programming already succeeded) - surface it as a warning.
    if let Some(state) = sdi {
        let on = matches!(state, SwitchState::On);
        if let Err(e) = session.link().set_sdi_print_enabled(on) {
            eprintln!(
                "warning[sdi]: could not set SDI print {}: {e}",
                if on { "on" } else { "off" }
            );
        }
    }

    let exit = finish(
        cli,
        cmd,
        &mut session,
        total,
        erase_scope,
        skipped,
        verified,
        running,
        warnings,
        None,
    );

    // --monitor: hand off to a monitor session (runs until Ctrl-C). Drop the flash session first so
    // its USB handle is released and the monitor backend can open the probe / its CDC port.
    if let Some(source) = monitor {
        drop(session);
        let margs = crate::args::MonitorArgs {
            cmd: None,
            source,
            port: None,
            baud: 115_200,
            timestamps: false,
            log: None,
            raw: false,
            no_reconnect: false,
        };
        return crate::cmd_monitor::monitor(cli, &margs);
    }
    exit
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
    erase_scope: &str,
    skipped: bool,
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
            "skipped": skipped,
            "erase": erase_scope,
            "verify": verified,
            "running": running,
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        if ok {
            if skipped {
                // --preverify: the target already held the image, so nothing was erased/programmed.
                println!("preverify: target already matches - skipped");
            } else {
                println!("flashed {total_bytes} bytes to {}", session.family());
                println!("erase:   {erase_scope}");
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
    Session::attach(&entry, speed, timeout, cli.chip.as_deref(), &mut warnings)
        .map_err(|e| session_error(cli, cmd, e))
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
        RecoverMethod::Unprotect => recover_unprotect(cli),
        other => fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!("recover --method {} is not implemented yet", other.as_str()),
            Some("power-off, nrst, and unprotect are implemented; unbrick comes next"),
        ),
    }
}

/// en: `recover --method unprotect`: clear read protection by writing factory-default option bytes
/// (RDPR=0xA5). On a read-protected target this triggers the chip's mass erase, unbricking it. The
/// target must still attach over DMI (a fully-dead target needs power-off/nrst/unbrick instead).
/// ja: `recover --method unprotect`: 工場 option bytes(RDPR=0xA5)を書いて読み出し保護を解除。保護
/// 済み target ではこれが chip の mass erase を誘発して復旧する。attach は要る(完全死は power-off 等)。
fn recover_unprotect(cli: &Cli) -> ExitCode {
    const CMD: &str = "recover";
    // Factory defaults: RDPR=0xA5 (unprotected), USER/Data/WRPR = 0xff, each with its complement.
    const FACTORY: [u8; 16] = [
        0xA5, 0x5A, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
        0x00,
    ];
    if !cli.yes
        && !cli.non_interactive
        && !confirm("Remove read protection? This ERASES ALL FLASH on a protected target.")
    {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    let mut session = match attach_for(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
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
    if let Err(e) = dm.flash_program_option_bytes(&FACTORY) {
        return fail(
            cli,
            CMD,
            ErrorKind::TransportTimeout,
            format!("writing factory option bytes failed: {e}"),
            None,
        );
    }
    // Apply the option change with a system reset.
    let _ = session.link().soft_reset();
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "method": "unprotect", "family": family, "note": "read protection cleared (RDPR=0xA5); applies after reset",
        }));
        crate::print_envelope(&env)
    } else {
        println!("recover unprotect: read protection cleared (RDPR=0xA5) on {family}");
        println!("note: a protected target is mass-erased; re-flash your firmware");
        ExitCode::SUCCESS
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
pub(crate) fn family_byte_from_name(name: &str) -> Option<u8> {
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
        SessionError::ChipMismatch(msg) => fail(
            cli,
            cmd,
            ErrorKind::TargetAmbiguous,
            msg,
            Some("pass the correct --chip, or omit it to use auto-detection"),
        ),
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

#[cfg(test)]
mod tests {
    use super::{Segment, covered_pages, overlay_page, resolve_erase};
    use ch32rv_contract::policy::EraseMode;

    const BASE: u32 = 0x0800_0000;

    #[test]
    fn auto_full_image_is_chip() {
        // An image loaded from the flash base is a full program -> the one fast whole-chip erase.
        assert_eq!(
            resolve_erase(EraseMode::Auto, Some(BASE), BASE, false),
            EraseMode::Chip
        );
    }

    #[test]
    fn auto_partial_image_is_sector() {
        // An image at an offset must not wipe flash below it.
        assert_eq!(
            resolve_erase(EraseMode::Auto, Some(BASE + 0x8000), BASE, false),
            EraseMode::Sector
        );
    }

    #[test]
    fn auto_with_restore_unwritten_is_sector_even_from_base() {
        // restore-unwritten needs page-granular erase, so it overrides the full-image chip choice.
        assert_eq!(
            resolve_erase(EraseMode::Auto, Some(BASE), BASE, true),
            EraseMode::Sector
        );
    }

    #[test]
    fn explicit_modes_pass_through() {
        for m in [EraseMode::Chip, EraseMode::Sector, EraseMode::None] {
            assert_eq!(resolve_erase(m, Some(BASE), BASE, false), m);
            assert_eq!(resolve_erase(m, Some(BASE + 0x100), BASE, true), m);
        }
    }

    // en: The pages a sector erase must clear for a given image layout.
    fn pages(segs: &[(u32, u32)], page: u32) -> Vec<u32> {
        covered_pages(segs.iter().copied(), page)
            .into_iter()
            .collect()
    }

    fn seg(addr: u32, data: &[u8]) -> Segment {
        Segment {
            addr,
            data: data.to_vec(),
        }
    }

    #[test]
    fn single_aligned_segment_one_page() {
        // A 256-byte segment exactly on a page boundary touches exactly one 256-byte page.
        assert_eq!(pages(&[(0x0800_ff00, 256)], 256), vec![0x0800_ff00]);
    }

    #[test]
    fn segment_spanning_two_pages() {
        // 260 bytes at a page start spills 4 bytes into the next page -> two pages.
        assert_eq!(
            pages(&[(0x0800_0000, 260)], 256),
            vec![0x0800_0000, 0x0800_0100]
        );
    }

    #[test]
    fn unaligned_start_pulls_in_whole_first_page() {
        // Starting mid-page erases from that page's base, not the segment's address.
        assert_eq!(
            pages(&[(0x0800_0080, 256)], 256),
            vec![0x0800_0000, 0x0800_0100]
        );
    }

    #[test]
    fn segments_sharing_a_page_collapse() {
        // Two segments in the same page must not produce a duplicate erase of that page.
        assert_eq!(
            pages(&[(0x0800_0000, 16), (0x0800_0040, 16)], 256),
            vec![0x0800_0000]
        );
    }

    #[test]
    fn full_image_covers_every_page() {
        // A 1 KiB image at the code base covers four 256-byte pages, contiguous and sorted.
        assert_eq!(
            pages(&[(0x0800_0000, 1024)], 256),
            vec![0x0800_0000, 0x0800_0100, 0x0800_0200, 0x0800_0300]
        );
    }

    #[test]
    fn honours_a_128_byte_page_size() {
        // V103 uses 128-byte pages; a 256-byte segment then spans two pages.
        assert_eq!(
            pages(&[(0x0800_0000, 256)], 128),
            vec![0x0800_0000, 0x0800_0080]
        );
    }

    // --- overlay_page (restore-unwritten merge) ---

    #[test]
    fn overlay_writes_only_the_segment_span() {
        // A page pre-read as all-0xff, image writes 4 bytes at the start -> rest stays 0xff.
        let mut page = vec![0xff_u8; 8];
        overlay_page(0x0800_0000, &mut page, &[seg(0x0800_0000, &[1, 2, 3, 4])]);
        assert_eq!(page, [1, 2, 3, 4, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn overlay_preserves_pre_read_bytes_outside_the_image() {
        // The unwritten-byte-preservation case: original content survives where the image is absent.
        let mut page = vec![0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7];
        overlay_page(0x0800_0000, &mut page, &[seg(0x0800_0002, &[0xBB, 0xCC])]);
        assert_eq!(page, [0xA0, 0xA1, 0xBB, 0xCC, 0xA4, 0xA5, 0xA6, 0xA7]);
    }

    #[test]
    fn overlay_clips_a_segment_to_the_page() {
        // A segment that starts before and ends after the page only writes the in-page slice.
        // page covers [0x100, 0x108); segment covers [0x0FE, 0x106) -> writes page[0..6].
        let mut page = vec![0u8; 8];
        overlay_page(
            0x0800_0100,
            &mut page,
            &[seg(0x0800_00fe, &[10, 11, 12, 13, 14, 15, 16, 17])],
        );
        // segment bytes at offsets 2..8 land in the page (0x100-0x0FE = 2).
        assert_eq!(page, [12, 13, 14, 15, 16, 17, 0, 0]);
    }

    #[test]
    fn overlay_ignores_a_segment_in_another_page() {
        // A segment entirely outside the page leaves it untouched.
        let mut page = vec![0x55_u8; 4];
        overlay_page(0x0800_0000, &mut page, &[seg(0x0800_1000, &[1, 2, 3, 4])]);
        assert_eq!(page, [0x55, 0x55, 0x55, 0x55]);
    }
}
