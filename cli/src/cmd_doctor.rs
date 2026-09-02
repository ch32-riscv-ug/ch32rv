//! en: `doctor` (docs/cli.ja.md §4.10): diagnose the environment and suggest the next step.
//! It checks USB enumeration, permission/udev on Linux, WCH-Link presence, known-bad
//! firmware, and IAP-mode devices. `--emit-udev` prints a ready-to-install udev rule.
//! Read-only: it does not open a debug session on any target.
//! ja: `doctor`。環境診断と次の一手。USB 列挙・権限/udev(Linux)・WCH-Link 有無・既知不良
//! firmware・IAP 滞留を調べる。`--emit-udev` で udev rule を出す。target には触れない。

use std::process::ExitCode;

use ch32rv_contract::{ErrorKind, ProbeMode, ResultEnvelope};
use ch32rv_wchlink::{self as wchlink, WchLink};

use crate::args::{Cli, DoctorArgs};
use crate::cmd_probe::{mode_str, wch_devices};

/// The udev rule that grants non-root access to WCH-Link probes. Single source of truth: this same
/// file is bundled into the Linux release tarball, and `doctor --emit-udev` prints it verbatim, so
/// the two never drift (ArduinoCore-CH32 request B-6).
const UDEV_RULE: &str = include_str!("../60-ch32rv.rules");

struct Check {
    name: &'static str,
    ok: bool,
    detail: String,
    hint: Option<String>,
}

pub fn doctor(cli: &Cli, args: &DoctorArgs) -> ExitCode {
    if args.emit_udev {
        print!("{UDEV_RULE}");
        return ExitCode::SUCCESS;
    }

    let mut checks: Vec<Check> = Vec::new();

    // 1. USB enumeration.
    let devices = ch32rv_usb::enumerate();
    match &devices {
        Ok(_) => checks.push(Check {
            name: "usb-enumerate",
            ok: true,
            detail: "USB enumeration works".into(),
            hint: None,
        }),
        Err(e) => checks.push(Check {
            name: "usb-enumerate",
            ok: false,
            detail: format!("cannot enumerate USB: {e}"),
            hint: Some("check that libusb/usbfs is available".into()),
        }),
    }

    // 2. WCH probes present, and each openable (permission check).
    let entries = wch_devices().unwrap_or_default();
    if entries.is_empty() {
        checks.push(Check {
            name: "probe-present",
            ok: false,
            detail: "no WCH-Link / ISP device found".into(),
            hint: Some("plug in a WCH-LinkE (1a86:8010); check the cable".into()),
        });
    } else {
        checks.push(Check {
            name: "probe-present",
            ok: true,
            detail: format!("{} WCH device(s) found", entries.len()),
            hint: None,
        });

        for entry in &entries {
            let label = format!(
                "{} [{}]",
                entry.dev.serial().unwrap_or("no-serial"),
                mode_str(entry.mode)
            );
            match entry.mode {
                ProbeMode::Riscv => match WchLink::open(&entry.dev).and_then(|mut l| l.probe_info()) {
                    Ok(info) => {
                        let bad = wchlink::known_bad_firmware(info.fw_major, info.fw_minor);
                        checks.push(Check {
                            name: "probe-open",
                            ok: bad.is_none(),
                            detail: format!(
                                "{label}: opened, firmware {}.{}{}",
                                info.fw_major,
                                info.fw_minor,
                                bad.map(|_| " [KNOWN BAD]").unwrap_or("")
                            ),
                            hint: bad.map(|m| m.to_owned()),
                        });
                    }
                    Err(e) => {
                        let s = e.to_string();
                        let (detail, hint) = if s.contains("access denied") {
                            (
                                format!("{label}: permission denied"),
                                Some("run `ch32rv doctor --emit-udev` and install the rule (see its comment)".to_owned()),
                            )
                        } else if s.contains("busy") {
                            (format!("{label}: busy"), Some("another tool holds this probe".to_owned()))
                        } else {
                            (format!("{label}: cannot open: {s}"), None)
                        };
                        checks.push(Check { name: "probe-open", ok: false, detail, hint });
                    }
                },
                ProbeMode::Iap => checks.push(Check {
                    name: "probe-iap",
                    ok: false,
                    detail: format!("{label}: device is in IAP/ISP mode (4348:55e0)"),
                    hint: Some("a LinkE stuck in IAP needs a firmware update; a bare ISP device is a target, not a probe".into()),
                }),
                _ => checks.push(Check {
                    name: "probe-mode",
                    ok: true,
                    detail: format!("{label}: not a RISC-V-mode probe"),
                    hint: None,
                }),
            }
        }
    }

    // 3. Linux group membership hint (best-effort).
    #[cfg(target_os = "linux")]
    if !entries.is_empty() {
        checks.push(linux_group_check());
    }

    let all_ok = checks.iter().all(|c| c.ok);
    if cli.json {
        let mut env = if all_ok {
            ResultEnvelope::success("doctor")
        } else {
            ResultEnvelope::failure(
                "doctor",
                ErrorKind::DeviceOpenFailed,
                "one or more checks failed",
            )
        };
        env.result = Some(serde_json::json!({
            "ok": all_ok,
            "checks": checks.iter().map(|c| serde_json::json!({
                "name": c.name, "ok": c.ok, "detail": c.detail, "hint": c.hint,
            })).collect::<Vec<_>>(),
        }));
        crate::print_envelope(&env)
    } else {
        for c in &checks {
            println!("[{}] {}", if c.ok { "OK " } else { "!! " }, c.detail);
            if let Some(h) = &c.hint {
                println!("      -> {h}");
            }
        }
        if all_ok {
            println!("\nall checks passed");
            ExitCode::SUCCESS
        } else {
            eprintln!("\nsome checks failed (see hints above)");
            ErrorKind::DeviceOpenFailed.exit_code().into()
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_group_check() -> Check {
    // If the user is in plugdev, the udev rule above will grant access.
    let in_plugdev = std::process::Command::new("id")
        .arg("-nG")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|g| g.split_whitespace().any(|grp| grp == "plugdev"))
        .unwrap_or(false);
    Check {
        name: "linux-plugdev",
        ok: in_plugdev,
        detail: if in_plugdev {
            "user is in the 'plugdev' group".into()
        } else {
            "user is not in the 'plugdev' group".into()
        },
        hint: (!in_plugdev)
            .then(|| "add yourself: sudo usermod -aG plugdev $USER, then re-login".to_owned()),
    }
}
