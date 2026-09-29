//! en: Commands on an OEP probe (docs/oep-host.ja.md §4-§6): recognising one from `--probe`,
//! opening a session with the lock rules of ArduinoCore-CH32 oep-workflow §4.3, attaching, and
//! `flash` through ch32rv's RAM loader. The target is identified from the chip id the probe reads
//! at attach (WCH DMI 0x7F), never from a WCH family byte.
//! ja: OEP の probe でのコマンド。`--probe` から見分け、oep-workflow §4.3 の lock の規則で session を
//! 開き、attach して、ch32rv の RAM loader で `flash` する。target は attach で読む chip id から引く。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ch32rv_contract::{ErrorKind, ResultEnvelope};
use ch32rv_dmi::{ResetMode, TargetAccess};
use ch32rv_oep::registry::core as oep_core;
use ch32rv_oep::session::{OepError, Probe, random_session_id};
use ch32rv_oep::target::{AttachOptions, OepDtm, WireKind, attach, detach};
use ch32rv_usb::Selector;

use crate::args::{Cli, FlashArgs};
use crate::cmd_probe::fail;
use crate::parse;

/// Where an OEP probe is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OepAddr {
    /// A serial port no WCH-Link owns (COBS).
    Serial(String),
    /// `tcp:<host:port>`: a probe's TCP transport, or a ch32rv broker.
    Tcp(String),
}

/// en: Whether `--probe` names an OEP probe: `tcp:`, or `port:<path>` for a serial port that no
/// WCH-Link owns. `None` leaves the command on the WCH-Link path.
/// ja: `--probe` が OEP の probe を指すか(`tcp:`、または WCH-Link のものでない serial port)。
pub(crate) fn addr(cli: &Cli, cmd: &str) -> Result<Option<OepAddr>, ExitCode> {
    match crate::cmd_probe::parse_selector(cli, cmd)? {
        Some(Selector::Tcp(a)) => Ok(Some(OepAddr::Tcp(a))),
        Some(Selector::Port(p)) if !p.starts_with("wchlink://") && !p.starts_with("hid://") => {
            let sel = Selector::Port(p.clone());
            let owned = crate::cmd_probe::wch_devices()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .any(|(i, e)| sel.matches(&e.dev, i));
            Ok((!owned).then_some(OepAddr::Serial(p)))
        }
        _ => Ok(None),
    }
}

fn oep_fail(cli: &Cli, cmd: &str, e: OepError) -> ExitCode {
    let (kind, hint) = match &e {
        OepError::Locked { .. } => (
            ErrorKind::DeviceBusy,
            Some("another host holds the probe; wait for it, or retry with --force-lock"),
        ),
        OepError::Link(ch32rv_oep::link::LinkError::NotOep(_)) => (
            ErrorKind::DeviceNotFound,
            Some("this port does not answer OEP v1 (not an OEP probe, or its firmware is too old)"),
        ),
        OepError::Link(_) => (ErrorKind::TransferFailed, None),
        OepError::NoInterface(_) => (ErrorKind::CapabilityUnsupported, None),
        _ => (ErrorKind::TransferFailed, None),
    };
    fail(cli, cmd, kind, e.to_string(), hint)
}

/// Connect and confirm.
fn connect(cli: &Cli, cmd: &str, a: &OepAddr) -> Result<Probe, ExitCode> {
    let link = match a {
        OepAddr::Serial(p) => ch32rv_oep::link::open_serial(p),
        OepAddr::Tcp(t) => ch32rv_oep::link::open_tcp(t),
    }
    .map_err(|e| fail(cli, cmd, ErrorKind::DeviceOpenFailed, e.to_string(), None))?;
    Probe::connect(link).map_err(|e| oep_fail(cli, cmd, e))
}

/// en: The probe's only transport is one serial port (fn 0 describe `transport` lists a single
/// serial kind). Then an exclusive open that succeeded means the previous holder is gone, and its
/// lock may be taken at once (oep-workflow §4.3). A probe that does not say is treated as having
/// several (the safe side).
/// ja: probe の経路が serial 1 本だけか(describe `transport` が serial の種類 1 つ)。そうなら排他で
/// 開けた時点で前の持ち主は居ないので、lock はすぐ奪ってよい。宣言の無い probe は複数とみなす。
fn single_serial(p: &mut Probe) -> bool {
    use ch32rv_oep::registry::core::enums::transport_kind as k;
    let Ok(tlvs) = p.describe(oep_core::FN) else {
        return false;
    };
    let kinds: Vec<u8> = tlvs
        .iter()
        .filter(|t| t.tag == oep_core::tlvs::describe::TRANSPORT)
        .flat_map(|t| t.value.clone())
        .collect();
    matches!(kinds.as_slice(), [one] if [k::UART_BRIDGE, k::USB_CDC, k::USB_SERIAL_JTAG].contains(one))
}

/// en: Open a session. A lock held elsewhere is taken at once on a single-serial probe; otherwise
/// the remaining lease is waited out (up to 5 s) and a holder that keeps renewing is named.
/// ja: session を開く。serial 1 本の probe ならすぐ奪い、それ以外は残りの lease を待ち(最大 5 秒)、
/// 更新し続ける持ち主は名指しでエラーにする。
fn open_session(cli: &Cli, cmd: &str, p: &mut Probe, serial: bool) -> Result<(), ExitCode> {
    let owner = format!("ch32rv {cmd} pid {}", std::process::id());
    let sid = random_session_id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut force = false;
    loop {
        match p.open(sid, 3000, force, Some(&owner)) {
            Ok(_) => return Ok(()),
            Err(OepError::Locked { .. }) if serial && !force => force = true,
            Err(OepError::Locked {
                remaining_ms,
                owner,
            }) if Instant::now() < deadline => {
                let _ = owner;
                std::thread::sleep(Duration::from_millis(u64::from(
                    remaining_ms.clamp(50, 1000),
                )));
            }
            Err(e) => return Err(oep_fail(cli, cmd, e)),
        }
    }
}

/// The wire to attach on: the only one the probe has, else SWIO for the one-wire families named
/// by `--chip`, else RVSWD.
fn pick_wire(p: &mut Probe, chip: Option<&str>) -> Result<WireKind, OepError> {
    let has = |p: &mut Probe, k: WireKind| p.interface(k.interface()).is_ok();
    let (rv, sw) = (has(p, WireKind::Rvswd), has(p, WireKind::Swio));
    let one_wire = chip.is_some_and(|c| {
        let u = c.to_ascii_uppercase();
        u.starts_with("CH32V00") || u.starts_with("CH32M007") || u.starts_with("CH641")
    });
    match (rv, sw) {
        (true, false) => Ok(WireKind::Rvswd),
        (false, true) => Ok(WireKind::Swio),
        (true, true) if one_wire => Ok(WireKind::Swio),
        (true, true) => Ok(WireKind::Rvswd),
        (false, false) => Err(OepError::NoInterface(
            "oep.wire.rvswd / oep.wire.swio".into(),
        )),
    }
}

/// The DB family of the attached target, checked against `--chip` (fail-closed).
fn family(cli: &Cli, cmd: &str, chip_id: Option<u32>) -> Result<String, ExitCode> {
    let db = ch32rv_target::Db::builtin();
    let detected = chip_id.and_then(|id| match db.resolve_by_chip_id(id) {
        ch32rv_target::Resolution::Sku(s) => Some(s.family.clone()),
        ch32rv_target::Resolution::Family(f, _) => Some(f),
        ch32rv_target::Resolution::Unknown => None,
    });
    let requested = cli
        .chip
        .as_deref()
        .map(|c| (c, db.families_for_chip_name(c)));
    match (detected, requested) {
        (Some(d), Some((c, fams))) if !fams.iter().any(|f| f.eq_ignore_ascii_case(&d)) => {
            Err(fail(
                cli,
                cmd,
                ErrorKind::TargetAmbiguous,
                format!(
                    "--chip {c} conflicts with the detected {d} (chip id 0x{:08x})",
                    chip_id.unwrap_or(0)
                ),
                Some("pass the correct --chip, or omit it to use auto-detection"),
            ))
        }
        (Some(d), _) => Ok(d),
        (None, Some((_, fams))) if fams.len() == 1 => Ok(fams[0].clone()),
        (None, Some((c, _))) => Err(fail(
            cli,
            cmd,
            ErrorKind::TargetNotInDb,
            format!("the probe read no chip id and --chip {c} does not name one family"),
            None,
        )),
        (None, None) => Err(fail(
            cli,
            cmd,
            ErrorKind::TargetNoResponse,
            match chip_id {
                Some(id) => format!("chip id 0x{id:08x} is not in the target DB"),
                None => "the probe read no chip id at attach".to_owned(),
            },
            Some("pass --chip to name the target"),
        )),
    }
}

/// `flash` on an OEP probe.
pub(crate) fn flash(cli: &Cli, args: &FlashArgs, bytes: &[u8], a: &OepAddr) -> ExitCode {
    const CMD: &str = "flash";
    let mut p = match connect(cli, CMD, a) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let serial = matches!(a, OepAddr::Serial(_)) && single_serial(&mut p);
    if let Err(c) = open_session(cli, CMD, &mut p, serial) {
        return c;
    }
    let r = flash_in_session(cli, args, bytes, &mut p);
    // End on every path: the lock is released, the connection stays for the next open.
    let _ = p.end();
    r
}

fn flash_in_session(cli: &Cli, args: &FlashArgs, bytes: &[u8], p: &mut Probe) -> ExitCode {
    const CMD: &str = "flash";
    let wire = match pick_wire(p, cli.chip.as_deref()) {
        Ok(w) => w,
        Err(e) => return oep_fail(cli, CMD, e),
    };
    let max_speed_hz = match parse::speed(&cli.speed) {
        Ok((s, _)) => Some(match s {
            ch32rv_wchlink::Speed::Low => 400_000,
            ch32rv_wchlink::Speed::Medium => 4_000_000,
            ch32rv_wchlink::Speed::High => 6_000_000,
        }),
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let at = match attach(
        p,
        wire,
        AttachOptions {
            halt: true,
            max_speed_hz,
            pins: None,
        },
    ) {
        Ok(a) => a,
        Err(e) => return oep_fail(cli, CMD, e),
    };
    let r = flash_attached(cli, args, bytes, p, at.connection, at.wch_chip_id);
    let _ = detach(p, wire, at.connection, false);
    r
}

fn flash_attached(
    cli: &Cli,
    args: &FlashArgs,
    bytes: &[u8],
    p: &mut Probe,
    connection: u16,
    chip_id: Option<u32>,
) -> ExitCode {
    const CMD: &str = "flash";
    let family = match family(cli, CMD, chip_id) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let Some(plan) = ch32rv_flash::loader::plan_for_family(&family) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!("the device DB has no loader plan for {family}"),
            None,
        );
    };
    let bin_offset = match &args.at {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => Some(a),
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => None,
    };
    let image = match crate::cmd_flash::parse_image(
        bytes,
        args.format,
        &args.file,
        bin_offset,
        ch32rv_flash::CODE_FLASH_START,
    ) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
    };
    let mut t = match OepDtm::new(p, connection) {
        Ok(t) => t,
        Err(e) => return oep_fail(cli, CMD, e),
    };
    // Halt before the first instruction, so a running watchdog cannot reset the part mid-write.
    if let Err(e) = t.reset(ResetMode::HaltAtReset) {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("reset-halt: {e}"),
            None,
        );
    }
    let started = Instant::now();
    let report = match ch32rv_flash::loader::program(&mut t, plan, &image.segments, &mut |_, _| {})
    {
        Ok(r) => r,
        Err(e) => return fail(cli, CMD, ErrorKind::VerifyMismatch, e.to_string(), None),
    };
    let secs = started.elapsed().as_secs_f64();
    if args.reset == ch32rv_contract::policy::ResetPolicy::Run
        && let Err(e) = t.reset(ResetMode::RunVerified)
    {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("reset: {e}"),
            None,
        );
    }
    let total = image.total_len();
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "flash": {
                "written": total,
                "programmer": "oep-loader",
                "family": family,
                "chip_id": chip_id.map(|c| format!("0x{c:08x}")),
                "pages": report.pages,
                "rewritten": report.rewritten,
                "restarted_runs": report.restarted_runs,
                "verify": "readback",
                "seconds": secs,
            }
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "flashed {total} bytes to {family} over OEP in {secs:.2} s: {} page(s), {} rewritten, {} run(s) re-issued, verified",
            report.pages, report.rewritten, report.restarted_runs
        );
        ExitCode::SUCCESS
    }
}
