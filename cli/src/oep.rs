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
    /// `oep://<probe>/<slot>`: a slot of an OEP probe (reached on its serial port `path`).
    Slot { path: String, slot: String },
    /// `tcp:<host:port>`: a probe's TCP transport, or a ch32rv broker.
    Tcp(String),
    /// A WCH-Link, reached through its broker (which maps OEP onto the Link).
    Wch(crate::broker::BrokerTarget),
}

/// en: The one place that decides what an OEP probe is (docs/oep-host.ja.md §3.3, oep-core §3.3).
/// Until OEP has its own USB PID: a device whose product string (iProduct) starts with `OEP`,
/// read without opening it. When the PID exists, this becomes a VID:PID check and nothing else
/// changes.
/// ja: OEP の probe の判定はここだけ。専用 PID を取るまでは、iProduct が `OEP` で始まる device
/// (開かずに読める)。PID を取ったら VID:PID の判定に差し替える。
pub(crate) fn is_oep_device(dev: &ch32rv_usb::UsbDeviceInfo) -> bool {
    dev.product().is_some_and(|p| p.starts_with("OEP"))
}

/// The OEP probes on USB.
pub(crate) fn oep_devices() -> Vec<ch32rv_usb::UsbDeviceInfo> {
    ch32rv_usb::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter(is_oep_device)
        .collect()
}

/// A probe's name in `oep://<probe>/…`: its USB serial number, else its USB position.
pub(crate) fn probe_id(dev: &ch32rv_usb::UsbDeviceInfo) -> String {
    dev.serial()
        .map(str::to_owned)
        .unwrap_or_else(|| dev.topology())
}

/// The serial port that carries OEP: every CDC interface of an OEP probe takes OEP (core §3.3),
/// so the first.
pub(crate) fn oep_port(dev: &ch32rv_usb::UsbDeviceInfo) -> Option<String> {
    dev.serial_ports().into_iter().next()
}

/// Whether the serial port `path` belongs to an OEP probe (as [`is_oep_device`] decides).
fn port_of_oep_device(path: &str) -> bool {
    let sel = Selector::Port(path.to_owned());
    oep_devices()
        .iter()
        .enumerate()
        .any(|(i, d)| sel.matches(d, i))
}

/// Resolve `oep://<probe>/<slot>` to the probe's serial port.
pub(crate) fn resolve_oep_url(url: &str) -> Result<OepAddr, String> {
    let rest = url.strip_prefix("oep://").unwrap_or(url);
    let (id, slot) = rest
        .split_once('/')
        .filter(|(i, s)| !i.is_empty() && !s.is_empty())
        .ok_or_else(|| format!("`{url}` is not oep://<probe>/<slot>"))?;
    let dev = oep_devices()
        .into_iter()
        .find(|d| probe_id(d) == id)
        .ok_or_else(|| format!("no OEP probe {id} is connected"))?;
    let path = oep_port(&dev).ok_or_else(|| format!("OEP probe {id} has no serial port"))?;
    Ok(OepAddr::Slot {
        path,
        slot: slot.to_owned(),
    })
}

/// en: Whether `--probe` names an OEP probe: `tcp:`, `port:oep://<probe>/<slot>`, or
/// `port:<path>` for a serial port that no WCH-Link owns. `None` leaves the command on the
/// WCH-Link path.
/// ja: `--probe` が OEP の probe を指すか(`tcp:`、`port:oep://…`、WCH-Link のものでない serial port)。
pub(crate) fn addr(cli: &Cli, cmd: &str) -> Result<Option<OepAddr>, ExitCode> {
    match crate::cmd_probe::parse_selector(cli, cmd)? {
        Some(Selector::Tcp(a)) => Ok(Some(OepAddr::Tcp(a))),
        Some(Selector::Port(p)) if p.starts_with("oep://") => resolve_oep_url(&p)
            .map(Some)
            .map_err(|m| fail(cli, cmd, ErrorKind::DeviceNotFound, m, None)),
        Some(Selector::Port(p)) if !p.starts_with("wchlink://") && !p.starts_with("hid://") => {
            let sel = Selector::Port(p.clone());
            let owned = crate::cmd_probe::wch_devices()
                .unwrap_or_default()
                .iter()
                .enumerate()
                .any(|(i, e)| sel.matches(&e.dev, i));
            if owned {
                return Ok(running_wch_broker(cli, cmd));
            }
            Ok(Some(OepAddr::Serial(p)))
        }
        _ => Ok(running_wch_broker(cli, cmd)),
    }
}

/// en: The WCH-Link `--probe` names, when its broker is running (a monitor or another client holds
/// the Link through it): the command then goes through the broker too, since the broker holds the
/// Link. Otherwise None, and the command opens the Link directly as always.
/// ja: `--probe` の WCH-Link のブローカーが動いていれば(monitor などが Link をブローカー経由で持って
/// いる)、コマンドもブローカーを通す(Link はブローカーが持っている)。動いていなければ None で、今まで
/// どおり直接開く。
fn running_wch_broker(cli: &Cli, cmd: &str) -> Option<OepAddr> {
    let sel = crate::cmd_probe::parse_selector(cli, cmd).ok()?;
    let entries = crate::cmd_probe::wch_devices().ok()?;
    let i = ch32rv_usb::resolve(sel.as_ref(), entries.iter().map(|e| &e.dev)).ok()?;
    let t = crate::broker::BrokerTarget::wch(&entries[i]);
    crate::broker::existing_link_for(&t).map(|_| OepAddr::Wch(t))
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

/// en: Connect and confirm. A serial port goes through its broker (started if it is not running,
/// docs/oep-host.ja.md §7.2): the broker holds the port and the probe's session, and this command
/// is one of its clients. `tcp:` is a direct connection (a probe's TCP transport, or a broker).
/// ja: 接続して confirm。serial port はブローカー経由(無ければ起動)。`tcp:` は直接つなぐ。
fn connect(cli: &Cli, cmd: &str, a: &OepAddr) -> Result<Probe, ExitCode> {
    let link = match a {
        OepAddr::Serial(p) | OepAddr::Slot { path: p, .. } => crate::broker::client_link(p)
            .map_err(|m| {
                fail(
                    cli,
                    cmd,
                    ErrorKind::DeviceOpenFailed,
                    m,
                    Some("the probe's broker could not start or be reached"),
                )
            })?,
        OepAddr::Tcp(t) => ch32rv_oep::link::open_tcp(t)
            .map_err(|e| fail(cli, cmd, ErrorKind::DeviceOpenFailed, e.to_string(), None))?,
        OepAddr::Wch(t) => crate::broker::client_link_for(t)
            .map_err(|m| fail(cli, cmd, ErrorKind::DeviceOpenFailed, m, None))?,
    };
    Probe::connect(link).map_err(|e| oep_fail(cli, cmd, e))
}

/// en: The probe's only transport is one serial port (fn 0 describe `transport` lists a single
/// serial kind). Then an exclusive open that succeeded means the previous holder is gone, and its
/// lock may be taken at once (oep-workflow §4.3). A probe that does not say is treated as having
/// several (the safe side).
/// ja: probe の経路が serial 1 本だけか(describe `transport` が serial の種類 1 つ)。そうなら排他で
/// 開けた時点で前の持ち主は居ないので、lock はすぐ奪ってよい。宣言の無い probe は複数とみなす。
pub(crate) fn single_serial(p: &mut Probe) -> bool {
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

/// en: Open a session named `owner` with the oep-workflow §4.3 lock rule: a lock held elsewhere is
/// taken at once on a single-serial probe; otherwise the remaining lease is waited out (up to 5 s)
/// and a holder that keeps renewing comes back as `Locked` (with its owner name).
/// ja: `owner` の名で session を開く。serial 1 本の probe ならすぐ奪い、それ以外は残りの lease を
/// 待ち(最大 5 秒)、更新し続ける持ち主は `Locked`(owner の名前つき)で返す。
pub(crate) fn open_with_lock_rule(
    p: &mut Probe,
    serial: bool,
    owner: &str,
    lease_ms: u32,
) -> Result<u32, OepError> {
    let sid = random_session_id();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut force = false;
    loop {
        match p.open(sid, lease_ms, force, Some(owner)) {
            Ok(_) => return Ok(sid),
            Err(OepError::Locked { .. }) if serial && !force => force = true,
            Err(OepError::Locked { remaining_ms, .. }) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(u64::from(
                    remaining_ms.clamp(50, 1000),
                )));
            }
            Err(e) => return Err(e),
        }
    }
}

fn open_session(cli: &Cli, cmd: &str, p: &mut Probe, serial: bool) -> Result<(), ExitCode> {
    let owner = format!("ch32rv {cmd} pid {}", std::process::id());
    open_with_lock_rule(p, serial, &owner, 3000)
        .map(|_| ())
        .map_err(|e| oep_fail(cli, cmd, e))
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

/// The wire kind a wire interface's fn is.
fn wire_of_fn(p: &mut Probe, func: u16) -> Option<WireKind> {
    [WireKind::Rvswd, WireKind::Swio]
        .into_iter()
        .find(|k| p.interface(k.interface()).is_ok_and(|i| i.func == func))
}

/// The DB family a WCH chip id resolves to.
fn family_of_chip_id(id: u32) -> Option<String> {
    match ch32rv_target::Db::builtin().resolve_by_chip_id(id) {
        ch32rv_target::Resolution::Sku(s) => Some(s.family.clone()),
        ch32rv_target::Resolution::Family(f, _) => Some(f),
        ch32rv_target::Resolution::Unknown => None,
    }
}

/// en: Where to attach (oep-workflow §3.4): `oep://…/<slot>` names the slot; on a probe with
/// registered slots otherwise, the one slot whose target is of the board's family (`--chip`).
/// A connected slot's chip comes from its state; an unconnected one is attached without halting
/// to read it. None or several matching is an error listing the slots. A probe without slots
/// attaches where it allows.
/// ja: どこに attach するか。`oep://…/<slot>` はそのスロット。スロットのある probe では、板の家系
/// (`--chip`)に合うスロットが 1 つならそこ。接続済みは状態から、未接続は止めない attach で chip を
/// 読む。0 か 2 つ以上ならスロットの一覧つきで止める。スロットの無い probe は許す所に attach する。
fn choose_place(
    p: &mut Probe,
    a: &OepAddr,
    chip: Option<&str>,
) -> Result<(WireKind, Option<(u16, u16)>), String> {
    let slots = ch32rv_oep::config::slots(p).map_err(|e| e.to_string())?;
    if let OepAddr::Slot { slot, .. } = a {
        let s = slots
            .iter()
            .find(|s| s.name == *slot)
            .ok_or_else(|| format!("the probe has no slot `{slot}`"))?;
        let w = wire_of_fn(p, s.wire_fn)
            .ok_or_else(|| format!("slot `{slot}` is on an unknown wire (fn {})", s.wire_fn))?;
        return Ok((w, Some((s.swdio, s.swclk))));
    }
    if slots.is_empty() {
        return pick_wire(p, chip)
            .map(|w| (w, None))
            .map_err(|e| e.to_string());
    }
    let states = ch32rv_oep::config::slot_states(p).map_err(|e| e.to_string())?;
    struct Seen {
        name: String,
        wire: WireKind,
        pins: (u16, u16),
        family: Option<String>,
    }
    let mut seen: Vec<Seen> = Vec::new();
    for s in &slots {
        let Some(w) = wire_of_fn(p, s.wire_fn) else {
            continue;
        };
        let pins = (s.swdio, s.swclk);
        let from_state = states
            .iter()
            .find(|st| st.slot == s.slot)
            .and_then(|st| st.wch_chip_id());
        // Not connected: a non-halting attach reads the chip (the user asked to use the probe).
        let id = from_state.or_else(|| {
            let at = attach(
                p,
                w,
                AttachOptions {
                    halt: false,
                    max_speed_hz: None,
                    pins: Some(pins),
                },
            )
            .ok()?;
            let _ = detach(p, w, at.connection, false);
            at.wch_chip_id
        });
        seen.push(Seen {
            name: s.name.clone(),
            wire: w,
            pins,
            family: id.and_then(family_of_chip_id),
        });
    }
    let db = ch32rv_target::Db::builtin();
    let wanted = chip.map(|c| db.families_for_chip_name(c));
    let matches: Vec<&Seen> = seen
        .iter()
        .filter(|x| match (&wanted, &x.family) {
            (Some(ws), Some(f)) => ws.iter().any(|w| w.eq_ignore_ascii_case(f)),
            (None, Some(_)) => true,
            _ => false,
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok((one.wire, Some(one.pins))),
        _ => {
            let list: Vec<String> = seen
                .iter()
                .map(|x| format!("{}: {}", x.name, x.family.as_deref().unwrap_or("no target")))
                .collect();
            Err(format!(
                "{} slot(s) match {}: {}",
                matches.len(),
                chip.map_or("a target".to_owned(), |c| format!("--chip {c}")),
                list.join(", ")
            ))
        }
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
    // A probe that announces itself on USB is flashed by slot, never through its raw serial port
    // (oep-workflow §3.4): the IDE lists its slots as oep:// ports.
    if let OepAddr::Serial(p) = a
        && port_of_oep_device(p)
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("{p} is an OEP probe's serial port; flash one of its slots instead"),
            Some("pick its oep://<probe>/<slot> port (`ch32rv arduino discovery` lists them)"),
        );
    }
    let mut p = match connect(cli, CMD, a) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let serial = matches!(a, OepAddr::Serial(_)) && single_serial(&mut p);
    if let Err(c) = open_session(cli, CMD, &mut p, serial) {
        return c;
    }
    let r = flash_in_session(cli, args, bytes, &mut p, a);
    // End on every path: the lock is released, the connection stays for the next open.
    let _ = p.end();
    r
}

fn flash_in_session(
    cli: &Cli,
    args: &FlashArgs,
    bytes: &[u8],
    p: &mut Probe,
    a: &OepAddr,
) -> ExitCode {
    const CMD: &str = "flash";
    let (wire, pins) = match choose_place(p, a, cli.chip.as_deref()) {
        Ok(v) => v,
        Err(m) => {
            return fail(
                cli,
                CMD,
                ErrorKind::TargetAmbiguous,
                m,
                Some("name the slot with oep://<probe>/<slot>, or the board's family with --chip"),
            );
        }
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
            pins,
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

/// en: `--chip` against a chip id, for callers that report in text (the Arduino monitor): the DB
/// family when the id resolves, an error when it contradicts `chip`.
/// ja: chip id と `--chip` の照合(文字で報告する呼び出し側向け)。
fn check_family_text(chip_id: Option<u32>, chip: Option<&str>) -> Result<Option<String>, String> {
    let db = ch32rv_target::Db::builtin();
    let detected = chip_id.and_then(|id| match db.resolve_by_chip_id(id) {
        ch32rv_target::Resolution::Sku(s) => Some(s.family.clone()),
        ch32rv_target::Resolution::Family(f, _) => Some(f),
        ch32rv_target::Resolution::Unknown => None,
    });
    if let (Some(d), Some(c)) = (&detected, chip) {
        let fams = db.families_for_chip_name(c);
        if !fams.iter().any(|f| f.eq_ignore_ascii_case(d)) {
            return Err(format!(
                "--chip {c} conflicts with the detected {d} (chip id 0x{:08x})",
                chip_id.unwrap_or(0)
            ));
        }
    }
    Ok(detected)
}

/// What the Arduino monitor streams from an OEP probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamWanted {
    /// The target's console, by mechanism (needs a connection).
    Console(ch32rv_oep::stream::Mechanism),
    /// The fixture's UART at this baud (no connection needed).
    FixtureUart(u32),
}

/// en: A stream on an OEP probe, through the probe's broker, for the Arduino monitor: the target
/// console (attached without halting - a running target is watched, not stopped - at the slot the
/// address or the board's family picks, read from the last reset mark) or the fixture UART (its
/// speed set by `oep.fixture.uart` configure). Dropping it detaches and ends the session (the
/// broker would release both anyway when this client leaves).
/// ja: OEP の probe のストリーム(ブローカー経由、Arduino monitor 用)。target の console(止めずに
/// attach、場所は address か板の家系で選ぶ、最後の reset の mark から読む)か fixture の UART(速さは
/// configure で決める)。drop で detach と end(client が抜ければブローカーも外す)。
pub(crate) struct ConsoleSession {
    probe: Probe,
    stream: ch32rv_oep::stream::PosStream,
    /// The connection to detach at the end (the console's), if any.
    attached: Option<(WireKind, u16)>,
    max_read: u16,
}

impl ConsoleSession {
    pub(crate) fn open(
        a: &OepAddr,
        wanted: StreamWanted,
        chip: Option<&str>,
    ) -> Result<Self, String> {
        let link = match a {
            OepAddr::Serial(p) | OepAddr::Slot { path: p, .. } => crate::broker::client_link(p)?,
            OepAddr::Tcp(t) => ch32rv_oep::link::open_tcp(t).map_err(|e| e.to_string())?,
            OepAddr::Wch(t) => crate::broker::client_link_for(t)?,
        };
        let mut probe = Probe::connect(link).map_err(|e| e.to_string())?;
        let owner = format!("ch32rv monitor pid {}", std::process::id());
        probe
            .open(random_session_id(), 3000, false, Some(&owner))
            .map_err(|e| e.to_string())?;
        let (stream, attached) = match wanted {
            StreamWanted::FixtureUart(baud) => {
                let (s, _) = ch32rv_oep::stream::PosStream::open_uart(&mut probe, baud).map_err(
                    |e| match e {
                        OepError::Rejected { reason, .. }
                            if reason == ch32rv_oep::registry::reject_reasons::UNAVAILABLE =>
                        {
                            "the probe's fixture UART has no pins assigned (set its plan in the probe's configuration)".to_owned()
                        }
                        OepError::NoInterface(_) => "this OEP probe has no fixture UART".to_owned(),
                        e => format!("fixture UART: {e}"),
                    },
                )?;
                (s, None)
            }
            StreamWanted::Console(mech) => {
                let (wire, pins) = choose_place(&mut probe, a, chip)?;
                let at = attach(
                    &mut probe,
                    wire,
                    AttachOptions {
                        halt: false,
                        max_speed_hz: None,
                        pins,
                    },
                )
                .map_err(|e| e.to_string())?;
                check_family_text(at.wch_chip_id, chip)?;
                let mut s =
                    ch32rv_oep::stream::PosStream::open_console(&mut probe, at.connection, mech)
                        .map_err(|e| e.to_string())?;
                s.start_at_last_reset(&mut probe)
                    .map_err(|e| e.to_string())?;
                (s, Some((wire, at.connection)))
            }
        };
        let max_read = probe.limits().max_frame.saturating_sub(14).clamp(16, 1000);
        Ok(ConsoleSession {
            probe,
            stream,
            attached,
            max_read,
        })
    }

    /// What arrived since the last poll.
    pub(crate) fn poll(&mut self) -> Result<Vec<u8>, String> {
        self.stream
            .poll(&mut self.probe, self.max_read)
            .map(|c| c.data)
            .map_err(|e| e.to_string())
    }

    /// Send input; returns how many bytes were taken (resend the rest later).
    pub(crate) fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        self.stream
            .write(&mut self.probe, data)
            .map_err(|e| e.to_string())
    }

    /// The fixture UART's new speed (the monitor's baudrate changed while open).
    pub(crate) fn set_baud(&mut self, baud: u32) -> Result<(), String> {
        self.stream
            .configure_baud(&mut self.probe, baud)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

impl Drop for ConsoleSession {
    fn drop(&mut self) {
        if let Some((wire, conn)) = self.attached {
            let _ = detach(&mut self.probe, wire, conn, false);
        }
        let _ = self.probe.end();
    }
}

// ---- other commands on an OEP probe ----

/// What an attached command gets.
struct Attached<'a> {
    t: OepDtm<'a>,
    chip_id: Option<u32>,
    family: Option<String>,
}

/// en: Connect, open a session, attach at the place `choose_place` picks (halting when asked),
/// run `f`, then detach and end on every path. `family` is the chip id's DB family, checked
/// against `--chip`.
/// ja: 接続して session を開き、`choose_place` の場所に attach し(指定があれば止めて)、`f` を
/// 走らせ、どの経路でも detach と end をする。
fn with_attached(
    cli: &Cli,
    cmd: &str,
    a: &OepAddr,
    halt: bool,
    f: impl FnOnce(&mut Attached<'_>) -> ExitCode,
) -> ExitCode {
    let mut p = match connect(cli, cmd, a) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let serial = matches!(a, OepAddr::Serial(_)) && single_serial(&mut p);
    if let Err(c) = open_session(cli, cmd, &mut p, serial) {
        return c;
    }
    let r = (|| {
        let (wire, pins) = match choose_place(&mut p, a, cli.chip.as_deref()) {
            Ok(v) => v,
            Err(m) => return fail(cli, cmd, ErrorKind::TargetAmbiguous, m, None),
        };
        let at = match attach(
            &mut p,
            wire,
            AttachOptions {
                halt,
                max_speed_hz: None,
                pins,
            },
        ) {
            Ok(a) => a,
            Err(e) => return oep_fail(cli, cmd, e),
        };
        let family = match check_family_text(at.wch_chip_id, cli.chip.as_deref()) {
            Ok(f) => f,
            Err(m) => {
                let _ = detach(&mut p, wire, at.connection, false);
                return fail(cli, cmd, ErrorKind::TargetAmbiguous, m, None);
            }
        };
        let r = match OepDtm::new(&mut p, at.connection) {
            Ok(t) => f(&mut Attached {
                t,
                chip_id: at.wch_chip_id,
                family,
            }),
            Err(e) => oep_fail(cli, cmd, e),
        };
        let _ = detach(&mut p, wire, at.connection, false);
        r
    })();
    let _ = p.end();
    r
}

/// The DB SKU of a chip id (flash / SRAM sizes).
fn sku_of(chip_id: Option<u32>) -> Option<ch32rv_target::SkuRecord> {
    match ch32rv_target::Db::builtin().resolve_by_chip_id(chip_id?) {
        ch32rv_target::Resolution::Sku(s) => Some(s.clone()),
        _ => None,
    }
}

/// `reset` on an OEP probe: run (confirmed with `--confirm-run`) or halt at reset.
pub(crate) fn reset(cli: &Cli, args: &crate::args::ResetArgs, a: &OepAddr) -> ExitCode {
    const CMD: &str = "reset";
    if args.dm {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "--dm is a WCH-Link operation; an OEP probe resets the target",
            None,
        );
    }
    with_attached(cli, CMD, a, false, |x| {
        let mode = if args.halt {
            ResetMode::HaltAtReset
        } else if args.confirm_run.is_some() {
            ResetMode::RunVerified
        } else {
            ResetMode::Run
        };
        let (ok, pc) = match x.t.reset(mode) {
            Ok(r) => (true, r.pc),
            Err(e) if mode == ResetMode::RunVerified => {
                let _ = e;
                (false, 0)
            }
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("reset: {e}"),
                    None,
                );
            }
        };
        let running = (mode == ResetMode::RunVerified).then_some(ok);
        if cli.json {
            let mut env = if running == Some(false) {
                ResultEnvelope::failure(
                    CMD,
                    ErrorKind::NotRunningAfterWrite,
                    "target not running after reset",
                )
            } else {
                ResultEnvelope::success(CMD)
            };
            env.result = Some(serde_json::json!({
                "mode": if args.halt { "halt" } else { "run" },
                "running": running,
                "pc": format!("0x{pc:08x}"),
            }));
            crate::print_envelope(&env)
        } else if running == Some(false) {
            eprintln!("ch32rv: error[not-running-after-write]: target not running after reset");
            ErrorKind::NotRunningAfterWrite.exit_code().into()
        } else {
            println!(
                "{}",
                if args.halt {
                    "reset and halted"
                } else {
                    "reset, running"
                }
            );
            ExitCode::SUCCESS
        }
    })
}

/// Resume a hart this command halted (the CH32 rule: re-issued while dpc has not moved).
fn resume_after(t: &mut OepDtm<'_>) {
    let _ = ch32rv_dmi::resume_ch32(t, |t| {
        ch32rv_dmi::DebugModule::new(t).read_reg(ch32rv_dmi::RegName::Pc)
    });
}

/// `verify` on an OEP probe: read the image's ranges back and compare.
pub(crate) fn verify(
    cli: &Cli,
    args: &crate::args::VerifyArgs,
    bytes: &[u8],
    a: &OepAddr,
) -> ExitCode {
    const CMD: &str = "verify";
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
    with_attached(cli, CMD, a, true, |x| {
        let mut first_bad = None;
        for seg in &image.segments {
            let (lo, hi) = (
                seg.addr & !3,
                (seg.addr + seg.data.len() as u32).div_ceil(4) * 4,
            );
            let got = match x.t.read_words(lo, ((hi - lo) / 4) as usize) {
                Ok(w) => w.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>(),
                Err(e) => {
                    resume_after(&mut x.t);
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TransferFailed,
                        format!("read: {e}"),
                        None,
                    );
                }
            };
            let off = (seg.addr - lo) as usize;
            if let Some(i) = (0..seg.data.len()).find(|&i| got[off + i] != seg.data[i]) {
                first_bad = Some(seg.addr + i as u32);
                break;
            }
        }
        resume_after(&mut x.t);
        match first_bad {
            Some(at) => fail(
                cli,
                CMD,
                ErrorKind::VerifyMismatch,
                format!("mismatch at {at:#010x}"),
                None,
            ),
            None if cli.json => {
                let mut env = ResultEnvelope::success(CMD);
                env.result =
                    Some(serde_json::json!({ "bytes": image.total_len(), "verified": true }));
                crate::print_envelope(&env)
            }
            None => {
                println!("verify: OK ({} bytes match)", image.total_len());
                ExitCode::SUCCESS
            }
        }
    })
}

/// `read` on an OEP probe: `--range` / `--region`, dumped or blank-checked.
pub(crate) fn read(cli: &Cli, args: &crate::args::ReadArgs, a: &OepAddr) -> ExitCode {
    const CMD: &str = "read";
    with_attached(cli, CMD, a, true, |x| {
        let sku = sku_of(x.chip_id);
        let (flash, sram) = sku
            .as_ref()
            .map_or((0, 0), |s| (s.flash_bytes, s.sram_bytes));
        let option_base = x
            .family
            .as_deref()
            .and_then(ch32rv_target::option_bytes_layout)
            .map(|l| l.base);
        let (start, len) = match crate::cmd_dbg::resolve_range(args, flash, sram, option_base) {
            Ok(v) => v,
            Err(m) => {
                resume_after(&mut x.t);
                return fail(cli, CMD, ErrorKind::Usage, m, None);
            }
        };
        let lo = start & !3;
        let words = (start + len - lo).div_ceil(4) as usize;
        let data = match x.t.read_words(lo, words) {
            Ok(w) => {
                let b: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
                b[(start - lo) as usize..(start - lo + len) as usize].to_vec()
            }
            Err(e) => {
                resume_after(&mut x.t);
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("read: {e}"),
                    None,
                );
            }
        };
        resume_after(&mut x.t);
        if args.blank_check {
            let blank = data.iter().all(|&b| b == 0xff);
            if cli.json {
                let mut env = if blank {
                    ResultEnvelope::success(CMD)
                } else {
                    ResultEnvelope::failure(CMD, ErrorKind::BlankCheckFailed, "region is not blank")
                };
                env.result = Some(serde_json::json!({
                    "addr": format!("0x{start:08x}"), "len": len, "blank": blank,
                }));
                return crate::print_envelope(&env);
            }
            println!(
                "blank check 0x{start:08x}+{len}: {}",
                if blank { "BLANK" } else { "NOT BLANK" }
            );
            return if blank {
                ExitCode::SUCCESS
            } else {
                ErrorKind::BlankCheckFailed.exit_code().into()
            };
        }
        crate::cmd_dbg::output_data(cli, CMD, args, start, &data, Vec::new())
    })
}

/// `target info` on an OEP probe: what the chip id says, without halting the target.
pub(crate) fn target_info(cli: &Cli, a: &OepAddr) -> ExitCode {
    const CMD: &str = "target.info";
    with_attached(cli, CMD, a, false, |x| {
        let sku = sku_of(x.chip_id);
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({
                "probe": { "kind": "oep" },
                "target": {
                    "chip_id": x.chip_id.map(|c| format!("0x{c:08x}")),
                    "family": x.family,
                    "sku": sku.as_ref().map(|s| s.sku.clone()),
                    "flash_bytes": sku.as_ref().map(|s| s.flash_bytes),
                    "sram_bytes": sku.as_ref().map(|s| s.sram_bytes),
                },
            }));
            crate::print_envelope(&env)
        } else {
            println!("probe:    OEP");
            match x.chip_id {
                Some(c) => println!("chip_id:  0x{c:08x}"),
                None => println!("chip_id:  (the probe read none)"),
            }
            println!("family:   {}", x.family.as_deref().unwrap_or("unknown"));
            if let Some(s) = &sku {
                println!("sku:      {}", s.sku);
                println!("flash:    {} KiB", s.flash_bytes / 1024);
                println!("sram:     {} KiB", s.sram_bytes / 1024);
            }
            ExitCode::SUCCESS
        }
    })
}
