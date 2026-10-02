//! en: Commands on an OEP probe (docs/oep-host.ja.md §4-§6): recognising one from `--probe`,
//! opening a session with the lock rules of ArduinoCore-CH32RV oep-workflow §4.3, attaching, and
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

/// en: The probe opened on some port is also on USB as an OEP device that discovery lists (its
/// slots are the IDE's ports): its describe unit_id is the serial number of one of
/// [`oep_devices`] (oep-core §3.3 / §7.5, compared case aside). `discoverable` cannot tell this
/// until the project's VID:PID exists (oep-spec 2108125).
/// ja: どこかの口で開いた probe が、discovery の一覧に出る USB の OEP の device でもあるか(describe の
/// unit_id が USB の serial と同じか、大小は区別しない)。
fn listed_on_usb(p: &mut Probe) -> bool {
    let Ok(tlvs) = p.describe(oep_core::FN) else {
        return false;
    };
    let Some(unit) = tlvs
        .iter()
        .find(|t| t.tag == oep_core::tlvs::describe::UNIT_ID)
        .map(|t| String::from_utf8_lossy(&t.value).into_owned())
    else {
        return false;
    };
    let serials: Vec<String> = oep_devices()
        .iter()
        .filter_map(|d| d.serial().map(str::to_owned))
        .collect();
    unit_among(&unit, &serials)
}

/// Whether `unit_id` is one of `serials`, case aside (oep-core §3.3).
fn unit_among(unit_id: &str, serials: &[String]) -> bool {
    !unit_id.is_empty() && serials.iter().any(|s| s.eq_ignore_ascii_case(unit_id))
}

/// Whether the serial port `path` belongs to an OEP probe (as [`is_oep_device`] decides).
fn port_of_oep_device(path: &str) -> bool {
    let sel = Selector::Port(path.to_owned());
    oep_devices()
        .iter()
        .enumerate()
        .any(|(i, d)| sel.matches(d, i))
}

/// An OEP vendor bulk pair as a link's byte stream (`length(u16)` framing, oep-core §3.1).
struct VendorBulk(ch32rv_usb::BulkPipe);

impl ch32rv_oep::link::ByteStream for VendorBulk {
    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.0
            .write(data, Duration::from_secs(1))
            .map_err(std::io::Error::other)
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        self.0.read(buf, timeout).map_err(std::io::Error::other)
    }
}

/// An OEP HID interface as a link's byte stream: the length-framed stream packed into its vendor
/// reports (oep-core §3.1).
struct OepHid {
    dev: hidapi::HidDevice,
    shape: ch32rv_oep::hid::ReportShape,
    rx: std::collections::VecDeque<u8>,
}

impl OepHid {
    /// The vendor HID interface of the OEP device `dev`, if it has one that opens.
    fn open(dev: &ch32rv_usb::UsbDeviceInfo) -> Option<OepHid> {
        let api = hidapi::HidApi::new().ok()?;
        for info in api.device_list() {
            if info.vendor_id() != dev.vid()
                || info.product_id() != dev.pid()
                || info.serial_number() != dev.serial()
                || info.usage_page() != ch32rv_oep::registry::usb::HID_USAGE_PAGE
                || info.usage() != u16::from(ch32rv_oep::registry::usb::HID_USAGE)
            {
                continue;
            }
            let Ok(h) = info.open_device(&api) else {
                continue;
            };
            let mut desc = [0u8; 4096];
            let Ok(n) = h.get_report_descriptor(&mut desc) else {
                continue;
            };
            if let Some(shape) = ch32rv_oep::hid::vendor_report(&desc[..n]) {
                return Some(OepHid {
                    dev: h,
                    shape,
                    rx: std::collections::VecDeque::new(),
                });
            }
        }
        None
    }
}

impl ch32rv_oep::link::ByteStream for OepHid {
    fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        for r in self.shape.pack(data) {
            self.dev.write(&r).map_err(std::io::Error::other)?;
        }
        Ok(())
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> std::io::Result<usize> {
        if self.rx.is_empty() {
            let mut r = vec![0u8; 1 + self.shape.input];
            let ms = i32::try_from(timeout.as_millis().max(1)).unwrap_or(i32::MAX);
            let n = self
                .dev
                .read_timeout(&mut r, ms)
                .map_err(std::io::Error::other)?;
            if let Some(d) = self.shape.unpack(&r[..n]) {
                self.rx.extend(d);
            }
        }
        let n = buf.len().min(self.rx.len());
        for (d, s) in buf.iter_mut().zip(self.rx.drain(..n)) {
            *d = s;
        }
        Ok(n)
    }
}

/// en: Connect (confirm) to the probe behind the serial port `path` by the transports in the order
/// oep-core §3.3 gives: the vendor bulk pair of the OEP device that owns the port, its vendor HID,
/// then the port itself. `timeout` shortens each reply wait (discovery). `CH32RV_OEP_TRANSPORT`
/// (`vendor-bulk` / `hid` / `serial`) starts the order at that transport. Returns the probe and the
/// transport's name.
/// ja: serial port `path` の裏の probe に、§3.3 の順(その port を持つ OEP の device の vendor bulk、
/// vendor の HID、port そのもの)でつなぐ(confirm まで)。`CH32RV_OEP_TRANSPORT` はその経路から始める。
pub(crate) fn connect_upstream(
    path: &str,
    timeout: Option<Duration>,
) -> Result<(Probe, &'static str), String> {
    connect_ordered(path, timeout, false)
}

/// en: For discovery's lock-free reads (oep-workflow §3.3): the probe's HID first, which no other
/// tool holds and which does not contend with a monitor for the serial port (TIOCEXCL) or leave
/// DTR on a bound CDC; then vendor bulk, then the serial port.
/// ja: discovery の lock 無しの読み出し用。HID を先に(他の道具が握らず、monitor と serial port を
/// 取り合わず、bind のある CDC に DTR を残さない)、次に vendor bulk、最後に serial port。
pub(crate) fn connect_for_listing(
    path: &str,
    timeout: Duration,
) -> Result<(Probe, &'static str), String> {
    connect_ordered(path, Some(timeout), true)
}

fn connect_ordered(
    path: &str,
    timeout: Option<Duration>,
    hid_first: bool,
) -> Result<(Probe, &'static str), String> {
    let start = std::env::var("CH32RV_OEP_TRANSPORT").unwrap_or_default();
    let sel = Selector::Port(path.to_owned());
    let dev = oep_devices()
        .into_iter()
        .enumerate()
        .find(|(i, d)| sel.matches(d, *i))
        .map(|(_, d)| d);
    let try_link = |stream: Box<dyn ch32rv_oep::link::ByteStream>| {
        let mut link = ch32rv_oep::link::Link::new(stream, ch32rv_oep::link::Framing::Length);
        if let Some(t) = timeout {
            link.set_timeout(t);
        }
        Probe::connect(link).ok()
    };
    if let Some(d) = &dev {
        if hid_first
            && matches!(start.as_str(), "" | "hid")
            && let Some(p) = OepHid::open(d).and_then(|h| try_link(Box::new(h)))
        {
            return Ok((p, "hid"));
        }
        if !matches!(start.as_str(), "hid" | "serial")
            && let Some(p) = d
                // class 0xFF, subclass 'O', protocol 'E' (oep-core §3.3)
                .open_vendor_bulk(
                    ch32rv_oep::registry::usb::VENDOR_BULK_SUBCLASS,
                    ch32rv_oep::registry::usb::VENDOR_BULK_PROTOCOL,
                )
                .ok()
                .flatten()
                .and_then(|pipe| try_link(Box::new(VendorBulk(pipe))))
        {
            return Ok((p, "vendor-bulk"));
        }
        if !hid_first
            && start != "serial"
            && let Some(p) = OepHid::open(d).and_then(|h| try_link(Box::new(h)))
        {
            return Ok((p, "hid"));
        }
    }
    let mut link = ch32rv_oep::link::open_serial(path).map_err(|e| e.to_string())?;
    if let Some(t) = timeout {
        link.set_timeout(t);
    }
    Probe::connect(link)
        .map(|p| (p, "serial"))
        .map_err(|e| e.to_string())
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
        // unit_id against the USB serial, case aside (oep-core §3.3: some OSes and tools show the
        // serial in capitals; unit_id's characters keep distinct values distinct).
        .find(|d| probe_id(d).eq_ignore_ascii_case(id))
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
pub(crate) fn running_wch_broker(cli: &Cli, cmd: &str) -> Option<OepAddr> {
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
/// en: Where to attach: the wire, the slot's pins, and the line settings the slot carries (its
/// speed ceiling and idle clock, which the host passes on attach, oep-spec 5bfe052).
/// ja: attach する場所。線、スロットのピン、スロットの線の設定(速さの上限と休ませ方。attach で host が渡す)。
#[derive(Debug, Clone, Copy)]
struct Place {
    wire: WireKind,
    pins: Option<(u16, u16)>,
    max_speed: Option<u32>,
    idle_clock: Option<u8>,
}

impl Place {
    fn of_slot(wire: WireKind, s: &ch32rv_oep::config::Slot) -> Place {
        Place {
            wire,
            pins: Some((s.swdio, s.swclk)),
            max_speed: s.max_speed,
            idle_clock: Some(s.idle_clock),
        }
    }

    /// Attach options here: the lower of the slot's ceiling and `max_speed_hz` (the command's).
    fn options(&self, halt: bool, max_speed_hz: Option<u32>) -> AttachOptions {
        let max_speed_hz = match (self.max_speed, max_speed_hz) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        AttachOptions {
            halt,
            max_speed_hz,
            pins: self.pins,
            idle_clock: self.idle_clock,
            reset: None,
        }
    }
}

fn choose_place(p: &mut Probe, a: &OepAddr, chip: Option<&str>) -> Result<Place, String> {
    let slots = ch32rv_oep::config::slots(p).map_err(|e| e.to_string())?;
    if let OepAddr::Slot { slot, .. } = a {
        let s = slots
            .iter()
            .find(|s| s.name == *slot)
            .ok_or_else(|| format!("the probe has no slot `{slot}`"))?;
        let w = wire_of_fn(p, s.wire_fn)
            .ok_or_else(|| format!("slot `{slot}` is on an unknown wire (fn {})", s.wire_fn))?;
        return Ok(Place::of_slot(w, s));
    }
    if slots.is_empty() {
        let wire = pick_wire(p, chip).map_err(|e| e.to_string())?;
        return place_by_scan(p, wire, chip);
    }
    let states = ch32rv_oep::config::slot_states(p).map_err(|e| e.to_string())?;
    struct Seen {
        name: String,
        place: Place,
        family: Option<String>,
    }
    let mut seen: Vec<Seen> = Vec::new();
    for s in &slots {
        let Some(w) = wire_of_fn(p, s.wire_fn) else {
            continue;
        };
        let place = Place::of_slot(w, s);
        let from_state = states
            .iter()
            .find(|st| st.slot == s.slot)
            .and_then(|st| st.wch_chip_id());
        // Not connected: a non-halting attach reads the chip (the user asked to use the probe).
        let id = from_state.or_else(|| {
            let at = attach(p, w, place.options(false, None)).ok()?;
            let _ = detach(p, w, at.connection, false);
            at.wch_chip_id
        });
        seen.push(Seen {
            name: s.name.clone(),
            place,
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
        [one] => Ok(one.place),
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

/// en: No slot registered: where on `wire` the target is. A wire whose pins the probe fixes
/// (channel_group) takes an attach without pins; one whose pins the host picks (role_channels)
/// refuses that when it allows more than one pair (oep-if-debug §1), so its pairs are scanned and
/// the one with a target is used - several, told apart by the chip each one reads and `--chip`,
/// as with slots.
/// ja: スロットが無いとき、`wire` のどこに target が居るか。ピンを probe が決める線(channel_group)は
/// pins 無しの attach を受ける。host が選ぶ線(role_channels)は組が 2 つ以上だと断るので、scan して
/// target の居る組を使う(複数なら、スロットと同じく各組の chip と `--chip` で絞る)。
fn place_by_scan(p: &mut Probe, wire: WireKind, chip: Option<&str>) -> Result<Place, String> {
    let bare = Place {
        wire,
        pins: None,
        max_speed: None,
        idle_clock: None,
    };
    if !host_picks_pins(p, wire) {
        return Ok(bare);
    }
    let found = ch32rv_oep::target::scan_all(p, wire).map_err(|e| format!("scan: {e}"))?;
    let places: Vec<Place> = found
        .iter()
        .map(|f| Place {
            pins: Some((f.swdio, f.swclk)),
            ..bare
        })
        .collect();
    match places.as_slice() {
        [] => Err(format!(
            "no target found on any pin pair of {} (scan)",
            wire.interface()
        )),
        [one] => Ok(*one),
        _ => {
            let db = ch32rv_target::Db::builtin();
            let wanted = chip.map(|c| db.families_for_chip_name(c));
            let mut seen = Vec::new();
            for pl in &places {
                let family = attach(p, wire, pl.options(false, None))
                    .ok()
                    .and_then(|at| {
                        let _ = detach(p, wire, at.connection, false);
                        at.wch_chip_id.and_then(family_of_chip_id)
                    });
                seen.push((*pl, family));
            }
            let matches: Vec<&(Place, Option<String>)> = seen
                .iter()
                .filter(|(_, f)| match (&wanted, f) {
                    (Some(ws), Some(f)) => ws.iter().any(|w| w.eq_ignore_ascii_case(f)),
                    (None, Some(_)) => true,
                    _ => false,
                })
                .collect();
            match matches.as_slice() {
                [(one, _)] => Ok(*one),
                _ => {
                    let list: Vec<String> = seen
                        .iter()
                        .map(|(pl, f)| {
                            let (d, c) = pl.pins.unwrap_or_default();
                            format!("pins {d}/{c}: {}", f.as_deref().unwrap_or("no target"))
                        })
                        .collect();
                    Err(format!(
                        "{} pin pair(s) match {}: {} (register a slot for the board's pins)",
                        matches.len(),
                        chip.map_or("a target".to_owned(), |c| format!("--chip {c}")),
                        list.join(", ")
                    ))
                }
            }
        }
    }
}

/// Whether the probe lets the host pick `wire`'s pins (describe `role_channels`, not a fixed
/// channel_group).
fn host_picks_pins(p: &mut Probe, wire: WireKind) -> bool {
    let Ok(func) = p.interface(wire.interface()).map(|i| i.func) else {
        return false;
    };
    p.describe(func).is_ok_and(|tlvs| {
        tlvs.iter()
            .any(|t| t.tag == ch32rv_oep::registry::describe_common::ROLE_CHANNELS)
    })
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
        // A chip id the DB does not know is not-in-db; no chip id at all is no response.
        (None, None) => Err(match chip_id {
            Some(id) => fail(
                cli,
                cmd,
                ErrorKind::TargetNotInDb,
                format!("chip id 0x{id:08x} is not in the target DB"),
                Some("the part is not in this build's DB (`ch32rv db list`)"),
            ),
            None => fail(
                cli,
                cmd,
                ErrorKind::TargetNoResponse,
                "the probe read no chip id at attach",
                Some("check the wiring and power, or pass --chip to name the target"),
            ),
        }),
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
    // The same probe reached on another serial port (its USB-Serial/JTAG, a UART bridge) is also
    // on USB as an OEP device, whose slots are the IDE's ports: its unit_id is that serial.
    if matches!(a, OepAddr::Serial(_)) && listed_on_usb(&mut p) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "this serial port belongs to an OEP probe that is also on USB as an OEP device; flash one of its slots instead",
            Some("pick its oep://<probe>/<slot> port (`ch32rv arduino discovery` lists them)"),
        );
    }
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
    let place = match choose_place(p, a, cli.chip.as_deref()) {
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
    let wire = place.wire;
    let at = match attach(p, wire, place.options(true, max_speed_hz)) {
        Ok(a) => a,
        Err(e) => return oep_fail(cli, CMD, e),
    };
    // A broker that went away says why in its log (its stderr goes nowhere).
    let log_hint = match a {
        OepAddr::Serial(path) | OepAddr::Slot { path, .. } => Some(format!(
            "the probe's broker keeps a log: {}",
            crate::broker::log_path(&crate::broker::BrokerTarget::Serial(path.clone()).key())
                .display()
        )),
        _ => None,
    };
    let r = flash_attached(cli, args, bytes, p, at.connection, at.wch_chip_id, log_hint);
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
    log_hint: Option<String>,
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
        Err(e) => {
            let kind = crate::cmd_flash::loader_error_kind(&e);
            let hint = log_hint.filter(|_| kind != ErrorKind::VerifyMismatch);
            return fail(cli, CMD, kind, e.to_string(), hint.as_deref());
        }
    };
    let secs = started.elapsed().as_secs_f64();
    // A verified reset (--confirm-run) that came back is a running target; a plain one says nothing.
    let running = (args.reset == ch32rv_contract::policy::ResetPolicy::Run
        && args.confirm_run.is_some())
    .then_some(true);
    if args.reset == ch32rv_contract::policy::ResetPolicy::Run {
        // `--confirm-run` asks the probe to check that the hart runs (exit 50 when it does not).
        let mode = if args.confirm_run.is_some() {
            ResetMode::RunVerified
        } else {
            ResetMode::Run
        };
        match t.reset(mode) {
            Ok(_) => {}
            Err(e @ ch32rv_dmi::DmiError::NotReached(_)) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::NotRunningAfterWrite,
                    format!("target not running after reset: {e}"),
                    None,
                );
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
        }
    }
    let total = image.total_len();
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "flash": {
                "bytes": total,
                "programmer": "oep-loader",
                "family": family,
                "chip_id": chip_id.map(|c| format!("0x{c:08x}")),
                "pages": report.pages,
                "rewritten": report.rewritten,
                "restarted_runs": report.restarted_runs,
                // Every page is read back after it is written: verified, or the run failed.
                "verified": true,
                "skipped": false,
                "scope": "pages",
                "running": running,
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
    // A SKU is checked as that SKU (its device id), not only its family.
    if let (Some(id), Some(c)) = (chip_id, chip)
        && let Some(other) = db.sku_conflict(c, id)
    {
        return Err(format!(
            "--chip {c} conflicts with the detected {other} (chip id 0x{id:08x})"
        ));
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
    /// SEGGER RTT in the target's RAM, run by ch32rv over the probe's riscv-dm (the probe's
    /// console has no RTT mechanism; the host halts the hart briefly per poll, as on a WCH-Link).
    Rtt,
}

/// en: RTT over an OEP probe's riscv-dm (`crate::source::rtt_*`): run control through the probe's
/// own ops (its resume lets it poll its consoles again), memory by block reads and Debug Module
/// writes.
/// ja: OEP の probe の riscv-dm の上の RTT。run control は probe の op(その resume で probe は console の
/// poll に戻る)、memory は block 読みと Debug Module の書き込み。
// en: Only the probe's own operations between halt and resume (and DMSTATUS reads, which write
// nothing): its block ops borrow s0 / s1 / a0 / a1 and put them back on its resume, but a raw DMI
// write from the host makes it drop what it kept (oep-probe-arduino ac3e026, `writeDmi`), and the
// target then runs on with the probe's addresses in s0 (a CH32X035 sketch faulted in its loop,
// 2026-10-01). So: no `DebugModule` memory access, no dpc read around the resume.
// ja: halt から resume までは probe の op だけを使う(DMSTATUS の読み出しは何も書かないので可)。
// host の raw DMI write があると probe は退避した s0 / s1 / a0 / a1 を捨て、target は probe の番地を
// s0 に持ったまま走る(X035 の sketch が落ちた)。
impl crate::source::RttTarget for OepDtm<'_> {
    fn is_halted(&mut self) -> Result<bool, ch32rv_dmi::DmiError> {
        ch32rv_dmi::DebugModule::new(self).is_halted()
    }

    fn halt(&mut self) -> Result<(), ch32rv_dmi::DmiError> {
        TargetAccess::halt(self)
    }

    fn resume(&mut self) -> Result<(), ch32rv_dmi::DmiError> {
        // A resume that does not take (the CH32V006 now and then) is asked again while the hart is
        // still halted; the dpc check of `resume_ch32` would be a raw abstract command.
        for _ in 0..8 {
            match TargetAccess::resume_once(self) {
                Ok(()) => return Ok(()),
                Err(ch32rv_dmi::DmiError::OperationFailed(_)) => {
                    if !ch32rv_dmi::DebugModule::new(self).is_halted()? {
                        return Ok(());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(ch32rv_dmi::DmiError::OperationFailed(
            "hart did not resume".to_owned(),
        ))
    }

    fn read(&mut self, addr: u32, len: u32) -> Result<Vec<u8>, ch32rv_dmi::DmiError> {
        let start = addr & !3;
        let end = addr.saturating_add(len).saturating_add(3) & !3;
        let words = self.read_words(start, ((end - start) / 4) as usize)?;
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let from = (addr - start) as usize;
        Ok(bytes
            .get(from..from + len as usize)
            .map(<[u8]>::to_vec)
            .unwrap_or_default())
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), ch32rv_dmi::DmiError> {
        if data.is_empty() {
            return Ok(());
        }
        // Whole words through the probe's block write; the bytes around an unaligned edge are
        // read first and written back as they were.
        let start = addr & !3;
        let end = addr.saturating_add(data.len() as u32).saturating_add(3) & !3;
        let mut bytes = if start == addr && end - start == data.len() as u32 {
            vec![0; data.len()]
        } else {
            self.read(start, end - start)?
        };
        let from = (addr - start) as usize;
        if let Some(dst) = bytes.get_mut(from..from + data.len()) {
            dst.copy_from_slice(data);
        }
        let words: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        self.write_words(start, &words)
    }
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
    stream: Backing,
    /// The connection to detach at the end (the console's), if any.
    attached: Option<(WireKind, u16)>,
    max_read: u16,
}

/// Where a [`ConsoleSession`]'s bytes come from.
enum Backing {
    /// A position stream the probe keeps (console / fixture UART).
    Stream(ch32rv_oep::stream::PosStream),
    /// RTT the host runs: the connection, what OepDtm learned, the channels, input not yet taken.
    Rtt {
        connection: u16,
        parts: (u16, usize, u32),
        channels: crate::source::RttChannels,
        input: Vec<u8>,
    },
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
                // Like the console: from the last reset mark (else now), not the oldest kept byte.
                let mut s = s;
                s.start_at_last_reset(&mut probe)
                    .map_err(|e| e.to_string())?;
                (Backing::Stream(s), None)
            }
            StreamWanted::Rtt => {
                let place = choose_place(&mut probe, a, chip)?;
                let wire = place.wire;
                let at = attach(&mut probe, wire, place.options(false, None))
                    .map_err(|e| e.to_string())?;
                check_family_text(at.wch_chip_id, chip)?;
                let mut t = OepDtm::new(&mut probe, at.connection).map_err(|e| e.to_string())?;
                let mut warnings = Vec::new();
                let found = crate::source::rtt_find(
                    &mut t,
                    crate::source::rtt_scan_len(at.wch_chip_id),
                    &mut warnings,
                );
                // rtt_find leaves the hart halted (found or not): let it run again.
                let resumed = crate::source::RttTarget::resume(&mut t);
                let channels = found.map_err(|e| e.to_string())?;
                resumed.map_err(|e| format!("resume after the RTT scan: {e}"))?;
                for w in warnings {
                    eprintln!("warning[{}]: {}", w.code, w.msg);
                }
                let parts = t.parts();
                (
                    Backing::Rtt {
                        connection: at.connection,
                        parts,
                        channels,
                        input: Vec::new(),
                    },
                    Some((wire, at.connection)),
                )
            }
            StreamWanted::Console(mech) => {
                let place = choose_place(&mut probe, a, chip)?;
                let wire = place.wire;
                let at = attach(&mut probe, wire, place.options(false, None))
                    .map_err(|e| e.to_string())?;
                check_family_text(at.wch_chip_id, chip)?;
                let mut s =
                    ch32rv_oep::stream::PosStream::open_console(&mut probe, at.connection, mech)
                        .map_err(|e| e.to_string())?;
                s.start_at_last_reset(&mut probe)
                    .map_err(|e| e.to_string())?;
                (Backing::Stream(s), Some((wire, at.connection)))
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

    /// What arrived since the last poll (RTT: one exchange, which also hands over pending input).
    pub(crate) fn poll(&mut self) -> Result<Vec<u8>, String> {
        match &mut self.stream {
            Backing::Stream(s) => s
                .poll(&mut self.probe, self.max_read)
                .map(|c| c.data)
                .map_err(|e| e.to_string()),
            Backing::Rtt {
                connection,
                parts,
                channels,
                input,
            } => {
                let mut t = OepDtm::from_parts(&mut self.probe, *connection, *parts);
                crate::source::rtt_poll(&mut t, *channels, input).map_err(|e| e.to_string())
            }
        }
    }

    /// Send input; returns how many bytes were taken (resend the rest later). RTT keeps it and
    /// hands it to the target's down ring on the next polls.
    pub(crate) fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        match &mut self.stream {
            Backing::Stream(s) => s.write(&mut self.probe, data).map_err(|e| e.to_string()),
            Backing::Rtt { input, .. } => {
                input.extend_from_slice(data);
                Ok(data.len())
            }
        }
    }

    /// The fixture UART's new speed (the monitor's baudrate changed while open).
    pub(crate) fn set_baud(&mut self, baud: u32) -> Result<(), String> {
        match &self.stream {
            Backing::Stream(s) => s
                .configure_baud(&mut self.probe, baud)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Backing::Rtt { .. } => Ok(()),
        }
    }

    /// Whether this session is RTT (polled at RTT's pace: each poll halts the hart briefly).
    pub(crate) fn is_rtt(&self) -> bool {
        matches!(self.stream, Backing::Rtt { .. })
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
pub(crate) struct Attached<'a> {
    pub(crate) t: OepDtm<'a>,
    pub(crate) chip_id: Option<u32>,
    pub(crate) family: Option<String>,
    /// The attach found a connection already there (another client's).
    pub(crate) existing: bool,
    /// Where the hart stood when the attach halted it, if it did.
    pub(crate) halted_at: Option<u32>,
}

/// en: Connect, open a session, attach at the place `choose_place` picks (halting when asked),
/// run `f`, then detach and end on every path. `family` is the chip id's DB family, checked
/// against `--chip`.
/// ja: 接続して session を開き、`choose_place` の場所に attach し(指定があれば止めて)、`f` を
/// 走らせ、どの経路でも detach と end をする。
pub(crate) fn with_attached(
    cli: &Cli,
    cmd: &str,
    a: &OepAddr,
    halt: bool,
    f: impl FnOnce(&mut Attached<'_>) -> ExitCode,
) -> ExitCode {
    with_attached_reset(cli, cmd, a, halt, false, f)
}

/// How long the NRST line is held low on an attach under reset (the probe's own retry uses the
/// same, oep-spec registry `slot_retry_reset_hold_ms`).
const NRST_HOLD_MS: u16 = 20;

/// en: The slot's NRST line by the host guide's label rule (§8.1): the slot named in the address,
/// or the probe's only slot. ja: スロットの NRST の線(address のスロット、またはただ 1 つのスロット)。
fn nrst_line(p: &mut Probe, a: &OepAddr) -> Result<u16, String> {
    let slots = ch32rv_oep::config::slots(p).map_err(|e| e.to_string())?;
    let name = match a {
        OepAddr::Slot { slot, .. } => slot.clone(),
        _ if slots.len() == 1 => slots[0].name.clone(),
        _ => {
            return Err(format!(
                "name the slot (`oep://<probe>/<slot>`) to find its NRST line ({} slot(s) on this probe)",
                slots.len()
            ));
        }
    };
    let labels = ch32rv_oep::config::labels(p).map_err(|e| e.to_string())?;
    ch32rv_oep::config::find_line(&labels, &name, slots.len(), "nrst")?.ok_or_else(|| {
        format!("no NRST line for slot `{name}`: set a label `{name}.nrst` (or `nrst` on a one-slot probe) on its channel")
    })
}

/// [`with_attached`], optionally holding the slot's NRST line through the attach (`under_reset`).
pub(crate) fn with_attached_reset(
    cli: &Cli,
    cmd: &str,
    a: &OepAddr,
    halt: bool,
    under_reset: bool,
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
        let place = match choose_place(&mut p, a, cli.chip.as_deref()) {
            Ok(v) => v,
            Err(m) => return fail(cli, cmd, ErrorKind::TargetAmbiguous, m, None),
        };
        let wire = place.wire;
        let mut options = place.options(halt, None);
        if under_reset {
            match nrst_line(&mut p, a) {
                Ok(ch) => options.reset = Some((ch, NRST_HOLD_MS)),
                Err(m) => {
                    return fail(
                        cli,
                        cmd,
                        ErrorKind::CapabilityUnsupported,
                        m,
                        Some("oep-spec host-development-guide §8.1 names the lines"),
                    );
                }
            }
        }
        let at = match attach(&mut p, wire, options) {
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
                existing: at.existing,
                halted_at: at.halted,
            }),
            Err(e) => oep_fail(cli, cmd, e),
        };
        let _ = detach(&mut p, wire, at.connection, false);
        r
    })();
    let _ = p.end();
    r
}

/// en: `gdb` on an OEP probe, or on a WCH-Link through its broker (docs/cli.ja.md §4.6): attach
/// without halting, wait for GDB, halt when it connects, serve it, then take out any flash
/// breakpoint, resume and detach. A connection another client holds (a monitor) is shared, so the
/// console keeps streaming while GDB has the target stopped or running.
/// ja: OEP の probe、またはブローカー経由の WCH-Link での `gdb`。止めずに attach し、GDB を待ち、
/// つながったら止めて相手をし、最後に flash の breakpoint を外して走らせ、detach する。他の client
/// (monitor)の接続は共有するので、GDB の間も console は流れ続ける。
pub(crate) fn gdb(cli: &Cli, args: &crate::args::GdbArgs, a: &OepAddr) -> ExitCode {
    const CMD: &str = "gdb";
    with_attached(cli, CMD, a, false, |x| {
        let profile = x
            .family
            .as_deref()
            .and_then(ch32rv_flash::flash_controller_profile_for);
        // en: Behind a WCH-Link broker the attach is the Link's AttachChip, which overwrites s1 on
        // CH32V103: restart the program so it sets its registers again (as the direct gdb does).
        // A connection already there was attached before, and is left as it is.
        // ja: WCH-Link のブローカーの裏では attach が AttachChip で、CH32V103 では s1 を上書きする。
        // 直接の gdb と同じく program を起動し直す。既にあった接続はそのまま。
        if matches!(a, OepAddr::Wch(_))
            && !x.existing
            && profile.is_some_and(|p| p.attach_corrupts_regs)
        {
            let _ = x.t.reset(ResetMode::Run);
            std::thread::sleep(Duration::from_millis(50));
            eprintln!(
                "gdb: reset after attach (this core's attach corrupts a register; the target restarted)"
            );
        }
        let stream = match crate::cmd_gdb::listen(cli, args) {
            Ok(s) => s,
            Err(c) => return c,
        };
        let flash = profile
            .filter(|p| p.gdb_breakpoints)
            .map(|p| (p.page_size, p.mode));
        let mut target = match ch32rv_debug::Ch32Target::new(&mut x.t, flash) {
            Ok(t) => t,
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::AttachFailed,
                    format!("halt for gdb failed: {e}"),
                    None,
                );
            }
        };
        let code = crate::cmd_gdb::run_session(cli, &mut target, stream);
        // The direct path's detach resumes the core; an OEP detach leaves it as it is.
        let _ = target.resume_if_halted();
        code
    })
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
            Err(ch32rv_dmi::DmiError::NotReached(_)) if mode == ResetMode::RunVerified => {
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_unit_id_matches_a_serial_case_aside() {
        let serials = vec!["30EDA0E31108".to_owned(), "9489dd2ae0953650".to_owned()];
        assert!(super::unit_among("30eda0e31108", &serials));
        assert!(super::unit_among("9489DD2AE0953650", &serials));
        assert!(!super::unit_among("0070070d9394", &serials));
        assert!(!super::unit_among("", &serials));
    }
}
