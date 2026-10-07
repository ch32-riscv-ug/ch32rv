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
/// en: The broker's lease: long enough that a raised port speed that stopped answering can be
/// noticed and brought back to the boot speed (a timeout, then a confirm there) before it lapses
/// (oep-core §3.5); a broker that dies still frees a raised port within 3 s (the probe's
/// silence rule), and its lock goes with the lease. ja: ブローカーの lease。上げた速さで答えが
/// 止まったとき、気づいて起動時の速さへ戻すまで切れない長さ。
const LEASE_MS: u32 = 10_000;
const KEEPALIVE_EVERY: Duration = Duration::from_millis(1000);
/// How long after the speed went down the broker raises it again (when no request waits), and
/// how many times a session: one lost answer must not cost the raised speed for the rest of a
/// long session (two sweeps 150 / 136 s against 66 s on the V003 jig, 2026-10-07).
const RAISE_AGAIN_AFTER: Duration = Duration::from_secs(20);
const RAISES_AGAIN: u32 = 3;

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

/// en: The broker's session id on the probe, kept while it runs. A probe keeps a session across
/// a closed transport (transports §3: a USB device that re-enumerated without losing power, a
/// broker that died), so the next broker opens that id first - taking the session back - and
/// ends it, instead of meeting `locked` until the old lease runs out.
/// ja: ブローカーの probe の session id。経路が閉じても probe は session を保つので、次のブローカーは
/// まずその id で開いて end し、古い lease が切れるまで locked で待たずに済ませる。
fn sid_file(key: &str) -> PathBuf {
    ch32rv_usb::runtime_dir().join(format!("{key}.sid"))
}

fn save_sid(key: &str, sid: u32) {
    let _ = std::fs::write(sid_file(key), format!("{sid:08x}\n"));
}

fn saved_sid(key: &str) -> Option<u32> {
    let text = std::fs::read_to_string(sid_file(key)).ok()?;
    u32::from_str_radix(text.trim(), 16)
        .ok()
        .filter(|&s| s != 0)
}

/// Whether fn 0 describe's firmware (`<major>.<minor>.<patch>`, oep-probe-arduino) is at least
/// `want`; false when it does not say or says something else.
fn firmware_at_least(p: &mut ch32rv_oep::session::Probe, want: (u32, u32, u32)) -> bool {
    let Ok(tlvs) = p.describe(oep_core::FN) else {
        return false;
    };
    let Some(t) = tlvs
        .iter()
        .find(|t| t.tag == oep_core::tlvs::describe::FIRMWARE)
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&t.value);
    let mut parts = text
        .trim()
        .trim_start_matches('v')
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let got = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    got >= want
}

/// en: The rates the broker tries on a UART bridge: `CH32RV_PORT_SPEED` = `off`, or a comma list
/// (`921600,500000`); unset = [`ch32rv_oep::speed::DEFAULT_RATES`]. ja: ブローカーが試す速さ。
fn port_speed_rates() -> Option<Vec<u32>> {
    match std::env::var("CH32RV_PORT_SPEED") {
        Ok(v) if v.trim().eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.trim().is_empty() => {
            Some(v.split(',').filter_map(|r| r.trim().parse().ok()).collect())
        }
        _ => Some(ch32rv_oep::speed::DEFAULT_RATES.to_vec()),
    }
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
    /// An OEP probe on TCP: its endpoint, and its unit id when it was named by one (found by
    /// DNS-SD). Named by unit id, its broker shares the key a USB probe of that unit id has, so a
    /// probe reached both ways still has one owner on this host.
    Net { addr: String, unit: Option<String> },
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
            BrokerTarget::Net { unit: Some(u), .. } => {
                ch32rv_usb::sanitize_key(&format!("oep-{}", u.to_ascii_lowercase()))
            }
            BrokerTarget::Net { addr, unit: None } => {
                ch32rv_usb::sanitize_key(&format!("oep-tcp-{addr}"))
            }
            BrokerTarget::Wch { id, .. } => ch32rv_usb::sanitize_key(&format!("wch-{id}")),
        }
    }

    fn selector(&self) -> String {
        match self {
            BrokerTarget::Serial(p) => format!("port:{p}"),
            // By unit id the broker resolves it again by DNS-SD (the address may change when the
            // probe joins its Wi-Fi again).
            BrokerTarget::Net { unit: Some(u), .. } => format!("tcp:{u}"),
            BrokerTarget::Net { addr, unit: None } => format!("tcp:{addr}"),
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

/// en: Whether `addr` (`host:port`) is a ch32rv broker on this host, from the endpoint files: such
/// an address is a broker's client port, opened directly (a broker is never put in front of a
/// broker). ja: `addr` がこの PC の ch32rv のブローカーか(endpoint の file から)。そうなら直接つなぐ。
pub(crate) fn is_broker_endpoint(addr: &str) -> bool {
    let Some((host, port)) = addr.rsplit_once(':') else {
        return false;
    };
    if !matches!(host, "127.0.0.1" | "localhost") {
        return false;
    }
    let Ok(port) = port.parse::<u64>() else {
        return false;
    };
    std::fs::read_dir(ch32rv_usb::runtime_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "oep"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .any(|v| v.get("port").and_then(Value::as_u64) == Some(port))
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
        Ok(Some(a @ (OepAddr::Tcp(_) | OepAddr::Net { .. }))) => match crate::oep::net_target(&a) {
            Some(t) => t,
            None => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "this tcp: address is a broker itself",
                    None,
                );
            }
        },
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
        Ok(Some(a @ (OepAddr::Tcp(_) | OepAddr::Net { .. }))) => match crate::oep::net_target(&a) {
            Some(t) => t,
            None => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "this tcp: address is a broker itself",
                    None,
                );
            }
        },
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
    // The UART bridge port_speed raised, while it stays raised.
    let mut speed_port: Option<u8> = None;
    let mut speed_ratio_rule = false;
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
            // The previous broker's session, if the probe still keeps it: take it back and end it
            // (everything it held is released), then open as usual.
            if let Some(old) = saved_sid(&key)
                && probe.open(old, LEASE_MS, false, Some(&owner)).is_ok()
            {
                let _ = probe.end();
                broker_log(&key, "ended the previous broker's session");
            }
            let sid = match crate::oep::open_with_lock_rule(&mut probe, serial, &owner, LEASE_MS) {
                Ok(s) => s,
                Err(e) => return report_error(e.to_string()),
            };
            save_sid(&key, sid);
            (Upstream::Oep(Box::new(probe)), sid)
        }
        (BrokerTarget::Net { addr, unit }, _) => {
            transport = "tcp";
            let opened = match unit {
                Some(u) => crate::oep::open_net(addr, u),
                None => ch32rv_oep::link::open_tcp(addr)
                    .map_err(|e| e.to_string())
                    .and_then(|l| Probe::connect(l).map_err(|e| e.to_string())),
            };
            let mut probe = match opened {
                Ok(p) => p,
                Err(m) => return report_error(format!("{addr}: {m}")),
            };
            let owner = format!("ch32rv broker pid {}", std::process::id());
            if let Some(old) = saved_sid(&key)
                && probe.open(old, LEASE_MS, false, Some(&owner)).is_ok()
            {
                let _ = probe.end();
                broker_log(&key, "ended the previous broker's session");
            }
            let sid = match crate::oep::open_with_lock_rule(&mut probe, false, &owner, LEASE_MS) {
                Ok(s) => s,
                Err(e) => return report_error(e.to_string()),
            };
            save_sid(&key, sid);
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
    // en: A UART bridge's link is raised for the broker's long session (oep-core §3.5, by default
    // since 2026-10-01: "a feature that is not used breaks"), every trial logged so the rates can
    // be decided from what is seen. Clients that connect meanwhile wait (their TCP wait is 15 s).
    // ja: UART bridge の link を、ブローカーの長い session の間だけ上げる(既定、2026-10-01)。試した
    // 結果はすべて log に出す。その間に来た client は待つ(TCP の待ちは 15 秒)。
    if transport == "serial"
        && let Upstream::Oep(p) = &mut up
    {
        match port_speed_rates() {
            None => broker_log(&key, "port_speed: off (CH32RV_PORT_SPEED=off)"),
            Some(rates) => {
                p.link().resend_on_broken = true;
                speed_ratio_rule = firmware_at_least(p, (0, 0, 27));
                match ch32rv_oep::speed::raise_speed(
                    p,
                    &rates,
                    Duration::from_secs(6),
                    speed_ratio_rule,
                ) {
                    Ok(r) => {
                        broker_log(&key, &r.summary());
                        speed_port = r.port.filter(|_| r.trials.iter().any(|t| t.committed));
                    }
                    Err(e) => return report_error(format!("port_speed: {e}")),
                }
            }
        }
    }

    let mut b = Broker {
        up,
        sid,
        wires,
        clients: HashMap::new(),
        sessions: HashMap::new(),
        plan_fn: None,
        restart_fn: None,
        relayed_restart: false,
        ledger: Ledger::default(),
        key: key.clone(),
        endpoint: json!({"port": port, "pid": std::process::id(), "time": now_ms(), "transport": transport}),
        raise_rates: speed_port.and(port_speed_rates()),
        raise_again_at: None,
        raises_left: RAISES_AGAIN,
        speed_port,
        speed_window: std::collections::VecDeque::new(),
        speed_ratio_rule,
        fallbacks_seen: 0,
    };
    b.find_fns();
    let r = b.run(&rx);
    match &r {
        Ok(()) if b.relayed_restart => broker_log(&key, "down: the probe is restarting"),
        Ok(()) => broker_log(&key, "down: no client left"),
        Err(e) => broker_log(&key, &format!("down on an upstream error: {e}")),
    }
    // Remove the endpoint only if it is still ours, then end the probe session.
    if read_endpoint(&key).and_then(|v| v.get("pid")?.as_u64())
        == Some(u64::from(std::process::id()))
    {
        let _ = std::fs::remove_file(endpoint_file(&key));
    }
    if !b.relayed_restart {
        b.up.end();
    }
    // Kept after an upstream error: the probe may still hold the session for the next broker.
    if r.is_ok() {
        let _ = std::fs::remove_file(sid_file(&key));
    }
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
    /// The upstream link's (resends, broken, good, lost) so far (for the log and port_speed).
    fn losses(&mut self) -> (u64, u64, u64, u64) {
        match self {
            Upstream::Oep(p) => {
                let l = p.link();
                (l.resends, l.broken, l.good, l.lost)
            }
            Upstream::Wch(_) => (0, 0, 0, 0),
        }
    }

    /// en: After a request went unanswered: does the probe still answer? A confirm at the speed
    /// now, then (raised) at the boot speed (oep-core §3.5 host duty 5). ja: probe がまだ答えるか。
    fn recheck(&mut self) -> bool {
        match self {
            Upstream::Oep(p) => {
                let l = p.link();
                if (0..3).any(|_| l.confirm_raw(Duration::from_millis(500))) {
                    return true;
                }
                let back = l.baud() != l.base_baud()
                    && l.back_to_base(Duration::from_millis(
                        u64::from(ch32rv_oep::registry::timing::PORT_SPEED_IDLE_MS) + 1000,
                    ));
                if back {
                    l.speed_fallbacks += 1;
                }
                back
            }
            Upstream::Wch(_) => true,
        }
    }

    fn client_gone(&mut self, id: u64) {
        if let Upstream::Wch(w) = self {
            w.client_gone(id);
        }
    }

    /// Renew the lease; `Ok(true)` when the probe no longer knew the session (the lease lapsed:
    /// it released everything, core §6.4) and the caller is to open a new one.
    fn keepalive(&mut self) -> Result<bool, String> {
        match self {
            Upstream::Oep(p) => match p.keepalive() {
                Ok(()) => Ok(false),
                Err(ch32rv_oep::session::OepError::Rejected { reason, .. })
                    if reason == reject_reasons::NO_SESSION =>
                {
                    Ok(true)
                }
                Err(e) => Err(e.to_string()),
            },
            Upstream::Wch(_) => Ok(false),
        }
    }

    /// en: Open the probe under a new session id, its interfaces learned again: after a restart
    /// (a changed boot_id) or `no_session` (the probe no longer knows our session: there is no
    /// resume, core §6.4). Everything the clients held is gone; the link stays at the speed it is
    /// at (a restarted probe is at its boot speed). ja: 新しい session id で開き直し、interface を
    /// 取り直す(再起動か no_session の後。再開は無い)。
    fn reopen_fresh(&mut self) -> Result<(), String> {
        let Upstream::Oep(p) = self else {
            return Ok(());
        };
        p.forget_interfaces();
        let owner = format!("ch32rv broker pid {}", std::process::id());
        p.open(
            ch32rv_oep::session::random_session_id(),
            LEASE_MS,
            false,
            Some(&owner),
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// Whether the probe restarted since the broker's session was opened (seen in a confirm).
    fn rebooted(&self) -> bool {
        match self {
            Upstream::Oep(p) => p.rebooted(),
            Upstream::Wch(_) => false,
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
    /// Each client's open session (its id): a request under any other id is answered
    /// no_session, as a probe answers one for a session that ended (core §6.2).
    sessions: HashMap<u64, u32>,
    /// The probe's `oep.probe.plan` and `oep.probe.restart` fns, when it has them.
    plan_fn: Option<u16>,
    restart_fn: Option<u16>,
    /// A client's restart was relayed: the broker ends without ending its session (the probe is
    /// restarting and answers nothing).
    relayed_restart: bool,
    ledger: Ledger,
    /// The runtime key and what the endpoint file says, to take it down and put it back.
    key: String,
    endpoint: Value,
    /// The raised UART bridge (None: at the boot speed).
    speed_port: Option<u8>,
    /// (when, good, broken + lost) at the raised speed, the last few seconds' worth.
    speed_window: std::collections::VecDeque<(Instant, u64, u64)>,
    /// The probe goes back only at 3 broken frames in a row (oep-probe-arduino 0.0.27 and up):
    /// judge by the share over 3 s (host guide §7.4) instead of 2 broken in 5 s.
    speed_ratio_rule: bool,
    /// The link's `speed_fallbacks` already logged.
    fallbacks_seen: u64,
    /// The rates to raise to again after the speed went down (None: port_speed off or never
    /// committed), when to try next, and how many tries are left this session.
    raise_rates: Option<Vec<u32>>,
    raise_again_at: Option<Instant>,
    raises_left: u32,
}

/// How long a broker about to end still takes a new connection (after its endpoint is gone).
const LEAVE_GRACE: Duration = Duration::from_millis(150);

/// en: How long a broker whose last client left stays up (endpoint and keepalive, so the session
/// and a raised port speed hold): an IDE closes its monitor and then starts the upload, a moment
/// later (the user, 2026-10-01: long enough for that, no longer). ja: 最後の client が抜けた後に
/// ブローカーが残る時間(IDE がモニターを閉じてからアップロードを始めるまでの間)。
const LINGER: Duration = Duration::from_secs(3);

impl Broker {
    fn run(&mut self, rx: &mpsc::Receiver<Ev>) -> Result<(), String> {
        let started = Instant::now();
        let mut had_client = false;
        let mut last_upstream = Instant::now();
        let mut carried: Option<Ev> = None;
        // Since when no client is connected (after one had been).
        let mut empty_since: Option<Instant> = None;
        loop {
            let wait = self.up.tick();
            let got = match carried.take() {
                Some(ev) => Ok(ev),
                None => rx.recv_timeout(wait),
            };
            let ev = match got {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => {
                    let lingered = empty_since.is_some_and(|t| t.elapsed() >= LINGER);
                    if self.clients.is_empty()
                        && ((had_client && lingered)
                            || (!had_client && started.elapsed() > FIRST_CLIENT_WAIT))
                    {
                        match self.leave(rx) {
                            Some(ev) => carried = Some(ev),
                            None => return Ok(()),
                        }
                        continue;
                    }
                    if self.raise_again() {
                        last_upstream = Instant::now();
                    }
                    if last_upstream.elapsed() >= KEEPALIVE_EVERY {
                        self.keepalive()?;
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
                        empty_since = None;
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
                        self.sessions.remove(&id);
                        broker_log(&self.key, &format!("client {id} gone"));
                    }
                }
            }
            if !msgs.is_empty() {
                self.serve_batch(msgs)?;
                if self.relayed_restart {
                    return Ok(());
                }
                // Also between busy batches: a monitor's steady reads never leave the broker idle.
                self.raise_again();
                last_upstream = Instant::now();
            } else if last_upstream.elapsed() >= KEEPALIVE_EVERY {
                // Only connections and goodbyes came: the lease still needs its keepalive.
                self.keepalive()?;
                last_upstream = Instant::now();
            }
            if had_client && self.clients.is_empty() {
                // Also here, not only when idle: connections that only look (`broker endpoint`)
                // keep events coming and must not keep the broker up past its linger.
                match empty_since {
                    None => empty_since = Some(Instant::now()),
                    Some(t) if t.elapsed() >= LINGER => match self.leave(rx) {
                        Some(ev) => carried = Some(ev),
                        None => return Ok(()),
                    },
                    Some(_) => {}
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

    /// en: Answer `msgs` in order: session requests locally, the rest through pipelined
    /// exchanges. An end (it releases the client's share first) and a restart (forwarded alone,
    /// then the broker waits for the probe and opens a new session) split the batch, so what
    /// came before them reaches the probe before them.
    /// ja: `msgs` を順に答える。end と restart は batch を区切る(それより前のものを先に probe へ)。
    fn serve_batch(&mut self, msgs: Vec<(u64, Vec<u8>)>) -> Result<(), String> {
        let mut pending = Vec::with_capacity(msgs.len());
        let mut calls = Vec::new();
        let mut restarted = false;
        for (id, m) in msgs {
            let Some(req) = Request::decode(&m) else {
                continue; // not a request: dropped, as a probe drops an unknown role
            };
            // en: Sent before the clients could know of a restart earlier in this batch: not sent
            // on (oep-transports §1): no_session under a session, result_lost otherwise.
            // ja: 同じ batch の restart の後ろの要求は送らない(session つきは no_session、ほかは result_lost)。
            if restarted {
                let open = req.func == oep_core::FN && req.op == oep_core::op::OPEN;
                let reason = if req.session.is_some() && !open {
                    reject_reasons::NO_SESSION
                } else {
                    reject_reasons::RESULT_LOST
                };
                pending.push(Pending::Local(
                    id,
                    encode_result(req.corr, Resolution::Rejected(reason), &[]),
                ));
                continue;
            }
            if let Some(answer) = self.local(id, &req) {
                pending.push(Pending::Local(id, answer));
                continue;
            }
            if req.func == oep_core::FN && req.op == oep_core::op::END {
                self.flush(&mut pending, &mut calls)?;
                self.sessions.remove(&id);
                if let Err(e) = self.release(id) {
                    broker_log(
                        &self.key,
                        &format!("client {id}: release at end failed: {e}"),
                    );
                }
                let ok = encode_result(req.corr, Resolution::Completed(outcomes::SUCCESS), &[]);
                self.send(id, &ok);
                continue;
            }
            if self.restart_fn == Some(req.func)
                && req.op == registry::probe_restart::op::RESTART
                && matches!(self.up, Upstream::Oep(_))
            {
                self.flush(&mut pending, &mut calls)?;
                restarted = self.restart(id, &req)?;
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
        self.flush(&mut pending, &mut calls)
    }

    /// en: A client's restart (oep-if-restart §2, §3): forwarded under the broker's session like
    /// any locking op, its answer passed on; after a success the broker closes its transport to
    /// the probe and ends (transports §1: the transport to the probe is gone), so every client
    /// starts over - the one that restarted the probe waits for it (restart_max_ms) and its next
    /// open starts a new broker. True when the broker is to end.
    /// ja: client の restart を自分の session で中継し、答えを返す。成功ならブローカーは probe への
    /// 経路を閉じて終わる(client はやり直す)。
    fn restart(&mut self, id: u64, req: &Request) -> Result<bool, String> {
        let Upstream::Oep(p) = &mut self.up else {
            return Ok(false);
        };
        let (answer, restarting) = match p.request_restart() {
            Ok(()) => (
                encode_result(req.corr, Resolution::Completed(outcomes::SUCCESS), &[]),
                true,
            ),
            Err(ch32rv_oep::session::OepError::Rejected { reason, payload }) => (
                encode_result(req.corr, Resolution::Rejected(reason), &payload),
                false,
            ),
            Err(ch32rv_oep::session::OepError::Failed { outcome, payload }) => (
                encode_result(req.corr, Resolution::Completed(outcome), &payload),
                false,
            ),
            Err(ch32rv_oep::session::OepError::Locked { .. }) => (
                encode_result(req.corr, Resolution::Rejected(reject_reasons::LOCKED), &[]),
                false,
            ),
            // No answer: it may have restarted all the same. The client hears result_lost and
            // checks the boot_id itself; the broker ends either way (it cannot tell).
            Err(_) => (
                encode_result(
                    req.corr,
                    Resolution::Rejected(reject_reasons::RESULT_LOST),
                    &[],
                ),
                true,
            ),
        };
        self.send(id, &answer);
        if restarting {
            broker_log(
                &self.key,
                &format!("client {id}: restart relayed - closing the probe's transport and ending"),
            );
            self.relayed_restart = true;
        }
        Ok(restarting)
    }

    /// en: Send `calls` in one pipelined exchange and answer `pending` in order.
    /// ja: `calls` を 1 回の pipeline で送り、`pending` を順に答える。
    fn flush(
        &mut self,
        pending: &mut Vec<Pending>,
        calls: &mut Vec<(u64, Call)>,
    ) -> Result<(), String> {
        let pending = std::mem::take(pending);
        let calls = std::mem::take(calls);
        let before = self.up.losses();
        let n_calls = calls.len();
        let replies = if calls.is_empty() {
            Vec::new()
        } else {
            match self.up.exchange(calls) {
                Ok(r) => r,
                // en: No answer even after the resend (a slow or noisy moment of the probe): one
                // request's loss must not take every client down with the broker. The waiting
                // clients hear result_lost (a read asks again by itself; anything else is that
                // command's error), and the link is checked with confirm; only when the probe
                // answers nothing at all does the broker end.
                // ja: 送り直しても答えが無い: 1 つの要求のためにブローカーごと全 client を落とさない。待って
                // いる client には result_lost を返し(read は自分で読み直す)、confirm で link を確かめる。
                // probe が何も答えないときだけ終わる。
                Err(e) => {
                    let alive = self.up.recheck();
                    broker_log(
                        &self.key,
                        &format!(
                            "upstream: {e}; {n_calls} waiting request(s) answered result_lost, {}",
                            if alive {
                                "the probe answers confirm: going on"
                            } else {
                                "the probe answers nothing"
                            }
                        ),
                    );
                    if !alive {
                        return Err(e);
                    }
                    vec![
                        Reply {
                            resolution: Resolution::Rejected(reject_reasons::RESULT_LOST),
                            payload: Vec::new(),
                        };
                        n_calls
                    ]
                }
            }
        };
        // A lost answer on the probe's link costs a resend after its timeout: say so, since it is
        // what makes a client's request slow.
        let after = self.up.losses();
        if (after.0, after.1) != (before.0, before.1) {
            broker_log(
                &self.key,
                &format!(
                    "upstream: {} resend(s), {} broken frame(s) dropped",
                    after.0 - before.0,
                    after.1 - before.1
                ),
            );
        }
        self.watch_speed(after.1 - before.1, after.2 - before.2, after.3 - before.3);
        self.watch_restart()?;
        // en: The probe no longer knows the broker's session (its lease lapsed under a long stall
        // of the host, it restarted, or another host took it in between): pass the answers on,
        // and open a new session for what comes next.
        // ja: probe が session を知らない(lease 切れ、再起動など): 答えはそのまま返し、新しい session
        // で開き直す。
        if replies
            .iter()
            .any(|r| r.resolution == Resolution::Rejected(reject_reasons::NO_SESSION))
        {
            broker_log(&self.key, "upstream: no_session - opening a new session");
            self.restart_session()?;
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

    /// en: While the port speed is raised, watch how the frames fare and lower it together with
    /// the probe when they fare badly (oep-core §3.5: the host notices first; the probe's own
    /// fallback is the last resort); and note when the link found the probe back at the boot speed
    /// by itself. Either way the session stays at the boot speed. The rule depends on the probe:
    /// from oep-probe-arduino 0.0.27 (it goes back only at 3 broken frames in a row) the share of
    /// broken and lost frames over the last 3 s above 10 % (host guide §7.4, base 0, not judged
    /// under 50 frames); before it (3 in 1 s) 2 broken frames in 5 s, so as to lower first.
    /// ja: 上げた速さの間、フレームの様子を見て、悪ければ probe と揃えて下げる。0.0.27 以降の probe
    /// では直近 3 秒の(壊れ + 失われ)の割合が 10 % を超えたら(50 フレーム未満は判定しない)、それより
    /// 前の probe では 5 秒に 2 つ壊れたら。
    fn watch_speed(&mut self, broken: u64, good: u64, lost: u64) {
        if self.speed_port.is_none() {
            return;
        }
        let Upstream::Oep(p) = &mut self.up else {
            return;
        };
        let fallbacks = p.link().speed_fallbacks;
        if fallbacks != self.fallbacks_seen {
            self.fallbacks_seen = fallbacks;
            self.speed_port = None;
            self.lowered();
            broker_log(
                &self.key,
                "port_speed: a request at the raised speed got no answer (even resent), and the probe answered confirm at the boot speed (link duty 5): at the boot speed for now",
            );
            return;
        }
        let now = Instant::now();
        let window = if self.speed_ratio_rule {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(5)
        };
        if good + broken + lost > 0 {
            self.speed_window.push_back((now, good, broken + lost));
        }
        while self
            .speed_window
            .front()
            .is_some_and(|(t, ..)| now.duration_since(*t) > window)
        {
            self.speed_window.pop_front();
        }
        let (ok_n, bad_n) = self
            .speed_window
            .iter()
            .fold((0u64, 0u64), |(g, b), (_, gg, bb)| (g + gg, b + bb));
        let why = if self.speed_ratio_rule {
            let total = ok_n + bad_n;
            (total >= 50 && bad_n * 10 > total)
                .then(|| format!("{bad_n} of {total} frames broken or lost in 3 s (over 10 %)"))
        } else {
            (bad_n >= 2).then(|| format!("{bad_n} broken or lost frames in 5 s"))
        };
        if let Some(why) = why {
            let rate = p.link().baud().unwrap_or(0);
            let ok = ch32rv_oep::speed::revert(p);
            self.speed_port = None;
            self.speed_window.clear();
            self.lowered();
            broker_log(
                &self.key,
                &format!(
                    "port_speed: {why} at {rate}: back to the boot speed{}",
                    if ok { "" } else { " (no confirm there)" }
                ),
            );
        }
    }

    /// en: The speed went down: try raising it again later (link §3 lets the host raise again).
    /// ja: 速さが下がった。後でもう一度上げる。
    fn lowered(&mut self) {
        if self.raise_rates.is_some() && self.raises_left > 0 {
            self.raise_again_at = Some(Instant::now() + RAISE_AGAIN_AFTER);
        }
    }

    /// en: Once [`RAISE_AGAIN_AFTER`] has passed since the speed went down, between requests: run
    /// port_speed again (the same check as at the start), at most [`RAISES_AGAIN`] times a
    /// session. Clients that ask meanwhile wait for it (about a second). True when it talked to
    /// the probe. ja: 要求が無い間に、下がってから一定時間たったら port_speed をやり直す(1 session
    /// に数回まで)。
    fn raise_again(&mut self) -> bool {
        let due = self.raise_again_at.is_some_and(|t| Instant::now() >= t);
        let (Some(rates), true, Upstream::Oep(p)) = (self.raise_rates.clone(), due, &mut self.up)
        else {
            return false;
        };
        self.raise_again_at = None;
        self.raises_left = self.raises_left.saturating_sub(1);
        match ch32rv_oep::speed::raise_speed(
            p,
            &rates,
            Duration::from_secs(6),
            self.speed_ratio_rule,
        ) {
            Ok(r) => {
                let committed = r.trials.iter().any(|t| t.committed);
                if committed {
                    self.speed_port = r.port;
                    self.speed_window.clear();
                    self.fallbacks_seen = p.link().speed_fallbacks;
                }
                broker_log(&self.key, &format!("port_speed again: {}", r.summary()));
                if !committed {
                    self.lowered();
                }
            }
            Err(e) => broker_log(&self.key, &format!("port_speed again: {e}")),
        }
        true
    }

    /// The broker's own keepalive, watched like any request (a raised speed may break under it).
    fn keepalive(&mut self) -> Result<(), String> {
        let before = self.up.losses();
        if self.up.keepalive()? {
            broker_log(
                &self.key,
                "upstream: no_session on keepalive - opening a new session",
            );
            self.restart_session()?;
        }
        let after = self.up.losses();
        self.watch_speed(after.1 - before.1, after.2 - before.2, after.3 - before.3);
        self.watch_restart()?;
        Ok(())
    }

    /// en: A confirm the link sent (a speed fallback, a recheck) answered another boot_id than the
    /// broker's open: the probe restarted (oep-core §6.5, §3.5 host duty 5). Say so and open a new
    /// session, rather than wait for the next request's no_session.
    /// ja: link の confirm の boot_id が open と違う: probe が再起動した。log に出し、開き直す。
    fn watch_restart(&mut self) -> Result<(), String> {
        if self.up.rebooted() {
            broker_log(
                &self.key,
                "upstream: the probe restarted (its boot_id changed) - opening a new session",
            );
            self.restart_session()?;
        }
        Ok(())
    }

    /// en: A new upstream session after a restart or no_session: the clients' connections and
    /// plans are gone, and the raised speed with them. ja: 再起動か no_session の後の新しい session。
    /// Learn the plan and restart fns (again after a restart: the numbers may change).
    fn find_fns(&mut self) {
        if let Upstream::Oep(p) = &mut self.up {
            self.plan_fn = p.interface(registry::probe_plan::NAME).ok().map(|i| i.func);
            self.restart_fn = p
                .interface(registry::probe_restart::NAME)
                .ok()
                .map(|i| i.func);
        }
    }

    fn restart_session(&mut self) -> Result<(), String> {
        self.up.reopen_fresh()?;
        self.swept();
        // Every client's session ended with the broker's (core §9): they open again.
        self.sessions.clear();
        // A restarted probe may number its interfaces anew.
        self.wires = self.up.wires()?;
        self.find_fns();
        self.speed_port = None;
        self.speed_window.clear();
        self.lowered();
        if let Upstream::Oep(p) = &mut self.up {
            self.fallbacks_seen = p.link().speed_fallbacks;
            // The clients' requests now go in the new session.
            if let Some(sid) = p.session_id() {
                self.sid = sid;
                save_sid(&self.key, sid);
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
        let refuse = |r: u8| encode_result(req.corr, Resolution::Rejected(r), &[]);
        let open = req.func == oep_core::FN && req.op == oep_core::op::OPEN;
        // A session id that is not the client's open session (never opened, ended, or ended by a
        // restart): no_session, as the probe answers (core §6.2).
        if let Some(sid) = req.session
            && !open
            && self.sessions.get(&id) != Some(&sid)
        {
            return Some(refuse(reject_reasons::NO_SESSION));
        }
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
                    // A relaying broker's transport index is 0xFF (oep-core §7.1, transports §1).
                    ch32rv_oep::codec::put_tlv(
                        &mut p,
                        oep_core::tlvs::confirm_answer::TRANSPORT,
                        false,
                        &[0xFF],
                    );
                    Some(ok(&p))
                }
                // lease_ms(u32) force(u8) [TLV owner]; the session id is the header's (core §6.4).
                o if o == oep_core::op::OPEN => {
                    let Some(sid) = req.session else {
                        return Some(refuse(reject_reasons::MALFORMED));
                    };
                    let Some(lease) = req
                        .payload
                        .get(..4)
                        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    else {
                        return Some(refuse(reject_reasons::MALFORMED));
                    };
                    let lease = if lease == 0 { LEASE_MS } else { lease }.clamp(1000, 60_000);
                    self.sessions.insert(id, sid);
                    let mut p = lease.to_le_bytes().to_vec();
                    p.extend_from_slice(&self.up.boot_id().to_le_bytes());
                    Some(ok(&p))
                }
                o if o == oep_core::op::KEEPALIVE => Some(ok(&[])),
                o if o == oep_core::op::LOCK_STATE => {
                    let mut p = vec![1u8];
                    p.extend_from_slice(&LEASE_MS.to_le_bytes());
                    Some(ok(&p))
                }
                _ => None,
            };
        }
        // Pushes are not relayed: a subscribe to any interface (0x30 / 0x32 in every interface's op
        // space, core §11.3) is refused here.
        if req.func != oep_core::FN
            && (req.op == registry::constants::OP_SUBSCRIBE
                || req.op == registry::constants::OP_UNSUBSCRIBE)
        {
            return Some(refuse(reject_reasons::UNAVAILABLE));
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
        } else if Some(req.func) == self.plan_fn && req.op == registry::probe_plan::op::PLAN_APPLY {
            if let Ok(tlvs) = parse_tlvs(&req.payload) {
                let fns = self.ledger.plans.entry(id).or_default();
                for t in tlvs.iter().filter(|t| t.value.len() >= 2) {
                    fns.insert(u16::from_le_bytes([t.value[0], t.value[1]]));
                }
            }
        } else if Some(req.func) == self.plan_fn && req.op == registry::probe_plan::op::PLAN_RELEASE
        {
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
                    func: self.plan_fn.unwrap_or(oep_core::FN),
                    op: registry::probe_plan::op::PLAN_RELEASE,
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
