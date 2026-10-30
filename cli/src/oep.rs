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
    /// `oep://<probe>[/<port>[/<target>]]` (host guide §5.3): an OEP probe reached on its
    /// serial port `path`, the port named by its labels' `S`, and the target by its individual
    /// name (`<family>-<unique id hex>`, checked on the connection before any side effect).
    Probe {
        path: String,
        port: Option<String>,
        target: Option<String>,
        /// Written as `oep://<probe>` alone: the probe itself, no port (§5.3).
        whole: bool,
    },
    /// `tcp:<host:port>`: a probe's TCP transport, or a ch32rv broker.
    Tcp(String),
    /// A probe found by DNS-SD on the local network (`_oep._tcp`, transports §3), named by its
    /// unit id (`tcp:<unit_id>`, or `oep://<unit_id>[/<port>[/<target>]]` when it is not on USB):
    /// its endpoint, the unit id describe must answer, and the port / target the address names.
    Net {
        addr: String,
        unit: String,
        port: Option<String>,
        target: Option<String>,
        /// Written as `oep://<unit_id>` alone (not `tcp:`): the probe itself, no port.
        whole: bool,
    },
    /// A WCH-Link, reached through its broker (which maps OEP onto the Link).
    Wch(crate::broker::BrokerTarget),
}

impl OepAddr {
    /// The port the address names (`oep://<probe>/<port>`), if it does.
    pub(crate) fn port(&self) -> Option<&str> {
        match self {
            OepAddr::Probe { port, .. } | OepAddr::Net { port, .. } => port.as_deref(),
            _ => None,
        }
    }

    /// en: The address is `oep://<probe>` alone, which names the probe itself (host guide §5.3:
    /// a segment left out is not "unspecified"), so no port is chosen for it - not even the only
    /// one. ja: `oep://<probe>` だけは probe 自身。口は選ばない。
    pub(crate) fn whole_probe(&self) -> bool {
        matches!(
            self,
            OepAddr::Probe { whole: true, .. } | OepAddr::Net { whole: true, .. }
        )
    }

    /// The target the address names (`oep://<probe>/<port>/<target>`), if it does.
    pub(crate) fn target(&self) -> Option<&str> {
        match self {
            OepAddr::Probe { target, .. } | OepAddr::Net { target, .. } => target.as_deref(),
            _ => None,
        }
    }
}

/// How long a lookup by unit id browses DNS-SD (a probe answers its query at once; this covers a
/// slow Wi-Fi).
const BROWSE_WAIT: Duration = Duration::from_millis(1500);

/// en: The broker target of a TCP address: the probe's endpoint and unit id (`None`: the address
/// is a ch32rv broker on this host, opened directly). Every OEP probe is used through its broker,
/// whatever the transport (the user's decision, 2026-10-07: the same behaviour everywhere).
/// ja: TCP のアドレスのブローカーの対象。この PC のブローカー自身なら None(直接つなぐ)。
pub(crate) fn net_target(a: &OepAddr) -> Option<crate::broker::BrokerTarget> {
    match a {
        OepAddr::Tcp(t) if crate::broker::is_broker_endpoint(t) => None,
        // Named by address: read the unit id from the probe itself (confirm and describe, both
        // lock-free), so the address meets the broker its unit id has.
        OepAddr::Tcp(t) => {
            let unit = unit_at(t);
            if unit.is_none() {
                eprintln!(
                    "warning[oep-unit-unknown]: {t} did not say its unit id; its broker is keyed by the address"
                );
            }
            Some(crate::broker::BrokerTarget::Net {
                addr: t.clone(),
                unit,
                by_unit: false,
            })
        }
        OepAddr::Net { addr, unit, .. } => Some(crate::broker::BrokerTarget::Net {
            addr: addr.clone(),
            unit: Some(unit.clone()),
            by_unit: true,
        }),
        _ => None,
    }
}

/// en: The unit id the probe at `addr` describes, from a short direct look (connect, confirm,
/// describe; a few seconds at most). `None` when it cannot be reached or does not say.
/// ja: `addr` の probe が describe で言う unit id(短く直接つないで読む)。届かなければ None。
pub(crate) fn unit_at(addr: &str) -> Option<String> {
    // en: Known already, and its broker runs: that broker holds one of the probe's connections,
    // and a probe serves only a few (the reference probe and the virtual bench 3), so it is not
    // asked again.
    // ja: 既に分かっていてそのブローカーが動いていれば聞き直さない(probe は同時接続が限られる)。
    let memo = unit_memo(addr);
    if let Some(u) = std::fs::read_to_string(&memo)
        .ok()
        .and_then(|t| t.lines().next().map(|l| l.trim().to_owned()))
        .filter(|u| !u.is_empty())
    {
        let t = crate::broker::BrokerTarget::Net {
            addr: addr.to_owned(),
            unit: Some(u.clone()),
            by_unit: false,
        };
        if crate::broker::existing_link_for(&t).is_some() {
            return Some(u);
        }
    }
    let (u, firmware, model) = read_identity(addr)?;
    let _ = std::fs::create_dir_all(ch32rv_usb::runtime_dir());
    let _ = std::fs::write(
        &memo,
        format!(
            "{u}\n{}\n{}\n",
            firmware.unwrap_or_default(),
            model.unwrap_or_default()
        ),
    );
    Some(u)
}

/// en: Where the unit id, firmware and model (lines 1-3) last read at `addr` are kept.
/// ja: `addr` で最後に読んだ unit id、firmware、model(1〜3 行目)の控え。
fn unit_memo(addr: &str) -> std::path::PathBuf {
    ch32rv_usb::runtime_dir().join(format!(
        "{}.unit",
        ch32rv_usb::sanitize_key(&format!("oep-tcp-{addr}"))
    ))
}

/// en: The firmware the probe at `addr` described when [`unit_at`] last read it.
/// ja: [`unit_at`] が最後に読んだときの firmware。
pub(crate) fn firmware_at(addr: &str) -> Option<String> {
    memo_line(addr, 1)
}

/// en: The model (`esp32`) the probe at `addr` described when [`unit_at`] last read it.
/// ja: [`unit_at`] が最後に読んだときの model。
pub(crate) fn model_at(addr: &str) -> Option<String> {
    memo_line(addr, 2)
}

/// en: An OEP probe on the network named as the USB ones are (`OEP probe (esp32)`).
/// ja: ネットワークの OEP の probe の名前(USB のものと同じ形)。
pub(crate) fn net_model(addr: &str) -> String {
    model_at(addr).map_or_else(|| "OEP probe".to_owned(), |m| format!("OEP probe ({m})"))
}

fn memo_line(addr: &str, n: usize) -> Option<String> {
    std::fs::read_to_string(unit_memo(addr))
        .ok()?
        .lines()
        .nth(n)
        .map(|l| l.trim().to_owned())
        .filter(|f| !f.is_empty())
}

/// The unit id, firmware and model fn 0's describe gives, read directly over TCP.
fn read_identity(addr: &str) -> Option<(String, Option<String>, Option<String>)> {
    use std::net::ToSocketAddrs;
    let sa = addr.to_socket_addrs().ok()?.next()?;
    let s = std::net::TcpStream::connect_timeout(&sa, Duration::from_secs(2)).ok()?;
    let _ = s.set_nodelay(true);
    let mut link = ch32rv_oep::link::Link::new(Box::new(s), ch32rv_oep::link::Framing::Length);
    link.set_timeout(Duration::from_secs(2));
    let mut p = Probe::connect(link).ok()?;
    let d = p.describe(ch32rv_oep::registry::core::FN).ok()?;
    let text = |tag: u8| {
        d.iter()
            .find(|t| t.tag == tag)
            .map(|t| String::from_utf8_lossy(&t.value).into_owned())
    };
    use ch32rv_oep::registry::core::tlvs::describe as dt;
    Some((
        text(dt::UNIT_ID)?.to_ascii_lowercase(),
        text(dt::FIRMWARE),
        text(dt::MODEL),
    ))
}

/// en: A probe found by DNS-SD, opened over TCP: used only when describe's unit_id is the one
/// named (transports §3; the TXT record is only a pointer). ja: DNS-SD で見つけた probe を TCP で開く。
/// describe の unit_id が名指したものと同じときだけ使う。
pub(crate) fn open_net(addr: &str, unit: &str) -> Result<Probe, String> {
    let link = ch32rv_oep::link::open_tcp(addr).map_err(|e| format!("{addr}: {e}"))?;
    let mut p = Probe::connect(link).map_err(|e| format!("{addr}: {e}"))?;
    let said = p
        .describe(ch32rv_oep::registry::core::FN)
        .ok()
        .and_then(|t| {
            t.into_iter()
                .find(|t| t.tag == ch32rv_oep::registry::core::tlvs::describe::UNIT_ID)
        })
        .map(|t| String::from_utf8_lossy(&t.value).into_owned());
    match said {
        Some(u) if u.eq_ignore_ascii_case(unit) => Ok(p),
        Some(u) => Err(format!(
            "{addr} announced unit {unit} but describes itself as {u}: not used"
        )),
        None => Err(format!("{addr} does not say its unit_id: not used")),
    }
}

/// en: The one place that decides what an OEP probe is (oep-core §3.3): a USB device with the
/// project's VID:PID (1209:4F45, registry `usb`), read without opening it. Any other device is
/// used only when the user names it (`oep://<unit_id>`, compared with its serial) or picks its
/// port. iProduct is display text and never decides it.
/// ja: OEP の probe の判定はここだけ。プロジェクトの VID:PID(1209:4F45)の device。iProduct では
/// 判定しない。ほかの device は利用者が名指すか口を選んだときだけ使う。
pub(crate) fn is_oep_device(dev: &ch32rv_usb::UsbDeviceInfo) -> bool {
    use ch32rv_oep::registry::usb;
    dev.vid() == usb::PROJECT_VID && dev.pid() == usb::PROJECT_PID
}

/// The OEP probes on USB.
pub(crate) fn oep_devices() -> Vec<ch32rv_usb::UsbDeviceInfo> {
    ch32rv_usb::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter(is_oep_device)
        .collect()
}

/// en: An OEP probe on USB, read lock-free and quickly for a listing - through its broker's link
/// when one runs (never started here), else on its serial port opened briefly. None when it
/// cannot be read within short waits (another tool holds the port, older firmware).
/// ja: 一覧用に OEP の probe を lock 無しで短く読む(ブローカーがあればその link、無ければ serial
/// port を短く開く)。読めなければ None。
pub(crate) fn probe_for_listing(dev: &ch32rv_usb::UsbDeviceInfo) -> Option<Probe> {
    let path = oep_port(dev)?;
    let quick = Duration::from_millis(300);
    match crate::broker::existing_link(&path) {
        Some(mut l) => {
            l.set_timeout(quick);
            Probe::connect(l).ok()
        }
        None => connect_for_listing(&path, quick).ok().map(|(p, _)| p),
    }
}

/// en: The firmware text fn 0's describe gives (`0.0.36+12e1222`), read as [`probe_for_listing`].
/// ja: fn 0 の describe の firmware(一覧用に短く読む)。
pub(crate) fn firmware_for_listing(dev: &ch32rv_usb::UsbDeviceInfo) -> Option<String> {
    let mut p = probe_for_listing(dev)?;
    p.describe(oep_core::FN)
        .ok()?
        .into_iter()
        .find(|t| t.tag == oep_core::tlvs::describe::FIRMWARE)
        .map(|t| String::from_utf8_lossy(&t.value).into_owned())
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
/// ports are the IDE's ports): its describe unit_id is the serial number of one of
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

/// Resolve `oep://<probe>[/<port>[/<target>]]` to the probe's serial port.
pub(crate) fn resolve_oep_url(url: &str) -> Result<OepAddr, String> {
    // oep://<probe>[/<port>[/<target>]] (host guide §5.3): segments of letters, digits, - and .
    // only, compared ignoring ASCII case; written in full (no wildcard). `oep-net://` names the
    // same, reached over the network only (the unit id found by DNS-SD, whatever the IP; `oep://`
    // prefers USB).
    let (rest, net_only) = match url.strip_prefix("oep-net://") {
        Some(r) => (r, true),
        None => (url.strip_prefix("oep://").unwrap_or(url), false),
    };
    let segs: Vec<&str> = rest.split('/').collect();
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    };
    if segs.len() > 3 || !segs.iter().all(|s| ok(s)) {
        return Err(format!(
            "`{url}` is not oep://<probe>[/<port>[/<target>]] (letters, digits, - and . only)"
        ));
    }
    if segs.iter().any(|s| s.eq_ignore_ascii_case("any")) {
        return Err(format!(
            "`{url}`: an address is written in full (`any` is gone, host guide §5.3); `ch32rv target list` lists the ports"
        ));
    }
    let id = segs[0];
    let whole = segs.len() == 1;
    let port = segs.get(1).map(|s| (*s).to_owned());
    let target = segs.get(2).map(|s| (*s).to_ascii_lowercase());
    let devs = if net_only { Vec::new() } else { oep_devices() };
    let dev = if net_only {
        None
    } else {
        // unit_id against the USB serial, case aside (transports §3).
        devs.into_iter()
            .find(|d| probe_id(d).eq_ignore_ascii_case(id))
    };
    // Not on USB: a probe announcing that unit id on the local network (DNS-SD).
    let Some(dev) = dev else {
        let f = crate::mdns::find_unit(id, BROWSE_WAIT).ok_or_else(|| {
            format!(
                "no OEP probe {id} is connected over USB or announced on the local network \
                 (mDNS: name it tcp:<host>:<port> where mDNS does not reach)"
            )
        })?;
        let addr = f
            .endpoint()
            .ok_or_else(|| format!("OEP probe {id} announces no reachable endpoint"))?;
        return Ok(OepAddr::Net {
            addr,
            unit: id.to_owned(),
            port,
            target,
            whole,
        });
    };
    let path = oep_port(&dev).ok_or_else(|| format!("OEP probe {id} has no serial port"))?;
    Ok(OepAddr::Probe {
        path,
        port,
        target,
        whole,
    })
}

/// en: A TCP address as written after `tcp:` (a leading `//` of `tcp://` already gone or not):
/// `<host>:<port>` as given; `<unit_id>` or `<host>` (no port) found by DNS-SD (the unit id in
/// TXT, or the host's SRV name or address). The one place every path - `--probe`, an IDE's port
/// address, `port:tcp:` - reads one, so each meets the same broker. Err: the message and a hint.
/// ja: `tcp:` の後ろのアドレスを解く。`--probe`、IDE の port、`port:tcp:` のどれもここを通る。
pub(crate) fn tcp_addr(name: &str) -> Result<OepAddr, (String, Option<&'static str>)> {
    let name = name.strip_prefix("//").unwrap_or(name);
    if name.contains(':') {
        return Ok(OepAddr::Tcp(name.to_owned()));
    }
    let Some(f) = crate::mdns::find_named(name, BROWSE_WAIT) else {
        return Err((
            format!("no OEP probe {name} is announced on the local network (DNS-SD _oep._tcp)"),
            Some("mDNS stays on the local link: name it tcp:<host>:<port> where it does not reach"),
        ));
    };
    match (f.endpoint(), f.unit_id) {
        (Some(addr), Some(unit)) => Ok(OepAddr::Net {
            addr,
            unit,
            port: None,
            target: None,
            whole: false,
        }),
        (Some(addr), None) => Ok(OepAddr::Tcp(addr)),
        (None, _) => Err((
            format!("OEP probe {name} announces no reachable endpoint"),
            None,
        )),
    }
}

/// en: Whether `--probe` names an OEP probe: `tcp:`, `port:oep://<probe>[/<port>]`, or
/// `port:<path>` for a serial port that no WCH-Link owns. `None` leaves the command on the
/// WCH-Link path.
/// ja: `--probe` が OEP の probe を指すか(`tcp:`、`port:oep://…`、WCH-Link のものでない serial port)。
pub(crate) fn addr(cli: &Cli, cmd: &str) -> Result<Option<OepAddr>, ExitCode> {
    let sel = match crate::cmd_probe::parse_selector(cli, cmd)? {
        // `tcp://host[:port]` as the other OEP clients write it, also behind `port:` (an IDE's
        // port address).
        Some(Selector::Tcp(a)) => {
            Some(Selector::Tcp(a.strip_prefix("//").unwrap_or(&a).to_owned()))
        }
        Some(Selector::Port(p)) if p.starts_with("tcp:") => {
            let rest = &p["tcp:".len()..];
            Some(Selector::Tcp(
                rest.strip_prefix("//").unwrap_or(rest).to_owned(),
            ))
        }
        other => other,
    };
    match sel {
        Some(Selector::Tcp(name)) => tcp_addr(&name)
            .map(Some)
            .map_err(|(m, hint)| fail(cli, cmd, ErrorKind::DeviceNotFound, m, hint)),
        Some(Selector::Port(p)) if p.starts_with("oep://") || p.starts_with("oep-net://") => {
            resolve_oep_url(&p)
                .map(Some)
                .map_err(|m| fail(cli, cmd, ErrorKind::DeviceNotFound, m, None))
        }
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
        OepError::PinsNeeded { .. } => (ErrorKind::Usage, None),
        _ => (ErrorKind::TransferFailed, None),
    };
    fail(cli, cmd, kind, e.to_string(), hint)
}

/// en: Connect and confirm, through the probe's broker whatever the transport (started if it is
/// not running, docs/oep-host.ja.md §7.2): the broker holds the probe and its session, and this
/// command is one of its clients. Only a `tcp:` address that is a ch32rv broker itself is
/// connected to directly. ja: 接続して confirm。経路によらずブローカー経由(無ければ起動)。
/// この PC のブローカー自身を指す `tcp:` だけ直接つなぐ。
fn connect(cli: &Cli, cmd: &str, a: &OepAddr) -> Result<Probe, ExitCode> {
    let link = match a {
        OepAddr::Serial(p) | OepAddr::Probe { path: p, .. } => crate::broker::client_link(p)
            .map_err(|m| {
                fail(
                    cli,
                    cmd,
                    ErrorKind::DeviceOpenFailed,
                    m,
                    Some("the probe's broker could not start or be reached"),
                )
            })?,
        OepAddr::Tcp(_) | OepAddr::Net { .. } => match net_target(a) {
            Some(t) => crate::broker::client_link_for(&t).map_err(|m| {
                fail(
                    cli,
                    cmd,
                    ErrorKind::DeviceOpenFailed,
                    m,
                    Some("the probe's broker could not start or be reached"),
                )
            })?,
            None => {
                let OepAddr::Tcp(t) = a else {
                    return Err(fail(cli, cmd, ErrorKind::Usage, "no tcp address", None));
                };
                ch32rv_oep::link::open_tcp(t)
                    .map_err(|e| fail(cli, cmd, ErrorKind::DeviceOpenFailed, e.to_string(), None))?
            }
        },
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
    // Each transport TLV is index(u8), kind(u8), interface(u8) (oep-core §7.5 0x49): the kind.
    let kinds: Vec<u8> = tlvs
        .iter()
        .filter(|t| t.tag == oep_core::tlvs::describe::TRANSPORT)
        .filter_map(|t| t.value.get(1).copied())
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

/// en: Where to attach: the wire, the port's pins and line settings. The probe keeps no wiring
/// (host guide §5.3): a port is the `S` of the saved labels `S.swdio` / `S.swclk` / `S.swio` /
/// `S.nrst`, and its pins are found by those labels. Speed ceiling, idle clock and console
/// mechanism are the host's to choose (`None`: the probe's defaults, dmseq for a console).
/// ja: attach する場所。probe は配線を持たず、ポートは保存したラベルの `S`、ピンはラベルで引く。
#[derive(Debug, Clone)]
struct Place {
    wire: WireKind,
    pins: Option<(u16, u16)>,
    max_speed: Option<u32>,
    idle_clock: Option<u8>,
    /// The console mechanism the host chose, if any.
    mechanism: Option<ch32rv_oep::stream::Mechanism>,
}

impl Place {
    /// Attach options here, at most `max_speed_hz` (the command's).
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

/// Why no place was chosen, with the exit kind it is.
struct PlaceError {
    kind: ErrorKind,
    msg: String,
}

impl From<String> for PlaceError {
    fn from(msg: String) -> Self {
        PlaceError {
            kind: ErrorKind::TargetAmbiguous,
            msg,
        }
    }
}

impl From<PlaceError> for String {
    fn from(e: PlaceError) -> String {
        e.msg
    }
}

/// en: `--pins`: the pins given by role (`swdio`, `swclk`, `swio`, `nrst`, `rx`, `tx`), each a
/// channel name or number, used instead of a port's labels. Set once by `main`.
/// ja: `--pins`。役目ごとのピン(channel の名前か番号)。口のラベルの代わりに使う。
pub(crate) static PINS: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();

const PIN_ROLES: [&str; 6] = ["swdio", "swclk", "swio", "nrst", "rx", "tx"];

/// `role=pin,...` as (role, pin); an unknown role, a role twice or an empty pin is an error.
pub(crate) fn parse_pins(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((role, pin)) = part.split_once('=') else {
            return Err(format!("--pins: `{part}` is not role=pin"));
        };
        let role = role.trim().to_ascii_lowercase();
        let pin = pin.trim();
        if !PIN_ROLES.contains(&role.as_str()) {
            return Err(format!(
                "--pins: unknown role `{role}` (roles: {})",
                PIN_ROLES.join(", ")
            ));
        }
        if pin.is_empty() || out.iter().any(|(r, _)| *r == role) {
            return Err(format!("--pins: `{role}` given empty or twice"));
        }
        out.push((role, pin.to_owned()));
    }
    if out.is_empty() {
        return Err("--pins: no pins given".to_owned());
    }
    Ok(out)
}

/// en: The channel of `--pins`' `role`: a number as it is, else the channel whose name fn 0's
/// describe gives (`channel_name`, core §7.5), ASCII case aside. ja: `--pins` の役目の channel。
fn pin_channel(p: &mut Probe, role: &str) -> Result<Option<u16>, String> {
    let Some((_, pin)) = PINS.get().and_then(|v| v.iter().find(|(r, _)| r == role)) else {
        return Ok(None);
    };
    if let Ok(n) = pin.parse::<u16>() {
        return Ok(Some(n));
    }
    let names = p.describe(oep_core::FN).map_err(|e| e.to_string())?;
    names
        .iter()
        .filter(|t| t.tag == oep_core::tlvs::describe::CHANNEL_NAME && t.value.len() > 2)
        .find(|t| String::from_utf8_lossy(&t.value[2..]).eq_ignore_ascii_case(pin))
        .map(|t| Some(u16::from_le_bytes([t.value[0], t.value[1]])))
        .ok_or_else(|| format!("--pins: the probe has no pin `{pin}` (its channel names)"))
}

/// en: The place `--pins` gives (no labels read): `swio` makes a one-wire place, `swdio` with
/// `swclk` a two-wire one. ja: `--pins` の場所(ラベルを読まない)。
fn place_from_pins(p: &mut Probe, chip: Option<&str>) -> Result<Place, PlaceError> {
    let swio = pin_channel(p, "swio")?;
    let swdio = pin_channel(p, "swdio")?;
    let swclk = pin_channel(p, "swclk")?;
    let (wire, pins) = match (swio, swdio, swclk) {
        (Some(d), None, None) => (WireKind::Swio, (d, 0xFFFF)),
        (None, Some(d), Some(c)) => (WireKind::Rvswd, (d, c)),
        (None, Some(d), None) => (WireKind::Swio, (d, 0xFFFF)),
        _ => {
            return Err(PlaceError {
                kind: ErrorKind::Usage,
                msg: "--pins: give swio=<pin> (one wire) or swdio=<pin>,swclk=<pin> (two)"
                    .to_owned(),
            });
        }
    };
    let idle_clock = (wire == WireKind::Rvswd && chip_idles_low(chip))
        .then_some(ch32rv_oep::registry::wire_rvswd::enums::idle_clock::LOW);
    Ok(Place {
        wire,
        pins: Some(pins),
        max_speed: None,
        idle_clock,
        mechanism: None,
    })
}

/// Whether `--chip` names a family whose rvswd line must idle low.
fn chip_idles_low(chip: Option<&str>) -> bool {
    chip.is_some_and(|c| {
        ch32rv_target::Db::builtin()
            .families_for_chip_name(c)
            .iter()
            .any(|f| idles_low(f))
    })
}

/// en: Choose the port without attaching (host guide §5.3: resolving an address narrows probe and
/// port by names only and stops unless one is left; an attach to search could wake and reset a
/// target on rvswd). The port the address names (ASCII case aside, used as its labels write it);
/// an address with no port (a serial port, `tcp:`) takes the probe's one named port, as a
/// convenience of this tool. No port named by the labels: nothing to choose. The wire comes from
/// the port's labels (`S.swio`: one wire, `S.swclk`: two), else from `--chip`.
/// ja: attach せずに口を選ぶ。アドレスの口(大小文字を区別しない)、口の無いアドレスなら名前つきの
/// 口がちょうど 1 つのときそれ。名前つきの口が無ければ選べない。
fn choose_place(p: &mut Probe, a: &OepAddr, chip: Option<&str>) -> Result<Place, PlaceError> {
    use ch32rv_oep::config::{find_line, labels, ports_named};
    if PINS.get().is_some() {
        return place_from_pins(p, chip);
    }
    // en: A WCH-Link behind its broker has one place, its own debug lines, and no settings to
    // name it: the wire from the chip, the Link's lines as they are.
    // ja: ブローカーの裏の WCH-Link の場所は自分の debug の線だけで、名前を付ける設定も無い。
    if matches!(a, OepAddr::Wch(_)) {
        return Ok(Place {
            wire: pick_wire(p, chip).map_err(|e| e.to_string())?,
            pins: None,
            max_speed: None,
            idle_clock: None,
            mechanism: None,
        });
    }
    let labels = labels(p).map_err(|e| e.to_string())?;
    let ports = ports_named(&labels);
    if a.whole_probe() {
        return Err(PlaceError {
            kind: ErrorKind::Usage,
            msg: whole_probe_msg(&ports),
        });
    }
    let port = match a.port() {
        Some(name) => match ports.iter().find(|p| p.eq_ignore_ascii_case(name)) {
            Some(p) => p.clone(),
            None => {
                return Err(PlaceError {
                    kind: ErrorKind::DeviceNotFound,
                    msg: if ports.is_empty() {
                        format!("the probe names no ports, so not `{name}`: {NO_PORTS_HINT}")
                    } else {
                        format!(
                            "the probe has no port `{name}` (its ports: {})",
                            ports.join(", ")
                        )
                    },
                });
            }
        },
        None => match ports.as_slice() {
            [one] => one.clone(),
            [] => {
                return Err(PlaceError {
                    kind: ErrorKind::Usage,
                    msg: format!("the probe names no ports: {NO_PORTS_HINT}"),
                });
            }
            many => {
                return Err(PlaceError::from(format!(
                    "the probe has {} ports ({}): name one, oep://<probe>/<port>",
                    many.len(),
                    many.join(", ")
                )));
            }
        },
    };
    let line = |name: &str| find_line(&labels, &port, name);
    let has = |p: &mut Probe, k: WireKind| p.interface(k.interface()).is_ok();
    let wire = match (line("swio"), line("swclk")) {
        (Some(_), None) if has(p, WireKind::Swio) => WireKind::Swio,
        (None, Some(_)) if has(p, WireKind::Rvswd) => WireKind::Rvswd,
        _ => pick_wire(p, chip).map_err(|e| e.to_string())?,
    };
    let pins = match wire {
        WireKind::Rvswd => line("swdio").zip(line("swclk")),
        WireKind::Swio => line("swio").or_else(|| line("swdio")).map(|d| (d, 0xFFFF)),
    };
    // en: CH32L103's debug module resets while an rvswd line idles high, so its line idles low
    // (the old slots carried that; now it is the host's, measured by dev_oep 2026-10-16).
    // ja: L103 は rvswd の線を high で休ませると debug module がリセットされるので low にする。
    // The family comes from --chip, else from the target the address names (`<family>-<uid>`);
    // neither: [`attach_at`] learns it from the chip id and attaches again.
    let named = a
        .target()
        .and_then(|t| t.split_once('-'))
        .is_some_and(|(f, _)| idles_low(f));
    let idle_clock = (wire == WireKind::Rvswd
        && (named
            || chip.is_some_and(|c| {
                ch32rv_target::Db::builtin()
                    .families_for_chip_name(c)
                    .iter()
                    .any(|f| idles_low(f))
            })))
    .then_some(ch32rv_oep::registry::wire_rvswd::enums::idle_clock::LOW);
    Ok(Place {
        wire,
        pins,
        max_speed: None,
        idle_clock,
        mechanism: None,
    })
}

/// Whether a family's rvswd line must idle low (CH32L103: its debug module resets otherwise).
fn idles_low(family: &str) -> bool {
    family.eq_ignore_ascii_case("CH32L103")
}

/// en: An attach under reset, tried again: whether the hart is caught right after NRST is
/// released is a race against the firmware (measured on CH32L103, 2026-10-23: one try in several
/// got through), and while the family is unknown the rvswd rest is too - CH32L103's debug module
/// resets while the line rests high - so each round tries the probe's own rest and then low.
/// ja: リセットを押さえたままの attach をやり直す。離した直後に捕まえられるかは firmware との競争で、
/// 族が分からない間は rvswd の休ませ方も分からない(L103 は high だと DM がリセットされる)ので、
/// 毎回、probe の休ませ方と low を試す。
fn attach_under_reset_retrying(
    p: &mut Probe,
    wire: WireKind,
    options: AttachOptions,
) -> Result<ch32rv_oep::target::Attached, ch32rv_oep::session::OepError> {
    const ROUNDS: usize = 4;
    let low = ch32rv_oep::registry::wire_rvswd::enums::idle_clock::LOW;
    let rests: Vec<Option<u8>> = match (wire, options.idle_clock) {
        (WireKind::Rvswd, None) => vec![None, Some(low)],
        (_, rest) => vec![rest],
    };
    let mut last = None;
    for _ in 0..ROUNDS {
        for rest in &rests {
            let mut o = options;
            o.idle_clock = *rest;
            match attach(p, wire, o) {
                Ok(at) => return Ok(at),
                Err(e) => last = Some(e),
            }
        }
    }
    Err(last
        .unwrap_or_else(|| ch32rv_oep::session::OepError::Malformed("no attach was tried".into())))
}

/// en: `--attach-wait`: attach, halting, until the target answers or the deadline passes (the
/// user power-cycles it or presses its RESET meanwhile), so firmware that sleeps or takes the
/// debug pins after its boot wait is stopped inside that wait.
/// ja: `--attach-wait`。答えるか期限まで、止める attach を繰り返す(その間に電源か RESET)。
fn attach_waiting(
    p: &mut Probe,
    wire: WireKind,
    mut options: AttachOptions,
    deadline: std::time::Instant,
) -> Result<ch32rv_oep::target::Attached, ch32rv_oep::session::OepError> {
    options.halt = true;
    loop {
        match attach_at(p, wire, options) {
            Ok(at) => return Ok(at),
            // en: The probe or the session, not the target: waiting does not help.
            Err(
                e @ (OepError::Locked { .. }
                | OepError::Link(_)
                | OepError::NoInterface(_)
                | OepError::PinsNeeded { .. }),
            ) => return Err(e),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => {}
        }
    }
}

/// en: Attach; when the target that answers needs its rvswd line idling low ([`idles_low`]), the
/// place did not know it (no `--chip`, no family in the address), and the connection does not
/// rest low - the probe's setting `rest.{}` (oep.wire.rvswd) turned its own choice off, or an older probe
/// answers no rest (oep-if-debug §3.1) - detach and attach again with idle_clock low: the first
/// attach was short enough to read the chip id before the debug module reset. A connection that
/// already existed is left as it is.
/// ja: attach し、答えた target が low で休ませる必要のある族なのに場所が知らず、接続の休ませ方も
/// low でなければ(probe の設定 `rest.{}`(oep.wire.rvswd)が自動を切った、または休ませ方を答えない古い probe)、
/// detach して idle_clock low で attach し直す。既にあった接続はそのまま。
fn attach_at(
    p: &mut Probe,
    wire: WireKind,
    mut options: AttachOptions,
) -> Result<ch32rv_oep::target::Attached, ch32rv_oep::session::OepError> {
    let at = attach(p, wire, options)?;
    let family =
        at.wch_chip_id.and_then(
            |id| match ch32rv_target::Db::builtin().resolve_by_chip_id(id) {
                ch32rv_target::Resolution::Sku(s) => Some(s.family.clone()),
                ch32rv_target::Resolution::Family(f, _) => Some(f),
                ch32rv_target::Resolution::Unknown => None,
            },
        );
    let low = ch32rv_oep::registry::wire_rvswd::enums::idle_clock::LOW;
    if wire != WireKind::Rvswd
        || options.idle_clock.is_some()
        || at.existing
        || at.idle_clock == Some(low)
        || !family.is_some_and(|f| idles_low(&f))
    {
        return Ok(at);
    }
    detach(p, wire, at.connection)?;
    options.idle_clock = Some(low);
    attach(p, wire, options)
}

/// Why `oep://<probe>` alone picks no port, with the addresses that would.
fn whole_probe_msg(ports: &[String]) -> String {
    if ports.is_empty() {
        format!(
            "oep://<probe> names the probe itself, not a port, and it names no ports: {NO_PORTS_HINT}"
        )
    } else {
        format!(
            "oep://<probe> names the probe itself, not a port: name one, oep://<probe>/<port> \
             (its ports: {})",
            ports.join(", ")
        )
    }
}

/// What to do when a probe names no ports (host guide §5.3: only `S.N` labels name ports).
const NO_PORTS_HINT: &str = "label its pins `<port>.swdio` / `<port>.swclk` (or `<port>.swio`) \
     with `oep settings set <probe> core channel.<pin>.label=<port>.swdio ...`";

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
    // A probe that announces itself on USB is flashed by its oep:// address, never through its raw
    // serial port (oep-workflow §3.4): the IDE lists its ports as oep:// ports.
    if let OepAddr::Serial(p) = a
        && port_of_oep_device(p)
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("{p} is an OEP probe's serial port; flash it by its oep:// address instead"),
            Some("use port:oep://<probe>[/<port>] (`ch32rv arduino discovery` lists them)"),
        );
    }
    let mut p = match connect(cli, CMD, a) {
        Ok(p) => p,
        Err(c) => return c,
    };
    // The same probe reached on another serial port (its USB-Serial/JTAG, a UART bridge) is also
    // on USB as an OEP device, whose ports are the IDE's: its unit_id is that serial.
    if matches!(a, OepAddr::Serial(_)) && listed_on_usb(&mut p) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "this serial port belongs to an OEP probe that is also on USB as an OEP device; flash it by its oep:// address instead",
            Some("use port:oep://<probe>[/<port>] (`ch32rv arduino discovery` lists them)"),
        );
    }
    let serial = matches!(a, OepAddr::Serial(_)) && single_serial(&mut p);
    if let Err(c) = open_session(cli, CMD, &mut p, serial) {
        return c;
    }
    let r = flash_in_session(cli, args, bytes, &mut p, a);
    // End on every path (flash_in_session has detached its connection): the lock is released.
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
        Err(e) => {
            let hint = (e.kind == ErrorKind::TargetAmbiguous)
                .then_some("name the port with oep://<probe>/<port>");
            return fail(cli, CMD, e.kind, e.msg, hint);
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
    let at = match attach_at(p, wire, place.options(true, max_speed_hz)) {
        Ok(a) => a,
        Err(e) => return oep_fail(cli, CMD, e),
    };
    // A broker that went away says why in its log (its stderr goes nowhere).
    let log_hint = match a {
        OepAddr::Serial(path) | OepAddr::Probe { path, .. } => Some(format!(
            "the probe's broker keeps a log: {}",
            crate::broker::log_path(&crate::broker::BrokerTarget::Serial(path.clone()).key())
                .display()
        )),
        _ => None,
    };
    let r = flash_attached(cli, args, bytes, p, at.connection, at.wch_chip_id, log_hint);
    let _ = detach(p, wire, at.connection);
    r
}

fn flash_attached(
    cli: &Cli,
    args: &FlashArgs,
    bytes: &[u8],
    p: &mut Probe,
    connection: u32,
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
    // `--preverify`: a target that already holds the image is not written (no erase, no wear);
    // the reset below still runs as asked. ja: 既に同じ中身なら書かない(reset は指定どおり)。
    let skipped = args.preverify
        && image.segments.iter().all(|seg| {
            let lo = seg.addr & !3;
            let words = (seg.addr + seg.data.len() as u32 - lo).div_ceil(4) as usize;
            t.read_words(lo, words).is_ok_and(|w| {
                let b: Vec<u8> = w.iter().flat_map(|w| w.to_le_bytes()).collect();
                let at = (seg.addr - lo) as usize;
                b[at..at + seg.data.len()] == seg.data[..]
            })
        });
    let report = if skipped {
        ch32rv_flash::loader::LoaderReport::default()
    } else {
        match ch32rv_flash::loader::program(&mut t, plan, &image.segments, &mut |_, _| {}) {
            Ok(r) => r,
            Err(e) => {
                let kind = crate::cmd_flash::loader_error_kind(&e);
                let hint = log_hint.filter(|_| kind != ErrorKind::VerifyMismatch);
                return fail(cli, CMD, kind, e.to_string(), hint.as_deref());
            }
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
                "skipped": skipped,
                "scope": if skipped { "none" } else { "pages" },
                "running": running,
                "seconds": secs,
            }
        }));
        crate::print_envelope(&env)
    } else if skipped {
        println!("preverify: target already matches - skipped");
        ExitCode::SUCCESS
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
    /// The target's console by dmseq (`monitor` without --source), the host's default.
    ConsoleDefault,
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
/// console (attached without halting - a running target is watched, not stopped - at the port the
/// address names, read from the last reset mark) or the fixture UART (its
/// speed set by `oep.fixture.uart` configure). Dropping it detaches and ends the session (the
/// broker would release both anyway when this client leaves).
/// ja: OEP の probe のストリーム(ブローカー経由、Arduino monitor 用)。target の console(止めずに
/// attach、場所は address か板の家系で選ぶ、最後の reset の mark から読む)か fixture の UART(速さは
/// configure で決める)。drop で detach と end(client が抜ければブローカーも外す)。
pub(crate) struct ConsoleSession {
    probe: Probe,
    stream: Backing,
    /// The connection to detach at the end (the console's), if any.
    attached: Option<(WireKind, u32)>,
    max_read: u16,
    /// Where a console attached and with which mechanism, to attach again after a lost line.
    relink: Option<(Place, ch32rv_oep::stream::Mechanism)>,
    /// Since when the console has read nothing (its stream is checked after RELINK_CHECK).
    quiet_since: Instant,
    /// What is read, as `--source` names it (the mechanism chosen when none was given).
    source_name: &'static str,
}

/// How long a console reads nothing before the monitor checks that its stream is still open.
const RELINK_CHECK: Duration = Duration::from_secs(1);

/// Where a [`ConsoleSession`]'s bytes come from.
enum Backing {
    /// A position stream the probe keeps (console / fixture UART).
    Stream(ch32rv_oep::stream::PosStream),
    /// RTT the host runs: the connection, what OepDtm learned, the channels, input not yet taken.
    Rtt {
        connection: u32,
        parts: (u16, usize, u32, ch32rv_oep::target::Offered),
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
            OepAddr::Serial(p) | OepAddr::Probe { path: p, .. } => crate::broker::client_link(p)?,
            OepAddr::Tcp(_) | OepAddr::Net { .. } => match net_target(a) {
                Some(t) => crate::broker::client_link_for(&t)?,
                None => match a {
                    OepAddr::Tcp(t) => ch32rv_oep::link::open_tcp(t).map_err(|e| e.to_string())?,
                    _ => return Err("no tcp address".into()),
                },
            },
            OepAddr::Wch(t) => crate::broker::client_link_for(t)?,
        };
        let mut probe = Probe::connect(link).map_err(|e| e.to_string())?;
        if a.whole_probe() {
            let labels = ch32rv_oep::config::labels(&mut probe).unwrap_or_default();
            return Err(whole_probe_msg(&ch32rv_oep::config::ports_named(&labels)));
        }
        let owner = format!("ch32rv monitor pid {}", std::process::id());
        // A probe on TCP may still be held by another host's session - a broker on its other
        // transport lingering after its last client, ending it seconds later: wait out its
        // lease as the other commands do (a broker answers open itself and never refuses).
        open_with_lock_rule(&mut probe, false, &owner, 3000).map_err(|e| e.to_string())?;
        let mut relink = None;
        let mut source_name = match wanted {
            StreamWanted::FixtureUart(_) => "fixture-uart",
            StreamWanted::Rtt => "rtt",
            StreamWanted::Console(m) => m.name(),
            StreamWanted::ConsoleDefault => "dmseq",
        };
        let (stream, attached) = match wanted {
            StreamWanted::FixtureUart(baud) => {
                let pins = uart_pins(&mut probe, a);
                let (s, _) = ch32rv_oep::stream::PosStream::open_uart(&mut probe, baud, pins).map_err(
                    |e| match e {
                        OepError::Rejected { reason, .. }
                            if reason == ch32rv_oep::registry::reject_reasons::UNAVAILABLE =>
                        {
                            "the probe's fixture UART has no pins: label its channels `<port>.rx` (and `<port>.tx` where the target's RX is wired)".to_owned()
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
                let at = attach_at(&mut probe, wire, place.options(false, None))
                    .map_err(|e| e.to_string())?;
                check_family_text(at.wch_chip_id, chip)?;
                let mut t = OepDtm::new(&mut probe, at.connection).map_err(|e| e.to_string())?;
                let mut warnings = Vec::new();
                let found = crate::source::rtt_find(
                    &mut t,
                    crate::source::rtt_window(at.wch_chip_id),
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
            StreamWanted::Console(_) | StreamWanted::ConsoleDefault => {
                let place = choose_place(&mut probe, a, chip)?;
                let mech = match wanted {
                    StreamWanted::Console(m) => m,
                    _ => place
                        .mechanism
                        .unwrap_or(ch32rv_oep::stream::Mechanism::Dmseq),
                };
                source_name = mech.name();
                let wire = place.wire;
                let at = attach_at(&mut probe, wire, place.options(false, None))
                    .map_err(|e| e.to_string())?;
                check_family_text(at.wch_chip_id, chip)?;
                let mut s =
                    ch32rv_oep::stream::PosStream::open_console(&mut probe, at.connection, mech)
                        .map_err(|e| e.to_string())?;
                s.start_at_last_reset(&mut probe)
                    .map_err(|e| e.to_string())?;
                relink = Some((place, mech));
                (Backing::Stream(s), Some((wire, at.connection)))
            }
        };
        let max_read = probe.limits().max_frame.saturating_sub(14).clamp(16, 1000);
        Ok(ConsoleSession {
            probe,
            stream,
            attached,
            max_read,
            relink,
            quiet_since: Instant::now(),
            source_name,
        })
    }

    /// What arrived since the last poll (RTT: one exchange, which also hands over pending input).
    pub(crate) fn poll(&mut self) -> Result<Vec<u8>, String> {
        match &mut self.stream {
            Backing::Stream(s) => {
                // A closed stream answers no_resource (oep-if-console §1): its connection was
                // lost and closed - attach again and reopen.
                let data = match s.poll(&mut self.probe, self.max_read) {
                    Err(OepError::Rejected { reason, .. })
                        if reason == ch32rv_oep::registry::reject_reasons::NO_RESOURCE =>
                    {
                        self.quiet_since = Instant::now();
                        self.relink()?;
                        return Ok(Vec::new());
                    }
                    r => r.map(|c| c.data).map_err(|e| e.to_string())?,
                };
                if !data.is_empty() {
                    self.quiet_since = Instant::now();
                } else if self.quiet_since.elapsed() >= RELINK_CHECK {
                    self.quiet_since = Instant::now();
                    self.relink_if_closed()?;
                }
                Ok(data)
            }
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

    /// en: A console that went quiet: if its stream was closed - the probe gave up on a lost line
    /// and closed the connection (oep-if-debug §2, P1: it no longer wakes a live line on its own,
    /// so a board reset or sleep can end it) - attach again (a host-requested attach may wake the
    /// line) and reopen the console, which comes back under a new stream number from the oldest byte kept.
    /// ja: 黙った console のストリームが閉じていたら(probe が線を失って接続を閉じた)、attach し直して
    /// console を開き直す(新しい番号で、残っている最古から)。
    fn relink_if_closed(&mut self) -> Result<(), String> {
        let (Backing::Stream(s), Some((place, mech))) = (&mut self.stream, &self.relink) else {
            return Ok(());
        };
        // en: A broker from before STREAMS (ch32rv 0.20.0's WCH-Link broker) rejects it as
        // unknown or unsupported: take the stream as still open rather than stopping the monitor.
        // ja: STREAMS を知らない古いブローカーは断る。止めずに「開いている」とみなす。
        let open = match s.is_open(&mut self.probe) {
            Err(OepError::Rejected { reason, .. })
                if reason == ch32rv_oep::registry::reject_reasons::UNKNOWN_OPERATION
                    || reason == ch32rv_oep::registry::reject_reasons::UNSUPPORTED =>
            {
                None
            }
            r => r.map_err(|e| e.to_string())?,
        };
        let _ = (place, mech);
        if open != Some(false) {
            return Ok(());
        }
        self.relink()
    }

    /// en: Attach again and open the console anew (a new connection and a new stream: numbers
    /// are not reused, core §9), reading from its oldest byte. Not back yet (still in reset,
    /// unplugged): left for the next quiet check. ja: attach し直して console を新しく開く。
    fn relink(&mut self) -> Result<(), String> {
        let (Backing::Stream(s), Some((place, mech))) = (&mut self.stream, &self.relink) else {
            return Ok(());
        };
        let Ok(at) = attach_at(&mut self.probe, place.wire, place.options(false, None)) else {
            return Ok(());
        };
        s.reopen_console(&mut self.probe, at.connection, *mech)
            .map_err(|e| e.to_string())?;
        self.attached = Some((place.wire, at.connection));
        eprintln!("monitor: the target's debug line was lost; attached again");
        Ok(())
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

    /// What is read, as `--source` names it.
    pub(crate) fn source_name(&self) -> &'static str {
        self.source_name
    }

    /// Whether this session is RTT (polled at RTT's pace: each poll halts the hart briefly).
    pub(crate) fn is_rtt(&self) -> bool {
        matches!(self.stream, Backing::Rtt { .. })
    }
}

impl Drop for ConsoleSession {
    fn drop(&mut self) {
        if let Some((wire, conn)) = self.attached {
            let _ = detach(&mut self.probe, wire, conn);
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
    // `--connect-under-reset`: hold the port's NRST line through the attach (its label).
    with_attached_reset(cli, cmd, a, halt, cli.connect_under_reset, f)
}

/// en: The port an address names among the probe's named ports (ASCII case aside), or its one
/// named port when the address names none (a serial port, `tcp:`). ja: アドレスの口、または唯一の口。
fn port_of(ports: &[String], a: &OepAddr) -> Option<String> {
    match a.port() {
        Some(name) => ports.iter().find(|p| p.eq_ignore_ascii_case(name)).cloned(),
        None => match ports {
            [one] => Some(one.clone()),
            _ => None,
        },
    }
}

/// en: The fixture UART's (rx, tx) channels from the port's labels `<port>.rx` / `<port>.tx`
/// (the port as for a target). A port with `rx` alone is received only (the target's RX not
/// wired). ja: UART の rx / tx の channel。rx だけなら受信だけ。
fn uart_pins(p: &mut Probe, a: &OepAddr) -> Option<(u16, Option<u16>)> {
    use ch32rv_oep::config::{find_line, labels, ports_named};
    if PINS.get().is_some() {
        let rx = pin_channel(p, "rx").ok()??;
        return Some((rx, pin_channel(p, "tx").ok().flatten()));
    }
    let labels = labels(p).ok()?;
    let port = port_of(&ports_named(&labels), a)?;
    let line = |name| find_line(&labels, &port, name);
    line("rx").map(|rx| (rx, line("tx")))
}

/// How long the NRST line is held low on an attach under reset (the host's own value).
const NRST_HOLD_MS: u16 = 20;

/// en: The port's NRST line by the label convention (host guide §5.3): `<port>.nrst`.
/// ja: 口の NRST の線(ラベル `<port>.nrst`)。
fn nrst_line(p: &mut Probe, a: &OepAddr) -> Result<u16, String> {
    use ch32rv_oep::config::{find_line, labels, ports_named};
    if PINS.get().is_some() {
        return pin_channel(p, "nrst")?
            .ok_or_else(|| "--pins: give nrst=<pin> for an attach under reset".to_owned());
    }
    let labels = labels(p).map_err(|e| e.to_string())?;
    let ports = ports_named(&labels);
    let Some(port) = port_of(&ports, a) else {
        return Err(if ports.is_empty() {
            format!("the probe names no ports: {NO_PORTS_HINT}")
        } else {
            format!(
                "name the port (`oep://<probe>/<port>`) to find its NRST line (ports: {})",
                ports.join(", ")
            )
        });
    };
    find_line(&labels, &port, "nrst").ok_or_else(|| {
        format!("no NRST line for port `{port}`: set a label `{port}.nrst` on its channel")
    })
}

/// [`with_attached`], optionally holding the port's NRST line through the attach (`under_reset`).
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
            Err(e) => return fail(cli, cmd, e.kind, e.msg, None),
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
        let at = if under_reset {
            attach_under_reset_retrying(&mut p, wire, options)
        } else if let Some(deadline) = crate::session::attach_wait_deadline() {
            attach_waiting(&mut p, wire, options, deadline)
        } else {
            attach_at(&mut p, wire, options)
        };
        let at = match at {
            Ok(a) => a,
            Err(e) => return oep_fail(cli, cmd, e),
        };
        let family = match check_family_text(at.wch_chip_id, cli.chip.as_deref()) {
            Ok(f) => f,
            Err(m) => {
                let _ = detach(&mut p, wire, at.connection);
                return fail(cli, cmd, ErrorKind::TargetAmbiguous, m, None);
            }
        };
        let r = match OepDtm::new(&mut p, at.connection) {
            Ok(t) => {
                let mut x = Attached {
                    t,
                    chip_id: at.wch_chip_id,
                    family,
                    existing: at.existing,
                    halted_at: at.halted,
                };
                // en: A target the address names (`<family>-<unique id hex>`, host guide §5.3) is
                // checked on this connection before anything else is done to it.
                // ja: アドレスが名指す target を、何かをする前にこの接続で確かめる。
                match a.target().map(|want| (want, target_name(&mut x))) {
                    Some((want, Some(name))) if !name.eq_ignore_ascii_case(want) => fail(
                        cli,
                        cmd,
                        ErrorKind::TargetAmbiguous,
                        format!("the target here is {name}, not {want}"),
                        None,
                    ),
                    Some((want, None)) => fail(
                        cli,
                        cmd,
                        ErrorKind::TargetNoResponse,
                        format!("cannot read the target's unique id to check that it is {want}"),
                        None,
                    ),
                    _ => f(&mut x),
                }
            }
            Err(e) => oep_fail(cli, cmd, e),
        };
        let _ = detach(&mut p, wire, at.connection);
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
        let ram_base = crate::parse::ram_base(x.family.as_deref());
        let (start, len) =
            match crate::cmd_dbg::resolve_range(args, flash, sram, option_base, ram_base) {
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

/// en: `write` on an OEP probe: whole words to RAM / peripheral / core addresses through the Debug
/// Module. RAM is read back to verify; a peripheral or core register is not (key registers read
/// as zero, status bits move), so `verified` is null there. Flash, the system area and the option block need the FLASH
/// controller's sequences, which on an OEP probe only `flash` / `erase` drive, so they are refused.
/// ja: OEP の probe での `write`。RAM・peripheral・core の番地へ Debug Module で word 単位に書き、
/// RAM は読み戻して検証し、peripheral・core は検証しない(key レジスタは 0 を返し、status は動く)。flash・system・option は FLASH controller の手順が要り、OEP では `flash` /
/// `erase` だけが回すので断る。
pub(crate) fn write(cli: &Cli, addr: u32, bytes: &[u8], a: &OepAddr) -> ExitCode {
    const CMD: &str = "write";
    if addr < 0x2000_0000 {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "0x{addr:08x} is flash / system / option space; an OEP probe writes it only through `flash` or `erase`"
            ),
            None,
        );
    }
    if !addr.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "an OEP probe writes whole words: the address and length must be multiples of 4",
            None,
        );
    }
    let words: Vec<u32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    let len = bytes.len();
    let check = addr < 0x4000_0000;
    with_attached(cli, CMD, a, true, |x| {
        let result = x.t.write_words(addr, &words).and_then(|()| {
            if check {
                x.t.read_words(addr, words.len())
            } else {
                Ok(words.clone())
            }
        });
        resume_after(&mut x.t);
        match result {
            Ok(back) if back == words => {}
            Ok(_) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::VerifyMismatch,
                    format!("write to 0x{addr:08x} did not verify (readback differs)"),
                    None,
                );
            }
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("write failed: {e}"),
                    None,
                );
            }
        }
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({
                "addr": format!("0x{addr:08x}"),
                "len": len,
                "region": "memory",
                "verified": check.then_some(true),
            }));
            crate::print_envelope(&env)
        } else {
            println!(
                "wrote {len} byte(s) to 0x{addr:08x} (memory) - {}",
                if check {
                    "verified"
                } else {
                    "not read back (register)"
                }
            );
            ExitCode::SUCCESS
        }
    })
}

/// `target info` on an OEP probe: what the chip id says, without halting the target.
/// en: The chip's 64-bit unique ID from the ESIG (`0x1FFF_F7E8`, two words), in the form the
/// WCH-Link path prints from ChipInfo: each word read little-endian, then the two in hex
/// (`cd ab f9 1f 48 bc 0e 88` -> `1ff9abcd880ebc48`, matched on CH32X035C8T6). Read without
/// halting where the probe can; otherwise halted for the read and resumed if it was running.
/// `None` when unreadable or erased-looking. A test harness matches DUTs by it, and a WCH-Link
/// behind its broker answers `target info` through this path (ArduinoCore-CH32RV, 2026-10-15).
/// ja: ESIG の 64 bit の UID(WCH-Link の ChipInfo と同じ表記)。止めずに読めなければ一瞬止めて読む。
fn read_uid(x: &mut Attached<'_>) -> Option<String> {
    // Most families keep it at 0x1FFFF7E8; CH32M030 at 0x1FFFF3A8 (ch32-device-data esig).
    let at = x
        .family
        .as_deref()
        .and_then(ch32rv_target::uid_addr)
        .unwrap_or(0x1FFF_F7E8);
    let words = match x.t.read_words(at, 2) {
        Ok(w) => w,
        Err(_) => {
            let was_running = x.halted_at.is_none();
            x.t.halt().ok()?;
            let w = x.t.read_words(at, 2);
            if was_running {
                resume_after(&mut x.t);
            }
            w.ok()?
        }
    };
    let [w0, w1] = words[..] else {
        return None;
    };
    if (w0 == 0 && w1 == 0) || (w0 == u32::MAX && w1 == u32::MAX) {
        return None;
    }
    Some(format!("{w0:08x}{w1:08x}"))
}

/// en: The target's individual name (host guide §5.3): `<family>-<unique id hex>`, both lowercase
/// (`ch32x035-1ff9abcd880ebc48`). ja: target の個体名。
fn target_name(x: &mut Attached<'_>) -> Option<String> {
    let family = x.family.clone()?.to_ascii_lowercase();
    Some(format!("{family}-{}", read_uid(x)?))
}

pub(crate) fn target_info(cli: &Cli, a: &OepAddr) -> ExitCode {
    const CMD: &str = "target.info";
    with_attached(cli, CMD, a, false, |x| {
        let sku = sku_of(x.chip_id);
        let uid = read_uid(x);
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            // The same `target` block the WCH-Link path fills, so a caller reads one place.
            env.target = Some(ch32rv_contract::envelope::TargetReport {
                sku: sku.as_ref().map(|s| s.sku.clone()),
                family: x.family.clone(),
                chip_id: x.chip_id.map(|c| format!("0x{c:08x}")),
                uid: uid.clone(),
                verified: sku.as_ref().map(|s| s.verified),
                provisional: sku.as_ref().map(|s| s.provisional),
                protected: None,
                flash_bytes: sku.as_ref().map(|s| s.flash_bytes),
            });
            env.result = Some(serde_json::json!({
                "probe": { "kind": "oep" },
                "target": {
                    "chip_id": x.chip_id.map(|c| format!("0x{c:08x}")),
                    "family": x.family,
                    "sku": sku.as_ref().map(|s| s.sku.clone()),
                    "uid": uid,
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
            println!("uid:      {}", uid.as_deref().unwrap_or("-"));
            if let Some(s) = &sku {
                println!("sku:      {}", s.sku);
                println!("flash:    {} KiB", s.flash_bytes / 1024);
                println!("sram:     {} KiB", s.sram_bytes / 1024);
            }
            ExitCode::SUCCESS
        }
    })
}

/// en: `probe info` on an OEP probe, lock-free: what fn 0's describe says (firmware, model, unit
/// id, chip, max_op_ms), the interfaces it lists, and the ports its labels name.
/// ja: OEP の probe の `probe info`(lock 無し): fn 0 の describe、interface の一覧、ラベルの口。
pub(crate) fn probe_info(cli: &Cli, a: &OepAddr) -> ExitCode {
    const CMD: &str = "probe.info";
    let mut p = match connect(cli, CMD, a) {
        Ok(p) => p,
        Err(c) => return c,
    };
    let describe = match p.describe(oep_core::FN) {
        Ok(d) => d,
        Err(e) => return oep_fail(cli, CMD, e),
    };
    let text = |tag: u8| {
        describe
            .iter()
            .find(|t| t.tag == tag)
            .map(|t| String::from_utf8_lossy(&t.value).into_owned())
    };
    use oep_core::tlvs::describe as d;
    let firmware = text(d::FIRMWARE);
    let model = text(d::MODEL);
    let unit_id = text(d::UNIT_ID);
    let chip = text(d::CHIP);
    let max_op_ms = describe
        .iter()
        .find(|t| t.tag == d::MAX_OP_MS && t.value.len() == 4)
        .map(|t| u32::from_le_bytes([t.value[0], t.value[1], t.value[2], t.value[3]]));
    let interfaces = p.list("").unwrap_or_default();
    let labels = ch32rv_oep::config::labels(&mut p).unwrap_or_default();
    let ports = ch32rv_oep::config::ports_named(&labels);
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "probe": {
                "kind": "oep",
                "firmware": firmware,
                "model": model,
                "unit_id": unit_id,
                "chip": chip,
                "max_op_ms": max_op_ms,
                "interfaces": interfaces.iter().map(|i| serde_json::json!({
                    "fn": i.func, "name": i.name, "instance": i.instance, "revision": i.revision,
                })).collect::<Vec<_>>(),
                "ports": ports,
                "labels": labels.iter().map(|(c, l)| serde_json::json!({ "channel": c, "label": l }))
                    .collect::<Vec<_>>(),
            }
        }));
        crate::print_envelope(&env)
    } else {
        let or = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_owned());
        println!("probe:      OEP");
        println!("model:      {}", or(&model));
        println!("unit id:    {}", or(&unit_id));
        println!("chip:       {}", or(&chip));
        println!("firmware:   {}", or(&firmware));
        if let Some(ms) = max_op_ms {
            println!("max_op_ms:  {ms}");
        }
        println!(
            "ports:      {}",
            if ports.is_empty() {
                "(none named)".to_owned()
            } else {
                ports.join(", ")
            }
        );
        println!("interfaces:");
        for i in &interfaces {
            println!(
                "  fn {:<3} {}/{} (revision {})",
                i.func, i.name, i.instance, i.revision
            );
        }
        ExitCode::SUCCESS
    }
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
