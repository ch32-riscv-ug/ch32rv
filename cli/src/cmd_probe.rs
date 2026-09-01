//! en: `probe list` / `probe info` (docs/cli.ja.md §4.2). Read-only against the probe:
//! only GetProbeInfo is issued; no target attach, no reset, no power control.
//! ja: `probe list` / `probe info`。probe への読み取りのみ(GetProbeInfo だけを発行し、
//! target attach・reset・電源制御は行わない)。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{
    ErrorKind, FirmwareVersion, ProbeMode, ProbeReport, ResultEnvelope, Warning,
};
use ch32rv_usb::{ResolveError, Selector, UsbDeviceInfo, UsbError};
use ch32rv_wchlink::{self as wchlink, WchLink, WchLinkError, known_bad_firmware};

use crate::args::Cli;

/// A WCH device relevant to `probe` commands, with its mode derived from VID:PID.
struct Entry {
    dev: UsbDeviceInfo,
    mode: ProbeMode,
}

fn wch_devices() -> Result<Vec<Entry>, UsbError> {
    let devs = ch32rv_usb::enumerate()?;
    Ok(devs
        .into_iter()
        .filter_map(|dev| {
            let mode = match (dev.vid(), dev.pid()) {
                (wchlink::VID_WCH, wchlink::PID_LINK_RISCV) => ProbeMode::Riscv,
                (wchlink::VID_WCH, wchlink::PID_LINK_DAP) => ProbeMode::Dap,
                (wchlink::VID_IAP, wchlink::PID_IAP) => ProbeMode::Iap,
                _ => return None,
            };
            Some(Entry { dev, mode })
        })
        .collect())
}

fn mode_str(mode: ProbeMode) -> &'static str {
    match mode {
        ProbeMode::Riscv => "riscv",
        ProbeMode::Dap => "dap",
        ProbeMode::Iap => "iap",
        ProbeMode::Isp => "isp",
        ProbeMode::Unknown => "unknown",
    }
}

/// en: Query firmware/variant over GetProbeInfo (RISC-V mode only).
/// ja: GetProbeInfo で firmware/型番を取得(RISC-V mode のみ)。
fn query_riscv(dev: &UsbDeviceInfo) -> Result<(String, FirmwareVersion), WchLinkError> {
    let mut link = WchLink::open(dev)?;
    let info = link.probe_info()?;
    let mut fw = FirmwareVersion::from_major_minor(info.fw_major, info.fw_minor);
    fw.known_bad = Some(known_bad_firmware(info.fw_major, info.fw_minor).is_some());
    Ok((info.variant.name(), fw))
}

fn report_for(entry: &Entry) -> (ProbeReport, Vec<Warning>, Option<String>) {
    let mut report = ProbeReport {
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
    };
    let mut warnings = Vec::new();
    let mut error = None;
    if entry.mode == ProbeMode::Riscv {
        match query_riscv(&entry.dev) {
            Ok((model, fw)) => {
                if fw.known_bad == Some(true)
                    && let Some(msg) = known_bad_firmware_msg(&fw)
                {
                    warnings.push(Warning {
                        code: "fw-known-bad".to_owned(),
                        msg,
                    });
                }
                if model.contains("unknown variant") {
                    warnings.push(Warning {
                        code: "probe-variant-unknown".to_owned(),
                        msg: format!("unrecognized probe variant: {model}"),
                    });
                }
                report.model = model;
                report.firmware = Some(fw);
            }
            Err(e) => error = Some(e.to_string()),
        }
    }
    (report, warnings, error)
}

fn known_bad_firmware_msg(fw: &FirmwareVersion) -> Option<String> {
    // en: Re-derive the message from the normalized version (single source: wchlink crate).
    // ja: 正規化版から不良メッセージを引き直す(情報源は wchlink crate に一元化)。
    let mut it = fw.norm.split('.');
    let major: u8 = it.next()?.parse().ok()?;
    let minor: u8 = it.next()?.parse().ok()?;
    known_bad_firmware(major, minor).map(str::to_owned)
}

pub fn list(cli: &Cli, watch: bool) -> ExitCode {
    if watch {
        return crate::unimplemented_cmd(cli, "probe.list --watch");
    }
    let entries = match wch_devices() {
        Ok(e) => e,
        Err(e) => return fail(cli, "probe.list", ErrorKind::Internal, e.to_string(), None),
    };

    let mut probes_json = Vec::new();
    let mut lines = Vec::new();
    let mut all_warnings = Vec::new();
    for entry in &entries {
        let (report, warnings, error) = report_for(entry);
        lines.push(format_row(&report, error.as_deref()));
        let mut v = match serde_json::to_value(&report) {
            Ok(v) => v,
            Err(e) => return fail(cli, "probe.list", ErrorKind::Internal, e.to_string(), None),
        };
        if let (Some(err), Some(obj)) = (error, v.as_object_mut()) {
            obj.insert("error".to_owned(), serde_json::Value::String(err));
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
                "{:<6} {:<10} {:<16} {:<10} {:<22} FIRMWARE",
                "MODE", "USB", "SERIAL", "TOPOLOGY", "MODEL"
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
        "{:<6} {:<10} {:<16} {:<10} {:<22} {}",
        r.mode.map(mode_str).unwrap_or("?"),
        r.usb.as_deref().unwrap_or("-"),
        r.serial.as_deref().unwrap_or("-"),
        r.topology.as_deref().unwrap_or("-"),
        r.model,
        fw
    )
}

pub fn info(cli: &Cli) -> ExitCode {
    let entries = match wch_devices() {
        Ok(e) => e,
        Err(e) => return fail(cli, "probe.info", ErrorKind::Internal, e.to_string(), None),
    };
    let selector = match parse_selector(cli) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let devs: Vec<&Entry> = entries.iter().collect();
    let idx = match ch32rv_usb::resolve(selector.as_ref(), devs.iter().map(|e| &e.dev)) {
        Ok(i) => i,
        Err(ResolveError::NotFound) => {
            return fail(
                cli,
                "probe.info",
                ErrorKind::DeviceNotFound,
                "no probe matched the selector",
                Some("run `ch32rv probe list` to see available probes"),
            );
        }
        Err(ResolveError::Ambiguous(indices)) => {
            let candidates: Vec<serde_json::Value> = indices
                .iter()
                .filter_map(|&i| devs.get(i))
                .map(|e| {
                    serde_json::json!({
                        "usb": e.dev.usb_id(),
                        "serial": e.dev.serial(),
                        "topology": e.dev.topology(),
                        "mode": mode_str(e.mode),
                    })
                })
                .collect();
            return fail_with_candidates(
                cli,
                "probe.info",
                ErrorKind::DeviceAmbiguous,
                format!("{} probes match; specify --probe", candidates.len()),
                Some("select one with --probe VID:PID:SERIAL or usb:<bus>-<ports>"),
                candidates,
            );
        }
        Err(ResolveError::UnresolvedName(n)) => {
            return fail(
                cli,
                "probe.info",
                ErrorKind::Usage,
                format!("alias `{n}` not found in ch32rv.toml / ~/.config/ch32rv/config.toml"),
                None,
            );
        }
    };
    let entry = devs[idx];

    // en: Open with retry (3 attempts, 1 s apart) per docs/cli.ja.md §3.7; permission errors
    // are not retried.
    // ja: docs/cli.ja.md §3.7 に従い 1 秒間隔 3 回まで再試行。権限エラーは再試行しない。
    let (report, warnings, error) = {
        let mut last = report_for(entry);
        if entry.mode == ProbeMode::Riscv {
            for _ in 0..2 {
                if last.2.is_none()
                    || last
                        .2
                        .as_deref()
                        .is_some_and(|e| e.contains("access denied"))
                {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
                last = report_for(entry);
            }
        }
        last
    };

    if let Some(e) = error {
        let kind = if e.contains("access denied") {
            ErrorKind::DeviceOpenFailed
        } else if e.contains("busy") {
            ErrorKind::DeviceBusy
        } else {
            ErrorKind::DeviceOpenFailed
        };
        return fail(
            cli,
            "probe.info",
            kind,
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
        println!("model:    {}", report.model);
        println!("mode:     {}", report.mode.map(mode_str).unwrap_or("?"));
        println!("usb:      {}", report.usb.as_deref().unwrap_or("-"));
        println!("serial:   {}", report.serial.as_deref().unwrap_or("-"));
        println!("topology: {}", report.topology.as_deref().unwrap_or("-"));
        if let Some(fw) = &report.firmware {
            println!("firmware: {} (WCH {}, raw {})", fw.norm, fw.wch, fw.raw);
        }
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

/// en: Parse --probe, resolving `name:` aliases via the config files and rejecting `index:`
/// under --non-interactive (docs/cli.ja.md §3.3-§3.4).
/// ja: --probe をパースする。`name:` は設定ファイルで解決し、`index:` は
/// --non-interactive 時に拒否する。
fn parse_selector(cli: &Cli) -> Result<Option<Selector>, ExitCode> {
    let Some(raw) = cli.probe.as_deref() else {
        return Ok(None);
    };
    let sel: Selector = raw.parse().map_err(|e| {
        fail(
            cli,
            "probe.info",
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
                    "probe.info",
                    ErrorKind::Usage,
                    format!("alias `{name}` resolves to an invalid selector: {e}"),
                    None,
                )
            })?,
            None => {
                return Err(fail(
                    cli,
                    "probe.info",
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
            "probe.info",
            ErrorKind::Usage,
            "index: selectors are rejected under --non-interactive (not stable across replug)",
            Some("use VID:PID:SERIAL, serial:, name:, or usb:<bus>-<ports>"),
        ));
    }
    Ok(Some(sel))
}

fn fail(
    cli: &Cli,
    cmd: &str,
    kind: ErrorKind,
    msg: impl Into<String>,
    hint: Option<&str>,
) -> ExitCode {
    fail_with_candidates(cli, cmd, kind, msg, hint, Vec::new())
}

fn fail_with_candidates(
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
