//! en: `arduino discovery` / `arduino monitor` (docs/cli.ja.md §4.11): the Arduino Pluggable
//! Discovery and Monitor protocols (line-based stdio JSON). Discovery exposes each WCH probe as a
//! `wchlink://<serial>` port; Monitor serves the IDE's Serial Monitor from the probe's CDC (uart /
//! sdi) or the DMI sources (dmdata / dmseq / rtt), both ways (what the user types reaches the
//! sketch). Machine-facing: never mixes human text onto stdout.
//! ja: `arduino discovery`/`monitor`。Arduino の Pluggable Discovery/Monitor プロトコル(行単位の
//! stdio JSON)。discovery は各 WCH probe を `wchlink://<serial>` port として公開、monitor は DMI
//! IDE の Serial Monitor を probe の CDC(uart / sdi)か DMI の source(dmdata / dmseq / rtt)で
//! 双方向に提供する。

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::args::Cli;

/// `arduino discovery`: the Pluggable Discovery protocol over stdio.
pub fn discovery(_cli: &Cli) -> ExitCode {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let say = |v: &Value| {
        if let Ok(mut o) = out.lock() {
            emit(&mut *o, v);
        }
    };
    // The sync watcher's stop flag. Not joined: a round may be reading a probe for a few seconds,
    // and the answer to STOP / QUIT must not wait for it; the round checks the flag before it
    // says anything.
    let mut sync: Option<Arc<AtomicBool>> = None;
    let stop_sync = |sync: &mut Option<Arc<AtomicBool>>| {
        if let Some(stop) = sync.take() {
            stop.store(true, Ordering::SeqCst);
        }
    };
    for line in stdin_lines_or_exit() {
        let word = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match word.as_str() {
            "" => {}
            "HELLO" => say(&json!({"eventType":"hello","protocolVersion":1,"message":"OK"})),
            "START" => say(&json!({"eventType":"start","message":"OK"})),
            "STOP" => {
                stop_sync(&mut sync);
                say(&json!({"eventType":"stop","message":"OK"}));
            }
            "LIST" => say(&json!({"eventType":"list","ports": list_ports()})),
            "START_SYNC" => {
                stop_sync(&mut sync);
                say(&json!({"eventType":"start_sync","message":"OK"}));
                let stop = Arc::new(AtomicBool::new(false));
                let (o, s) = (out.clone(), stop.clone());
                std::thread::spawn(move || sync_watch(o, s));
                sync = Some(stop);
            }
            "QUIT" => {
                stop_sync(&mut sync);
                say(&json!({"eventType":"quit","message":"OK"}));
                return ExitCode::SUCCESS;
            }
            other => say(
                &json!({"eventType": other.to_ascii_lowercase(), "error": true, "message": format!("unknown command {other}")}),
            ),
        }
    }
    ExitCode::SUCCESS
}

/// en: How long one command may still run after stdin closed before the process ends anyway.
/// A LIST over many probes takes seconds (6 s on the bench); a stuck one never ends.
/// ja: stdin が閉じた後、1 つのコマンドをどれだけ待ってから終えるか。
const ORPHAN_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// en: stdin's lines, read on a thread of their own, so that stdin closing (arduino-cli quit or
/// died) is seen even while a command is stuck - a LIST waiting on a probe or on USB. The lines
/// already read are still answered; but when one command is still running [`ORPHAN_GRACE`] after
/// stdin closed, the process ends, so a discovery never outlives its IDE holding a port.
/// ja: stdin の行を別の thread で読む。stdin が閉じても読んだ行には答えるが、閉じた後 1 つの
/// コマンドが 30 秒を超えて終わらなければプロセスを終える(IDE より長く port を持って残らない)。
struct StdinLines {
    rx: std::sync::mpsc::Receiver<String>,
    /// Milliseconds since `epoch` when the command now running started; 0 while waiting.
    busy_since: std::sync::Arc<std::sync::atomic::AtomicU64>,
    epoch: std::time::Instant,
}

impl Iterator for StdinLines {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        use std::sync::atomic::Ordering;
        self.busy_since.store(0, Ordering::Relaxed);
        let line = self.rx.recv().ok()?;
        let now = (self.epoch.elapsed().as_millis() as u64).max(1);
        self.busy_since.store(now, Ordering::Relaxed);
        Some(line)
    }
}

fn stdin_lines_or_exit() -> StdinLines {
    use std::sync::atomic::{AtomicU64, Ordering};
    let (tx, rx) = std::sync::mpsc::channel();
    let busy_since = std::sync::Arc::new(AtomicU64::new(0));
    let epoch = std::time::Instant::now();
    let busy = busy_since.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                return;
            }
        }
        drop(tx);
        // stdin closed: the lines read are answered, a command stuck past the grace is not.
        let closed = epoch.elapsed();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let since = busy.load(Ordering::Relaxed);
            if since == 0 {
                continue;
            }
            let started = std::time::Duration::from_millis(since).max(closed);
            if epoch.elapsed().saturating_sub(started) > ORPHAN_GRACE {
                std::process::exit(0);
            }
        }
    });
    StdinLines {
        rx,
        busy_since,
        epoch,
    }
}

/// Enumerate WCH probes as Pluggable-Discovery ports (from USB descriptors only - no AttachChip,
/// so it never disturbs an in-flight upload/monitor on the same probe).
pub(crate) fn list_ports() -> Vec<Value> {
    // One USB enumeration for the whole listing (it asked for 49, 6-30 s on a bench of ~200
    // devices, past arduino-cli's 10 s for a LIST). ja: 一覧全体で USB の列挙を 1 回に。
    ch32rv_usb::with_one_enumeration(|| {
        let mut ports = wchlink_ports();
        ports.extend(oep_ports());
        ports.extend(hid_ports());
        ports.extend(isp_route_ports());
        ports.push(waiting_port());
        ports
    })
}

/// en: The port that is always listed (docs/freeze-decisions.ja.md §2): uploading through it waits
/// for the board's own USB bootloader - the factory ISP, or the HID bootloader on a board that has
/// one (boards.txt picks the tool) - which the user brings up after pressing Upload. It is there
/// with nothing connected, so the IDE can keep it selected; a probe is never written through it.
/// ja: いつも一覧にある待つポート。押してから BOOT で入れたボードのブートローダーに書く。
fn waiting_port() -> Value {
    json!({
        "address": format!("{}{}", crate::cmd_isp::BOOTLOADER_SCHEME, crate::cmd_isp::WAIT_FOR_BOOT),
        "label": "USB bootloader (waits for BOOT)",
        "protocol": "ch32rv-bootloader",
        "protocolLabel": "CH32RV USB bootloader",
        "properties": {},
    })
}

/// en: OEP probes (`crate::oep::is_oep_device`) as one `oep://<probe>[/<port>]` port per port its
/// labels name. The labels are read lock-free - through the probe's broker when one runs (never started
/// here), else on its serial port opened briefly - and when the probe cannot be read (another tool
/// holds the port), its last listing stands in for a short while. Plain serial ports are never opened.
/// ja: OEP の probe を、ラベルが名指すポートごとに `oep://<probe>[/<port>]` で出す。ラベルは lock 無しで読む
/// (ブローカーが動いていればそこ経由、ここでは起動しない。無ければ serial port を短く開く)。読めなければ
/// 少しの間だけ前回の一覧。普通の serial port は開かない。
/// en: Whether this user may not read and write the serial port `path` (checked without opening
/// it: an open would toggle its modem lines).
/// ja: この利用者が serial port `path` を読み書きできないか(開かずに調べる。開くと modem 線が動く)。
#[cfg(unix)]
fn no_access(path: &str) -> bool {
    use rustix::fs::{Access, access};
    matches!(
        access(path, Access::READ_OK | Access::WRITE_OK),
        Err(e) if e == rustix::io::Errno::ACCESS || e == rustix::io::Errno::PERM
    )
}

#[cfg(not(unix))]
fn no_access(_path: &str) -> bool {
    false
}

fn oep_ports() -> Vec<Value> {
    let mut out = Vec::new();
    let devices = crate::oep::oep_devices();
    let on_usb: Vec<String> = devices.iter().map(crate::oep::probe_id).collect();
    for dev in devices {
        out.extend(oep_device_ports(dev));
    }
    out.extend(net_oep_ports(&on_usb));
    out
}

/// One USB OEP probe's ports (its labels read), or the unit when this user may not open it.
fn oep_device_ports(dev: ch32rv_usb::UsbDeviceInfo) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(path) = crate::oep::oep_port(&dev) else {
        return out;
    };
    let id = crate::oep::probe_id(&dev);
    let denied = no_access(&path);
    let read = if denied {
        None
    } else {
        read_oep_listing_within(dev.clone(), path.clone(), OEP_LISTING_WITHIN)
    };
    let listed = listing_or_recent(read, &id);
    // en: A probe this user may not open (no udev rule) is still shown, as the unit, with the
    // reason in its label: its ports cannot be read, and leaving it out made the probe look
    // absent with nothing saying why. Opening it fails with the exact fix.
    // ja: 開く権限の無い probe(udev のルールが無い)も、unit として理由を label に添えて出す。
    // 口は読めず、出さないと probe が無いように見えて理由も分からなかった。開けば直し方が出る。
    if listed.is_empty() && denied {
        out.push(json!({
            "address": format!("oep://{id}"),
            "label": format!("OEP probe {id} (no permission: udev rules missing)"),
            "protocol": "oep",
            "protocolLabel": "OEP probe",
            "hardwareId": id,
            "properties": {
                "vid": format!("0x{:04x}", dev.vid()),
                "pid": format!("0x{:04x}", dev.pid()),
                "port": path,
                "error": "permission",
                "fix": crate::session::access_denied_fix(),
            },
        }));
    }
    out.extend(listed);
    out
}

/// en: How often sync mode looks at USB again, and how often it browses the network for OEP probes
/// (a browse takes 600 ms).
/// ja: sync mode が USB を見直す間隔と、ネットワークの OEP の probe を探す間隔(1 回 600 ms)。
const SYNC_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
const SYNC_NET_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

/// en: Sync mode's view of the ports. USB is enumerated every round (it opens nothing); a USB OEP
/// probe's labels are read once when it appears (a read opens its serial port) and kept while it
/// stays; the network is browsed now and then.
/// ja: sync mode の一覧。USB は毎回列挙(何も開かない)。USB の OEP の probe のラベルは現れたときに
/// 1 回読み(serial port を開くので)、居る間は覚えておく。ネットワークは時々探す。
#[derive(Default)]
struct SyncView {
    oep: std::collections::HashMap<String, Vec<Value>>,
    net: Vec<Value>,
    net_at: Option<std::time::Instant>,
}

/// en: The ports that need nothing opened (WCH-Links, HID / ISP bootloaders, CH32RV applications,
/// the waiting port): sync mode says these first, so that a port is known within arduino-cli's 1 s
/// wait at an upload - the whole first round (OEP labels read) took 6 s on the bench, and an
/// upload to a port not yet known goes out with the protocol `serial`.
/// ja: 何も開かずに分かるポート。sync mode はこれを先に出す(書き込みの時の arduino-cli の待ちは
/// 1 秒。OEP のラベルを読む最初の 1 周は台で 6 秒かかり、まだ知らない port への書き込みは
/// protocol `serial` で出ていった)。
fn quick_ports() -> Vec<Value> {
    ch32rv_usb::with_one_enumeration(|| {
        let mut ports = wchlink_ports();
        ports.extend(hid_ports());
        ports.extend(isp_route_ports());
        ports.push(waiting_port());
        ports
    })
}

impl SyncView {
    fn ports(&mut self) -> Vec<Value> {
        ch32rv_usb::with_one_enumeration(|| {
            let mut ports = wchlink_ports();
            let devices = crate::oep::oep_devices();
            let on_usb: Vec<String> = devices.iter().map(crate::oep::probe_id).collect();
            self.oep.retain(|id, _| on_usb.contains(id));
            for dev in devices {
                let id = crate::oep::probe_id(&dev);
                if !self.oep.contains_key(&id) {
                    let listed = oep_device_ports(dev);
                    self.oep.insert(id.clone(), listed);
                }
                ports.extend(self.oep[&id].iter().cloned());
            }
            if self.net_at.is_none_or(|t| t.elapsed() >= SYNC_NET_EVERY) {
                self.net = net_oep_ports(&on_usb);
                self.net_at = Some(std::time::Instant::now());
            }
            ports.extend(self.net.iter().cloned());
            ports.extend(hid_ports());
            ports.extend(isp_route_ports());
            ports.push(waiting_port());
            ports
        })
    }
}

/// en: The events that take the IDE from `known` to `now`: `remove` for a port gone or changed,
/// then `add` for a port new or changed. ja: `known` から `now` への event。
fn sync_events(known: &[Value], now: &[Value]) -> Vec<Value> {
    let same =
        |a: &Value, b: &Value| a["address"] == b["address"] && a["protocol"] == b["protocol"];
    let mut events = Vec::new();
    for old in known {
        match now.iter().find(|n| same(n, old)) {
            Some(n) if n == old => {}
            _ => events.push(json!({"eventType": "remove", "port": {
                "address": old["address"], "protocol": old["protocol"],
            }})),
        }
    }
    for new in now {
        if !known.iter().any(|o| o == new) {
            events.push(json!({"eventType": "add", "port": new}));
        }
    }
    events
}

/// en: Sync mode (START_SYNC): the ports now as `add` events, then every [`SYNC_EVERY`] the ports
/// that came (`add`) and went (`remove`; a port whose description changed is removed and added
/// again), until `stop` is set. arduino-cli and IDE 2 do not LIST again in sync mode: before this,
/// a device that appeared after the start (a bootloader brought up with BOOT, a probe plugged in)
/// never showed.
/// ja: sync mode。今のポートを `add` で出し、その後 [`SYNC_EVERY`] ごとに、来たもの(`add`)と
/// 消えたもの(`remove`。説明が変わったものは remove して add)を知らせる。arduino-cli と IDE 2 は
/// sync mode では LIST し直さないので、以前は開始の後に現れた device(BOOT で出した bootloader、
/// 挿した probe)が出なかった。
fn sync_watch(
    out: std::sync::Arc<std::sync::Mutex<std::io::Stdout>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;
    let mut view = SyncView::default();
    let mut known: Vec<Value> = Vec::new();
    // The quick ones first; the next round adds the OEP probes' ports.
    let first = quick_ports();
    if !stop.load(Ordering::SeqCst) {
        let Ok(mut o) = out.lock() else { return };
        for e in sync_events(&known, &first) {
            emit(&mut *o, &e);
        }
        drop(o);
        known = first;
    }
    loop {
        let now = view.ports();
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let events = sync_events(&known, &now);
        if !events.is_empty() {
            let Ok(mut o) = out.lock() else { return };
            for e in &events {
                emit(&mut *o, e);
            }
        }
        known = now;
        let until = std::time::Instant::now() + SYNC_EVERY;
        while std::time::Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// en: How long one USB OEP probe may take to give its labels in a LIST (one takes well under 1 s on the
/// bench). A probe whose USB stalls can hold the open of its serial port in the kernel for about
/// a minute (an ESP32-P4 probe on the bench, 2026-11-20: 55 s), past arduino-cli's 10 s for
/// the whole LIST.
/// ja: LIST で USB の OEP の probe 1 台がラベルを返すまでの時間(台では 1 台 1 秒未満)。USB が
/// 止まった probe は serial port の open をカーネルの中で 1 分近く止める(台の ESP32-P4 の probe、
/// 2026-11-20、55 秒)。
const OEP_LISTING_WITHIN: std::time::Duration = std::time::Duration::from_secs(3);

/// en: [`read_oep_listing`] on a thread of its own: a probe that has not answered `within` counts
/// as unreadable (its recent listing stands in), and its thread is left to finish or stay stuck on
/// its own. The thread uses this one's enumeration ([`ch32rv_usb::with_one_enumeration`]).
/// ja: [`read_oep_listing`] を別の thread で。`within` までに返らなければ読めないものとし(前回の
/// 一覧で代える)、その thread は待たずに放っておく。
fn read_oep_listing_within(
    dev: ch32rv_usb::UsbDeviceInfo,
    path: String,
    within: std::time::Duration,
) -> Option<Vec<Value>> {
    let snapshot = ch32rv_usb::current_enumeration();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let read = || read_oep_listing(&dev, &path);
        let _ = tx.send(match snapshot {
            Some(devs) => ch32rv_usb::with_enumeration(devs, read),
            None => read(),
        });
    });
    rx.recv_timeout(within).ok().flatten()
}

/// How long a probe's last listing stands in for it while it cannot be read (another tool holds
/// it): enough for a short tussle such as one upload, not for a probe that changed meanwhile.
const LISTING_LIFE: std::time::Duration = std::time::Duration::from_secs(30);

/// en: The probe's ports: `read` when it could be read (kept as its last listing, by unit id),
/// else that last listing while it is younger than [`LISTING_LIFE`], only its entries that name
/// a port of this very probe, or the probe itself, in today's form (`oep://<unit>[/<port>]`).
/// ja: 読めればそれ(unit id ごとに前回の一覧として残す)。読めなければ 30 秒以内の前回の一覧を、
/// 同じ probe の今の形のアドレスの分だけ。
fn listing_or_recent(read: Option<Vec<Value>>, unit: &str) -> Vec<Value> {
    let cache = crate::broker::listing_cache(&format!("oep-{}", unit.to_ascii_lowercase()));
    if let Some(ports) = read {
        let _ = ch32rv_usb::create_runtime_dir();
        let _ = std::fs::write(&cache, Value::Array(ports.clone()).to_string());
        return ports;
    }
    let fresh = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < LISTING_LIFE);
    let Some(Value::Array(ports)) = fresh
        .then(|| std::fs::read_to_string(&cache).ok())
        .flatten()
        .and_then(|t| serde_json::from_str(&t).ok())
    else {
        return Vec::new();
    };
    let whole = format!("oep://{unit}");
    let prefix = format!("{whole}/");
    ports
        .into_iter()
        .filter(|p| {
            p["address"].as_str().is_some_and(|a| {
                a.eq_ignore_ascii_case(&whole)
                    || a.get(..prefix.len())
                        .is_some_and(|h| h.eq_ignore_ascii_case(&prefix))
                        && port_name_ok(&a[prefix.len()..])
            })
        })
        .collect()
}

/// A port name as an address carries it: the `S` of an `S.N` label.
fn port_name_ok(name: &str) -> bool {
    ch32rv_oep::config::port_line(&format!("{name}.x")).is_some()
}

fn read_oep_listing(dev: &ch32rv_usb::UsbDeviceInfo, path: &str) -> Option<Vec<Value>> {
    // A LIST must stay quick: a probe that does not answer (older firmware, another framing) costs
    // two short waits, not the usual reply timeout twice.
    let mut p = crate::oep::probe_for_listing(dev)?;
    let props = json!({
        "vid": format!("0x{:04x}", dev.vid()),
        "pid": format!("0x{:04x}", dev.pid()),
        "port": path,
    });
    port_entries(&mut p, &crate::oep::probe_id(dev), &props)
}

/// en: OEP probes on the local network that are not on USB (DNS-SD `_oep._tcp`, oep-spec
/// transports §3), listed like the USB ones: one `oep://<unit_id>[/<port>]` per port, read over TCP
/// (lock-free) after describe confirmed the unit id; its recent listing for a short while when it cannot be read.
/// ja: USB に無い、ネットワークの OEP の probe(DNS-SD)。USB のものと同じくポートごとに出す。
fn net_oep_ports(on_usb: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for f in crate::mdns::browse(std::time::Duration::from_millis(600)) {
        let (Some(unit), Some(addr)) = (f.unit_id.clone(), f.endpoint()) else {
            continue;
        };
        if on_usb.iter().any(|u| u.eq_ignore_ascii_case(&unit)) {
            continue;
        }
        // Through its broker when one runs (never started here), else a brief direct look.
        let target = crate::broker::BrokerTarget::Net {
            addr: addr.clone(),
            unit: unit.clone(),
            via: None,
        };
        let opened = match crate::broker::existing_link_for(&target) {
            Some(l) => ch32rv_oep::session::Probe::connect(l).map_err(|e| e.to_string()),
            None => crate::oep::open_net(&addr, &unit),
        };
        let read = opened.ok().and_then(|mut p| {
            p.link().set_timeout(std::time::Duration::from_millis(500));
            // Not on USB: `oep://` without `via` too (host guide §5.3: one line per place; the
            // address reaches it over the network whatever its IP).
            port_entries(
                &mut p,
                &unit,
                &json!({"tcp": addr, "instance": f.instance, "topology": "network"}),
            )
        });
        out.extend(listing_or_recent(read, &unit));
    }
    out
}

/// en: The probe's ports as discovery ports (host guide §5.3): one `oep://<id>/<port>` per port
/// its labels name (the `S` of the `S.N` labels, as written); a probe naming none is listed as
/// itself, `oep://<id>` (its pins then come from `--pins`). `props` added to each one's
/// properties. Read lock-free, without attaching: the target is not known until a tool attaches.
/// ja: probe の口を discovery の port として出す。ラベルが名指す口ごとに 1 つ。名前つきの口が
/// 無い probe は `oep://<id>` で出す。attach しないので target は分からない。
fn port_entries(p: &mut ch32rv_oep::session::Probe, id: &str, props: &Value) -> Option<Vec<Value>> {
    let labels = ch32rv_oep::config::labels(p).ok()?;
    let ports = ch32rv_oep::config::ports_named(&labels);
    let entry = |port: Option<&str>| {
        let mut properties = json!({ "port_name": port });
        if let (Some(m), Some(extra)) = (properties.as_object_mut(), props.as_object()) {
            for (k, v) in extra {
                m.insert(k.clone(), v.clone());
            }
        }
        let (address, label, hardware) = match port {
            Some(port) => (
                format!("oep://{id}/{port}"),
                format!("{port} on OEP probe {id}"),
                format!("{id}/{port}"),
            ),
            None => (
                format!("oep://{id}"),
                format!("OEP probe {id}"),
                id.to_owned(),
            ),
        };
        json!({
            "address": address,
            "label": label,
            "protocol": "oep",
            "protocolLabel": "OEP probe",
            "hardwareId": hardware,
            "properties": properties,
        })
    };
    if ports.is_empty() {
        return Some(vec![entry(None)]);
    }
    Some(ports.iter().map(|s| entry(Some(s))).collect())
}

/// WCH-Links as `wchlink://<serial>`, or `wchlink://usb-<bus>-<ports>` for one without a serial.
fn wchlink_ports() -> Vec<Value> {
    let entries = crate::cmd_probe::wch_devices().unwrap_or_default();
    entries
        .iter()
        // A Link with neither a serial nor a stable position has no address that would still name
        // it after a replug; it is left out (select it with --probe).
        .filter_map(|e| e.dev.port_id().map(|id| (e, id)))
        .map(|(e, id)| {
            // en: Still listed when its driver keeps ch32rv out (the user sees it and why), with
            // the driver named in the label and the properties.
            // ja: driver のために開けない Link も出す(見えて、理由が分かるように)。label と properties に
            // driver の名前。
            let driver = e.dev.foreign_driver();
            let label = match &driver {
                Some(d) => format!("WCH-Link {id} (cannot open: driver {d})"),
                None => format!("WCH-Link {id}"),
            };
            let mut v = json!({
                "address": format!("wchlink://{id}"),
                "label": label,
                "protocol": "wchlink",
                "protocolLabel": "WCH-Link (RISC-V debug)",
                "hardwareId": id,
                "properties": {
                    "serial": e.dev.serial(),
                    "vid": format!("0x{:04x}", e.dev.vid()),
                    "pid": format!("0x{:04x}", e.dev.pid()),
                    "mode": crate::cmd_probe::mode_str(e.mode),
                },
            });
            if let Some(d) = driver {
                v["properties"]["driver"] = json!(d);
            }
            v
        })
        .collect()
}

/// en: rv003usb / UIAPduino HID bootloaders (`1209:b803`, `1209:b003`) as
/// `ch32rv-hid://<topology>`, protocol `ch32rv-hid`: a bootloader has no serial number, and the
/// position is what `boot hid flash --device ch32rv-hid://…` finds it by again. `vid` / `pid`
/// let boards.txt's `upload_port.N.vid/pid` name the board.
/// ja: rv003usb / UIAPduino の HID bootloader を `ch32rv-hid://<topology>`(protocol `ch32rv-hid`)で
/// 出す(serial 番号が無く、`boot hid flash --device ch32rv-hid://…` も位置で引く)。`vid` / `pid` で
/// boards.txt が板名を出す。
fn hid_ports() -> Vec<Value> {
    let devs = ch32rv_usb::enumerate().unwrap_or_default();
    devs.iter()
        .filter(|d| ch32rv_boot::DEFAULT_HID_IDS.contains(&(d.vid(), d.pid())))
        .map(|d| {
            let topology = d.topology();
            json!({
                "address": format!("{}{topology}", crate::cmd_boot::HID_SCHEME),
                "label": format!("HID bootloader {topology}"),
                "protocol": "ch32rv-hid",
                "protocolLabel": "USB HID bootloader (rv003usb)",
                "hardwareId": topology,
                "properties": {
                    "vid": format!("0x{:04x}", d.vid()),
                    "pid": format!("0x{:04x}", d.pid()),
                    "topology": topology,
                },
            })
        })
        .collect()
}

/// en: The factory ISP route's ports. A running CH32RV application (a DFU run-time interface named
/// `CH32RV`, ArduinoCore-CH32RV's default) as `ch32rv-app://<topology>`, also when it has a CDC
/// port - a serial port alone does not say the device is a CH32RV, so the application port is
/// the one to upload through, and its monitor opens that CDC port. A factory ISP bootloader
/// (`1a86:55e0`) as `ch32rv-isp://<topology>`, while it is up (it returns to the application
/// after ~10 s unattended). `4348:55e0` is not listed: a WCH-Link in IAP mode shares it. Nothing
/// is opened. ja: factory ISP 経路の port。CH32RV のアプリ(名前 `CH32RV` の DFU run-time)を
/// `ch32rv-app://`(CDC があっても。serial port だけでは CH32RV と分からない)、ISP bootloader
/// (`1a86:55e0`)を `ch32rv-isp://` で出す。`4348:55e0` は LinkE の IAP と同じなので出さない。
fn isp_route_ports() -> Vec<Value> {
    let devs = ch32rv_usb::enumerate().unwrap_or_default();
    let props = |d: &ch32rv_usb::UsbDeviceInfo| {
        json!({
            "vid": format!("0x{:04x}", d.vid()),
            "pid": format!("0x{:04x}", d.pid()),
            "serialNumber": d.serial().unwrap_or(""),
            "topology": d.topology(),
        })
    };
    let apps: Vec<ch32rv_usb::UsbDeviceInfo> = ch32rv_usb::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter(crate::cmd_isp::is_ch32rv_app)
        .collect();
    let mut ports = Vec::new();
    for d in &devs {
        let topology = d.topology();
        if crate::cmd_isp::is_ch32rv_app(d) {
            ports.push(json!({
                "address": format!("{}{}", crate::cmd_isp::APP_SCHEME, crate::cmd_isp::app_id(d, &apps)),
                "label": format!("{} (CH32RV, USB)", d.product().unwrap_or("CH32RV application")),
                "protocol": "ch32rv-app",
                "protocolLabel": "CH32RV application (USB)",
                "hardwareId": d.serial().filter(|s| !s.is_empty()).map_or_else(|| topology.clone(), str::to_owned),
                "properties": props(d),
            }));
        } else if (d.vid(), d.pid()) == (0x1a86, 0x55e0) {
            ports.push(json!({
                "address": format!("{}{topology}", crate::cmd_isp::ISP_SCHEME),
                "label": "CH32RV ISP bootloader (USB)",
                "protocol": "ch32rv-isp",
                "protocolLabel": "WCH factory ISP bootloader (USB)",
                "hardwareId": topology,
                "properties": props(d),
            }));
        }
    }
    ports
}

fn emit(out: &mut impl Write, v: &Value) {
    // One compact JSON object per line; flush so the IDE sees it immediately.
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

// ---- arduino monitor (Pluggable Monitor protocol) ----

/// en: `CH32RV_MONITOR_LOG=<file>`: the monitor appends what it is told, what it answers and what
/// its session does, one timestamped line each - for "which command went unanswered" when an IDE
/// times out (arduino-cli shows a tool's stderr to nobody).
/// ja: `CH32RV_MONITOR_LOG=<file>` に、受けた命令・返事・session の出来事を時刻付きで追記する。
pub(crate) fn mlog(msg: std::fmt::Arguments<'_>) {
    static LOG: std::sync::OnceLock<Option<Mutex<std::fs::File>>> = std::sync::OnceLock::new();
    let log = LOG.get_or_init(|| {
        let path = std::env::var_os("CH32RV_MONITOR_LOG")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(Mutex::new)
    });
    if let Some(f) = log {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let mut f = match f.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let _ = writeln!(
            f,
            "{}.{:03} [{}] {msg}",
            now.as_secs(),
            now.subsec_millis(),
            std::process::id()
        );
    }
}

/// [`emit`] for the monitor: logged too.
fn memit(out: &mut impl Write, v: &Value) {
    mlog(format_args!("> {v}"));
    emit(out, v);
}

use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ch32rv_contract::policy::MonitorSource;
use serialport::SerialPort;

use crate::cmd_probe::Entry;
use crate::session::Session;
use crate::source::{self, DmiSource};

/// en: The sources a serial port offers, in the order the IDE lists them; the first (`uart`,
/// the port as it is) is the default. A plain serial port (no WCH probe behind it) can only do
/// `uart`, which is what the IDE would do anyway.
/// ja: serial port の source。先頭(`uart` = port をそのまま)が既定。WCH probe の無い素の serial
/// port は `uart` だけ。
const SERIAL_SOURCES: [MonitorSource; 7] = [
    MonitorSource::Auto,
    MonitorSource::Uart,
    MonitorSource::Sdi,
    MonitorSource::Dmdata,
    MonitorSource::Dmseq,
    MonitorSource::Rtt,
    MonitorSource::FixtureUart,
];

/// en: The sources an `oep://<probe>[/<port>]` port offers: the target's console (dmseq, the default,
/// dmdata, sdi), the probe's fixture UART, and RTT (run by ch32rv over the probe's riscv-dm).
/// ja: `oep://` の port の source。target の console(dmseq 既定 / dmdata / sdi)、fixture の UART、RTT
/// (ch32rv が probe の riscv-dm の上で行う)。
const OEP_SOURCES: [MonitorSource; 6] = [
    MonitorSource::Auto,
    MonitorSource::Dmseq,
    MonitorSource::Dmdata,
    MonitorSource::Sdi,
    MonitorSource::FixtureUart,
    MonitorSource::Rtt,
];

/// en: The sources a `wchlink://` port offers. `uart` (the default) is the Link's CDC, found as
/// `probe list`'s `ports` finds it, opened as a serial port's `uart` is (no attach, no probe lock):
/// what the IDE picks for a `wchlink://` upload port then shows the sketch's `Serial` without
/// touching the target (an attach re-clocks it, and breaks a CH32V205's s0).
/// ja: `wchlink://` の source。既定の `uart` は Link の CDC(`probe list` の `ports` と同じに引く)を
/// serial の `uart` と同じに開く(attach しない、lock なし)。IDE が書き込みの `wchlink://` のまま
/// モニターを開いても、target に触れずにスケッチの `Serial` が見える(attach はクロックを組み替え、
/// CH32V205 の s0 を壊す)。
const WCHLINK_SOURCES: [MonitorSource; 6] = [
    MonitorSource::Auto,
    MonitorSource::Uart,
    MonitorSource::Dmdata,
    MonitorSource::Dmseq,
    MonitorSource::Sdi,
    MonitorSource::Rtt,
];

/// The builtin serial-monitor's baud list, so a board's `monitor_port.serial.baudrate` means the
/// same here.
const BAUDS: [u32; 20] = [
    300, 600, 750, 1200, 2400, 4800, 9600, 19200, 31250, 38400, 57600, 74880, 115200, 230400,
    250000, 460800, 500000, 921600, 1_000_000, 2_000_000,
];

/// How long a backend waits for data before it looks at `stop` / the settings again.
const TICK: Duration = Duration::from_millis(50);

/// en: How long an OEP console waits between empty reads (each read is one lock-free request
/// through the broker, about a millisecond): short, so the target's output shows up within a few
/// ms; the wait also ends at once when input comes.
/// ja: OEP の console の、空の読み出しの間の待ち(読み出し 1 回はブローカー越しの lock 無しの要求で
/// 約 1 ms)。短くして target の出力が数 ms で届くように。入力が来たらすぐ終わる。
const OEP_IDLE: Duration = Duration::from_millis(5);

/// en: The monitor's settings (the DESCRIBE parameters). `baudrate`, `dtr` and `rts` apply to
/// `uart` and are accepted (and ignored) with the other sources. `chip` is `--chip`: the board's
/// family (`monitor_port.serial.chip`), checked fail-closed against the target wherever the
/// source attaches anyway (`uart` does not attach, since a WCH-Link attach re-clocks the target).
/// ja: DESCRIBE の設定。`baudrate` / `dtr` / `rts` は `uart` 用で、他の source でも受けて無視する。
/// `chip` は `--chip` と同じ(板の家系)。source が attach するところで fail-closed に照合する
/// (`uart` は attach しない。WCH-Link の attach は target のクロックを組み替えるため)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Settings {
    sources: &'static [MonitorSource],
    source: MonitorSource,
    baud: u32,
    dtr: bool,
    rts: bool,
    chip: Option<String>,
    /// The debug speed (`--speed`'s values) the sources that attach use (wchlink / oep).
    speed: String,
}

impl Settings {
    fn new(protocol: &str, chip: Option<String>, speed: String) -> Self {
        let sources: &'static [MonitorSource] = match protocol {
            "wchlink" => &WCHLINK_SOURCES,
            "oep" => &OEP_SOURCES,
            // serial / ch32rv-app (its CDC) / ch32rv-isp / ch32rv-hid / ch32rv-bootloader: the
            // UART source only.
            _ => &SERIAL_SOURCES,
        };
        // Line settings default to the builtin serial-monitor's.
        Settings {
            sources,
            source: sources[0],
            baud: 9600,
            dtr: true,
            rts: true,
            chip,
            speed,
        }
    }

    /// Apply `CONFIGURE <key> <value>`; `Err` is the message the IDE shows.
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let on_off = |v: &str| match v {
            "on" => Ok(true),
            "off" => Ok(false),
            _ => Err(format!("{key} is on or off, not `{value}`")),
        };
        match key {
            "source" => {
                self.source = *self
                    .sources
                    .iter()
                    .find(|s| s.as_str() == value)
                    .ok_or_else(|| format!("unknown source `{value}`"))?;
            }
            "baudrate" => {
                self.baud = value
                    .parse()
                    .ok()
                    .filter(|b| BAUDS.contains(b))
                    .ok_or_else(|| format!("unsupported baudrate `{value}`"))?;
            }
            "dtr" => self.dtr = on_off(value)?,
            "rts" => self.rts = on_off(value)?,
            // en: No longer offered (0.35.1 and before did): IDE 2 keeps every setting it was once
            // offered in pluggable-monitor-settings.json and sends it back on each open, and a saved
            // part number made OPEN fail after the board's Part Number changed. Taken and ignored,
            // so such a file still opens; `--chip` / `--speed` on the command line still apply.
            // ja: もう出さない(0.35.1 までは出していた)。IDE 2 は一度出された設定を保存し、開くたびに
            // 送り返す。保存された型番は Part Number を変えた後の OPEN を失敗させた。受けて無視する
            // (そのファイルでも開ける)。コマンド行の `--chip` / `--speed` は効く。
            "chip" | "speed" => {}
            _ => return Err(format!("unknown setting `{key}`")),
        }
        Ok(())
    }

    fn describe(&self, protocol: &str) -> Value {
        let on_off = |b: bool| if b { "on" } else { "off" };
        json!({
            "protocol": protocol,
            "configuration_parameters": {
                "source": {
                    "label": "Monitor Source", "type": "enum",
                    "value": self.sources.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    "selected": self.source.as_str(),
                },
                "baudrate": {
                    "label": "Baud Rate (UART)", "type": "enum",
                    "value": BAUDS.iter().map(u32::to_string).collect::<Vec<_>>(),
                    "selected": self.baud.to_string(),
                },
                "dtr": {
                    "label": "DTR (UART)", "type": "enum",
                    "value": ["on", "off"], "selected": on_off(self.dtr),
                },
                "rts": {
                    "label": "RTS (UART)", "type": "enum",
                    "value": ["on", "off"], "selected": on_off(self.rts),
                },
            }
        })
    }
}

/// What the backends need from the command line (the `Cli` itself stays on the main thread).
#[derive(Clone)]
struct Env {
    lock_timeout: Duration,
}

/// State shared between the protocol loop and the session thread.
struct Shared {
    settings: Mutex<Settings>,
    stop: AtomicBool,
}

impl Shared {
    fn settings(&self) -> Settings {
        match self.settings.lock() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }
}

/// `arduino monitor`: the Pluggable Monitor protocol over stdio. OPEN resolves the port address
/// (a probe's serial port such as `/dev/ttyACM0` / `COM3`, or `wchlink://<serial>`), opens the
/// selected source, connects (TCP client) to the IDE-provided address and pipes the target's
/// output to it and the IDE's input back. The process ends when stdin closes (arduino-cli may die
/// without sending CLOSE / QUIT), and when the session fails after OPEN, having first sent the
/// reason as a data line (arduino-cli shows the tool's stderr to nobody).
pub fn monitor(cli: &Cli, protocol: &str) -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let env = Env {
        lock_timeout: Duration::from_secs(cli.lock_timeout),
    };
    let shared = Arc::new(Shared {
        settings: Mutex::new(Settings::new(protocol, cli.chip.clone(), cli.speed.clone())),
        stop: AtomicBool::new(false),
    });
    let mut running: Option<(JoinHandle<()>, TcpStream)> = None;

    // CLOSE / QUIT / EOF: stop the session thread and shut the socket so its blocked reads end.
    // Every backend waits in `TICK` steps, so the join is short.
    let close = |running: &mut Option<(JoinHandle<()>, TcpStream)>| {
        shared.stop.store(true, Ordering::SeqCst);
        if let Some((h, sock)) = running.take() {
            let _ = sock.shutdown(Shutdown::Both);
            let _ = h.join();
        }
    };

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        mlog(format_args!("< {line}"));
        let parts: Vec<&str> = line.split_whitespace().collect();
        let word = parts.first().copied().unwrap_or("").to_ascii_uppercase();
        match word.as_str() {
            "" => {}
            "HELLO" => memit(
                &mut out,
                &json!({"eventType":"hello","protocolVersion":1,"message":"OK"}),
            ),
            "DESCRIBE" => memit(
                &mut out,
                &json!({
                    "eventType":"describe","message":"OK",
                    "port_description": shared.settings().describe(protocol),
                }),
            ),
            "CONFIGURE" => {
                let result = match (parts.get(1), parts.get(2)) {
                    (Some(k), Some(v)) => match shared.settings.lock() {
                        Ok(mut g) => g.set(k, v),
                        Err(p) => p.into_inner().set(k, v),
                    },
                    _ => Err("CONFIGURE <key> <value>".to_owned()),
                };
                match result {
                    Ok(()) => memit(&mut out, &json!({"eventType":"configure","message":"OK"})),
                    Err(m) => memit(
                        &mut out,
                        &json!({"eventType":"configure","error":true,"message":m}),
                    ),
                }
            }
            "OPEN" => {
                // OPEN <client-host:port> <port-address>
                let (Some(&client), Some(&address)) = (parts.get(1), parts.get(2)) else {
                    memit(
                        &mut out,
                        &json!({"eventType":"open","error":true,"message":"OPEN needs <host:port> <port>"}),
                    );
                    continue;
                };
                if running.is_some() {
                    memit(
                        &mut out,
                        &json!({"eventType":"open","error":true,"message":"already open"}),
                    );
                    continue;
                }
                shared.stop.store(false, Ordering::SeqCst);
                let slot: Arc<OpenSlot> = Arc::default();
                let (client, address) = (client.to_owned(), address.to_owned());
                let (shared2, env2, slot2) = (shared.clone(), env.clone(), slot.clone());
                let handle = std::thread::spawn(move || {
                    session(&client, &address, &shared2, &env2, &slot2);
                    // Whatever happened, the OPEN is not left waiting.
                    slot2.finish(Err("monitor session ended before it opened".to_owned()));
                });
                match slot.wait(OPEN_ANSWER_WITHIN) {
                    Opened::Ready(sock) => {
                        running = Some((handle, sock));
                        memit(&mut out, &json!({"eventType":"open","message":"OK"}));
                    }
                    Opened::Still(mut sock) => {
                        mlog(format_args!("open: still opening, answered OK"));
                        let _ = write!(
                            sock,
                            "[ch32rv monitor] still connecting to the target; its output follows when it answers\r\n"
                        );
                        running = Some((handle, sock));
                        memit(&mut out, &json!({"eventType":"open","message":"OK"}));
                    }
                    Opened::Failed(m) => {
                        let _ = handle.join();
                        memit(
                            &mut out,
                            &json!({"eventType":"open","error":true,"message":m}),
                        );
                    }
                }
            }
            "CLOSE" => {
                close(&mut running);
                memit(&mut out, &json!({"eventType":"close","message":"OK"}));
            }
            "QUIT" => {
                // en: Answer first: arduino-cli waits 250 ms for it and then kills the process
                // whatever came back (internal/arduino/monitor/monitor.go, 1.3.1), so a session
                // still open (no CLOSE before) gets its clean-up only while it is not killed.
                // CLOSE, which arduino-cli waits 5 s for, is where the clean-up is done.
                // ja: 先に返事をする(arduino-cli は 250 ms 待ち、返事の有無によらず process を止める)。
                // 後始末は 5 秒待つ CLOSE で済ませる。
                memit(&mut out, &json!({"eventType":"quit","message":"OK"}));
                close(&mut running);
                return ExitCode::SUCCESS;
            }
            other => memit(
                &mut out,
                &json!({
                "eventType": other.to_ascii_lowercase(),"error":true,
                "message":format!("unknown command {other}")}),
            ),
        }
    }
    // stdin closed: arduino-cli is gone (or going). Never outlive it.
    mlog(format_args!("stdin closed"));
    close(&mut running);
    ExitCode::SUCCESS
}

/// A resolved port address: the WCH probe behind it (if any) and the serial port to use for
/// `uart` / `sdi` (if any).
struct Resolved {
    entry: Option<Entry>,
    port: Option<String>,
    /// An OEP probe's address (`oep://` or a serial port no WCH-Link owns).
    oep: Option<crate::oep::OepAddr>,
}

/// en: `wchlink://<serial>` names a probe (its first CDC serves uart / sdi). Anything else is a
/// serial port path: the WCH probe that owns it, if one does (found through `probe list`'s
/// `ports`, so a Link without a USB serial number is found by position), else a plain serial
/// port, which only `uart` can use.
/// ja: `wchlink://<serial>` は probe(uart / sdi は最初の CDC)。それ以外は serial port の path で、
/// それを持つ WCH probe があればそれ(`probe list` の `ports` で引くので serial 番号の無い Link も
/// 位置で見つかる)、無ければ素の serial port(uart だけが使える)。
fn resolve_address(address: &str) -> Result<Resolved, String> {
    // en: Enumerated only for the addresses that name a WCH-Link (an enumeration waits on any USB
    // device the kernel is still probing). ja: WCH-Link を指すアドレスでだけ列挙する。
    let entries = || crate::cmd_probe::wch_devices().map_err(|e| format!("USB enumeration: {e}"));
    if let Some(id) = address.strip_prefix("wchlink://") {
        let entry = entries()?
            .into_iter()
            .find(|e| e.dev.port_id().as_deref() == Some(id))
            .ok_or_else(|| format!("no WCH-Link {id} is connected"))?;
        let port = entry.dev.serial_ports().into_iter().next();
        return Ok(Resolved {
            entry: Some(entry),
            port,
            oep: None,
        });
    }
    if let Some(instead) = ch32rv_usb::selector::removed(address) {
        return Err(format!("`{address}`: this form is gone; write {instead}"));
    }
    // A CH32RV application: its CDC port, if it has one. The bootloader has no console.
    if let Some(id) = address.strip_prefix(crate::cmd_isp::APP_SCHEME) {
        let dev = crate::cmd_isp::find_app(id).map_err(|(_, m)| m)?;
        let port = dev.serial_ports().into_iter().next().ok_or_else(|| {
            "this CH32RV application has no serial (CDC) port to monitor".to_owned()
        })?;
        return Ok(Resolved {
            entry: None,
            port: Some(port),
            oep: None,
        });
    }
    if address.starts_with(crate::cmd_isp::ISP_SCHEME) {
        return Err("the ISP bootloader has no console to monitor".to_owned());
    }
    if address.starts_with(crate::cmd_boot::HID_SCHEME) {
        return Err("the HID bootloader has no console to monitor".to_owned());
    }
    if address.starts_with(crate::cmd_isp::BOOTLOADER_SCHEME) {
        return Err(
            "this port waits for a bootloader to upload through; it has no console to monitor (pick the board's own port)"
                .to_owned(),
        );
    }
    // `oep://` with any `via`: the same resolution as `--probe`, so the monitor meets the broker
    // the upload used.
    if address.starts_with("oep://") {
        let a = crate::oep::resolve_oep_url(address)?;
        return Ok(Resolved {
            entry: None,
            port: None,
            oep: Some(a),
        });
    }
    let sel = ch32rv_usb::Selector::Port(address.to_owned());
    let mut owners: Vec<Entry> = entries()?
        .into_iter()
        .enumerate()
        .filter(|(i, e)| sel.matches(&e.dev, *i))
        .map(|(_, e)| e)
        .collect();
    if owners.len() > 1 {
        return Err(format!(
            "{} WCH-Links could own {address} (without a USB serial number they cannot be told \
             apart on this OS); pick the Link's wchlink://<serial> port, or name it with --probe",
            owners.len()
        ));
    }
    let entry = owners.pop();
    let oep = entry
        .is_none()
        .then(|| crate::oep::OepAddr::Serial(address.to_owned()));
    Ok(Resolved {
        entry,
        port: Some(address.to_owned()),
        oep,
    })
}

/// One open source. Each holds what keeps it alive (the probe lock, the attach) until dropped.
enum Backend {
    Uart {
        port: Box<dyn SerialPort>,
        applied: Settings,
    },
    Dmi {
        src: Box<DmiSource>,
        session: Box<Session>,
    },
    /// `auto`: every path the target talks on, merged.
    Auto {
        auto: Box<crate::auto_console::Auto>,
        applied: Settings,
    },
    /// A console stream on an OEP probe, through its broker.
    Oep {
        console: Box<crate::oep::ConsoleSession>,
        source: MonitorSource,
        /// The fixture UART's baud as last configured.
        baud: u32,
    },
}

/// Why a backend returned.
#[derive(Debug)]
enum Leave {
    /// CLOSE / QUIT / stdin EOF.
    Stop,
    /// `source` changed while open: set up the new one on the same socket.
    Switch,
    /// The session is lost; the string is the reason for the user.
    Failed(String),
}

impl Backend {
    fn open(r: &Resolved, s: Settings, env: &Env) -> Result<Self, String> {
        if s.source == MonitorSource::Auto {
            return Self::open_auto(r, s, env);
        }
        // en: A debug source on a serial port no WCH-Link owns: an OEP probe, through its broker.
        // (uart stays the port as it is - opening an unknown port to send an OEP confirm would put
        // bytes on someone's device; an OEP UART-bridge probe streams its bound console raw there.)
        // ja: WCH-Link のものでない serial port で debug の source なら OEP の probe(ブローカー経由)。
        // uart は port の素通しのまま(知らない port に OEP の confirm を送ると誰かの device に
        // バイトが入る。OEP の UART bridge の probe は bind した console をそこに生で流す)。
        // en: A WCH-Link's dmdata / dmseq also go through the Link's broker, so a gdb or a flash
        // can use the same Link meanwhile (the broker polls the mailbox as the Link's console).
        // rtt and sdi still open the Link themselves.
        // ja: WCH-Link の dmdata / dmseq もブローカー経由(同じ Link を gdb や flash が同時に使える)。
        let wch_via_broker = r.entry.as_ref().and_then(|e| {
            matches!(
                s.source,
                MonitorSource::Dmdata | MonitorSource::Dmseq | MonitorSource::Sdi
            )
            .then(|| crate::oep::OepAddr::Wch(crate::broker::BrokerTarget::wch(e)))
        });
        if let Some(oep) = wch_via_broker.as_ref().or(r.oep.as_ref())
            && (s.source != MonitorSource::Uart || matches!(oep, crate::oep::OepAddr::Named { .. }))
        {
            use crate::oep::StreamWanted;
            use ch32rv_oep::stream::Mechanism;
            let wanted = match s.source {
                MonitorSource::Sdi => StreamWanted::Console(Mechanism::Sdi),
                MonitorSource::Dmdata => StreamWanted::Console(Mechanism::Dmdata),
                MonitorSource::Dmseq => StreamWanted::Console(Mechanism::Dmseq),
                MonitorSource::FixtureUart => StreamWanted::FixtureUart(s.baud),
                MonitorSource::Rtt => StreamWanted::Rtt,
                MonitorSource::Auto => unreachable!("auto opens in open_auto"),
                MonitorSource::Uart => {
                    return Err(
                        "uart is not available on an OEP probe (it offers dmseq, dmdata, sdi, fixture-uart and rtt)"
                            .to_owned(),
                    );
                }
            };
            let (speed, _) = crate::parse::speed(&s.speed)?;
            let c = crate::oep::ConsoleSession::open(
                oep,
                wanted,
                s.chip.as_deref(),
                Some(crate::oep::speed_hz(speed)),
            )?;
            return Ok(Backend::Oep {
                console: Box::new(c),
                source: s.source,
                baud: s.baud,
            });
        }
        match s.source {
            MonitorSource::Auto => unreachable!("auto opens in open_auto"),
            MonitorSource::FixtureUart => {
                Err("fixture-uart is an OEP probe's source (this port is not one)".to_owned())
            }
            MonitorSource::Uart => {
                let path = r
                    .port
                    .as_deref()
                    .ok_or("this probe has no serial port for uart")?;
                // en: No probe lock: uart uses only the CDC (a separate interface from the
                // debug one), and the tty itself is exclusive. A gdb or flash on the same probe
                // may run meanwhile.
                // ja: probe の lock は取らない。uart は CDC(debug の口とは別)だけを使い、tty は排他。
                // The serialport crate opens exclusively (TIOCEXCL) on unix; Windows always does.
                let mut port = serialport::new(path, s.baud)
                    .timeout(TICK)
                    .open()
                    .map_err(|e| format!("open {path}: {e}"))?;
                let _ = port.write_data_terminal_ready(s.dtr);
                let _ = port.write_request_to_send(s.rts);
                Ok(Backend::Uart { port, applied: s })
            }
            MonitorSource::Dmdata
            | MonitorSource::Dmseq
            | MonitorSource::Sdi
            | MonitorSource::Rtt => {
                let entry = r.entry.as_ref().ok_or_else(|| {
                    format!(
                        "{} needs a WCH-Link (this serial port is not one)",
                        s.source.as_str()
                    )
                })?;
                let (speed, _) = crate::parse::speed(&s.speed)?;
                let mut warnings = Vec::new();
                let mut session = Session::attach(
                    entry,
                    speed,
                    Duration::from_millis(1000),
                    env.lock_timeout,
                    s.chip.as_deref(),
                    None,
                    &mut warnings,
                )
                .map_err(|e| format!("attach: {}", e.with_fix()))?;
                // CH32V103: the attach overwrote s1; restart the program before watching it.
                session.keep_running();
                let src = DmiSource::open(&mut session, s.source, &mut warnings)
                    .map_err(|e| format!("open {}: {e}", s.source.as_str()))?;
                // The sources only move while the core runs.
                let _ = session.dm().resume();
                Ok(Backend::Dmi {
                    src: Box::new(src),
                    session: Box::new(session),
                })
            }
        }
    }

    /// en: `auto`: a WCH-Link's CDC and its debug paths (reset to run), an OEP probe's fixture
    /// UART and consoles, or only the serial port (a port no WCH-Link owns is never sent an OEP
    /// confirm: it may be anyone's device).
    /// ja: `auto`。WCH-Link は CDC とデバッグの経路(reset して走らせる)、OEP は fixture の UART と
    /// console、それ以外は serial の口だけ(WCH-Link のものでない口に OEP の confirm は送らない)。
    fn open_auto(r: &Resolved, s: Settings, env: &Env) -> Result<Self, String> {
        use crate::auto_console::Auto;
        let open_port = |path: &str| -> Result<Box<dyn SerialPort>, String> {
            let mut port = serialport::new(path, s.baud)
                .timeout(Duration::from_millis(1))
                .open()
                .map_err(|e| format!("open {path}: {e}"))?;
            let _ = port.write_data_terminal_ready(s.dtr);
            let _ = port.write_request_to_send(s.rts);
            Ok(port)
        };
        let auto = if let Some(entry) = r.entry.as_ref() {
            // The CDC is the UART group; a Link without one (or with it busy) still has the rest.
            let port = r.port.as_deref().and_then(|p| open_port(p).ok());
            let (speed, _) = crate::parse::speed(&s.speed)?;
            match Auto::open_wch(entry, port, speed, env.lock_timeout, s.chip.as_deref()) {
                Ok(a) => a,
                // The debug paths do not open (no USB access, no target): the UART still does.
                Err(why) => match r.port.as_deref() {
                    Some(p) => Auto::open_uart_only(open_port(p)?, &why),
                    None => return Err(why),
                },
            }
        } else if let Some(oep @ crate::oep::OepAddr::Named { .. }) = r.oep.as_ref() {
            let (speed, _) = crate::parse::speed(&s.speed)?;
            Auto::open_oep(
                oep,
                s.chip.as_deref(),
                Some(crate::oep::speed_hz(speed)),
                s.baud,
            )?
        } else {
            let path = r
                .port
                .as_deref()
                .ok_or("this port has no serial port to monitor")?;
            Auto::open_uart(open_port(path)?)
        };
        Ok(Backend::Auto {
            auto: Box::new(auto),
            applied: s,
        })
    }

    /// Pump until stopped, switched or failed.
    fn run(&mut self, shared: &Shared, sock: &mut TcpStream, input: &Receiver<Vec<u8>>) -> Leave {
        let mut pending = Vec::new();
        // en: `auto` on a WCH-Link has just reset the target: what was typed before is dropped.
        // ja: WCH-Link の `auto` は target を reset したところなので、それまでの入力は捨てる。
        if matches!(self, Backend::Auto { .. }) {
            source::drain_input(input, &mut Vec::new());
        }
        let mut buf = [0u8; 512];
        loop {
            if shared.stop.load(Ordering::SeqCst) {
                return Leave::Stop;
            }
            let now = shared.settings();
            source::drain_input(input, &mut pending);
            let got: Result<Vec<u8>, String> = match self {
                Backend::Uart { port, applied, .. } => {
                    if now.source != MonitorSource::Uart {
                        return Leave::Switch;
                    }
                    // Line settings follow the IDE live, like the builtin serial-monitor.
                    if now.baud != applied.baud {
                        let _ = port.set_baud_rate(now.baud);
                    }
                    if now.dtr != applied.dtr {
                        let _ = port.write_data_terminal_ready(now.dtr);
                    }
                    if now.rts != applied.rts {
                        let _ = port.write_request_to_send(now.rts);
                    }
                    *applied = now.clone();
                    if !pending.is_empty() {
                        if let Err(e) = port.write_all(&pending) {
                            return Leave::Failed(format!("serial port write failed: {e}"));
                        }
                        pending.clear();
                    }
                    match port.read(&mut buf) {
                        Ok(n) => Ok(buf[..n].to_vec()),
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(Vec::new()),
                        Err(e) => Err(format!("serial port read failed: {e}")),
                    }
                }
                Backend::Auto { auto, applied } => {
                    if now.source != MonitorSource::Auto {
                        return Leave::Switch;
                    }
                    if (now.baud, now.dtr, now.rts) != (applied.baud, applied.dtr, applied.rts) {
                        if let Err(e) = auto.set_line(now.baud, now.dtr, now.rts) {
                            return Leave::Failed(e);
                        }
                        *applied = now.clone();
                    }
                    auto.input(&pending);
                    pending.clear();
                    match auto.poll() {
                        Ok(b) => {
                            if b.is_empty()
                                && let Ok(chunk) = input.recv_timeout(auto.idle())
                            {
                                pending.extend_from_slice(&chunk);
                            }
                            Ok(b)
                        }
                        Err(e) => Err(e),
                    }
                }
                Backend::Oep {
                    console,
                    source,
                    baud,
                } => {
                    if now.source != *source {
                        return Leave::Switch;
                    }
                    // The fixture UART follows the IDE's baud live, like a serial monitor.
                    if *source == MonitorSource::FixtureUart && now.baud != *baud {
                        if let Err(e) = console.set_baud(now.baud) {
                            return Leave::Failed(format!("fixture UART baud: {e}"));
                        }
                        *baud = now.baud;
                    }
                    if !pending.is_empty() {
                        match console.write(&pending) {
                            Ok(n) => {
                                pending.drain(..n.min(pending.len()));
                            }
                            Err(e) => return Leave::Failed(format!("console write failed: {e}")),
                        }
                    }
                    match console.poll() {
                        Ok(b) => {
                            // en: Nothing came: wait a little, but for the IDE's input too, so a
                            // line typed (or a test's command) goes to the probe at once rather
                            // than after the wait (it waited up to 20 ms before).
                            // ja: 何も来なければ少し待つ。ただし IDE の入力も待ち、来たらすぐ probe へ送る
                            // (前は最大 20 ms 待たせていた)。
                            if b.is_empty()
                                && let Ok(chunk) = input.recv_timeout(if console.is_rtt() {
                                    // RTT halts the hart per poll: RTT's pace, as on a WCH-Link.
                                    Duration::from_millis(50)
                                } else {
                                    OEP_IDLE
                                })
                            {
                                pending.extend_from_slice(&chunk);
                            }
                            Ok(b)
                        }
                        Err(e) => Err(format!("console read failed: {e}")),
                    }
                }
                Backend::Dmi { src, session } => {
                    if now.source.as_str() != src.name() {
                        return Leave::Switch;
                    }
                    match src.poll(session, &mut pending) {
                        Ok(b) => {
                            if b.is_empty() {
                                std::thread::sleep(src.idle());
                            }
                            Ok(b)
                        }
                        Err(e) => Err(format!("{} failed: {e}", src.name())),
                    }
                }
            };
            match got {
                Ok(b) if b.is_empty() => {}
                Ok(b) => {
                    if sock.write_all(&b).and_then(|()| sock.flush()).is_err() {
                        // The IDE side went away; nothing left to tell.
                        return Leave::Stop;
                    }
                }
                Err(m) => return Leave::Failed(m),
            }
        }
    }
}

/// en: The session thread: resolve `address`, open the selected source, connect to the IDE and
/// report the socket (or why not) through `ready`, then pump until CLOSE. A `source` change while
/// open swaps the backend on the same socket. A lost session sends its reason to the IDE as a
/// data line and ends the whole process: arduino-cli shows the tool's stderr to nobody, and the
/// monitor has to end rather than sit silent.
/// ja: セッションのスレッド。address を解き、source を開き、IDE へ接続して socket(か理由)を
/// `ready` で返し、CLOSE まで流す。開いたままの source 変更は同じ socket で backend を替える。
/// 失ったときは理由を data の行で IDE に送ってプロセスごと終わる(arduino-cli は tool の stderr を
/// 見せず、黙ったまま残ってはいけない)。
/// en: How long OPEN waits for the source to open before it answers OK anyway: arduino-cli gives
/// OPEN 10 s (1.3.1), and an OEP attach right after an upload, while the target enumerates USB,
/// took longer (2026-11-14). Past it the monitor says so as data and streams once the source is
/// open, or reports why not and ends, as a session lost later does.
/// ja: OPEN が source を開くのを待つ時間。これを過ぎたら OK を返し、開けたら流す(開けなければ理由を
/// data で流して終わる)。arduino-cli は OPEN を 10 秒しか待たない。
const OPEN_ANSWER_WITHIN: Duration = Duration::from_secs(7);

/// en: Between OPEN and its session thread: the socket (connected first, so OPEN can answer
/// before the source is open), the open's result, and whether OPEN has already answered.
/// ja: OPEN と session の間の受け渡し(先につないだ socket、開いた結果、OPEN が返事をしたか)。
#[derive(Default)]
struct OpenSlot {
    state: Mutex<OpenState>,
    changed: Condvar,
}

#[derive(Default)]
struct OpenState {
    sock: Option<TcpStream>,
    result: Option<Result<(), String>>,
    answered: bool,
}

/// What OPEN answers.
enum Opened {
    /// The source is open: OK.
    Ready(TcpStream),
    /// Still opening past [`OPEN_ANSWER_WITHIN`]: OK, and the session goes on.
    Still(TcpStream),
    /// The open failed in time: the error.
    Failed(String),
}

impl OpenSlot {
    fn lock(&self) -> std::sync::MutexGuard<'_, OpenState> {
        match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// The session's socket, once connected.
    fn connected(&self, sock: TcpStream) {
        self.lock().sock = Some(sock);
        self.changed.notify_all();
    }

    /// The open's result; true when OPEN has already answered (the session then reports a
    /// failure itself). Only the first result counts.
    fn finish(&self, r: Result<(), String>) -> bool {
        let mut g = self.lock();
        if g.answered {
            return true;
        }
        if g.result.is_none() {
            g.result = Some(r);
        }
        drop(g);
        self.changed.notify_all();
        false
    }

    /// OPEN's side: the result within `within`, else (once the socket is there) answer anyway.
    fn wait(&self, within: Duration) -> Opened {
        let deadline = std::time::Instant::now() + within;
        let mut g = self.lock();
        loop {
            match g.result.take() {
                Some(Ok(())) => {
                    if let Some(sock) = g.sock.take() {
                        g.answered = true;
                        return Opened::Ready(sock);
                    }
                    return Opened::Failed("the monitor's socket went missing".to_owned());
                }
                Some(Err(m)) => {
                    g.answered = true;
                    return Opened::Failed(m);
                }
                None => {}
            }
            let now = std::time::Instant::now();
            if now >= deadline
                && let Some(sock) = g.sock.take()
            {
                g.answered = true;
                return Opened::Still(sock);
            }
            let step = deadline
                .saturating_duration_since(now)
                .max(Duration::from_millis(100));
            g = match self.changed.wait_timeout(g, step) {
                Ok((g, _)) => g,
                Err(p) => p.into_inner().0,
            };
        }
    }
}

fn session(client: &str, address: &str, shared: &Shared, env: &Env, slot: &OpenSlot) {
    mlog(format_args!("session: opening {address}"));
    let fail = |m: String| {
        mlog(format_args!("session: open failed: {m}"));
        slot.finish(Err(m))
    };
    // en: Connected before anything else, so OPEN can answer in time however long the rest takes
    // (arduino-cli listens before it sends OPEN and accepts after the answer): resolving the
    // address enumerates USB, which waits on any device the kernel is still probing - a CH32H417
    // sketch whose HID interface timed out held it 20 s right after an upload (2026-11-14).
    // ja: 何より先につなぐ(残りがどれだけかかっても OPEN が時間内に返事をできるように)。アドレスの
    // 解決は USB を列挙し、kernel が probe 中の機器を待つ(HID が時間切れの H417 の sketch で 20 秒)。
    let mut sock = match TcpStream::connect(client) {
        Ok(s) => s,
        Err(e) => {
            fail(format!("connect {client}: {e}"));
            return;
        }
    };
    let (Ok(keep), Ok(reader)) = (sock.try_clone(), sock.try_clone()) else {
        fail("socket clone failed".to_owned());
        return;
    };
    slot.connected(keep);
    let resolved = resolve_address(address);
    mlog(format_args!(
        "session: address resolved: {}",
        resolved.is_ok()
    ));
    let resolved = match resolved {
        Ok(r) => r,
        Err(m) => {
            if fail(m.clone()) {
                lost(&mut sock, &m);
            }
            return;
        }
    };
    let mut backend = match Backend::open(&resolved, shared.settings(), env) {
        Ok(b) => b,
        Err(m) => {
            if fail(m.clone()) {
                lost(&mut sock, &m);
            }
            return;
        }
    };
    slot.finish(Ok(()));
    mlog(format_args!("session: open"));
    if shared.stop.load(Ordering::SeqCst) {
        return;
    }
    let input = source::spawn_reader(reader);
    let wch = resolved.entry.is_some();
    say_reclock(&mut sock, shared.settings().source, wch);
    loop {
        let leave = backend.run(shared, &mut sock, &input);
        mlog(format_args!("session: backend left: {leave:?}"));
        match leave {
            Leave::Stop => return,
            Leave::Switch => {
                drop(backend);
                let next = shared.settings();
                match Backend::open(&resolved, next.clone(), env) {
                    Ok(b) => {
                        backend = b;
                        say_reclock(&mut sock, next.source, wch);
                    }
                    Err(m) => lost(&mut sock, &m),
                }
            }
            Leave::Failed(m) => {
                drop(backend);
                lost(&mut sock, &m);
            }
        }
    }
}

/// en: Every source but `uart` goes through a WCH-Link attach, which may re-clock the target and
/// leave it that way (docs/cli.ja.md §4.5, `attach-reclocks-target`): say so in the monitor
/// itself, since that is the only place an IDE user reads, and on every target, since which
/// families it happens to is not fully known.
/// ja: `uart` 以外の source は WCH-Link の attach を通り、target のクロックを組み替えたままにする
/// 可能性がある。IDE の利用者が読むのはモニタだけなので、そこに全 target で「可能性」として出す。
fn say_reclock(sock: &mut TcpStream, source: MonitorSource, wch_link: bool) {
    // Only a WCH-Link's attach touches the clock; an OEP probe's does not.
    // `auto` resets the target after its attach, which undoes it.
    if matches!(source, MonitorSource::Uart | MonitorSource::Auto) || !wch_link {
        return;
    }
    let _ = write!(
        sock,
        "[ch32rv monitor] attaching may have changed the target clock (UART baud rate, millis, \
         timers); reset the board to run at its own clock\r\n"
    );
    let _ = sock.flush();
}

/// Tell the IDE why the monitor stops, then end the process.
fn lost(sock: &mut TcpStream, why: &str) -> ! {
    mlog(format_args!("session: stopped, process exits: {why}"));
    let _ = write!(sock, "\r\n[ch32rv monitor] stopped: {why}\r\n");
    let _ = sock.flush();
    let _ = sock.shutdown(Shutdown::Both);
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn open_answers_in_time_however_long_the_source_takes() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let sock = || TcpStream::connect(addr).unwrap();
        // Opened in time: OK with the socket.
        let slot = OpenSlot::default();
        slot.connected(sock());
        assert!(!slot.finish(Ok(())));
        assert!(matches!(
            slot.wait(Duration::from_millis(50)),
            Opened::Ready(_)
        ));
        // Failed in time: the error, whether or not the socket was connected.
        let slot = OpenSlot::default();
        assert!(!slot.finish(Err("no probe".to_owned())));
        assert!(
            matches!(slot.wait(Duration::from_millis(50)), Opened::Failed(m) if m == "no probe")
        );
        // Still opening: OK once the socket is there, and a later failure is the session's to
        // report (finish says OPEN has answered).
        let slot = OpenSlot::default();
        slot.connected(sock());
        assert!(matches!(
            slot.wait(Duration::from_millis(50)),
            Opened::Still(_)
        ));
        assert!(slot.finish(Err("attach failed".to_owned())));
    }

    #[test]
    fn chip_and_speed_come_from_the_command_line_only() {
        let mut s = Settings::new("oep", Some("CH32V20x".to_owned()), "high".to_owned());
        let d = s.describe("oep");
        // Not offered, so IDE 2 does not save them...
        assert!(d["configuration_parameters"].get("speed").is_none());
        assert!(d["configuration_parameters"].get("chip").is_none());
        // ...and a file that saved them still opens: taken, ignored.
        assert!(s.set("speed", "low").is_ok());
        assert!(s.set("chip", "CH32V305RBT6").is_ok());
        assert_eq!(s.speed, "high");
        assert_eq!(s.chip.as_deref(), Some("CH32V20x"));
        let s = Settings::new("wchlink", None, "4000".to_owned());
        // A `wchlink://` port reads everything the target talks on unless told otherwise.
        assert_eq!(s.source, MonitorSource::Auto);
        assert_eq!(
            s.describe("wchlink")["configuration_parameters"]["source"]["value"],
            json!(["auto", "uart", "dmdata", "dmseq", "sdi", "rtt"])
        );
        assert_eq!(s.speed, "4000");
    }

    #[test]
    fn sync_mode_says_what_came_went_and_changed() {
        let a = json!({"address": "wchlink://A", "protocol": "wchlink", "label": "A"});
        let b = json!({"address": "ch32rv-hid://1-4", "protocol": "ch32rv-hid", "label": "HID"});
        let a2 = json!({"address": "wchlink://A", "protocol": "wchlink", "label": "A (changed)"});
        let one = std::slice::from_ref(&a);
        assert!(sync_events(one, one).is_empty());
        // A bootloader brought up after the start is added; gone, it is removed.
        let e = sync_events(one, &[a.clone(), b.clone()]);
        assert_eq!(e, vec![json!({"eventType": "add", "port": b})]);
        let e = sync_events(&[a.clone(), b.clone()], one);
        assert_eq!(e[0]["eventType"], "remove");
        assert_eq!(e[0]["port"]["address"], "ch32rv-hid://1-4");
        // A changed description: removed, then added again.
        let e = sync_events(one, std::slice::from_ref(&a2));
        assert_eq!(e[0]["eventType"], "remove");
        assert_eq!(e[1], json!({"eventType": "add", "port": a2}));
    }

    #[test]
    fn a_recent_listing_stands_in_only_for_its_own_probe() {
        let unit = format!("t{:x}", std::process::id());
        let mine = json!({"address": format!("oep://{unit}/l103")});
        let other = json!({"address": "oep://ffffffffffff/l103"});
        let bad = json!({"address": format!("oep://{unit}/My Port")});
        let read = vec![mine.clone(), other, bad];
        assert_eq!(listing_or_recent(Some(read.clone()), &unit), read);
        assert_eq!(listing_or_recent(None, &unit), vec![mine]);
        let _ = std::fs::remove_file(crate::broker::listing_cache(&format!("oep-{unit}")));
    }
}
