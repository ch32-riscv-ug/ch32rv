//! en: `probe list` / `probe info` (docs/cli.ja.md §4.2), plus probe selection helpers shared
//! with other commands. Read-only against the probe: only GetProbeInfo is issued here; no
//! target attach, no reset, no power control.
//! ja: `probe list` / `probe info` と、他 command と共有する probe 選択ヘルパー。
//! probe への読み取りのみ(GetProbeInfo だけを発行し、attach・reset・電源制御は行わない)。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{
    ErrorKind, FirmwareVersion, ProbeMode, ProbeReport, ResultEnvelope, Warning,
};
use ch32rv_usb::{ResolveError, Selector, UsbDeviceInfo, UsbError};
use ch32rv_wchlink::{self as wchlink, WchLink, known_bad_firmware};

use crate::args::Cli;
use crate::parse;
use crate::session::{Session, SessionError};

/// A WCH device relevant to `probe` commands, with its mode derived from VID:PID.
pub(crate) struct Entry {
    pub(crate) dev: UsbDeviceInfo,
    pub(crate) mode: ProbeMode,
}

pub(crate) fn wch_devices() -> Result<Vec<Entry>, UsbError> {
    let devs = ch32rv_usb::enumerate()?;
    Ok(devs
        .into_iter()
        .filter_map(|dev| {
            let mode = match (dev.vid(), dev.pid()) {
                (wchlink::VID_WCH, wchlink::PID_LINK_RISCV | wchlink::PID_LINK_RISCV2) => {
                    ProbeMode::Riscv
                }
                (wchlink::VID_WCH, wchlink::PID_LINK_DAP) => ProbeMode::Dap,
                (wchlink::VID_IAP, wchlink::PID_IAP) => ProbeMode::Iap,
                _ => return None,
            };
            Some(Entry { dev, mode })
        })
        .collect())
}

pub(crate) fn mode_str(mode: ProbeMode) -> &'static str {
    match mode {
        ProbeMode::Riscv => "riscv",
        ProbeMode::Dap => "dap",
        ProbeMode::Iap => "iap",
        ProbeMode::Isp => "isp",
        ProbeMode::Unknown => "unknown",
    }
}

/// en: Resolve --probe (env/config aliases included) to exactly one WCH device, fail-closed.
/// Shared by every probe-routed command.
/// ja: --probe(env・設定別名込み)を fail-closed にちょうど 1 台へ解決する。
/// probe 経路の全 command が共用する。
pub(crate) fn select_entry(cli: &Cli, cmd: &str) -> Result<Entry, ExitCode> {
    let mut entries = wch_devices().map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::DeviceOpenFailed,
            e.to_string(),
            Some("USB enumeration failed; check permissions/udev (`ch32rv doctor`)"),
        )
    })?;
    let selector = parse_selector(cli, cmd)?;
    let idx = match ch32rv_usb::resolve(selector.as_ref(), entries.iter().map(|e| &e.dev)) {
        Ok(i) => i,
        Err(ResolveError::NotFound) => {
            return Err(fail(
                cli,
                cmd,
                ErrorKind::DeviceNotFound,
                "no probe matched the selector",
                Some("run `ch32rv probe list` to see available probes"),
            ));
        }
        Err(ResolveError::Ambiguous(indices)) => {
            let candidates: Vec<serde_json::Value> = indices
                .iter()
                .filter_map(|&i| entries.get(i))
                .map(|e| {
                    serde_json::json!({
                        "usb": e.dev.usb_id(),
                        "serial": e.dev.serial(),
                        "topology": e.dev.topology(),
                        "mode": mode_str(e.mode),
                    })
                })
                .collect();
            return Err(fail_with_candidates(
                cli,
                cmd,
                ErrorKind::DeviceAmbiguous,
                format!("{} probes match; specify --probe", candidates.len()),
                Some("select one with --probe VID:PID:SERIAL or usb:<bus>-<ports>"),
                candidates,
            ));
        }
        Err(ResolveError::UnresolvedName(n)) => {
            return Err(fail(
                cli,
                cmd,
                ErrorKind::Usage,
                format!("alias `{n}` not found in ch32rv.toml / ~/.config/ch32rv/config.toml"),
                None,
            ));
        }
    };
    Ok(entries.swap_remove(idx))
}

/// en: Probe report from enumeration data alone (no device I/O).
/// ja: 列挙情報だけで作る probe report(device I/O なし)。
pub(crate) fn base_report(entry: &Entry) -> ProbeReport {
    ProbeReport {
        model: match entry.mode {
            ProbeMode::Dap => "WCH-Link (DAP mode)".to_owned(),
            ProbeMode::Iap => "WCH-Link (IAP mode) or WCH factory ISP device".to_owned(),
            _ => entry.dev.product().unwrap_or("WCH-Link").to_owned(),
        },
        serial: entry.dev.serial().map(str::to_owned),
        usb: Some(entry.dev.usb_id()),
        topology: Some(entry.dev.topology()),
        mode: Some(entry.mode),
        firmware: None,
        ports: entry.dev.serial_ports(),
    }
}

/// en: Merge a GetProbeInfo result into a report, attaching contract warnings.
/// ja: GetProbeInfo の結果を report へ反映し、契約上の warning を付ける。
pub(crate) fn apply_probe_info(
    report: &mut ProbeReport,
    info: &wchlink::ProbeInfo,
    warnings: &mut Vec<Warning>,
) {
    let mut fw = FirmwareVersion::from_major_minor(info.fw_major, info.fw_minor);
    fw.mode = info.fw_mode.map(|m| m.as_str().to_owned());
    if let Some(msg) = known_bad_firmware(info.fw_major, info.fw_minor) {
        fw.known_bad = Some(true);
        warnings.push(Warning {
            code: "fw-known-bad".to_owned(),
            msg: msg.to_owned(),
        });
    } else {
        fw.known_bad = Some(false);
    }
    let model = info.variant.name();
    if model.contains("unknown variant") {
        warnings.push(Warning {
            code: "probe-variant-unknown".to_owned(),
            msg: format!("unrecognized probe variant: {model}"),
        });
    }
    report.model = model;
    report.firmware = Some(fw);
}

pub(crate) fn report_for(entry: &Entry) -> (ProbeReport, Vec<Warning>, Option<String>) {
    let mut report = base_report(entry);
    let mut warnings = Vec::new();
    let mut error = None;
    if entry.mode == ProbeMode::Riscv {
        let queried = WchLink::open(&entry.dev).and_then(|mut link| link.probe_info());
        match queried {
            Ok(info) => apply_probe_info(&mut report, &info, &mut warnings),
            Err(e) => error = Some(e.to_string()),
        }
    }
    (report, warnings, error)
}

/// en: Like [`report_for`] but with the open retry from docs/cli.ja.md §3.7 (3 attempts,
/// 1 s apart; permission errors are not retried).
/// ja: [`report_for`] + open 再試行(1 秒間隔 3 回。権限エラーは再試行しない)。
pub(crate) fn report_with_retry(entry: &Entry) -> (ProbeReport, Vec<Warning>, Option<String>) {
    let mut last = report_for(entry);
    if entry.mode == ProbeMode::Riscv {
        for _ in 0..2 {
            match &last.2 {
                Some(e) if !e.contains("access denied") => {
                    std::thread::sleep(Duration::from_secs(1));
                    last = report_for(entry);
                }
                _ => break,
            }
        }
    }
    last
}

/// en: `probe list --watch`: poll enumeration and print a `+`/`-` line as WCH devices are
/// plugged/unplugged, until Ctrl-C. Human-only (a live stream is not a single JSON object).
/// ja: `probe list --watch`。列挙を poll し、WCH device の挿抜を `+`/`-` 行で Ctrl-C まで表示。
fn watch_probes(cli: &Cli) -> ExitCode {
    if cli.json {
        return fail(
            cli,
            "probe.list",
            ErrorKind::Usage,
            "--watch streams events and has no single-object JSON form",
            Some("run `probe list --json` without --watch for a snapshot"),
        );
    }
    eprintln!("watching for WCH-Link / ISP devices (Ctrl-C to stop) ...");
    let mut present: Vec<(String, String)> = Vec::new(); // (key, description)
    loop {
        let entries = wch_devices().unwrap_or_default();
        let current: Vec<(String, String)> = entries
            .iter()
            .map(|e| {
                let key = e
                    .dev
                    .serial()
                    .map(str::to_owned)
                    .unwrap_or_else(|| e.dev.topology());
                let desc = format!(
                    "{:<5} {} {} {}",
                    mode_str(e.mode),
                    e.dev.usb_id(),
                    e.dev.serial().unwrap_or("-"),
                    e.dev.topology()
                );
                (key, desc)
            })
            .collect();
        // Removed devices.
        for (key, desc) in &present {
            if !current.iter().any(|(k, _)| k == key) {
                println!("- {desc}");
            }
        }
        // Added devices.
        for (key, desc) in &current {
            if !present.iter().any(|(k, _)| k == key) {
                println!("+ {desc}");
            }
        }
        // Flush so events appear live even when stdout is a pipe/file (block-buffered).
        let _ = std::io::Write::flush(&mut std::io::stdout());
        present = current;
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn list(cli: &Cli, watch: bool) -> ExitCode {
    if watch {
        return watch_probes(cli);
    }
    let entries = match wch_devices() {
        Ok(e) => e,
        Err(e) => {
            return fail(
                cli,
                "probe.list",
                ErrorKind::DeviceOpenFailed,
                e.to_string(),
                Some("USB enumeration failed; check permissions/udev (`ch32rv doctor`)"),
            );
        }
    };

    let mut probes_json = Vec::new();
    let mut lines = Vec::new();
    let mut all_warnings = Vec::new();
    for entry in &entries {
        let (report, warnings, error) = report_for(entry);
        lines.push(format_row(&report, error.as_deref()));
        let mut v = match serde_json::to_value(&report) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    cli,
                    "probe.list",
                    ErrorKind::DeviceOpenFailed,
                    e.to_string(),
                    Some("USB enumeration failed; check permissions/udev (`ch32rv doctor`)"),
                );
            }
        };
        if let (Some(err), Some(obj)) = (error, v.as_object_mut()) {
            obj.insert("probe_error".to_owned(), serde_json::Value::String(err));
        }
        probes_json.push(v);
        all_warnings.extend(warnings);
    }

    if cli.json {
        let mut env = ResultEnvelope::success("probe.list");
        env.result = Some(serde_json::json!({ "probes": probes_json }));
        env.warnings = all_warnings;
        crate::print_envelope(&env)
    } else {
        if entries.is_empty() {
            eprintln!("no WCH-Link / ISP devices found (run `ch32rv doctor` for diagnostics)");
        } else {
            println!(
                "{:<6} {:<10} {:<14} {:<9} {:<18} {:<13} FIRMWARE",
                "MODE", "USB", "SERIAL", "TOPOLOGY", "MODEL", "PORTS"
            );
            for l in &lines {
                println!("{l}");
            }
        }
        for w in &all_warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

fn format_row(r: &ProbeReport, error: Option<&str>) -> String {
    let fw = match (&r.firmware, error) {
        (Some(f), _) => {
            let bad = if f.known_bad == Some(true) {
                "  [KNOWN BAD]"
            } else {
                ""
            };
            format!("{} ({}, raw {}){bad}", f.norm, f.wch, f.raw)
        }
        (None, Some(e)) => format!("error: {e}"),
        (None, None) => "-".to_owned(),
    };
    format!(
        "{:<6} {:<10} {:<14} {:<9} {:<18} {:<13} {}",
        r.mode.map(mode_str).unwrap_or("?"),
        r.usb.as_deref().unwrap_or("-"),
        r.serial.as_deref().unwrap_or("-"),
        r.topology.as_deref().unwrap_or("-"),
        r.model,
        if r.ports.is_empty() {
            "-".to_owned()
        } else {
            r.ports.join(",")
        },
        fw
    )
}

pub fn info(cli: &Cli) -> ExitCode {
    let entry = match select_entry(cli, "probe.info") {
        Ok(e) => e,
        Err(code) => return code,
    };
    let (report, warnings, error) = report_with_retry(&entry);

    if let Some(e) = error {
        // A probe-open failure is always exit 11 (device-open-failed); exit 13 (device-busy) is
        // reserved for the advisory-lock timeout and a typed USB-claimed error (see session_error).
        return fail(
            cli,
            "probe.info",
            ErrorKind::DeviceOpenFailed,
            e,
            Some("check permissions/driver binding, or whether another tool holds the probe"),
        );
    }

    if cli.json {
        let mut env = ResultEnvelope::success("probe.info");
        env.probe = Some(report);
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        print_probe_human(&report);
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

pub(crate) fn print_probe_human(report: &ProbeReport) {
    println!("model:    {}", report.model);
    println!("mode:     {}", report.mode.map(mode_str).unwrap_or("?"));
    println!("usb:      {}", report.usb.as_deref().unwrap_or("-"));
    println!("serial:   {}", report.serial.as_deref().unwrap_or("-"));
    println!("topology: {}", report.topology.as_deref().unwrap_or("-"));
    if !report.ports.is_empty() {
        println!("ports:    {}", report.ports.join(", "));
    }
    if let Some(fw) = &report.firmware {
        let mode = fw
            .mode
            .as_deref()
            .map(|m| format!(", {m} firmware"))
            .unwrap_or_default();
        println!(
            "firmware: {} (WCH {}, raw {}{mode})",
            fw.norm, fw.wch, fw.raw
        );
    }
}

/// en: Parse --probe, resolving `name:` aliases via the config files and rejecting `index:`
/// under --non-interactive (docs/cli.ja.md §3.3-§3.4).
/// ja: --probe をパースする。`name:` は設定ファイルで解決し、`index:` は
/// --non-interactive 時に拒否する。
fn parse_selector(cli: &Cli, cmd: &str) -> Result<Option<Selector>, ExitCode> {
    let Some(raw) = cli.probe.as_deref() else {
        return Ok(None);
    };
    let sel: Selector = raw.parse().map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::Usage,
            format!("invalid --probe selector: {e}"),
            None,
        )
    })?;
    let sel = match sel {
        Selector::Name(name) => match crate::config::probe_alias(&name) {
            Some(aliased) => aliased.parse().map_err(|e| {
                fail(
                    cli,
                    cmd,
                    ErrorKind::Usage,
                    format!("alias `{name}` resolves to an invalid selector: {e}"),
                    None,
                )
            })?,
            None => {
                return Err(fail(
                    cli,
                    cmd,
                    ErrorKind::Usage,
                    format!(
                        "alias `{name}` not found in ch32rv.toml / ~/.config/ch32rv/config.toml"
                    ),
                    None,
                ));
            }
        },
        other => other,
    };
    if matches!(sel, Selector::Index(_)) && cli.non_interactive {
        return Err(fail(
            cli,
            cmd,
            ErrorKind::Usage,
            "index: selectors are rejected under --non-interactive (not stable across replug)",
            Some("use VID:PID:SERIAL, serial:, name:, or usb:<bus>-<ports>"),
        ));
    }
    Ok(Some(sel))
}

/// en: Take the per-probe advisory lock (docs/cli.ja.md §3.7) for a direct-open command (gdb,
/// monitor uart/sdi) - `Session::attach` locks its own path. Keyed by the probe serial, or bus
/// topology when there is none. On timeout the exit-13 (device-busy) code is already emitted.
/// ja: 直接 open するコマンド(gdb・monitor uart/sdi)用に probe 単位 advisory lock を取る。
pub(crate) fn lock_probe(
    cli: &Cli,
    cmd: &str,
    entry: &Entry,
) -> Result<ch32rv_usb::DeviceLock, ExitCode> {
    let key = entry
        .dev
        .serial()
        .map(str::to_owned)
        .unwrap_or_else(|| entry.dev.topology());
    ch32rv_usb::DeviceLock::acquire(&key, Duration::from_secs(cli.lock_timeout)).map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::DeviceBusy,
            e.to_string(),
            Some("another ch32rv is using this probe; wait for it, or raise --lock-timeout"),
        )
    })
}

pub(crate) fn fail(
    cli: &Cli,
    cmd: &str,
    kind: ErrorKind,
    msg: impl Into<String>,
    hint: Option<&str>,
) -> ExitCode {
    fail_with_candidates(cli, cmd, kind, msg, hint, Vec::new())
}

pub(crate) fn fail_with_candidates(
    cli: &Cli,
    cmd: &str,
    kind: ErrorKind,
    msg: impl Into<String>,
    hint: Option<&str>,
    candidates: Vec<serde_json::Value>,
) -> ExitCode {
    let msg = msg.into();
    if cli.json {
        let mut env = ResultEnvelope::failure(cmd, kind, msg);
        if let Some(e) = env.error.as_mut() {
            e.hint = hint.map(str::to_owned);
            if !candidates.is_empty() {
                e.candidates = Some(candidates);
            }
        }
        let _ = crate::print_envelope(&env);
    } else {
        eprintln!("ch32rv: error[{}]: {msg}", kind.as_str());
        for c in &candidates {
            eprintln!("  candidate: {c}");
        }
        if let Some(h) = hint {
            eprintln!("  hint: {h}");
        }
    }
    kind.exit_code().into()
}

/// en: Shared attach boilerplate for every probe-routed command that talks to a target: select the
/// probe, require RISC-V mode, parse `--speed`, and attach (locking the probe, honoring `--chip`).
/// Errors are already rendered (via [`fail`] / [`session_error`]); callers just propagate the code.
/// ja: target と会話する probe 経路コマンド共通の attach。probe 選択→RISC-V mode 要求→`--speed`
/// 解釈→attach(lock + `--chip` 突合)。エラーは描画済みなので呼び出し側は code を返すだけ。
pub(crate) fn attach(cli: &Cli, cmd: &str) -> Result<Session, ExitCode> {
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
        parse::speed(&cli.speed).map_err(|m| fail(cli, cmd, ErrorKind::Usage, m, None))?;
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    Session::attach(
        &entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        &mut warnings,
    )
    .map_err(|e| session_error(cli, cmd, e))
}

/// en: Render a [`SessionError`] as the CLI failure envelope with an actionable hint. The single
/// home for this mapping so the wording cannot drift between commands.
/// ja: `SessionError` を CLI 失敗 envelope + 実用的な hint として描画。コマンド間で文言がぶれない
/// よう一箇所に集約。
pub(crate) fn session_error(cli: &Cli, cmd: &str, e: SessionError) -> ExitCode {
    use ch32rv_usb::UsbError;
    use ch32rv_wchlink::WchLinkError;
    match e {
        SessionError::ChipMismatch(msg) => fail(
            cli,
            cmd,
            ErrorKind::TargetAmbiguous,
            msg,
            Some("pass the correct --chip, or omit it to use auto-detection"),
        ),
        SessionError::Open(err) | SessionError::ProbeInfo(err) => {
            // Classify off the *typed* error, never a substring of its Display text.
            let (kind, hint) = match &err {
                WchLinkError::Usb(UsbError::Busy(_)) => (
                    ErrorKind::DeviceBusy,
                    "another process has claimed this probe; close it and retry",
                ),
                WchLinkError::Usb(UsbError::AccessDenied(_)) => (
                    ErrorKind::DeviceOpenFailed,
                    "check permissions/udev rules (run `ch32rv doctor`) or the driver binding",
                ),
                _ => (
                    ErrorKind::DeviceOpenFailed,
                    "check the probe connection, or whether another tool holds it",
                ),
            };
            fail(cli, cmd, kind, err.to_string(), Some(hint))
        }
        SessionError::NoTarget => fail(
            cli,
            cmd,
            ErrorKind::TargetNoResponse,
            "no target detected on the debug pins",
            Some(
                "check target wiring/power/BOOT; for a protected or bricked target see `ch32rv recover`",
            ),
        ),
        SessionError::Attach(msg) => fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            msg,
            Some("check target wiring/power/BOOT, and the debug speed (--speed)"),
        ),
        SessionError::Busy(err) => fail(
            cli,
            cmd,
            ErrorKind::DeviceBusy,
            err.to_string(),
            Some("another ch32rv is using this probe; wait for it, or raise --lock-timeout"),
        ),
    }
}

/// Open the probe and read its firmware major/minor + mode (for `probe firmware ...`).
fn read_firmware(cli: &Cli, cmd: &str) -> Result<(u8, u8, Option<String>), ExitCode> {
    let entry = select_entry(cli, cmd)?;
    let mut link = WchLink::open(&entry.dev)
        .map_err(|e| fail(cli, cmd, ErrorKind::DeviceOpenFailed, e.to_string(), None))?;
    let info = link
        .probe_info()
        .map_err(|e| fail(cli, cmd, ErrorKind::DeviceOpenFailed, e.to_string(), None))?;
    Ok((
        info.fw_major,
        info.fw_minor,
        info.fw_mode.map(|m| m.as_str().to_owned()),
    ))
}

/// `probe firmware info`: the probe's firmware version (raw / WCH notation), mode, known-bad status.
pub fn firmware_info(cli: &Cli) -> ExitCode {
    const CMD: &str = "probe.firmware.info";
    let (maj, min, mode) = match read_firmware(cli, CMD) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let fw = FirmwareVersion::from_major_minor(maj, min);
    let bad = known_bad_firmware(maj, min);
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "firmware": format!("{maj}.{min:02}"),
            "raw": fw.raw,
            "wch": fw.wch,
            "firmware_mode": mode,
            "known_bad": bad.is_some(),
            "known_bad_reason": bad,
        }));
        crate::print_envelope(&env)
    } else {
        println!("firmware:  {maj}.{min:02} (WCH {}, raw {})", fw.wch, fw.raw);
        if let Some(m) = &mode {
            println!("mode:      {m}");
        }
        match bad {
            Some(reason) => println!("known-bad: YES - {reason}"),
            None => println!("known-bad: no"),
        }
        ExitCode::SUCCESS
    }
}

/// `probe firmware check [--min <major.minor>]`: for CI. Exit 12 when the firmware is known-bad or
/// below `--min`; exit 0 otherwise.
pub fn firmware_check(cli: &Cli, min: Option<&str>) -> ExitCode {
    const CMD: &str = "probe.firmware.check";
    let (maj, mn, _mode) = match read_firmware(cli, CMD) {
        Ok(v) => v,
        Err(c) => return c,
    };
    if let Some(reason) = known_bad_firmware(maj, mn) {
        return fail(
            cli,
            CMD,
            ErrorKind::DeviceFirmwareKnownBad,
            format!("probe firmware {maj}.{mn:02} is known-bad: {reason}"),
            Some("update the probe firmware (WCH-LinkUtility / `probe firmware update`)"),
        );
    }
    if let Some(m) = min {
        let Some((rmaj, rmin)) = parse_version(m) else {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("bad --min {m:?} (use major.minor, e.g. 2.20)"),
                None,
            );
        };
        if (maj, mn) < (rmaj, rmin) {
            return fail(
                cli,
                CMD,
                ErrorKind::DeviceFirmwareUnsupported,
                format!("probe firmware {maj}.{mn:02} is below the required {rmaj}.{rmin:02}"),
                Some("update the probe firmware"),
            );
        }
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "firmware": format!("{maj}.{mn:02}"),
            "min": min,
        }));
        crate::print_envelope(&env)
    } else {
        println!("probe firmware {maj}.{mn:02}: OK");
        ExitCode::SUCCESS
    }
}

/// Parse a `major.minor` version.
fn parse_version(s: &str) -> Option<(u8, u8)> {
    let (a, b) = s.trim().split_once('.')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// `probe mode get`: the probe's current mode (RISC-V vs DAP/ARM), from VID:PID and the firmware.
/// en: `probe firmware exit-iap` - leave IAP mode without writing anything. IAP entry is
/// persistent (a probe left in IAP stays there across a power cycle even with an intact
/// application), and the end frame `83 02 0000` is what makes the bootloader jump, so this is the
/// escape hatch for a probe that was put into IAP but does not need new firmware.
/// ja: `probe firmware exit-iap`。何も書かずに IAP mode を抜ける。IAP entry は不揮発で、app が
/// 無傷でも電源再投入では戻らない。jump させるのは終了 frame(`83 02 0000`)なので、これが
/// 「IAP に入れたが firmware は要らない」個体の脱出口になる。
pub fn firmware_exit_iap(cli: &Cli) -> ExitCode {
    use ch32rv_wchlink::iap::IapDevice;

    const CMD: &str = "probe.firmware.exit-iap";
    const REENUMERATE_TIMEOUT: Duration = Duration::from_secs(20);

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ProbeMode::Iap {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!("probe is in {} mode, not IAP", mode_str(entry.mode)),
            None,
        );
    }
    let topology = entry.dev.topology();
    let mut dev = match IapDevice::open(&entry.dev) {
        Ok(d) => d,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    if let Err(e) = dev.end() {
        return fail(cli, CMD, ErrorKind::TransferFailed, e.to_string(), None);
    }
    let mut after = None;
    for _ in 0..5 {
        let Some(e) = wait_for_mode(&topology, ProbeMode::Riscv, REENUMERATE_TIMEOUT) else {
            break;
        };
        if let Ok(mut link) = WchLink::open(&e.dev)
            && let Ok(i) = link.probe_info()
        {
            after = Some(format!("{}.{:02}", i.fw_major, i.fw_minor));
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "firmware": after, "left_iap": after.is_some() }));
        return crate::print_envelope(&env);
    }
    match &after {
        Some(v) => println!("left IAP mode; firmware {v}"),
        None => println!(
            "sent the end frame; the probe has not come back as a normal probe (its application may be incomplete - write one with `probe firmware update`)"
        ),
    }
    ExitCode::SUCCESS
}

/// en: Wait until a probe on `topology` shows up in `want` mode, or the deadline passes.
/// Re-enumeration keeps the physical port, so topology is the identity that survives a PID
/// change; a lone match is accepted when the port chain is unknown.
/// ja: `topology` の probe が `want` mode で現れるのを待つ。再列挙で PID は変わるが物理位置は
/// 変わらないので topology を同一性の鍵に使う。
fn wait_for_mode(topology: &str, want: ProbeMode, timeout: Duration) -> Option<Entry> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(devs) = wch_devices() {
            let mut same_mode = devs
                .into_iter()
                .filter(|e| e.mode == want)
                .collect::<Vec<_>>();
            if let Some(i) = same_mode.iter().position(|e| e.dev.topology() == topology) {
                return Some(same_mode.swap_remove(i));
            }
            if same_mode.len() == 1 {
                return same_mode.pop();
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// en: `probe firmware update --image <file>` - rewrite the probe's OWN firmware over IAP
/// (docs/protocol/wch-link.ja.md §6.1). The probe leaves RISC-V mode, re-enumerates as
/// `4348:55e0`, takes the image twice (write pass + verify pass) and jumps back into the new
/// application. A probe already sitting in IAP mode is written directly - that is the rescue
/// path for an interrupted update. The target is never touched.
/// ja: `probe firmware update --image <file>`。probe 自身の firmware を IAP 経由で書き換える。
/// 既に IAP mode の probe はそのまま書ける(中断した更新の復旧経路)。target には触れない。
pub fn firmware_update(cli: &Cli, image_path: &std::path::Path) -> ExitCode {
    use ch32rv_contract::event::Event;
    use ch32rv_contract::progress::ProgressSink;
    use ch32rv_wchlink::iap::{self, IapDevice, Pass};

    const CMD: &str = "probe.firmware.update";
    const REENUMERATE_TIMEOUT: Duration = Duration::from_secs(20);

    let image = match std::fs::read(image_path) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("cannot read {}: {e}", image_path.display()),
                Some(
                    "probe firmware is not bundled: get it from WCH-LinkUtility (https://www.wch.cn/downloads/WCH-LinkUtility_ZIP.html), Firmware_Link/FIRMWARE_CH32V305.bin for a WCH-LinkE",
                ),
            );
        }
    };
    if image.is_empty() {
        return fail(cli, CMD, ErrorKind::Usage, "image is empty", None);
    }
    let info = iap::inspect(&image);
    // The most likely accident: WCH ships `*-APP-IAP.bin` (bootloader + application) next to the
    // application-only image. Writing the former over IAP puts a bootloader image where the
    // application belongs.
    if let Some(bl) = info.bootloader_prefix {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!(
                "{} carries a {bl:#x}-byte bootloader before the application - that file is for an external programmer at 0x08000000, not for IAP",
                image_path.display()
            ),
            Some(
                "write the application-only image next to it (Firmware_Link/FIRMWARE_*.bin; FIRMWARE_CH32V305.bin for a WCH-LinkE)",
            ),
        );
    }
    let image_version = info.version.map(|(a, b)| format!("{a}.{b:02}"));

    // `--dry-run`: everything that can be said about the image without opening a device.
    if cli.dry_run {
        let frames = image.len().div_ceil(iap::CHUNK);
        let usb_ids = info
            .pids
            .iter()
            .map(|p| format!("{:04x}:{p:04x}", wchlink::VID_WCH))
            .collect::<Vec<_>>();
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({
                "dry_run": true,
                "image": {
                    "path": image_path.display().to_string(),
                    "bytes": image.len(),
                    "version": image_version,
                    "usb_ids": usb_ids,
                    "riscv": info.looks_riscv,
                },
                "frames_per_pass": frames,
            }));
            return crate::print_envelope(&env);
        }
        println!("image:    {}", image_path.display());
        println!("bytes:    {}", image.len());
        println!(
            "version:  {} (from bcdDevice)",
            image_version.as_deref().unwrap_or("unknown")
        );
        println!(
            "usb ids:  {}",
            if usb_ids.is_empty() {
                "none found".to_owned()
            } else {
                usb_ids.join(", ")
            }
        );
        println!("riscv:    {}", if info.looks_riscv { "yes" } else { "no" });
        println!("plan:     enter IAP, write {frames} frames, verify {frames} frames, end");
        println!("dry-run:  no device opened");
        return ExitCode::SUCCESS;
    }

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    let topology = entry.dev.topology();
    let _lock = match lock_probe(cli, CMD, &entry) {
        Ok(l) => l,
        Err(c) => return c,
    };

    let mut before: Option<String> = None;
    let resumed_in_iap = entry.mode == ProbeMode::Iap;
    match entry.mode {
        // Rescue: the probe is already in its bootloader, so no entry command is needed.
        ProbeMode::Iap => {}
        ProbeMode::Riscv => {
            let mut link = match WchLink::open(&entry.dev) {
                Ok(l) => l,
                Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
            };
            let probe_info = match link.probe_info() {
                Ok(i) => i,
                Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
            };
            if probe_info.variant != wchlink::Variant::LinkE {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    format!(
                        "IAP update is verified for the WCH-LinkE only (this probe is {})",
                        probe_info.variant.name()
                    ),
                    Some("use WCH-LinkUtility on Windows for other probe models"),
                );
            }
            // A LinkE runs a CH32V305, so its firmware is RISC-V; the CH549 Link images in the
            // same directory are 8051 code and would brick it.
            if !info.looks_riscv {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "image does not start with a RISC-V jal - it is not CH32V probe firmware",
                    Some("the WCH-LinkE image is Firmware_Link/FIRMWARE_CH32V305.bin"),
                );
            }
            // The image announces the PIDs it will enumerate as; a LinkE image carries the
            // RISC-V-mode one. Refuse anything that does not.
            if !info.pids.is_empty() && !info.pids.contains(&wchlink::PID_LINK_RISCV) {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!(
                        "image does not look like WCH-LinkE firmware (embedded USB ids: {})",
                        info.pids
                            .iter()
                            .map(|p| format!("{:04x}:{p:04x}", wchlink::VID_WCH))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None,
                );
            }
            // Nothing in a WCH probe image names its probe model (the USB product string is
            // "WCH-Link" in all of them), so a LinkW image would pass every check here.
            crate::progress::sink(cli).event(&Event::Warn {
                code: "image-model-unverified".into(),
                msg: "the image cannot be checked against the probe model; for a WCH-LinkE it must be Firmware_Link/FIRMWARE_CH32V305.bin".into(),
            });
            before = Some(format!(
                "{}.{:02}",
                probe_info.fw_major, probe_info.fw_minor
            ));
            // docs/cli.ja.md §1.5: a destructive step prompts, `--yes` skips the prompt, and
            // `--non-interactive` without `--yes` refuses instead of prompting.
            if !cli.yes {
                if cli.non_interactive {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::Usage,
                        "refusing to rewrite the probe firmware without --yes",
                        Some("pass --yes to confirm in --non-interactive mode"),
                    );
                }
                if !confirm_mode(&format!(
                    "Rewrite the firmware of probe {} ({} -> {})? It re-enumerates into IAP mode; an interruption leaves it there.",
                    entry.dev.serial().unwrap_or("?"),
                    before.as_deref().unwrap_or("?"),
                    image_version.as_deref().unwrap_or("?"),
                )) {
                    return fail(cli, CMD, ErrorKind::Usage, "aborted", None);
                }
            }
            if let Err(e) = link.enter_iap() {
                return fail(cli, CMD, ErrorKind::TransferFailed, e.to_string(), None);
            }
            drop(link);
        }
        other => {
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                format!("cannot update from {} mode", mode_str(other)),
                Some("switch to RISC-V mode first (`ch32rv probe mode set riscv`)"),
            );
        }
    }

    let sink = crate::progress::sink(cli);
    sink.event(&Event::Phase {
        name: if resumed_in_iap {
            "iap-resume".into()
        } else {
            "iap-enter".into()
        },
        total: None,
    });
    let iap_entry = match wait_for_mode(&topology, ProbeMode::Iap, REENUMERATE_TIMEOUT) {
        Some(e) => e,
        None => {
            return fail(
                cli,
                CMD,
                ErrorKind::DeviceNotFound,
                "the probe did not re-enumerate in IAP mode (4348:55e0)",
                Some(
                    "under usbipd the new USB id is not forwarded automatically: attach it to the VM, then re-run this command (it resumes from IAP mode)",
                ),
            );
        }
    };

    let mut dev = match IapDevice::open(&iap_entry.dev) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::DeviceOpenFailed,
                e.to_string(),
                Some("the probe is in IAP mode; re-run this command once it can be opened"),
            );
        }
    };
    let total = image.len() as u64;
    sink.event(&Event::Phase {
        name: Pass::Write.as_str().into(),
        total: Some(total),
    });
    let mut phase = Pass::Write;
    let result = dev.update(&image, |pass, done| {
        if pass != phase {
            phase = pass;
            sink.event(&Event::Phase {
                name: pass.as_str().into(),
                total: Some(total),
            });
        }
        sink.event(&Event::Progress {
            phase: pass.as_str().into(),
            done,
            total: Some(total),
        });
    });
    if let Err(e) = result {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            e.to_string(),
            Some(
                "the probe is still in IAP mode and its application is incomplete: re-run this command to write it again",
            ),
        );
    }

    // The probe jumps into the new application and comes back as a normal probe. Right after a
    // re-enumeration the CDC function is claimed before the vendor interface is ready, so the
    // first open can fail with "interface is busy" - retry rather than call it a no-show
    // (docs/protocol/wch-link.ja.md §7, the same race `report_with_retry` handles).
    let mut after = None;
    for attempt in 0..5 {
        let Some(e) = wait_for_mode(&topology, ProbeMode::Riscv, REENUMERATE_TIMEOUT) else {
            break;
        };
        if let Ok(mut link) = WchLink::open(&e.dev)
            && let Ok(i) = link.probe_info()
        {
            after = Some(format!("{}.{:02}", i.fw_major, i.fw_minor));
            break;
        }
        sink.event(&Event::Retry {
            phase: "reopen".into(),
            attempt: attempt + 1,
            cause: "probe not openable yet after re-enumeration".into(),
        });
        std::thread::sleep(Duration::from_secs(1));
    }

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "image": {
                "path": image_path.display().to_string(),
                "bytes": image.len(),
                "version": image_version,
            },
            "firmware_before": before,
            "firmware_after": after,
        }));
        return crate::print_envelope(&env);
    }
    match (&before, &after) {
        (Some(b), Some(a)) => println!(
            "firmware {b} -> {a} ({} bytes written and verified)",
            image.len()
        ),
        (_, Some(a)) => println!(
            "firmware now {a} ({} bytes written and verified)",
            image.len()
        ),
        _ => println!(
            "wrote and verified {} bytes; the probe did not answer yet after re-enumerating (check `ch32rv probe list`)",
            image.len()
        ),
    }
    ExitCode::SUCCESS
}

pub fn mode_get(cli: &Cli) -> ExitCode {
    const CMD: &str = "probe.mode.get";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    let usb_mode = mode_str(entry.mode);
    let fw_mode = WchLink::open(&entry.dev)
        .and_then(|mut l| l.probe_info())
        .ok()
        .and_then(|i| i.fw_mode.map(|m| m.as_str().to_owned()));
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "mode": usb_mode,
            "vid": format!("0x{:04x}", entry.dev.vid()),
            "pid": format!("0x{:04x}", entry.dev.pid()),
            "firmware_mode": fw_mode,
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "mode:  {usb_mode}  (USB {:04x}:{:04x}{})",
            entry.dev.vid(),
            entry.dev.pid(),
            fw_mode
                .as_deref()
                .map(|m| format!("; firmware reports {m}"))
                .unwrap_or_default()
        );
        ExitCode::SUCCESS
    }
}

/// Confirm a mode switch on the terminal (fail-closed under `--non-interactive`).
fn confirm_mode(prompt: &str) -> bool {
    eprint!("{prompt} [y/N] ");
    use std::io::Write as _;
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    matches!(s.trim(), "y" | "Y" | "yes")
}

/// `probe mode set <riscv|dap>` — switch a WCH-LinkE between RISC-V and DAP/ARM mode. The probe
/// re-enumerates (its USB PID changes), so this fails closed on the CH549 (no switch support) and
/// reports the re-enumeration. RISC-V->DAP sends `81 ff 01 41`; DAP->RISC-V sends `81 ff 01 52`
/// to the 0x8012 device's OUT endpoint.
pub fn mode_set(cli: &Cli, mode: crate::args::ProbeModeSet) -> ExitCode {
    use crate::args::ProbeModeSet;
    const CMD: &str = "probe.mode.set";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    let (target, target_str) = match mode {
        ProbeModeSet::Riscv => (ProbeMode::Riscv, "riscv"),
        ProbeModeSet::Dap => (ProbeMode::Dap, "dap"),
    };
    if entry.mode == target {
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({ "mode": target_str, "changed": false }));
            return crate::print_envelope(&env);
        }
        println!("already in {target_str} mode");
        return ExitCode::SUCCESS;
    }
    if !cli.yes
        && !cli.non_interactive
        && !confirm_mode(&format!(
            "Switch probe {} to {target_str} mode? It re-enumerates (USB PID changes).",
            entry.dev.serial().unwrap_or("?")
        ))
    {
        return fail(cli, CMD, ErrorKind::Usage, "aborted", None);
    }
    let serial = entry.dev.serial().map(str::to_owned);
    match (entry.mode, target) {
        (ProbeMode::Riscv, ProbeMode::Dap) => {
            let mut link = match WchLink::open(&entry.dev) {
                Ok(l) => l,
                Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
            };
            match link.probe_info() {
                Ok(info) if info.variant == wchlink::Variant::LinkE => {}
                Ok(_) => {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::CapabilityUnsupported,
                        "mode switch is only supported on a WCH-LinkE",
                        None,
                    );
                }
                Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
            }
            let _ = link.switch_to_dap();
        }
        (ProbeMode::Dap, ProbeMode::Riscv) => {
            // The DAP-mode device (PID 0x8012) takes `81 ff 01 52` on interface 0's OUT endpoint
            // 0x02 (its bulk IN is 0x83).
            let mut iface = match entry.dev.open_interface(0, 0x02, 0x83) {
                Ok(i) => i,
                Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
            };
            let _ = iface.write(&[0x81, 0xff, 0x01, 0x52], Duration::from_millis(1000));
        }
        _ => {
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                "unsupported mode transition",
                None,
            );
        }
    }
    // Wait for the probe to re-enumerate in the new mode (same serial, new PID). Under usbipd the
    // new PID is not auto-forwarded to the VM, so not reappearing here is not necessarily a failure.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut seen = None;
    while std::time::Instant::now() < deadline {
        if let Ok(devs) = wch_devices()
            && let Some(e) = devs
                .iter()
                .find(|e| e.dev.serial().map(str::to_owned) == serial)
        {
            seen = Some(e.mode);
            if e.mode == target {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    if seen == Some(target) {
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({ "mode": target_str, "changed": true }));
            return crate::print_envelope(&env);
        }
        println!("switched to {target_str} mode");
        return ExitCode::SUCCESS;
    }
    // Switch command sent and the device left; it is re-enumerating but not visible here yet.
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "mode": target_str, "changed": true, "reenumerating": true,
        }));
        crate::print_envelope(&env)
    } else {
        eprintln!(
            "sent mode switch to {target_str}; the probe is re-enumerating (new USB PID). \
             If it does not reappear, re-plug it (under usbipd, attach the new PID to the VM)."
        );
        ExitCode::SUCCESS
    }
}

/// `probe power 3v3|5v <on|off>` / `probe power cycle` — control the WCH-LinkE target-power output.
/// WCH-LinkE only (the CH549 Link has no power output, fails closed).
pub fn power(cli: &Cli, cmd: &crate::args::PowerCmd) -> ExitCode {
    use crate::args::{PowerCmd, SwitchState};
    const CMD: &str = "probe.power";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    let mut link = match WchLink::open(&entry.dev) {
        Ok(l) => l,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    match link.probe_info() {
        Ok(info) if info.variant == wchlink::Variant::LinkE => {}
        Ok(_) => {
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                "power output is only available on a WCH-LinkE",
                None,
            );
        }
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    }
    let (desc, json, result) = match cmd {
        PowerCmd::V3v3 { state } => {
            let on = matches!(state, SwitchState::On);
            (
                format!("3v3 {}", if on { "on" } else { "off" }),
                serde_json::json!({ "rail": "3v3", "on": on }),
                link.set_power(false, on),
            )
        }
        PowerCmd::V5 { state } => {
            let on = matches!(state, SwitchState::On);
            (
                format!("5v {}", if on { "on" } else { "off" }),
                serde_json::json!({ "rail": "5v", "on": on }),
                link.set_power(true, on),
            )
        }
        PowerCmd::Cycle { off_ms } => {
            // Cycle the 3.3V rail: off, wait, on.
            let r = match link.set_power(false, false) {
                Ok(()) => {
                    std::thread::sleep(Duration::from_millis(*off_ms));
                    link.set_power(false, true)
                }
                Err(e) => Err(e),
            };
            (
                format!("cycle 3v3 (off {off_ms}ms)"),
                serde_json::json!({ "rail": "3v3", "on": true, "cycled": true, "off_ms": off_ms }),
                r,
            )
        }
    };
    match result {
        Ok(()) => {
            if cli.json {
                let mut env = ResultEnvelope::success(CMD);
                env.result = Some(json);
                crate::print_envelope(&env)
            } else {
                println!("power: {desc}");
                ExitCode::SUCCESS
            }
        }
        Err(e) => fail(cli, CMD, ErrorKind::TransferFailed, e.to_string(), None),
    }
}
