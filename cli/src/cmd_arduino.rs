//! en: `arduino discovery` / `arduino monitor` (docs/cli.ja.md §4.11): the Arduino Pluggable
//! Discovery and Monitor protocols (line-based stdio JSON). Discovery exposes each WCH probe as a
//! `wchlink://<serial>` port; Monitor wraps the DMI sources (dmdata / rtt) as the IDE's Serial
//! Monitor, both ways (what the user types reaches the sketch). uart / sdi need no wrapper: the
//! IDE opens the probe's CDC port with its builtin serial monitor. Machine-facing: never mixes
//! human text onto stdout.
//! ja: `arduino discovery`/`monitor`。Arduino の Pluggable Discovery/Monitor プロトコル(行単位の
//! stdio JSON)。discovery は各 WCH probe を `wchlink://<serial>` port として公開、monitor は DMI
//! source(dmdata / rtt)を IDE の Serial Monitor として双方向に wrap する。uart / sdi は IDE の
//! builtin serial monitor が CDC を直接開くので wrap 不要。

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::args::Cli;

/// `arduino discovery`: the Pluggable Discovery protocol over stdio.
pub fn discovery(_cli: &Cli) -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let word = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        match word.as_str() {
            "" => {}
            "HELLO" => emit(
                &mut out,
                &json!({"eventType":"hello","protocolVersion":1,"message":"OK"}),
            ),
            "START" => emit(&mut out, &json!({"eventType":"start","message":"OK"})),
            "STOP" => emit(&mut out, &json!({"eventType":"stop","message":"OK"})),
            "LIST" => emit(&mut out, &json!({"eventType":"list","ports": list_ports()})),
            "START_SYNC" => {
                // We do not watch USB hotplug yet: acknowledge, emit the current ports once as
                // `add` events, then rely on the IDE's periodic re-LIST for changes.
                emit(&mut out, &json!({"eventType":"start_sync","message":"OK"}));
                for p in list_ports() {
                    emit(&mut out, &json!({"eventType":"add","port": p}));
                }
            }
            "QUIT" => {
                emit(&mut out, &json!({"eventType":"quit","message":"OK"}));
                return ExitCode::SUCCESS;
            }
            other => emit(
                &mut out,
                &json!({"eventType": other.to_ascii_lowercase(), "error": true, "message": format!("unknown command {other}")}),
            ),
        }
    }
    ExitCode::SUCCESS
}

/// Enumerate WCH probes as Pluggable-Discovery ports (from USB descriptors only - no AttachChip,
/// so it never disturbs an in-flight upload/monitor on the same probe).
fn list_ports() -> Vec<Value> {
    let mut ports = wchlink_ports();
    ports.extend(hid_ports());
    ports
}

/// WCH-Links as `wchlink://<serial>`.
fn wchlink_ports() -> Vec<Value> {
    let entries = crate::cmd_probe::wch_devices().unwrap_or_default();
    entries
        .iter()
        .map(|e| {
            let serial = e.dev.serial().unwrap_or("unknown");
            json!({
                "address": format!("wchlink://{serial}"),
                "label": format!("WCH-Link {serial}"),
                "protocol": "wchlink",
                "protocolLabel": "WCH-Link (RISC-V debug)",
                "hardwareId": serial,
                "properties": {
                    "serial": serial,
                    "vid": format!("0x{:04x}", e.dev.vid()),
                    "pid": format!("0x{:04x}", e.dev.pid()),
                    "mode": crate::cmd_probe::mode_str(e.mode),
                },
            })
        })
        .collect()
}

/// en: rv003usb / UIAPduino HID bootloaders (`1209:b803`, `1209:b003`) as `hid://<topology>`: a
/// bootloader has no serial number, and the position is what `boot hid flash --probe
/// port:hid://…` finds it by again. `vid` / `pid` let boards.txt's `upload_port.N.vid/pid` name
/// the board.
/// ja: rv003usb / UIAPduino の HID bootloader を `hid://<topology>` で出す(serial 番号が無く、
/// `boot hid flash --probe port:hid://…` も位置で引く)。`vid` / `pid` で boards.txt が板名を出す。
fn hid_ports() -> Vec<Value> {
    let devs = ch32rv_usb::enumerate().unwrap_or_default();
    devs.iter()
        .filter(|d| ch32rv_boot::DEFAULT_HID_IDS.contains(&(d.vid(), d.pid())))
        .map(|d| {
            let topology = d.topology();
            json!({
                "address": format!("hid://{topology}"),
                "label": format!("HID bootloader {topology}"),
                "protocol": "hid",
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

fn emit(out: &mut impl Write, v: &Value) {
    // One compact JSON object per line; flush so the IDE sees it immediately.
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

// ---- arduino monitor (Pluggable Monitor protocol) ----

use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ch32rv_contract::policy::MonitorSource;
use ch32rv_usb::DeviceLock;
use serialport::SerialPort;

use crate::cmd_probe::Entry;
use crate::session::Session;
use crate::source::{self, DmiSource};

/// en: The sources a serial port offers, in the order the IDE lists them; the first (`uart`,
/// the port as it is) is the default. A plain serial port (no WCH probe behind it) can only do
/// `uart`, which is what the IDE would do anyway.
/// ja: serial port の source。先頭(`uart` = port をそのまま)が既定。WCH probe の無い素の serial
/// port は `uart` だけ。
const SERIAL_SOURCES: [MonitorSource; 5] = [
    MonitorSource::Uart,
    MonitorSource::Sdi,
    MonitorSource::Dmdata,
    MonitorSource::Dmseq,
    MonitorSource::Rtt,
];

/// en: The sources a `wchlink://` port offers (uart needs the user to pick the Link's CDC port,
/// where the baud rate and modem lines are theirs to set); `dmdata` is the default.
/// ja: `wchlink://` の source(uart は Link の CDC を選んで使う。baud と modem 線はそちらで)。既定は dmdata。
const WCHLINK_SOURCES: [MonitorSource; 4] = [
    MonitorSource::Dmdata,
    MonitorSource::Sdi,
    MonitorSource::Dmseq,
    MonitorSource::Rtt,
];

/// The `chip` value that means "no board family given: detect".
const CHIP_AUTO: &str = "auto";

/// The builtin serial-monitor's baud list, so a board's `monitor_port.serial.baudrate` means the
/// same here.
const BAUDS: [u32; 20] = [
    300, 600, 750, 1200, 2400, 4800, 9600, 19200, 31250, 38400, 57600, 74880, 115200, 230400,
    250000, 460800, 500000, 921600, 1_000_000, 2_000_000,
];

/// How long a backend waits for data before it looks at `stop` / the settings again.
const TICK: Duration = Duration::from_millis(50);

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
}

impl Settings {
    fn new(protocol: &str, chip: Option<String>) -> Self {
        let sources: &'static [MonitorSource] = if protocol == "wchlink" {
            &WCHLINK_SOURCES
        } else {
            &SERIAL_SOURCES
        };
        // Line settings default to the builtin serial-monitor's.
        Settings {
            sources,
            source: sources[0],
            baud: 9600,
            dtr: true,
            rts: true,
            chip,
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
            "chip" if value == CHIP_AUTO => self.chip = None,
            "chip" => {
                if !chip_names().iter().any(|n| n == value) {
                    return Err(format!("`{value}` is not a chip in ch32rv's DB"));
                }
                self.chip = Some(value.to_owned());
            }
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
                    "label": "Runtime output source", "type": "enum",
                    "value": self.sources.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                    "selected": self.source.as_str(),
                },
                "baudrate": {
                    "label": "Baudrate (uart)", "type": "enum",
                    "value": BAUDS.iter().map(u32::to_string).collect::<Vec<_>>(),
                    "selected": self.baud.to_string(),
                },
                "dtr": {
                    "label": "DTR (uart)", "type": "enum",
                    "value": ["on", "off"], "selected": on_off(self.dtr),
                },
                "rts": {
                    "label": "RTS (uart)", "type": "enum",
                    "value": ["on", "off"], "selected": on_off(self.rts),
                },
                "chip": {
                    "label": "Board chip", "type": "enum",
                    "value": std::iter::once(CHIP_AUTO.to_owned()).chain(chip_names()).collect::<Vec<_>>(),
                    "selected": self.chip.as_deref().unwrap_or(CHIP_AUTO),
                },
            }
        })
    }
}

/// en: Every name `chip` accepts: the DB's families and SKUs, spelled as `--chip` takes them
/// (the core's `build.ch32rv_chip`). The IDE only passes values listed here.
/// ja: `chip` が受ける名前 = DB の family と SKU(`--chip` / core の `build.ch32rv_chip` と同じ綴り)。
fn chip_names() -> Vec<String> {
    let db = ch32rv_target::Db::builtin();
    let mut names: Vec<String> = db
        .skus()
        .iter()
        .flat_map(|s| [s.family.clone(), s.sku.clone()])
        .collect();
    names.sort();
    names.dedup();
    names
}

/// What the backends need from the command line (the `Cli` itself stays on the main thread).
#[derive(Clone)]
struct Env {
    speed: String,
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
        speed: cli.speed.clone(),
        lock_timeout: Duration::from_secs(cli.lock_timeout),
    };
    let shared = Arc::new(Shared {
        settings: Mutex::new(Settings::new(protocol, cli.chip.clone())),
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
        let parts: Vec<&str> = line.split_whitespace().collect();
        let word = parts.first().copied().unwrap_or("").to_ascii_uppercase();
        match word.as_str() {
            "" => {}
            "HELLO" => emit(
                &mut out,
                &json!({"eventType":"hello","protocolVersion":1,"message":"OK"}),
            ),
            "DESCRIBE" => emit(
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
                    Ok(()) => emit(&mut out, &json!({"eventType":"configure","message":"OK"})),
                    Err(m) => emit(
                        &mut out,
                        &json!({"eventType":"configure","error":true,"message":m}),
                    ),
                }
            }
            "OPEN" => {
                // OPEN <client-host:port> <port-address>
                let (Some(&client), Some(&address)) = (parts.get(1), parts.get(2)) else {
                    emit(
                        &mut out,
                        &json!({"eventType":"open","error":true,"message":"OPEN needs <host:port> <port>"}),
                    );
                    continue;
                };
                if running.is_some() {
                    emit(
                        &mut out,
                        &json!({"eventType":"open","error":true,"message":"already open"}),
                    );
                    continue;
                }
                shared.stop.store(false, Ordering::SeqCst);
                let (tx, rx) = mpsc::channel();
                let (client, address) = (client.to_owned(), address.to_owned());
                let (shared2, env2) = (shared.clone(), env.clone());
                let handle =
                    std::thread::spawn(move || session(&client, &address, &shared2, &env2, tx));
                match rx.recv() {
                    Ok(Ok(sock)) => {
                        running = Some((handle, sock));
                        emit(&mut out, &json!({"eventType":"open","message":"OK"}));
                    }
                    Ok(Err(m)) => {
                        let _ = handle.join();
                        emit(
                            &mut out,
                            &json!({"eventType":"open","error":true,"message":m}),
                        );
                    }
                    Err(_) => {
                        let _ = handle.join();
                        emit(
                            &mut out,
                            &json!({"eventType":"open","error":true,"message":"monitor session ended before it opened"}),
                        );
                    }
                }
            }
            "CLOSE" => {
                close(&mut running);
                emit(&mut out, &json!({"eventType":"close","message":"OK"}));
            }
            "QUIT" => {
                close(&mut running);
                emit(&mut out, &json!({"eventType":"quit","message":"OK"}));
                return ExitCode::SUCCESS;
            }
            other => emit(
                &mut out,
                &json!({
                "eventType": other.to_ascii_lowercase(),"error":true,
                "message":format!("unknown command {other}")}),
            ),
        }
    }
    // stdin closed: arduino-cli is gone (or going). Never outlive it.
    close(&mut running);
    ExitCode::SUCCESS
}

/// A resolved port address: the WCH probe behind it (if any) and the serial port to use for
/// `uart` / `sdi` (if any).
struct Resolved {
    entry: Option<Entry>,
    port: Option<String>,
}

/// en: `wchlink://<serial>` names a probe (its first CDC serves uart / sdi). Anything else is a
/// serial port path: the WCH probe that owns it, if one does (found through `probe list`'s
/// `ports`, so a Link without a USB serial number is found by position), else a plain serial
/// port, which only `uart` can use.
/// ja: `wchlink://<serial>` は probe(uart / sdi は最初の CDC)。それ以外は serial port の path で、
/// それを持つ WCH probe があればそれ(`probe list` の `ports` で引くので serial 番号の無い Link も
/// 位置で見つかる)、無ければ素の serial port(uart だけが使える)。
fn resolve_address(address: &str) -> Result<Resolved, String> {
    let entries = crate::cmd_probe::wch_devices().map_err(|e| format!("USB enumeration: {e}"))?;
    if let Some(serial) = address.strip_prefix("wchlink://") {
        let entry = entries
            .into_iter()
            .find(|e| e.dev.serial() == Some(serial))
            .ok_or_else(|| format!("no WCH-Link with serial {serial} is connected"))?;
        let port = entry.dev.serial_ports().into_iter().next();
        return Ok(Resolved {
            entry: Some(entry),
            port,
        });
    }
    let sel = ch32rv_usb::Selector::Port(address.to_owned());
    let mut owners: Vec<Entry> = entries
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
    Ok(Resolved {
        entry: owners.pop(),
        port: Some(address.to_owned()),
    })
}

/// One open source. Each holds what keeps it alive (the probe lock, the attach) until dropped.
enum Backend {
    Uart {
        port: Box<dyn SerialPort>,
        applied: Settings,
    },
    Sdi {
        rx: Receiver<std::io::Result<Vec<u8>>>,
        _lock: DeviceLock,
    },
    Dmi {
        src: Box<DmiSource>,
        session: Box<Session>,
    },
}

/// Why a backend returned.
enum Leave {
    /// CLOSE / QUIT / stdin EOF.
    Stop,
    /// `source` changed while open: set up the new one on the same socket.
    Switch,
    /// The session is lost; the string is the reason for the user.
    Failed(String),
}

fn lock_for(entry: &Entry, env: &Env) -> Result<DeviceLock, String> {
    let key = entry
        .dev
        .serial()
        .map(str::to_owned)
        .unwrap_or_else(|| entry.dev.topology());
    DeviceLock::acquire(&key, env.lock_timeout)
        .map_err(|e| format!("{e} (another ch32rv is using this probe)"))
}

impl Backend {
    fn open(r: &Resolved, s: Settings, env: &Env) -> Result<Self, String> {
        match s.source {
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
            MonitorSource::Sdi => {
                let entry = r
                    .entry
                    .as_ref()
                    .ok_or("sdi needs a WCH-LinkE (this serial port is not one)")?;
                let path = r
                    .port
                    .as_deref()
                    .ok_or("this probe has no serial port for sdi")?;
                let lock = lock_for(entry, env)?;
                let (speed, _) = crate::parse::speed(&env.speed)?;
                crate::cmd_monitor::enable_sdi(entry, speed, s.chip.as_deref())
                    .map_err(|e| e.msg)?;
                Ok(Backend::Sdi {
                    rx: raw_reader(path)?,
                    _lock: lock,
                })
            }
            MonitorSource::Dmdata | MonitorSource::Dmseq | MonitorSource::Rtt => {
                let entry = r.entry.as_ref().ok_or_else(|| {
                    format!(
                        "{} needs a WCH-Link (this serial port is not one)",
                        s.source.as_str()
                    )
                })?;
                let (speed, _) = crate::parse::speed(&env.speed)?;
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
                .map_err(|e| format!("attach: {e}"))?;
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

    /// Pump until stopped, switched or failed.
    fn run(&mut self, shared: &Shared, sock: &mut TcpStream, input: &Receiver<Vec<u8>>) -> Leave {
        let mut pending = Vec::new();
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
                Backend::Sdi { rx, .. } => {
                    if now.source != MonitorSource::Sdi {
                        return Leave::Switch;
                    }
                    // SDI carries no input.
                    pending.clear();
                    match rx.recv_timeout(TICK) {
                        Ok(Ok(d)) if !d.is_empty() => Ok(d),
                        Err(RecvTimeoutError::Timeout) => Ok(Vec::new()),
                        Ok(Err(e)) => Err(format!("serial port read failed: {e}")),
                        Ok(Ok(_)) | Err(RecvTimeoutError::Disconnected) => {
                            Err("the serial port closed (probe disconnected?)".to_owned())
                        }
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

/// en: Read `path` as a plain blocking file, exactly like `cat`, on a thread of its own: opening
/// it through the serialport crate asserts DTR, and O_NONBLOCK too makes the WCH-LinkE stop
/// forwarding SDI after one line (measured, see `cmd_monitor::stream_port`). On CLOSE the thread
/// stays blocked in its read and ends at the next chunk or with the process.
/// ja: `path` を cat と同じ素のブロッキング file として別スレッドで読む(serialport は DTR を立て、
/// O_NONBLOCK も LinkE の SDI 転送を 1 行で止める実測)。CLOSE 後のスレッドは read で待ったまま、
/// 次の chunk かプロセスの終わりで消える。
fn raw_reader(path: &str) -> Result<Receiver<std::io::Result<Vec<u8>>>, String> {
    use std::io::Read;
    let mut file = crate::cmd_monitor::open_raw(path).map_err(|e| format!("open {path}: {e}"))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 512];
        loop {
            let r = file.read(&mut chunk).map(|n| chunk[..n].to_vec());
            let last = !matches!(&r, Ok(v) if !v.is_empty());
            if tx.send(r).is_err() || last {
                break;
            }
        }
    });
    Ok(rx)
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
fn session(
    client: &str,
    address: &str,
    shared: &Shared,
    env: &Env,
    ready: mpsc::Sender<Result<TcpStream, String>>,
) {
    let setup = || -> Result<(Resolved, Backend, TcpStream), String> {
        let r = resolve_address(address)?;
        let backend = Backend::open(&r, shared.settings(), env)?;
        let sock = TcpStream::connect(client).map_err(|e| format!("connect {client}: {e}"))?;
        Ok((r, backend, sock))
    };
    let (resolved, mut backend, mut sock) = match setup() {
        Ok(v) => v,
        Err(m) => {
            let _ = ready.send(Err(m));
            return;
        }
    };
    let (Ok(keep), Ok(reader)) = (sock.try_clone(), sock.try_clone()) else {
        let _ = ready.send(Err("socket clone failed".to_owned()));
        return;
    };
    if ready.send(Ok(keep)).is_err() {
        return;
    }
    let input = source::spawn_reader(reader);
    say_reclock(&mut sock, shared.settings().source);
    loop {
        match backend.run(shared, &mut sock, &input) {
            Leave::Stop => return,
            Leave::Switch => {
                drop(backend);
                let next = shared.settings();
                match Backend::open(&resolved, next.clone(), env) {
                    Ok(b) => {
                        backend = b;
                        say_reclock(&mut sock, next.source);
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
fn say_reclock(sock: &mut TcpStream, source: MonitorSource) {
    if source == MonitorSource::Uart {
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
    let _ = write!(sock, "\r\n[ch32rv monitor] stopped: {why}\r\n");
    let _ = sock.flush();
    let _ = sock.shutdown(Shutdown::Both);
    std::process::exit(0)
}
