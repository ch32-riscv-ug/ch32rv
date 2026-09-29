//! en: The per-probe broker (docs/oep-host.ja.md §7.2, ArduinoCore-CH32 oep-workflow §7.2). One
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
/// The broker's own lease on the probe, renewed by keepalive while no client talks.
const LEASE_MS: u32 = 3000;
const KEEPALIVE_EVERY: Duration = Duration::from_millis(1000);

// ---- where a broker is ----

/// The key a probe's broker files are named by: from the address alone, since the probe cannot
/// be opened to learn more (the broker holds it).
fn key_for(path: &str) -> String {
    ch32rv_usb::sanitize_key(&format!("oep-{}", ch32rv_usb::normalize_port(path)))
}

fn endpoint_file(key: &str) -> PathBuf {
    ch32rv_usb::runtime_dir().join(format!("{key}.oep"))
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
        match entry.dev.serial() {
            Some(sn) => BrokerTarget::Wch {
                id: sn.to_owned(),
                selector: format!("serial:{sn}"),
            },
            None => {
                let t = entry.dev.topology();
                BrokerTarget::Wch {
                    id: t.clone(),
                    selector: format!("usb:{t}"),
                }
            }
        }
    }

    fn key(&self) -> String {
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
    let try_connect = |v: &Value| -> Option<Link> {
        let port = v.get("port")?.as_u64()?;
        ch32rv_oep::link::open_tcp(&format!("127.0.0.1:{port}")).ok()
    };
    if let Some(v) = read_endpoint(&key)
        && let Some(l) = try_connect(&v)
    {
        return Ok(l);
    }
    let t0 = now_ms();
    spawn_broker(target)?;
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
    let (up, sid) = match (&target, wch_entry) {
        (BrokerTarget::Serial(path), _) => {
            let mut probe = match ch32rv_oep::link::open_serial(path)
                .map_err(|e| e.to_string())
                .and_then(|l| Probe::connect(l).map_err(|e| e.to_string()))
            {
                Ok(p) => p,
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
        &json!({"port": port, "pid": std::process::id(), "time": now_ms()}),
    )
    .is_err()
    {
        return ExitCode::from(ErrorKind::DeviceOpenFailed.exit_code());
    }

    let (tx, rx) = mpsc::channel::<Ev>();
    std::thread::spawn(move || accept_loop(listener, tx));

    let mut b = Broker {
        up,
        sid,
        wires,
        clients: HashMap::new(),
        ledger: Ledger::default(),
    };
    let r = b.run(&rx);
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
            Upstream::Wch(_) => 0,
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

    fn exchange(&mut self, calls: Vec<Call>) -> Result<Vec<Reply>, String> {
        match self {
            Upstream::Oep(p) => p.link().exchange(calls).map_err(|e| e.to_string()),
            Upstream::Wch(w) => Ok(calls.iter().map(|c| w.handle(c)).collect()),
        }
    }

    fn keepalive(&mut self) -> Result<(), String> {
        match self {
            Upstream::Oep(p) => p.keepalive().map_err(|e| e.to_string()),
            Upstream::Wch(_) => Ok(()),
        }
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
}

impl Broker {
    fn run(&mut self, rx: &mpsc::Receiver<Ev>) -> Result<(), String> {
        let started = Instant::now();
        let mut had_client = false;
        let mut last_upstream = Instant::now();
        loop {
            let wait = self.up.tick();
            let ev = match rx.recv_timeout(wait) {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => {
                    if self.clients.is_empty()
                        && (had_client || started.elapsed() > FIRST_CLIENT_WAIT)
                    {
                        return Ok(());
                    }
                    if last_upstream.elapsed() >= KEEPALIVE_EVERY {
                        self.up.keepalive()?;
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
                        self.release(id)?;
                        self.clients.remove(&id);
                    }
                }
            }
            if !msgs.is_empty() {
                self.serve_batch(msgs)?;
                last_upstream = Instant::now();
            }
            if had_client && self.clients.is_empty() {
                return Ok(());
            }
        }
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
            calls.push(Call {
                func: req.func,
                op: req.op,
                // Lock-free requests may come without a session; the rest run in the broker's.
                session: req.session.map(|_| self.sid),
                payload: req.payload.clone(),
            });
            pending.push(Pending::Forward(id, req));
        }
        let mut replies = if calls.is_empty() {
            Vec::new()
        } else {
            self.up.exchange(calls)?
        }
        .into_iter();
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
                o if o == wire_rvswd::op::ATTACH || o == wire_rvswd::op::ATTACH_UNDER_RESET => {
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
                calls.push(Call {
                    func: *func,
                    op: wire_rvswd::op::DETACH,
                    session: Some(self.sid),
                    payload: conn.to_le_bytes().to_vec(),
                });
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
            calls.push(Call {
                func: oep_core::FN,
                op: oep_core::op::PLAN_RELEASE,
                session: Some(self.sid),
                payload: p,
            });
        }
        if !calls.is_empty() {
            self.up.exchange(calls)?;
        }
        Ok(())
    }
}
