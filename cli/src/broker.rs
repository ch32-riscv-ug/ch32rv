//! en: The per-probe broker (docs/oep-host.ja.md §7.2, ArduinoCore-CH32RV oep-workflow §7.2). One
//! detached process per probe, parented by no one, holds the probe's transport and its one OEP
//! session; every ch32rv command that uses the probe (flash, monitor, gdb, one-shot commands) and
//! pytest's `oep_host` is a client of it over 127.0.0.1 TCP, speaking OEP (`length(u16)
//! message`). The broker renumbers each client's corrs onto the probe's pipeline, answers the
//! session requests itself (confirm / open / end / keepalive / lock_state), keeps per-client
//! resources in a ledger (connections, plans) and releases a client's share when it leaves, and
//! ends as soon as its last client has gone. A client starts it when it is not running; a flock
//! keeps the start race to one broker.
//!
//! ja: probe ごとのブローカー。誰の子でもない 1 つのプロセスが probe の transport と OEP の session を
//! 持ち、probe を使う ch32rv のコマンドと pytest の `oep_host` はすべて 127.0.0.1 の TCP で OEP を話す
//! client になる。ブローカーは client の corr を付け替えて probe の pipeline に乗せ、session の要求は
//! 自分で答え、client ごとの資源(接続・plan)を台帳に持って、抜けた client の分を外し、最後の client
//! が抜けたらすぐ終わる。無ければ client が起動し、起動の取り合いは flock で 1 つにする。

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ch32rv_contract::{ErrorKind, ResultEnvelope};
use ch32rv_oep::codec::{
    LengthDeframer, Request, Resolution, encode_result, length_frame, parse_tlvs,
};
use ch32rv_oep::link::{Call, Link, Reply};
use ch32rv_oep::registry::{self, core as oep_core, outcomes, reject_reasons, wire_rvswd};
use ch32rv_oep::session::Probe;
use serde_json::{Value, json};

use crate::args::Cli;
use crate::cmd_probe::fail;
use crate::oep::OepAddr;

/// A broker that has had no client this long after starting gives up (its starter died).
const FIRST_CLIENT_WAIT: Duration = Duration::from_secs(10);
/// How long a client waits for a broker it started to publish its endpoint.
const START_WAIT: Duration = Duration::from_secs(5);
/// How often a client that finds no endpoint starts a broker again while it waits.
const RESPAWN_EVERY: Duration = Duration::from_millis(400);
/// The broker's own lease on the probe, renewed by keepalive while no client talks.
const LEASE_MS: u32 = 3000;
const KEEPALIVE_EVERY: Duration = Duration::from_millis(1000);

// ---- where a broker is ----

/// en: The key a probe's broker files are named by: the probe's identity, so every way to reach
/// one probe (`oep://`, `port:` on either of its CDC ports) meets the same broker
/// (docs/freeze-decisions.ja.md §4). An OEP USB device's serial is its unit id and is read
/// without opening it; a serial port with no OEP USB device behind it (a UART bridge) is known
/// only by its path, since the probe cannot be opened to learn more (the broker holds it).
/// ja: ブローカーの file の key は probe の同一性。同じ probe へのどの道(`oep://`、どちらの CDC の
/// `port:`)も同じブローカーに着く。OEP の USB device の serial は unit id で、開かずに読める。OEP の
/// USB device を持たない serial port(UART bridge)は path でしか分からない。
fn key_for(path: &str) -> String {
    let sel = ch32rv_usb::Selector::Port(path.to_owned());
    let dev = crate::oep::oep_devices()
        .into_iter()
        .enumerate()
        .find(|(i, d)| sel.matches(d, *i))
        .map(|(_, d)| d);
    let key = match dev.and_then(|d| d.port_id()) {
        Some(id) => format!("oep-{id}"),
        None => format!("oep-port-{}", ch32rv_usb::normalize_port(path)),
    };
    ch32rv_usb::sanitize_key(&key)
}

fn endpoint_file(key: &str) -> PathBuf {
    ch32rv_usb::runtime_dir().join(format!("{key}.oep"))
}

/// The broker's log file for runtime key `key`.
pub(crate) fn log_path(key: &str) -> PathBuf {
    ch32rv_usb::runtime_dir().join(format!("{key}.broker.log"))
}

/// en: A line in the broker's log (`<runtime>/<key>.broker.log`): its stderr goes nowhere, and
/// how it ended is what a client that saw its connection close needs. Kept under 256 KiB.
/// ja: ブローカーの log に 1 行(stderr はどこにも出ないので、終わり方はここで分かる)。256 KiB まで。
pub(crate) fn broker_log(key: &str, msg: &str) {
    use std::io::Write as _;
    let path = log_path(key);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{} pid {} {msg}", now_ms(), std::process::id());
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_endpoint(key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(endpoint_file(key)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the endpoint file atomically (0600 on unix).
fn write_endpoint(key: &str, v: &Value) -> std::io::Result<()> {
    let dir = ch32rv_usb::runtime_dir();
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{key}.oep.{}", std::process::id()));
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(v.to_string().as_bytes())?;
    }
    std::fs::rename(&tmp, endpoint_file(key))
}

// ---- the client side ----

/// Which probe a broker serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrokerTarget {
    /// An OEP probe on this serial port.
    Serial(String),
    /// A WCH-Link, named by its USB serial (else its position).
    Wch { id: String, selector: String },
}

impl BrokerTarget {
    /// The WCH-Link behind `entry`.
    pub(crate) fn wch(entry: &crate::cmd_probe::Entry) -> Self {
        match entry.dev.serial().filter(|s| !s.is_empty()) {
            Some(sn) => BrokerTarget::Wch {
                id: sn.to_owned(),
                selector: format!("serial:{sn}"),
            },
            None => {
                let t = entry.dev.topology();
                BrokerTarget::Wch {
                    id: format!("usb-{t}"),
                    selector: format!("usb:{t}"),
                }
            }
        }
    }

    pub(crate) fn key(&self) -> String {
        match self {
            BrokerTarget::Serial(p) => key_for(p),
            BrokerTarget::Wch { id, .. } => ch32rv_usb::sanitize_key(&format!("wch-{id}")),
        }
    }

    fn selector(&self) -> String {
        match self {
            BrokerTarget::Serial(p) => format!("port:{p}"),
            BrokerTarget::Wch { selector, .. } => selector.clone(),
        }
    }
}

/// Start `ch32rv broker serve` for `target`, detached from this process and its group.
fn spawn_broker(target: &BrokerTarget) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["broker", "serve", "--probe", &target.selector()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        // A group of its own: arduino-cli's end (or a Ctrl-C to this command) does not reach it.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("start the broker: {e}"))
}

/// en: A link to the broker of `target`, starting the broker when none answers. An endpoint the
/// broker wrote before this call counts only if it answers; an error it wrote counts only if it
/// is newer than our start.
/// ja: `target` のブローカーへの link。答えるブローカーが無ければ起動する。
pub(crate) fn client_link_for(target: &BrokerTarget) -> Result<Link, String> {
    let key = target.key();
    // en: A connection counts only if the endpoint still names the same broker once connected: a
    // broker about to end takes its endpoint down first and then still accepts for a moment, so a
    // client that read the file just before would otherwise talk to a process on its way out.
    // ja: つないだ後も endpoint が同じブローカーを指すときだけ有効。終わりかけのブローカーは先に
    // endpoint を下ろしてからも少し受け付けるので、その直前に読んだ client が消えるプロセスと話さないように。
    let try_connect = |v: &Value| -> Option<Link> {
        let port = v.get("port")?.as_u64()?;
        let pid = v.get("pid")?.as_u64()?;
        let l = ch32rv_oep::link::open_tcp(&format!("127.0.0.1:{port}")).ok()?;
        let still = read_endpoint(&key).and_then(|w| w.get("pid")?.as_u64()) == Some(pid);
        still.then_some(l)
    };
    if let Some(v) = read_endpoint(&key)
        && let Some(l) = try_connect(&v)
    {
        return Ok(l);
    }
    let t0 = now_ms();
    spawn_broker(target)?;
    let mut spawned = Instant::now();
    let deadline = Instant::now() + START_WAIT;
    while Instant::now() < deadline {
        if let Some(v) = read_endpoint(&key) {
            let fresh = v.get("time").and_then(Value::as_u64).unwrap_or(0) + 50 >= t0;
            if let Some(l) = try_connect(&v) {
                return Ok(l);
            }
            if fresh && let Some(e) = v.get("error").and_then(Value::as_str) {
                return Err(e.to_owned());
            }
        } else if spawned.elapsed() >= RESPAWN_EVERY {
            // The one we started may have found the old broker still holding the probe (its lock)
            // and left; start another now that the old one may be gone.
            spawn_broker(target)?;
            spawned = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(format!(
        "the broker for {} did not start within {} s",
        target.selector(),
        START_WAIT.as_secs()
    ))
}

/// The broker of the OEP probe on serial port `path` (started if needed).
pub(crate) fn client_link(path: &str) -> Result<Link, String> {
    client_link_for(&BrokerTarget::Serial(path.to_owned()))
}

/// A link to `target`'s broker if one runs; never starts one.
pub(crate) fn existing_link_for(target: &BrokerTarget) -> Option<Link> {
    let v = read_endpoint(&target.key())?;
    let port = v.get("port")?.as_u64()?;
    ch32rv_oep::link::open_tcp(&format!("127.0.0.1:{port}")).ok()
}

/// A link to the OEP probe's broker if one runs; never starts one (discovery only looks).
pub(crate) fn existing_link(path: &str) -> Option<Link> {
    existing_link_for(&BrokerTarget::Serial(path.to_owned()))
}

/// The runtime file discovery keeps a probe's last listing in (for when the probe is busy).
pub(crate) fn listing_cache(path: &str) -> PathBuf {
    ch32rv_usb::runtime_dir().join(format!("{}.slots.json", key_for(path)))
}

/// `broker endpoint --probe <sel> [--json]`: where the probe's broker listens, if it runs.
pub(crate) fn endpoint(cli: &Cli) -> ExitCode {
    const CMD: &str = "broker.endpoint";
    let target = match crate::oep::addr(cli, CMD) {
        Ok(Some(OepAddr::Serial(p) | OepAddr::Slot { path: p, .. })) => BrokerTarget::Serial(p),
        Ok(Some(OepAddr::Wch(t))) => t,
        Ok(Some(OepAddr::Tcp(_))) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                "a tcp: endpoint has no broker",
                None,
            );
        }
        Ok(None) => match crate::cmd_probe::select_entry(cli, CMD) {
            Ok(e) => BrokerTarget::wch(&e),
            Err(c) => return c,
        },
        Err(c) => return c,
    };
    let v = read_endpoint(&target.key());
    let live = v.as_ref().and_then(|v| {
        let port = v.get("port")?.as_u64()?;
        TcpStream::connect(("127.0.0.1", port as u16)).ok()?;
        Some(format!("127.0.0.1:{port}"))
    });
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(json!({
            "endpoint": live,
            "pid": live.as_ref().and(v.as_ref().and_then(|v| v.get("pid").cloned())),
            "transport": live.as_ref().and(v.as_ref().and_then(|v| v.get("transport").cloned())),
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "{}",
            live.as_deref()
                .unwrap_or("no broker is running for this probe")
        );
        ExitCode::SUCCESS
    }
}

// ---- the broker ----

enum Ev {
    Connected(u64, TcpStream),
    Msg(u64, Vec<u8>),
    Gone(u64),
}

/// Per-client resources.
#[derive(Default)]
struct Ledger {
    /// connection -> (its wire fn, the clients using it)
    conns: HashMap<u16, (u16, BTreeSet<u64>)>,
    /// client -> the fns it has plans on
    plans: HashMap<u64, BTreeSet<u16>>,
}

/// What to answer, in the client's order.
enum Pending {
    Local(u64, Vec<u8>),
    Forward(u64, Request),
}

/// `broker serve --probe port:<path>` (not for users; clients start it).
pub(crate) fn serve(cli: &Cli) -> ExitCode {
    const CMD: &str = "broker.serve";
    // An OEP probe's serial port, else a WCH-Link (any selector that resolves to one).
    let target = match crate::oep::addr(cli, CMD) {
        Ok(Some(OepAddr::Serial(p) | OepAddr::Slot { path: p, .. })) => BrokerTarget::Serial(p),
        // A broker for this Link already answers: this one leaves (the flock would say so too).
        Ok(Some(OepAddr::Wch(_))) => return ExitCode::SUCCESS,
        Ok(Some(OepAddr::Tcp(_))) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                "a tcp: endpoint needs no broker",
                None,
            );
        }
        Ok(None) => match crate::cmd_probe::select_entry(cli, CMD) {
            Ok(e) => {
                let t = BrokerTarget::wch(&e);
                return serve_target(cli, t, Some(e));
            }
            Err(c) => return c,
        },
        Err(c) => return c,
    };
    serve_target(cli, target, None)
}

fn serve_target(
    cli: &Cli,
    target: BrokerTarget,
    wch_entry: Option<crate::cmd_probe::Entry>,
) -> ExitCode {
    let key = target.key();
    // One broker per probe: a second one started in a race leaves quietly.
    let Ok(_guard) = ch32rv_usb::DeviceLock::acquire(&format!("{key}.broker"), Duration::ZERO)
    else {
        return ExitCode::SUCCESS;
    };
    let report_error = |m: String| {
        let _ = write_endpoint(
            &key,
            &json!({"error": m, "pid": std::process::id(), "time": now_ms()}),
        );
        ExitCode::from(ErrorKind::DeviceOpenFailed.exit_code())
    };
    let mut transport = "wchlink";
    let (up, sid) = match (&target, wch_entry) {
        (BrokerTarget::Serial(path), _) => {
            let mut probe = match crate::oep::connect_upstream(path, None) {
                Ok((p, t)) => {
                    transport = t;
                    p
                }
                Err(m) => return report_error(format!("{path}: {m}")),
            };
            let serial = crate::oep::single_serial(&mut probe);
            let owner = format!("ch32rv broker pid {}", std::process::id());
            let sid = match crate::oep::open_with_lock_rule(&mut probe, serial, &owner, LEASE_MS) {
                Ok(s) => s,
                Err(e) => return report_error(e.to_string()),
            };
            (Upstream::Oep(Box::new(probe)), sid)
        }
        (BrokerTarget::Wch { .. }, Some(entry)) => (
            Upstream::Wch(Box::new(crate::broker_wch::WchUpstream::new(
                entry,
                Duration::from_secs(cli.lock_timeout),
            ))),
            0,
        ),
        (BrokerTarget::Wch { .. }, None) => return report_error("no WCH-Link entry".to_owned()),
    };
    let mut up = up;
    let wires: BTreeSet<u16> = match up.wires() {
        Ok(w) => w,
        Err(e) => return report_error(e),
    };
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(l) => l,
        Err(e) => return report_error(format!("listen: {e}")),
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if write_endpoint(
        &key,
        &json!({"port": port, "pid": std::process::id(), "time": now_ms(), "transport": transport}),
    )
    .is_err()
    {
        return ExitCode::from(ErrorKind::DeviceOpenFailed.exit_code());
    }

    broker_log(&key, &format!("up on 127.0.0.1:{port} over {transport}"));
    let (tx, rx) = mpsc::channel::<Ev>();
    std::thread::spawn(move || accept_loop(listener, tx));

    let mut b = Broker {
        up,
        sid,
        wires,
        clients: HashMap::new(),
        ledger: Ledger::default(),
        key: key.clone(),
        endpoint: json!({"port": port, "pid": std::process::id(), "time": now_ms(), "transport": transport}),
    };
    let r = b.run(&rx);
    match &r {
        Ok(()) => broker_log(&key, "down: no client left"),
        Err(e) => broker_log(&key, &format!("down on an upstream error: {e}")),
    }
    // Remove the endpoint only if it is still ours, then end the probe session.
    if read_endpoint(&key).and_then(|v| v.get("pid")?.as_u64())
        == Some(u64::from(std::process::id()))
    {
        let _ = std::fs::remove_file(endpoint_file(&key));
    }
    b.up.end();
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(ErrorKind::TransferFailed.exit_code()),
    }
}

/// What the broker serves its clients from: an OEP probe's session, or a WCH-Link it maps OEP onto.
enum Upstream {
    Oep(Box<Probe>),
    Wch(Box<crate::broker_wch::WchUpstream>),
}

impl Upstream {
    fn limits(&self) -> ch32rv_oep::link::Limits {
        match self {
            Upstream::Oep(p) => p.limits(),
            Upstream::Wch(w) => w.limits(),
        }
    }

    fn boot_id(&self) -> u32 {
        match self {
            Upstream::Oep(p) => p.boot_id().unwrap_or(0),
            Upstream::Wch(w) => w.limits().boot_id,
        }
    }

    fn wires(&mut self) -> Result<BTreeSet<u16>, String> {
        match self {
            Upstream::Oep(p) => p
                .list("oep.wire")
                .map(|l| l.into_iter().map(|i| i.func).collect())
                .map_err(|e| e.to_string()),
            Upstream::Wch(w) => Ok(w.wires().into_iter().collect()),
        }
    }

    /// Serve `calls` (each tagged with the client it came from), in order.
    fn exchange(&mut self, calls: Vec<(u64, Call)>) -> Result<Vec<Reply>, String> {
        match self {
            Upstream::Oep(p) => p
                .link()
                .exchange(calls.into_iter().map(|(_, c)| c).collect())
                .map_err(|e| e.to_string()),
            Upstream::Wch(w) => Ok(calls.iter().map(|(id, c)| w.handle(*id, c)).collect()),
        }
    }

    /// The upstream link's resends and dropped frames so far (diagnostics for the log).
    fn losses(&mut self) -> (u64, u64) {
        match self {
            Upstream::Oep(p) => (p.link().resends, p.link().dropped),
            Upstream::Wch(_) => (0, 0),
        }
    }

    fn client_gone(&mut self, id: u64) {
        if let Upstream::Wch(w) = self {
            w.client_gone(id);
        }
    }

    /// Renew the lease; `Ok(true)` when it had lapsed and the session was opened again (the
    /// probe released everything it held, core §6.2).
    fn keepalive(&mut self) -> Result<bool, String> {
        match self {
            Upstream::Oep(p) => match p.keepalive() {
                Ok(()) => Ok(false),
                Err(ch32rv_oep::session::OepError::Expired) => self.reopen().map(|()| true),
                Err(e) => Err(e.to_string()),
            },
            Upstream::Wch(_) => Ok(false),
        }
    }

    /// en: Open the probe again under the same session id after the lease lapsed (it answers
    /// `resumed` 2: nothing of the old session is left). ja: lease 切れの後、同じ session id で開き直す。
    fn reopen(&mut self) -> Result<(), String> {
        let Upstream::Oep(p) = self else {
            return Ok(());
        };
        let sid = p
            .session_id()
            .unwrap_or_else(ch32rv_oep::session::random_session_id);
        let owner = format!("ch32rv broker pid {}", std::process::id());
        p.open(sid, LEASE_MS, false, Some(&owner))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Work between requests (a WCH-Link's consoles are polled here); how soon to come back.
    fn tick(&mut self) -> Duration {
        match self {
            Upstream::Oep(_) => Duration::from_millis(250),
            Upstream::Wch(w) => w.tick(),
        }
    }

    fn end(&mut self) {
        if let Upstream::Oep(p) = self {
            let _ = p.end();
        }
    }
}

fn accept_loop(listener: TcpListener, tx: mpsc::Sender<Ev>) {
    for (id, stream) in listener.incoming().flatten().enumerate() {
        let id = id as u64 + 1;
        let _ = stream.set_nodelay(true);
        let Ok(reader) = stream.try_clone() else {
            continue;
        };
        if tx.send(Ev::Connected(id, stream)).is_err() {
            return;
        }
        let tx = tx.clone();
        std::thread::spawn(move || read_loop(id, reader, tx));
    }
}

fn read_loop(id: u64, mut s: TcpStream, tx: mpsc::Sender<Ev>) {
    let mut d = LengthDeframer::new(0xFFFF);
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => match d.push(&buf[..n]) {
                Ok(msgs) => {
                    for m in msgs {
                        if tx.send(Ev::Msg(id, m)).is_err() {
                            return;
                        }
                    }
                }
                // A client that breaks the framing is dropped (it is a local program).
                Err(_) => break,
            },
        }
    }
    let _ = tx.send(Ev::Gone(id));
}

struct Broker {
    up: Upstream,
    sid: u32,
    wires: BTreeSet<u16>,
    clients: HashMap<u64, TcpStream>,
    ledger: Ledger,
    /// The runtime key and what the endpoint file says, to take it down and put it back.
    key: String,
    endpoint: Value,
}

/// How long a broker about to end still takes a new connection (after its endpoint is gone).
const LEAVE_GRACE: Duration = Duration::from_millis(150);

impl Broker {
    fn run(&mut self, rx: &mpsc::Receiver<Ev>) -> Result<(), String> {
        let started = Instant::now();
        let mut had_client = false;
        let mut last_upstream = Instant::now();
        let mut carried: Option<Ev> = None;
        loop {
            let wait = self.up.tick();
            let got = match carried.take() {
                Some(ev) => Ok(ev),
                None => rx.recv_timeout(wait),
            };
            let ev = match got {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => {
                    if self.clients.is_empty()
                        && (had_client || started.elapsed() > FIRST_CLIENT_WAIT)
                    {
                        match self.leave(rx) {
                            Some(ev) => carried = Some(ev),
                            None => return Ok(()),
                        }
                        continue;
                    }
                    if last_upstream.elapsed() >= KEEPALIVE_EVERY {
                        if self.up.keepalive()? {
                            self.swept();
                        }
                        last_upstream = Instant::now();
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            // Gather what is already queued, keeping the order, and serve it as one batch.
            let mut batch = vec![ev];
            while let Ok(ev) = rx.try_recv() {
                batch.push(ev);
            }
            let mut msgs = Vec::new();
            for ev in batch {
                match ev {
                    Ev::Connected(id, s) => {
                        broker_log(&self.key, &format!("client {id} connected"));
                        self.clients.insert(id, s);
                    }
                    // A client counts once it has asked something: a connection that only checks
                    // the broker is alive (`broker endpoint`) and leaves must not end it.
                    Ev::Msg(id, m) => {
                        had_client = true;
                        msgs.push((id, m));
                    }
                    Ev::Gone(id) => {
                        // Serve what it sent before it left, then release its share.
                        self.serve_batch(std::mem::take(&mut msgs))?;
                        // en: A failed release (a detach the probe did not answer) must not end
                        // the broker under the clients still here; the probe drops what it holds
                        // when the broker's session ends anyway.
                        // ja: release の失敗で、残っている client ごとブローカーを終わらせない。
                        if let Err(e) = self.release(id) {
                            broker_log(&self.key, &format!("client {id}: release failed: {e}"));
                        }
                        self.up.client_gone(id);
                        self.clients.remove(&id);
                        broker_log(&self.key, &format!("client {id} gone"));
                    }
                }
            }
            if !msgs.is_empty() {
                self.serve_batch(msgs)?;
                last_upstream = Instant::now();
            }
            if had_client && self.clients.is_empty() {
                match self.leave(rx) {
                    Some(ev) => carried = Some(ev),
                    None => return Ok(()),
                }
            }
        }
    }

    /// en: About to end with no client: take the endpoint down first, so no new client finds this
    /// broker, then wait [`LEAVE_GRACE`] for a connection that read it just before. One that comes
    /// cancels the end (the endpoint goes back up) and is handed back to the loop; `None` means
    /// end. A client that still reaches the listener after this sees the endpoint gone and
    /// starts a new broker ([`client_link_for`]).
    /// ja: client 0 で終わる前に、まず endpoint を下ろし(新しい client に見つからないように)、直前に
    /// それを読んだ接続を [`LEAVE_GRACE`] だけ待つ。来たら終わるのをやめて endpoint を戻し、その event を
    /// loop に返す。`None` なら終わる。
    fn leave(&mut self, rx: &mpsc::Receiver<Ev>) -> Option<Ev> {
        let ours = read_endpoint(&self.key).and_then(|v| v.get("pid")?.as_u64())
            == Some(u64::from(std::process::id()));
        if ours {
            let _ = std::fs::remove_file(endpoint_file(&self.key));
        }
        let ev = rx.recv_timeout(LEAVE_GRACE).ok()?;
        broker_log(&self.key, "a client came while leaving: staying up");
        if ours {
            let _ = write_endpoint(&self.key, &self.endpoint);
        }
        Some(ev)
    }

    /// Answer `msgs` in order: session requests locally, the rest through one pipelined exchange.
    fn serve_batch(&mut self, msgs: Vec<(u64, Vec<u8>)>) -> Result<(), String> {
        let mut pending = Vec::with_capacity(msgs.len());
        let mut calls = Vec::new();
        for (id, m) in msgs {
            let Some(req) = Request::decode(&m) else {
                continue; // not a request: dropped, as a probe drops an unknown role
            };
            if let Some(answer) = self.local(id, &req) {
                pending.push(Pending::Local(id, answer));
                continue;
            }
            calls.push((
                id,
                Call {
                    func: req.func,
                    op: req.op,
                    // Lock-free requests may come without a session; the rest run in the broker's.
                    session: req.session.map(|_| self.sid),
                    payload: req.payload.clone(),
                },
            ));
            pending.push(Pending::Forward(id, req));
        }
        let before = self.up.losses();
        let replies = if calls.is_empty() {
            Vec::new()
        } else {
            self.up.exchange(calls)?
        };
        // A lost answer on the probe's link costs a resend after its timeout: say so, since it is
        // what makes a client's request slow.
        let after = self.up.losses();
        if after != before {
            broker_log(
                &self.key,
                &format!(
                    "upstream: {} resend(s), {} broken frame(s) dropped",
                    after.0 - before.0,
                    after.1 - before.1
                ),
            );
        }
        // en: The lease lapsed under these requests (a long stall of the host): the probe released
        // every client's connections. Pass the answers on (each client hears `expired` and starts
        // again) and open the probe again for what comes next.
        // ja: lease が切れていた(host が長く止まった)。probe は全 client の接続を外した。答えはそのまま
        // 返し(client は expired を受けてやり直す)、次の要求のために開き直す。
        if replies
            .iter()
            .any(|r| r.resolution == Resolution::Rejected(reject_reasons::EXPIRED))
        {
            self.up.reopen()?;
            self.swept();
        }
        let mut replies = replies.into_iter();
        for p in pending {
            match p {
                Pending::Local(id, answer) => self.send(id, &answer),
                Pending::Forward(id, req) => {
                    let Some(r) = replies.next() else { break };
                    self.note(id, &req, &r);
                    self.send(id, &encode_result(req.corr, r.resolution, &r.payload));
                }
            }
        }
        Ok(())
    }

    /// The probe released everything after a lapse: no client holds a connection or a plan.
    fn swept(&mut self) {
        self.ledger = Ledger::default();
    }

    fn send(&mut self, id: u64, msg: &[u8]) {
        if let Some(s) = self.clients.get_mut(&id) {
            let _ = s.write_all(&length_frame(msg));
        }
    }

    /// The broker's own answer to `req`, or `None` to forward it.
    fn local(&mut self, id: u64, req: &Request) -> Option<Vec<u8>> {
        let ok = |p: &[u8]| encode_result(req.corr, Resolution::Completed(outcomes::SUCCESS), p);
        if req.func == oep_core::FN {
            let op = req.op;
            return match op {
                o if o == oep_core::op::CONFIRM => {
                    let l = self.up.limits();
                    let mut p = registry::constants::CONFIRM_RESULT_MAGIC
                        .as_bytes()
                        .to_vec();
                    p.extend_from_slice(&[l.revision, 0]);
                    p.extend_from_slice(&l.max_frame.to_le_bytes());
                    p.extend_from_slice(&l.window.to_le_bytes());
                    p.push(l.max_inflight);
                    p.extend_from_slice(&l.boot_id.to_le_bytes());
                    Some(ok(&p))
                }
                o if o == oep_core::op::OPEN => {
                    let lease = req
                        .payload
                        .get(4..8)
                        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .filter(|&l| l != 0)
                        .unwrap_or(LEASE_MS)
                        .clamp(1000, 60_000);
                    let mut p = lease.to_le_bytes().to_vec();
                    p.extend_from_slice(&self.up.boot_id().to_le_bytes());
                    p.push(0);
                    Some(ok(&p))
                }
                o if o == oep_core::op::END || o == oep_core::op::KEEPALIVE => Some(ok(&[])),
                o if o == oep_core::op::LOCK_STATE => {
                    let mut p = vec![1u8];
                    p.extend_from_slice(&LEASE_MS.to_le_bytes());
                    Some(ok(&p))
                }
                // Pushes are not relayed yet (console rev 1 has none).
                o if o == oep_core::op::SUBSCRIBE || o == oep_core::op::UNSUBSCRIBE => {
                    Some(encode_result(
                        req.corr,
                        Resolution::Rejected(reject_reasons::UNAVAILABLE),
                        &[],
                    ))
                }
                _ => None,
            };
        }
        // A detach while another client still uses the connection only drops this client's use.
        if self.wires.contains(&req.func) && req.op == wire_rvswd::op::DETACH {
            let conn = u16::from_le_bytes([*req.payload.first()?, *req.payload.get(1)?]);
            let (_, users) = self.ledger.conns.get_mut(&conn)?;
            if users.len() > 1 || !users.contains(&id) {
                users.remove(&id);
                return Some(ok(&[]));
            }
        }
        None
    }

    /// Record what a forwarded request changed in the ledger.
    fn note(&mut self, id: u64, req: &Request, r: &Reply) {
        if !r.succeeded() {
            return;
        }
        if self.wires.contains(&req.func) {
            match req.op {
                o if o == wire_rvswd::op::ATTACH => {
                    if r.payload.len() >= 2 {
                        let conn = u16::from_le_bytes([r.payload[0], r.payload[1]]);
                        self.ledger
                            .conns
                            .entry(conn)
                            .or_insert((req.func, BTreeSet::new()))
                            .1
                            .insert(id);
                    }
                }
                o if o == wire_rvswd::op::DETACH && req.payload.len() >= 2 => {
                    let conn = u16::from_le_bytes([req.payload[0], req.payload[1]]);
                    self.ledger.conns.remove(&conn);
                }
                _ => {}
            }
        } else if req.func == oep_core::FN && req.op == oep_core::op::PLAN_APPLY {
            if let Ok(tlvs) = parse_tlvs(&req.payload) {
                let fns = self.ledger.plans.entry(id).or_default();
                for t in tlvs.iter().filter(|t| t.value.len() >= 2) {
                    fns.insert(u16::from_le_bytes([t.value[0], t.value[1]]));
                }
            }
        } else if req.func == oep_core::FN && req.op == oep_core::op::PLAN_RELEASE {
            let fns = self.ledger.plans.entry(id).or_default();
            match req.payload.first() {
                Some(0) | None => fns.clear(),
                Some(&n) => {
                    for i in 0..usize::from(n) {
                        if let Some(b) = req.payload.get(1 + 2 * i..3 + 2 * i) {
                            fns.remove(&u16::from_le_bytes([b[0], b[1]]));
                        }
                    }
                }
            }
        }
    }

    /// A client left: drop its uses; detach connections nobody uses any more, release its plans.
    fn release(&mut self, id: u64) -> Result<(), String> {
        let mut calls = Vec::new();
        let mut dead = Vec::new();
        for (&conn, (func, users)) in self.ledger.conns.iter_mut() {
            if users.remove(&id) && users.is_empty() {
                calls.push((
                    id,
                    Call {
                        func: *func,
                        op: wire_rvswd::op::DETACH,
                        session: Some(self.sid),
                        payload: conn.to_le_bytes().to_vec(),
                    },
                ));
                dead.push(conn);
            }
        }
        for c in dead {
            self.ledger.conns.remove(&c);
        }
        if let Some(fns) = self.ledger.plans.remove(&id)
            && !fns.is_empty()
        {
            let mut p = vec![fns.len() as u8];
            for f in fns {
                p.extend_from_slice(&f.to_le_bytes());
            }
            calls.push((
                id,
                Call {
                    func: oep_core::FN,
                    op: oep_core::op::PLAN_RELEASE,
                    session: Some(self.sid),
                    payload: p,
                },
            ));
        }
        if !calls.is_empty() {
            self.up.exchange(calls)?;
        }
        Ok(())
    }
}

/// en: A WCH-Link borrowed from its broker for a direct flash: while this lives, the broker has
/// let go of the Link (the command opens it itself, WCH's stub and all) and keeps serving its
/// consoles' kept output; dropping it hands the Link back, and so does this process ending.
/// ja: ブローカーから借りた WCH-Link(直接の flash のため)。生きている間ブローカーは Link を手放し、
/// console の手元の出力だけを答える。drop で返す(プロセスが終わっても返る)。
pub(crate) struct Lend {
    probe: Probe,
    func: u16,
}

/// Borrow the Link from its running broker.
pub(crate) fn lend(target: &BrokerTarget) -> Result<Lend, String> {
    let link = existing_link_for(target).ok_or("the Link's broker is not running")?;
    let mut probe = Probe::connect(link).map_err(|e| e.to_string())?;
    let owner = format!("ch32rv flash pid {}", std::process::id());
    probe
        .open(
            ch32rv_oep::session::random_session_id(),
            60_000,
            false,
            Some(&owner),
        )
        .map_err(|e| e.to_string())?;
    let func = probe
        .interface(crate::broker_wch::WCHLINK_IF)
        .map_err(|e| e.to_string())?
        .func;
    let r = probe
        .call(func, crate::broker_wch::OP_LEND, Vec::new())
        .map_err(|e| e.to_string())?;
    ch32rv_oep::session::check(r)
        .map_err(|e| format!("the broker would not lend the Link: {e}"))?;
    Ok(Lend { probe, func })
}

impl Lend {
    /// en: Hand the Link back and have the broker reset the target right after it has reopened
    /// its consoles, so the monitor's polling is already running when the firmware starts (dmseq
    /// drops what a target writes while no host answers). Returns whether the target was seen
    /// running.
    /// ja: Link を返し、ブローカーに console を開き直した直後に target を reset させる(firmware が
    /// 起動する時点で monitor の poll が動いている。dmseq は host が答えない間の出力を捨てる)。
    fn hand_back_with_reset(mut self) -> Option<bool> {
        let r = self
            .probe
            .call(self.func, crate::broker_wch::OP_RECLAIM, vec![1])
            .ok()?;
        let running = ch32rv_oep::session::check(r)
            .ok()
            .and_then(|p| p.first().map(|&b| b != 0));
        let _ = self.probe.end();
        self.func = 0; // handed back: the drop sends nothing more
        running
    }
}

impl Drop for Lend {
    fn drop(&mut self) {
        if self.func == 0 {
            return;
        }
        let _ = self
            .probe
            .call(self.func, crate::broker_wch::OP_RECLAIM, Vec::new());
        let _ = self.probe.end();
    }
}

thread_local! {
    /// The Link this command has borrowed, if any (see [`LendScope`]).
    static LENT: std::cell::RefCell<Option<Lend>> = const { std::cell::RefCell::new(None) };
}

/// en: Keeps a borrowed Link for the rest of a command; dropping it hands the Link back (unless
/// [`hand_back_with_reset`] already did).
/// ja: 借りた Link をコマンドの終わりまで持つ。drop で返す(`hand_back_with_reset` が返していなければ)。
pub(crate) struct LendScope;

impl LendScope {
    pub(crate) fn new(l: Lend) -> Self {
        LENT.with(|c| *c.borrow_mut() = Some(l));
        LendScope
    }
}

impl Drop for LendScope {
    fn drop(&mut self) {
        LENT.with(|c| drop(c.borrow_mut().take()));
    }
}

/// Whether this command holds a Link borrowed from its broker.
pub(crate) fn lend_active() -> bool {
    LENT.with(|c| c.borrow().is_some())
}

/// Hand the borrowed Link back with a reset by the broker; `None` when none is borrowed or the
/// broker did not answer.
pub(crate) fn hand_back_with_reset() -> Option<bool> {
    LENT.with(|c| c.borrow_mut().take())?.hand_back_with_reset()
}
