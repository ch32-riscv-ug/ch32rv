//! en: `capabilities` (docs/cli.ja.md §4.10): the probe x firmware x target x operation matrix -
//! what this exact setup can do, and why. Attaches to read the live probe variant and target
//! family, then reports each operation as supported/unsupported with a reason. There is no
//! `tool supports LinkE` boolean; every operation carries its own gated reason.
//! ja: `capabilities`。probe × FW × target × operation の可否 matrix。attach して probe variant と
//! target family を読み、各 operation の可否と理由を出す。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ProbeMode, ResultEnvelope};
use ch32rv_wchlink::Variant;

use crate::args::Cli;
use crate::cmd_probe::{fail, mode_str, select_entry};
use crate::parse;
use crate::session::Session;

/// One capability row: an operation, whether it is supported here, and why.
struct Cap {
    op: &'static str,
    supported: bool,
    reason: String,
}

impl Cap {
    fn yes(op: &'static str, reason: impl Into<String>) -> Self {
        Cap {
            op,
            supported: true,
            reason: reason.into(),
        }
    }
    fn no(op: &'static str, reason: impl Into<String>) -> Self {
        Cap {
            op,
            supported: false,
            reason: reason.into(),
        }
    }
}

pub fn capabilities(cli: &Cli) -> ExitCode {
    const CMD: &str = "capabilities";
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
                "probe is in {} mode; the RISC-V capability matrix needs RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    // `--chip` plans statically (probe info + named chip, no target needed); otherwise attach and
    // read the live target.
    if let Some(chip) = cli.chip.as_deref() {
        static_capabilities(cli, CMD, &entry, chip)
    } else {
        live_capabilities(cli, CMD, &entry)
    }
}

/// Attach and report the matrix for the connected target.
fn live_capabilities(cli: &Cli, cmd: &str, entry: &crate::cmd_probe::Entry) -> ExitCode {
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(msg) => return fail(cli, cmd, ErrorKind::Usage, msg, None),
    };
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let session = match Session::attach(entry, speed, timeout, cli.chip.as_deref(), &mut warnings) {
        Ok(s) => s,
        Err(e) => return crate::cmd_target::session_error(cli, cmd, e),
    };
    let fw = format!(
        "{}.{:02}",
        session.probe_info.fw_major, session.probe_info.fw_minor
    );
    let db = ch32rv_target::Db::builtin();
    let (sku, series) = match db.resolve_by_chip_id(session.attach.chip_id) {
        ch32rv_target::Resolution::Sku(s) => (Some(s.sku.clone()), Some(s.series.clone())),
        _ => (None, None),
    };
    let wiring = series.as_deref().and_then(ch32rv_target::debug_wiring);
    let caps = build_matrix(
        &session.probe_info.variant,
        session.attach.family_byte,
        wiring.as_ref(),
    );
    report(
        cli,
        cmd,
        &session.probe_info.variant.name(),
        &fw,
        &session.family(),
        sku.as_deref(),
        &caps,
        false,
        warnings,
    )
}

/// Report the matrix for a `--chip`-named target from the probe info alone (no attach).
fn static_capabilities(
    cli: &Cli,
    cmd: &str,
    entry: &crate::cmd_probe::Entry,
    chip: &str,
) -> ExitCode {
    let info = match ch32rv_wchlink::WchLink::open(&entry.dev).and_then(|mut l| l.probe_info()) {
        Ok(i) => i,
        Err(e) => {
            return fail(
                cli,
                cmd,
                ErrorKind::DeviceOpenFailed,
                format!("reading probe info failed: {e}"),
                None,
            );
        }
    };
    let Some(family_byte) = crate::cmd_flash::family_byte_from_name(chip) else {
        return fail(
            cli,
            cmd,
            ErrorKind::Usage,
            format!("unknown --chip {chip:?} (not a known family)"),
            Some("see `ch32rv db list` for known SKUs/families"),
        );
    };
    // A representative SKU (for the series' debug wiring).
    let up = chip.to_ascii_uppercase();
    let db = ch32rv_target::Db::builtin();
    let (sku, series) = db
        .skus()
        .iter()
        .find(|s| {
            s.sku.eq_ignore_ascii_case(chip)
                || s.family.eq_ignore_ascii_case(chip)
                || s.series.eq_ignore_ascii_case(chip)
                || s.sku.to_ascii_uppercase().starts_with(&up)
        })
        .map(|s| (Some(s.sku.clone()), Some(s.series.clone())))
        .unwrap_or((None, None));
    let wiring = series.as_deref().and_then(ch32rv_target::debug_wiring);
    let caps = build_matrix(&info.variant, family_byte, wiring.as_ref());
    let family = ch32rv_wchlink::family_name(family_byte)
        .unwrap_or(chip)
        .to_owned();
    let fw = format!("{}.{:02}", info.fw_major, info.fw_minor);
    report(
        cli,
        cmd,
        &info.variant.name(),
        &fw,
        &family,
        sku.as_deref(),
        &caps,
        true,
        Vec::new(),
    )
}

/// Print/emit the capability report (shared by the live and static paths).
#[allow(clippy::too_many_arguments)]
fn report(
    cli: &Cli,
    cmd: &str,
    probe_name: &str,
    fw: &str,
    family: &str,
    sku: Option<&str>,
    caps: &[Cap],
    static_mode: bool,
    warnings: Vec<ch32rv_contract::Warning>,
) -> ExitCode {
    if cli.json {
        let mut env = ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({
            "probe": probe_name,
            "firmware": fw,
            "family": family,
            "sku": sku,
            "planned": static_mode,
            "capabilities": caps.iter().map(|c| serde_json::json!({
                "op": c.op, "supported": c.supported, "reason": c.reason,
            })).collect::<Vec<_>>(),
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        println!("probe:   {probe_name} (fw {fw})");
        println!(
            "target:  {family}{}{}",
            sku.map(|s| format!(" / {s}")).unwrap_or_default(),
            if static_mode {
                "  (planned from --chip)"
            } else {
                ""
            }
        );
        println!("---");
        for c in caps {
            println!(
                "  [{}] {:<28} {}",
                if c.supported { "yes" } else { "NO " },
                c.op,
                c.reason
            );
        }
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

/// Compute the operation matrix from the probe variant + target family/wiring.
fn build_matrix(
    variant: &Variant,
    family_byte: u8,
    wiring: Option<&ch32rv_target::DebugWiring>,
) -> Vec<Cap> {
    let is_linke = matches!(variant, Variant::LinkE);
    let is_linke_or_w = matches!(variant, Variant::LinkE | Variant::LinkW);
    let one_wire = wiring.map(|w| w.wire == "1-wire").unwrap_or(false);
    let flash_ok = ch32rv_flash::params_for_family(family_byte).is_some();
    let ctrl = ch32rv_flash::flash_controller_profile(family_byte);

    let mut caps = Vec::new();

    // connect: the CH549 WCH-Link cannot drive a 1-wire SWIO target.
    if one_wire && !is_linke {
        caps.push(Cap::no(
            "connect",
            "target is 1-wire SWIO; only WCH-LinkE drives a single-wire target",
        ));
    } else {
        caps.push(Cap::yes(
            "connect",
            "the probe supports this target's debug wiring",
        ));
    }

    // flash (stub loader).
    caps.push(if flash_ok {
        Cap::yes("flash", "a flash loader stub is available for this family")
    } else {
        Cap::no("flash", "no flash stub for this family yet (interim table)")
    });

    // erase --range / --sector / gdb flash software breakpoints (direct FLASH controller).
    caps.push(match &ctrl {
        Some(p) => Cap::yes(
            "erase --range / flash-bp",
            format!(
                "direct FLASH controller ({}-byte page, {:?})",
                p.page_size, p.mode
            ),
        ),
        None => Cap::no(
            "erase --range / flash-bp",
            "no capture-verified FLASH-controller profile for this family",
        ),
    });

    // gdb HW breakpoints are detected live at attach (V4C/V4F expose 4 triggers, older cores 0).
    caps.push(Cap::yes(
        "gdb HW breakpoints",
        "detected at gdb attach (V4C/V4F have 4 trigger slots; V2/V3/V4B have 0)",
    ));

    // SDI print forwarding is a WCH-LinkE feature.
    caps.push(if is_linke {
        Cap::yes("monitor sdi", "SDI print forwarding (WCH-LinkE)")
    } else {
        Cap::no(
            "monitor sdi",
            "SDI print forwarding is WCH-LinkE only; use `monitor --source dmdata`",
        )
    });

    // dmdata/rtt monitoring is host-side over DMI - any probe.
    caps.push(Cap::yes(
        "monitor dmdata",
        "host polls the DM data mailbox over DMI (any probe)",
    ));

    // power-off / nrst recovery erase needs target-power control (LinkE/LinkW).
    caps.push(if is_linke_or_w {
        Cap::yes("recover power-off", "target-power erase (WCH-LinkE/LinkW)")
    } else {
        Cap::no(
            "recover power-off",
            "power-off erase needs a probe that controls target power (LinkE/LinkW)",
        )
    });

    caps
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    fn wiring(wire: &str) -> ch32rv_target::DebugWiring {
        ch32rv_target::DebugWiring {
            wire: wire.to_owned(),
            swdio: "PD1".to_owned(),
            swclk: "-".to_owned(),
        }
    }
    fn cap<'a>(caps: &'a [Cap], op: &str) -> &'a Cap {
        caps.iter().find(|c| c.op == op).expect("cap present")
    }

    #[test]
    fn ch549_cannot_drive_1wire_target() {
        // CH32V003 (0x09) is 1-wire; on a CH549 probe, connect must be unsupported.
        let caps = build_matrix(&Variant::Ch549, 0x09, Some(&wiring("1-wire")));
        assert!(!cap(&caps, "connect").supported);
        // ...but a LinkE drives it fine.
        let caps = build_matrix(&Variant::LinkE, 0x09, Some(&wiring("1-wire")));
        assert!(cap(&caps, "connect").supported);
    }

    #[test]
    fn sdi_and_poweroff_are_linke_only() {
        let ch549 = build_matrix(&Variant::Ch549, 0x01, Some(&wiring("2-wire")));
        assert!(!cap(&ch549, "monitor sdi").supported);
        assert!(!cap(&ch549, "recover power-off").supported);
        let linke = build_matrix(&Variant::LinkE, 0x01, Some(&wiring("2-wire")));
        assert!(cap(&linke, "monitor sdi").supported);
        assert!(cap(&linke, "recover power-off").supported);
    }

    #[test]
    fn erase_range_follows_the_controller_profile() {
        // L103 (0x0E) has a profile; an unsupported family (0x00) does not.
        assert!(
            cap(
                &build_matrix(&Variant::LinkE, 0x0E, None),
                "erase --range / flash-bp"
            )
            .supported
        );
        assert!(
            !cap(
                &build_matrix(&Variant::LinkE, 0x00, None),
                "erase --range / flash-bp"
            )
            .supported
        );
    }
}
