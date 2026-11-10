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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
use crate::broker_lock::{Access, CoreLock, Opened, Replay};
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
/// (oep-if-link §3); a broker that dies still frees a raised port within 3 s (the probe's
/// silence rule), and its lock goes with the lease. ja: ブローカーの lease。上げた速さで答えが
/// 止まったとき、気づいて起動時の速さへ戻すまで切れない長さ。
const LEASE_MS: u32 = 10_000;
const KEEPALIVE_EVERY: Duration = Duration::from_millis(1000);
/// How long after the speed went down the broker raises it again (when no request waits), and
/// how many times a session: one lost answer must not cost the raised speed for the rest of a
/// long session (two sweeps 150 / 136 s against 66 s on the V003 jig, 2026-10-07).
const RAISE_AGAIN_AFTER: Duration = Duration::from_secs(20);
const RAISES_AGAIN: u32 = 3;

// ---- which ch32rv a broker is ----

/// en: The client-broker protocol: what a ch32rv client and its broker expect of each other beyond
/// OEP (the endpoint record, the session answers, the ledger, the extensions). Raised when any of
/// that changes. ja: client とブローカーの間の取り決め(OEP の外の部分)の番号。変えたら上げる。
const BROKER_PROTOCOL: u64 = 1;
/// The ch32rv a broker of this build is.
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// en: The firmware text (fn 0 describe, core §7.5) of a broker that is the probe itself (the
/// WCH-Link broker): which ch32rv it is, for users outside ch32rv. A broker in front of an OEP
/// probe passes the probe's own on. ja: WCH-Link のブローカーの describe の firmware(どの ch32rv か)。
pub(crate) fn broker_firmware() -> String {
    format!("ch32rv-broker {VERSION} protocol {BROKER_PROTOCOL}")
}

/// en: Which other ch32rv wrote endpoint record `v` (`ch32rv 0.24.0 (pid 123)`), or `None` when
/// it is a broker of this ch32rv. A client talks only to a broker of its own protocol and
/// version: one of another version may answer differently, and that is not found out until it
/// does (docs/cli.ja.md §4.12). A record with no version is from a ch32rv before this check.
/// ja: endpoint の記録 `v` を書いたのが別の ch32rv ならその名前、この ch32rv のブローカーなら
/// None。client は同じ取り決めと版のブローカーとだけ話す。
fn other_version(v: &Value) -> Option<String> {
    let protocol = v.get("protocol").and_then(Value::as_u64);
    let version = v.get("version").and_then(Value::as_str);
    if protocol == Some(BROKER_PROTOCOL) && version == Some(VERSION) {
        return None;
    }
    let pid = v.get("pid").and_then(Value::as_u64).unwrap_or(0);
    Some(match version {
        Some(ver) => format!("ch32rv {ver} (pid {pid})"),
        None => format!("a ch32rv older than the broker version check (pid {pid})"),
    })
}

/// Whether an open's payload (lease force [TLV]) carries `SHARE_EXTENSION`.
fn opens_shared(payload: &[u8]) -> bool {
    payload
        .get(5..)
        .and_then(|b| {
            ch32rv_oep::codec::request_tlvs_named(
                b,
                &[oep_core::tlvs::open::OWNER],
                &[ch32rv_oep::session::SHARE_EXTENSION],
            )
            .ok()
        })
        .is_some_and(|t| {
            t.iter()
                .any(|t| t.tag & 0x7F == registry::constants::TAG_NAMED_EXTENSION)
        })
}

/// rejected locked: remaining_ms(u32) [TLV owner] (core §4.3).
fn locked(corr: u16, remaining_ms: u32, owner: Option<&[u8]>) -> Vec<u8> {
    let mut p = remaining_ms.to_le_bytes().to_vec();
    if let Some(o) = owner {
        ch32rv_oep::codec::put_tlv(&mut p, oep_core::tlvs::locked_payload::OWNER, false, o);
    }
    encode_result(corr, Resolution::Rejected(reject_reasons::LOCKED), &p)
}

// ---- where a broker is ----

/// en: The key a probe's broker files are named by: the probe's identity, so every way to reach
/// one probe (`oep://` with any `via`, the path of either of its CDC ports) meets the same broker
/// (docs/freeze-decisions.ja.md §4; host guide §5.3: a lock key ignores `via`). An OEP USB
/// device's serial is its unit id and is read without opening it; a serial port given as it is
/// with no OEP USB device behind it (a UART bridge) is known only by its path, since the probe
/// cannot be opened to learn more (the broker holds it).
/// ja: ブローカーの file の key は probe の同一性。同じ probe へのどの道(どの `via` の `oep://`、
/// どちらの CDC の path)も同じブローカーに着く。OEP の USB device の serial は unit id で、開かずに
/// 読める。OEP の USB device を持たない serial port(UART bridge)は path でしか分からない。
fn key_for(path: &str) -> String {
    let sel = ch32rv_usb::Selector::Port(path.to_owned());
    let dev = crate::oep::oep_devices()
        .into_iter()
        .enumerate()
        .find(|(i, d)| sel.matches(d, *i))
        .map(|(_, d)| d);
    match dev.and_then(|d| d.port_id()) {
        Some(id) => unit_key(&id),
        None => ch32rv_usb::sanitize_key(&format!("oep-port-{}", ch32rv_usb::normalize_port(path))),
    }
}

/// The key of the probe with unit id `unit`, whatever the route (case aside, oep-transports §3).
fn unit_key(unit: &str) -> String {
    ch32rv_usb::sanitize_key(&format!("oep-{}", unit.to_ascii_lowercase()))
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

/// en: Remove what brokers that no longer run left in the runtime directory: an endpoint file
/// whose broker does not hold its flock (it died), and, older than a day, a kept session id or
/// an address's unit id; logs and lock files older than a week (a lock only when not held).
/// Only ch32rv's broker files (`oep-*`, `wch-*`); `own` (this broker's key) is left alone.
/// ja: もう動いていないブローカーが runtime の場所に残したものを消す(死んだブローカーの endpoint、
/// 1 日より古い session id と unit id、1 週間より古い log と lock)。
fn sweep_runtime(own: &str) {
    const DAY: Duration = Duration::from_secs(24 * 3600);
    let Ok(dir) = std::fs::read_dir(ch32rv_usb::runtime_dir()) else {
        return;
    };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("oep-") || name.starts_with("wch-")) || name.starts_with(own) {
            continue;
        }
        let age = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .unwrap_or_default();
        let held = |key: &str| {
            ch32rv_usb::DeviceLock::acquire(&format!("{key}.broker"), Duration::ZERO).is_err()
        };
        let stale = if let Some(key) = name.strip_suffix(".oep") {
            !held(key)
        } else if name.ends_with(".sid") || name.ends_with(".unit") {
            age > DAY
        } else if let Some(key) = name.strip_suffix(".broker.lock") {
            age > 7 * DAY && !held(key)
        } else if name.ends_with(".broker.log") || name.ends_with(".broker.log.1") {
            age > 7 * DAY
        } else {
            false
        };
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
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

/// en: Write the endpoint file atomically (0600 on unix), with this broker's protocol and version.
/// ja: endpoint の file を書く(0600、rename で置き換え)。このブローカーの取り決めの番号と版を入れる。
fn write_endpoint(key: &str, v: &Value) -> std::io::Result<()> {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        o.insert("protocol".to_owned(), json!(BROKER_PROTOCOL));
        o.insert("version".to_owned(), json!(VERSION));
    }
    let dir = ch32rv_usb::create_runtime_dir()?;
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
    /// An OEP probe on this serial port, given as it is (no unit named).
    Serial(String),
    /// en: The OEP probe `oep://<unit>` names, on USB (`path` is its serial port, which finds the
    /// device; `via` none or `usb|bulk|hid|cdc`) or on the serial port `via=tty:<path>` names.
    /// Keyed by unit id. ja: `oep://<unit>` の probe(USB、または `via=tty:` の serial port)。
    Oep {
        unit: String,
        path: String,
        via: Option<ch32rv_oep::address::Via>,
    },
    /// An OEP probe on TCP: its endpoint, its unit id, and the `via` it was named with (none or
    /// `net`: the broker finds it by DNS-SD again at each start; `tcp:` that endpoint). Keyed by
    /// unit id - the key a USB probe of that unit id has - so every way to one probe meets one
    /// broker.
    Net {
        addr: String,
        unit: String,
        via: Option<ch32rv_oep::address::Via>,
    },
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
                    selector: format!("topology:{t}"),
                }
            }
        }
    }

    pub(crate) fn key(&self) -> String {
        match self {
            BrokerTarget::Serial(p) => key_for(p),
            BrokerTarget::Oep { unit, .. } | BrokerTarget::Net { unit, .. } => unit_key(unit),
            BrokerTarget::Wch { id, .. } => ch32rv_usb::sanitize_key(&format!("wch-{id}")),
        }
    }

    /// en: What the broker is started with (`broker serve --probe`): the address with the route
    /// it keeps to. ja: ブローカーを起動するときの `--probe`(守る経路つきのアドレス)。
    fn selector(&self) -> String {
        use ch32rv_oep::address::Via;
        match self {
            BrokerTarget::Serial(p) => p.clone(),
            BrokerTarget::Oep { unit, via, .. } => match via {
                Some(v) => format!("oep://{unit}?via={v}"),
                None => format!("oep://{unit}?via=usb"),
            },
            // By unit id the broker resolves it again by DNS-SD (the address may change when the
            // probe joins its Wi-Fi again); a `tcp:` endpoint stays that endpoint.
            BrokerTarget::Net { unit, via, .. } => match via {
                Some(v @ Via::Tcp(_)) => format!("oep://{unit}?via={v}"),
                _ => format!("oep://{unit}?via=net"),
            },
            BrokerTarget::Wch { selector, .. } => selector.clone(),
        }
    }

    /// en: Whether a running broker over `transport` (its endpoint record's) keeps to the route
    /// the address picked: without `via` any broker of the probe does. A broker serves every
    /// route to its probe (one lock key), so one over another route cannot honour this `via`.
    /// ja: 動いているブローカーの経路が、アドレスの選んだ経路に合うか。`via` が無ければどれでもよい。
    fn accepts(&self, transport: &str) -> bool {
        use ch32rv_oep::address::Via;
        let via = match self {
            BrokerTarget::Oep { via: Some(v), .. } | BrokerTarget::Net { via: Some(v), .. } => v,
            _ => return true,
        };
        match via {
            Via::Usb => matches!(transport, "vendor-bulk" | "hid" | "serial"),
            Via::Bulk => transport == "vendor-bulk",
            Via::Hid => transport == "hid",
            Via::Cdc | Via::Tty(_) => transport == "serial",
            Via::Net | Via::Tcp(_) => transport == "tcp",
        }
    }
}

/// Start `ch32rv broker serve` for `target`, detached from this process and its group.
fn spawn_broker(target: &BrokerTarget) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["broker", "serve", "--probe", &target.selector()])
        // How long it waits for a probe another host holds, and one transfer's limit: the
        // command's that started it.
        .args([
            "--lock-timeout".to_owned(),
            crate::oep::lock_wait().as_secs().to_string(),
        ]);
    let timeout = crate::oep::LINK_TIMEOUT_S.load(std::sync::atomic::Ordering::Relaxed);
    if timeout != 0 {
        cmd.args(["--timeout".to_owned(), timeout.to_string()]);
    }
    cmd.stdin(std::process::Stdio::null())
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

/// en: Why no link to a broker came: `busy` when the broker found the probe held by another host
/// through `--lock-timeout`, or when a broker of another ch32rv kept the probe that long
/// (`other_version`); both exit 13. ja: ブローカーへの link が来なかった理由。`busy` は、ブローカーが
/// `--lock-timeout` の間 probe を他の host に持たれていたとき、または別の版の ch32rv のブローカーが
/// その間 probe を持ち続けたとき(`other_version`)。どちらも exit 13。
#[derive(Debug)]
pub(crate) struct ClientLinkError {
    pub(crate) msg: String,
    pub(crate) busy: bool,
    pub(crate) other_version: bool,
}

impl From<String> for ClientLinkError {
    fn from(msg: String) -> Self {
        Self {
            msg,
            busy: false,
            other_version: false,
        }
    }
}

impl From<&str> for ClientLinkError {
    fn from(msg: &str) -> Self {
        msg.to_owned().into()
    }
}

impl From<ClientLinkError> for String {
    fn from(e: ClientLinkError) -> Self {
        e.msg
    }
}

/// en: A broker of another ch32rv (`who`) still held the probe after `--lock-timeout`.
/// ja: 別の版の ch32rv のブローカー(`who`)が `--lock-timeout` の後も probe を持っていた。
fn other_version_busy(who: &str) -> ClientLinkError {
    ClientLinkError {
        msg: format!(
            "the probe's broker is {who}, not ch32rv {VERSION}, and it was still running after --lock-timeout ({} s)",
            crate::oep::lock_wait().as_secs()
        ),
        busy: true,
        other_version: true,
    }
}

/// What an endpoint record leads to.
enum Found {
    /// A connection to a broker of this ch32rv.
    Link(Box<Link>),
    /// A running broker of another ch32rv (who it is).
    Other(String),
    /// Nothing to use: an error record, a broker that is gone or on its way out.
    Nothing,
}

/// en: Connect to the broker endpoint record `v` names. A connection counts only if the endpoint
/// still names the same broker once connected: a broker about to end takes its endpoint down
/// first and then still accepts for a moment, so a client that read the file just before would
/// otherwise talk to a process on its way out. A broker of another ch32rv counts while it holds
/// its flock (the record of one that died is no one's), named by its record.
/// ja: endpoint の記録 `v` のブローカーにつなぐ。つないだ後も endpoint が同じブローカーを指すときだけ
/// 有効(終わりかけのブローカーは先に endpoint を下ろしてからも少し受け付ける)。別の版の ch32rv の
/// ブローカーは flock を持っている間だけ数える。版は記録で確かめる。
fn connect_endpoint(key: &str, v: &Value) -> Found {
    let (Some(port), Some(pid)) = (
        v.get("port").and_then(Value::as_u64),
        v.get("pid").and_then(Value::as_u64),
    ) else {
        return Found::Nothing;
    };
    if let Some(who) = other_version(v) {
        let alive =
            ch32rv_usb::DeviceLock::acquire(&format!("{key}.broker"), Duration::ZERO).is_err();
        return if alive {
            Found::Other(who)
        } else {
            Found::Nothing
        };
    }
    let Ok(l) = ch32rv_oep::link::open_tcp(&format!("127.0.0.1:{port}")) else {
        return Found::Nothing;
    };
    if read_endpoint(key).and_then(|w| w.get("pid")?.as_u64()) != Some(pid) {
        return Found::Nothing;
    }
    Found::Link(Box::new(l))
}

/// en: A link to the broker of `target`, starting the broker when none answers. An endpoint the
/// broker wrote before this call counts only if it answers; an error it wrote counts only if it
/// is newer than our start. A broker of another ch32rv is waited for up to `--lock-timeout` (it
/// ends when its last client leaves; then this one starts its own), then exit 13 naming it.
/// ja: `target` のブローカーへの link。答えるブローカーが無ければ起動する。別の版の ch32rv の
/// ブローカーは `--lock-timeout` まで待ち(最後の client が抜けると終わるので、そのあと自分のを
/// 起動する)、終わらなければ相手の版と PID を出して exit 13。
pub(crate) fn client_link_for(target: &BrokerTarget) -> Result<Link, ClientLinkError> {
    let key = target.key();
    let lock_wait = crate::oep::lock_wait();
    if let Some(v) = read_endpoint(&key)
        && let Some(t) = v.get("transport").and_then(Value::as_str)
        && !target.accepts(t)
        && other_version(&v).is_none()
        && broker_runs(target)
    {
        return Err(ClientLinkError {
            msg: format!(
                "the probe's broker runs over {t}, not the route {} names; it ends when the last \
                 command using the probe (monitor, gdb) ends",
                target.selector()
            ),
            busy: true,
            other_version: false,
        });
    }
    // The other ch32rv's broker in the way, and since when.
    let mut other: Option<(String, Instant)> = None;
    if let Some(v) = read_endpoint(&key) {
        match connect_endpoint(&key, &v) {
            Found::Link(l) => return Ok(*l),
            Found::Other(who) => other = Some((who, Instant::now())),
            Found::Nothing => {}
        }
    }
    let t0 = now_ms();
    spawn_broker(target)?;
    let mut spawned = Instant::now();
    // The broker may wait `--lock-timeout` for a probe another host holds before it answers.
    let mut deadline = Instant::now() + START_WAIT + lock_wait;
    loop {
        if let Some((who, since)) = &other
            && since.elapsed() >= lock_wait
        {
            return Err(other_version_busy(who));
        }
        if Instant::now() >= deadline {
            break;
        }
        let mut respawn = spawned.elapsed() >= RESPAWN_EVERY;
        if let Some(v) = read_endpoint(&key) {
            let fresh = v.get("time").and_then(Value::as_u64).unwrap_or(0) + 50 >= t0;
            match connect_endpoint(&key, &v) {
                Found::Link(l) => return Ok(*l),
                Found::Other(who) => {
                    let since = other.as_ref().map_or_else(Instant::now, |o| o.1);
                    other = Some((who, since));
                    respawn = false;
                }
                Found::Nothing => {
                    if fresh
                        && other_version(&v).is_none()
                        && let Some(e) = v.get("error").and_then(Value::as_str)
                    {
                        return Err(ClientLinkError {
                            msg: e.to_owned(),
                            busy: v.get("busy").and_then(Value::as_bool).unwrap_or(false),
                            other_version: false,
                        });
                    }
                    // en: A record left by a broker that died (of any version): the one we
                    // started may have met its flock still held a moment ago, so start again.
                    // ja: 死んだブローカーの記録。起動したものが直前の flock に当たったかもしれない。
                    respawn &= other_version(&v).is_some();
                }
            }
        } else if other.take().is_some() {
            // en: Another ch32rv's broker went: ours starts now, with its full start time.
            // ja: 別の版のブローカーが終わった。今から自分のを起動し、起動の待ちを取り直す。
            deadline = deadline.max(Instant::now() + START_WAIT + lock_wait);
            respawn = true;
        }
        // en: The one we started may have found the old broker still holding the probe (its lock)
        // and left; start another now that the old one may be gone (a second one leaves at once
        // while the first runs: the flock). ja: 起動したものが古いブローカーの lock に当たって
        // 終わったかもしれないので、もう一度起動する(動いていれば flock で 2 つ目はすぐ終わる)。
        if respawn {
            spawn_broker(target)?;
            spawned = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    Err(format!(
        "the broker for {} did not start within {} s",
        target.selector(),
        (START_WAIT + lock_wait).as_secs()
    )
    .into())
}

/// en: Whether `addr` (`host:port`) is a ch32rv broker on this host, from the endpoint files: such
/// an address is a broker's client port, opened directly (a broker is never put in front of a
/// broker). ja: `addr` がこの PC の ch32rv のブローカーか(endpoint の file から)。そうなら直接つなぐ。
pub(crate) fn is_broker_endpoint(addr: &str) -> bool {
    broker_record_at(addr).is_some()
}

/// en: Which other ch32rv the broker at `addr` is (as [`other_version`] reads its endpoint
/// record), or `None` for a broker of this ch32rv or no broker. The version lives in the record
/// only: the confirm answer must fit 64 bytes (core §7.1).
/// ja: `addr` のブローカーが別の版の ch32rv ならその名前。版は endpoint の記録だけが持つ。
pub(crate) fn other_broker_at(addr: &str) -> Option<String> {
    other_version(&broker_record_at(addr)?)
}

/// The endpoint record of the running broker whose client port is `addr` (`127.0.0.1:<port>`).
fn broker_record_at(addr: &str) -> Option<Value> {
    let (host, port) = addr.rsplit_once(':')?;
    if !matches!(host, "127.0.0.1" | "localhost") {
        return None;
    }
    let port = port.parse::<u64>().ok()?;
    std::fs::read_dir(ch32rv_usb::runtime_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "oep"))
        .filter_map(|e| {
            let key = e.path().file_stem()?.to_string_lossy().into_owned();
            let v: Value = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
            Some((key, v))
        })
        .filter(|(_, v)| v.get("port").and_then(Value::as_u64) == Some(port))
        // Only a broker that still runs (holds its flock): a broker that died leaves its file,
        // and its port may now be anyone's.
        .find(|(key, _)| {
            ch32rv_usb::DeviceLock::acquire(&format!("{key}.broker"), Duration::ZERO).is_err()
        })
        .map(|(_, v)| v)
}

/// en: A link to `target`'s broker if one of this ch32rv runs; never starts one (a broker of
/// another ch32rv is not talked to). ja: この版のブローカーが動いていればその link(起動しない)。
pub(crate) fn existing_link_for(target: &BrokerTarget) -> Option<Link> {
    let v = read_endpoint(&target.key())?;
    if other_version(&v).is_some() {
        return None;
    }
    let port = v.get("port")?.as_u64()?;
    ch32rv_oep::link::open_tcp(&format!("127.0.0.1:{port}")).ok()
}

/// en: Whether a broker of any ch32rv answers for `target` (to send a command through the broker,
/// where a broker of another version is waited for and refused, not gone around).
/// ja: どの版でも `target` のブローカーが答えるか(コマンドをブローカーに回すかの判断用。別の版は
/// 回した先で待って断る。避けて直接開くことはしない)。
pub(crate) fn broker_runs(target: &BrokerTarget) -> bool {
    read_endpoint(&target.key())
        .and_then(|v| v.get("port")?.as_u64())
        .is_some_and(|port| TcpStream::connect(("127.0.0.1", port as u16)).is_ok())
}

/// A link to the OEP probe's broker if one runs; never starts one (discovery only looks).
pub(crate) fn existing_link(path: &str) -> Option<Link> {
    existing_link_for(&BrokerTarget::Serial(path.to_owned()))
}

/// The runtime file discovery keeps a probe's last listing in (for when the probe is busy), by `key`.
pub(crate) fn listing_cache(key: &str) -> PathBuf {
    ch32rv_usb::runtime_dir().join(format!("{}.ports.json", key_for(key)))
}

/// en: An address whose `via=tcp:` is a ch32rv broker on this host. ja: ブローカー自身のアドレス。
const BROKER_ITSELF: &str = "this address's via=tcp: endpoint is a broker itself";

/// `broker endpoint --probe <sel> [--json]`: where the probe's broker listens, if it runs.
pub(crate) fn endpoint(cli: &Cli) -> ExitCode {
    const CMD: &str = "broker.endpoint";
    let target = match crate::oep::addr(cli, CMD) {
        Ok(Some(a)) => match a.broker_target() {
            Some(t) => t,
            None => return fail(cli, CMD, ErrorKind::Usage, BROKER_ITSELF, None),
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
    // en: The running broker's field, null when none runs (or, for `version`, when the broker is
    // of a ch32rv before the field). ja: 動いているブローカーの field。無ければ null。
    let field = |name: &str| {
        live.as_ref()
            .and(v.as_ref().and_then(|v| v.get(name).cloned()))
            .unwrap_or(Value::Null)
    };
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(json!({
            "endpoint": live,
            "pid": field("pid"),
            "transport": field("transport"),
            "version": field("version"),
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
    /// connection -> (its wire fn, the clients using it, the attach answer to lend it with)
    conns: HashMap<u32, (u16, BTreeSet<u64>, Vec<u8>)>,
    /// client -> the fns it has plans on
    plans: HashMap<u64, BTreeSet<u16>>,
    /// fn -> the client whose subscribe is the fn's one subscription (core §10.3: one per fn, a
    /// second subscribe replaces it): that fn's pushes go to that client.
    subs: HashMap<u16, u64>,
}

/// What to answer, in the client's order.
enum Pending {
    Local(u64, Vec<u8>),
    Forward(u64, Request),
}

/// `broker serve --probe <address>` (not for users; clients start it).
pub(crate) fn serve(cli: &Cli) -> ExitCode {
    const CMD: &str = "broker.serve";
    // An OEP probe (an address, or a serial port), else a WCH-Link (any selector that resolves
    // to one).
    let target = match crate::oep::addr(cli, CMD) {
        // A broker for this Link already answers: this one leaves (the flock would say so too).
        Ok(Some(OepAddr::Wch(_))) => return ExitCode::SUCCESS,
        Ok(Some(a)) => match a.broker_target() {
            Some(t) => t,
            None => return fail(cli, CMD, ErrorKind::Usage, BROKER_ITSELF, None),
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
    sweep_runtime(&key);
    let report_error = |m: String| {
        let _ = write_endpoint(
            &key,
            &json!({"error": m, "pid": std::process::id(), "time": now_ms()}),
        );
        ExitCode::from(ErrorKind::DeviceOpenFailed.exit_code())
    };
    // en: The probe's lock, held by another host through `--lock-timeout`: its clients exit 13.
    // ja: probe の lock を `--lock-timeout` の間 他の host が持っていた。client は exit 13。
    let report_lock = |e: ch32rv_oep::session::OepError| {
        let busy = matches!(e, ch32rv_oep::session::OepError::Locked { .. });
        let _ = write_endpoint(
            &key,
            &json!({"error": e.to_string(), "busy": busy, "pid": std::process::id(), "time": now_ms()}),
        );
        let kind = if busy {
            ErrorKind::DeviceBusy
        } else {
            ErrorKind::DeviceOpenFailed
        };
        ExitCode::from(kind.exit_code())
    };
    let mut transport = "wchlink";
    // The UART bridge port_speed raised, while it stays raised.
    let mut speed_port: Option<u8> = None;
    let mut speed_ratio_rule = false;
    let (up, sid) = match (&target, wch_entry) {
        (BrokerTarget::Serial(path), _) | (BrokerTarget::Oep { path, .. }, _) => {
            let (via, unit) = match &target {
                BrokerTarget::Oep { via, unit, .. } => (via.as_ref(), Some(unit)),
                _ => (None, None),
            };
            let mut probe = match crate::oep::connect_upstream(path, via, None) {
                Ok((p, t)) => {
                    transport = t;
                    p
                }
                Err(m) => return report_error(format!("{path}: {m}")),
            };
            // The unit the address named, checked once open (host guide §5.3).
            if let Some(u) = unit
                && let Err(m) = crate::oep::verify_unit(&mut probe, u)
            {
                return report_error(format!("{path}: {m}"));
            }
            crate::oep::apply_timeout(probe.link());
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
                Err(e) => return report_lock(e),
            };
            save_sid(&key, sid);
            (Upstream::Oep(Box::new(probe)), sid)
        }
        (BrokerTarget::Net { addr, unit, .. }, _) => {
            transport = "tcp";
            let mut probe = match crate::oep::open_net(addr, unit) {
                Ok(p) => p,
                Err(m) => return report_error(m),
            };
            crate::oep::apply_timeout(probe.link());
            let owner = format!("ch32rv broker pid {}", std::process::id());
            if let Some(old) = saved_sid(&key)
                && probe.open(old, LEASE_MS, false, Some(&owner)).is_ok()
            {
                let _ = probe.end();
                broker_log(&key, "ended the previous broker's session");
            }
            let sid = match crate::oep::open_with_lock_rule(&mut probe, false, &owner, LEASE_MS) {
                Ok(s) => s,
                Err(e) => return report_lock(e),
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
    let max_frame = usize::from(up.limits().max_frame);
    std::thread::spawn(move || accept_loop(listener, tx, max_frame));
    // en: A UART bridge's link is raised for the broker's long session (oep-if-link §3, by default
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
        core: CoreLock::default(),
        awaiting: HashMap::new(),
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
    // What was queued for the clients goes out before the broker ends (a restart's answer is the
    // last thing it passes on). A client that is not reading is waited for 2 s at most.
    let deadline = Instant::now() + Duration::from_secs(2);
    for (_, c) in b.clients.drain() {
        c.finish(deadline);
    }
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

    /// en: Whether `func` offers `op`, from its describe's ops (core §4.3: unknown_function and
    /// unknown_operation come before the session checks). `None` when it cannot be told (a fn
    /// with no ops tag): the probe answers. ja: `func` が `op` を持つか(describe の ops から)。
    fn declares(&mut self, func: u16, op: u8) -> Option<Result<(), u8>> {
        let ops = match self {
            Upstream::Oep(p) => match p.ops(func) {
                Ok(o) => o?,
                Err(ch32rv_oep::session::OepError::Rejected { reason, .. })
                    if reason == reject_reasons::UNKNOWN_FUNCTION =>
                {
                    return Some(Err(reason));
                }
                Err(_) => return None,
            },
            Upstream::Wch(_) => match crate::broker_wch::declared_ops(func) {
                Some(o) => o.to_vec(),
                None => return Some(Err(reject_reasons::UNKNOWN_FUNCTION)),
            },
        };
        Some(if ops.contains(&op) {
            Ok(())
        } else {
            Err(reject_reasons::UNKNOWN_OPERATION)
        })
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

    /// en: The pushes that came, after reading for at most `wait` (none for a WCH-Link, which has
    /// no subscribe). ja: 来た通知(`wait` まで読む)。WCH-Link は購読が無いので無い。
    fn pushes(&mut self, wait: Duration) -> Result<Vec<ch32rv_oep::link::Push>, String> {
        match self {
            Upstream::Oep(p) => {
                let l = p.link();
                if !wait.is_zero() {
                    l.poll_pushes(wait).map_err(|e| e.to_string())?;
                }
                Ok(l.take_pushes())
            }
            Upstream::Wch(_) => Ok(Vec::new()),
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
    /// now, then (raised) at the boot speed (oep-if-link §3 host). ja: probe がまだ答えるか。
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

/// How much a client may have queued and unread before its pushes are dropped.
const CLIENT_BACKLOG: usize = 32 << 20;

/// en: What goes out to one client: its own writer thread, so a client that stops reading (one
/// blocked in a call of its own, waiting for another client the broker serves) never stalls the
/// broker and every other client with it. Results are always queued; a push (event or data)
/// past [`CLIENT_BACKLOG`] unread bytes is dropped, as the probe drops what it cannot send
/// (core §10.4: the client sees the gap in its positions). Measured: a client streaming a
/// capture through the broker that waited for a `ch32rv reset --halt` of its own stopped
/// reading; the broker's write to it blocked for 44.5 s, so the reset's answer never went out
/// and the probe's lease lapsed (no_session).
/// ja: client ごとの書込み thread。読まない client が broker と他の client を止めない。結果は必ず
/// 送り、通知は未読が上限を超えたら捨てる(probe と同じ)。
struct ClientOut {
    tx: mpsc::Sender<Vec<u8>>,
    writer: std::thread::JoinHandle<()>,
    queued: Arc<AtomicUsize>,
    /// Pushes dropped so far.
    dropped: u64,
}

impl ClientOut {
    fn spawn(mut s: TcpStream) -> Self {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let q = queued.clone();
        let writer = std::thread::spawn(move || {
            for frame in rx {
                let n = frame.len();
                let ok = s.write_all(&frame).is_ok();
                q.fetch_sub(n, Ordering::Relaxed);
                if !ok {
                    // Its reader sees the end and says it is gone.
                    let _ = s.shutdown(std::net::Shutdown::Both);
                    break;
                }
            }
        });
        Self {
            tx,
            writer,
            queued,
            dropped: 0,
        }
    }

    /// en: Let what is queued go out before the broker ends, waiting at most until `deadline`
    /// (a client that does not read is not waited for past it): an answer the broker passed on
    /// last - a restart's, say - reaches its client. ja: 終わる前に、キューを送り切る(期限まで)。
    fn finish(self, deadline: Instant) {
        drop(self.tx);
        while !self.writer.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Queue `frame`; false when it was a push dropped for the backlog.
    fn queue(&mut self, frame: Vec<u8>, push: bool) -> bool {
        if push && self.queued.load(Ordering::Relaxed) > CLIENT_BACKLOG {
            self.dropped += 1;
            return false;
        }
        self.queued.fetch_add(frame.len(), Ordering::Relaxed);
        let _ = self.tx.send(frame);
        true
    }
}

fn accept_loop(listener: TcpListener, tx: mpsc::Sender<Ev>, max_frame: usize) {
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
        std::thread::spawn(move || read_loop(id, reader, tx, max_frame));
    }
}

/// en: A client's messages. One longer than the broker's max_frame closes the connection, as a
/// TCP probe does (oep-transports §1). ja: max_frame を超える message は接続を閉じる。
fn read_loop(id: u64, mut s: TcpStream, tx: mpsc::Sender<Ev>, max_frame: usize) {
    let mut d = LengthDeframer::new(max_frame);
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
                Err(_) => {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                    break;
                }
            },
        }
    }
    let _ = tx.send(Ev::Gone(id));
}

struct Broker {
    up: Upstream,
    sid: u32,
    wires: BTreeSet<u16>,
    clients: HashMap<u64, ClientOut>,
    /// The ch32rv tools' shared sessions (opened with `SHARE_EXTENSION`), by client.
    sessions: HashMap<u64, u32>,
    /// The one lock for everyone else, as at a probe (core §5.1, §6).
    core: CoreLock,
    /// Requests of the lock's last session not answered yet: (client, corr) -> session id, so
    /// that their answers are kept for resends.
    awaiting: HashMap<(u64, u16), u32>,
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
    /// judge by the share over 3 s instead of 2 broken in 5 s.
    speed_ratio_rule: bool,
    /// The link's `speed_fallbacks` already logged.
    fallbacks_seen: u64,
    /// The rates to raise to again after the speed went down (None: port_speed off or never
    /// committed), when to try next, and how many tries are left this session.
    raise_rates: Option<Vec<u32>>,
    raise_again_at: Option<Instant>,
    raises_left: u32,
}

/// How long one read of the probe's link waits for pushes between requests while a fn is
/// subscribed: also how late a client's request can be picked up.
const PUSH_POLL: Duration = Duration::from_millis(4);

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
            // en: While a fn is subscribed, the probe's link is read between requests too, a few
            // ms at a time, and its pushes relayed (core §10.4: the probe drops what it cannot
            // send). ja: 購読がある間は、要求の合間も probe の link を数 ms ずつ読み、通知を中継する。
            let streaming = !self.ledger.subs.is_empty();
            if streaming && carried.is_none() {
                self.relay_after(PUSH_POLL)?;
            }
            let wait = if streaming {
                Duration::from_millis(1)
            } else {
                self.up.tick()
            };
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
                    if let Some(c) = self.core.expire(Instant::now()) {
                        self.release_quiet(c, "lease lapse");
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
                        self.clients.insert(id, ClientOut::spawn(s));
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
                        // The one lock's session outlives its connection until its lease
                        // ends (transports §3); what it holds goes then.
                        let holds = self.core.holder.as_ref().is_some_and(|h| h.client == id);
                        if !holds && let Err(e) = self.release(id) {
                            broker_log(&self.key, &format!("client {id}: release failed: {e}"));
                        }
                        self.awaiting.retain(|(c, _), _| *c != id);
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
                // A shared session ends alone; the one lock's end frees it (core §6.4).
                if req.session.is_some() && self.sessions.get(&id) == req.session.as_ref() {
                    self.sessions.remove(&id);
                    self.release_quiet(id, "end");
                } else if let Some(c) = self.core.end() {
                    self.release_quiet(c, "end");
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

    /// en: A client's restart (oep-if-restart §2, host guide §5.2): forwarded under the broker's session like
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
        // en: Pushes read while the answers were awaited came before them on the probe's link;
        // they go out first (a subscribe's answer lands after the pushes of the subscription it
        // replaced, as on the probe's own path, core §10.3).
        // ja: 答えを待つ間に読んだ通知は、答えより先に来たもの。先に送る。
        self.relay()?;
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

    /// en: Send the pushes the link holds to the fns' subscribers, reading for at most `wait`
    /// first (zero: only what an exchange already read). A push of a fn nobody here subscribed
    /// to (a subscription that ended, its last frames still on the way) is dropped. Nothing is
    /// held back: a stream's pushes go on at once, so the probe's queue never backs up (core
    /// §10.4). ja: link にある通知を購読者へ送る(先に `wait` まで読む)。購読者の居ない fn の通知は
    /// 捨てる。ためずにすぐ送る。
    fn relay_after(&mut self, wait: Duration) -> Result<(), String> {
        let pushes = self.up.pushes(wait)?;
        for push in pushes {
            if let Some(&id) = self.ledger.subs.get(&push.func()) {
                self.send(id, &push.encode());
            }
        }
        Ok(())
    }

    fn relay(&mut self) -> Result<(), String> {
        self.relay_after(Duration::ZERO)
    }

    /// en: While the port speed is raised, watch how the frames fare and lower it together with
    /// the probe when they fare badly (oep-if-link §3: the host notices first; the probe's own
    /// fallback is the last resort); and note when the link found the probe back at the boot speed
    /// by itself. Either way the session stays at the boot speed. The rule depends on the probe:
    /// from oep-probe-arduino 0.0.27 (it goes back only at 3 broken frames in a row) the share of
    /// broken and lost frames over the last 3 s above 10 % (base 0, not judged
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
                "port_speed: a request at the raised speed got no answer (even resent), and the probe answered confirm at the boot speed (oep-if-link §3): at the boot speed for now",
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
    /// broker's open: the probe restarted (oep-core §6.5, oep-if-link §3 host). Say so and open a new
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
        self.core.end();
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
        if msg.first() == Some(&registry::roles::RESULT)
            && let Some(c) = msg.get(1..3)
        {
            let corr = u16::from_le_bytes([c[0], c[1]]);
            if let Some(sid) = self.awaiting.remove(&(id, corr)) {
                self.core.answered(sid, corr, msg, Instant::now());
            }
        }
        if let Some(c) = self.clients.get_mut(&id) {
            // A push to a client that has stopped reading is dropped past its backlog (a
            // result is always queued): see [`ClientOut`].
            let push = msg.first() != Some(&registry::roles::RESULT);
            if !c.queue(length_frame(msg), push) && c.dropped == 1 {
                broker_log(
                    &self.key,
                    &format!(
                        "client {id} is not reading: its pushes past {} MiB are dropped",
                        CLIENT_BACKLOG >> 20
                    ),
                );
            }
        }
    }

    /// Release what client `c` holds, logging a failure (the broker goes on).
    fn release_quiet(&mut self, c: u64, why: &str) {
        if let Err(e) = self.release(c) {
            broker_log(
                &self.key,
                &format!("client {c}: release at {why} failed: {e}"),
            );
        }
    }

    /// The broker's own answer to `req`, or `None` to forward it.
    fn local(&mut self, id: u64, req: &Request) -> Option<Vec<u8>> {
        let ok = |p: &[u8]| encode_result(req.corr, Resolution::Completed(outcomes::SUCCESS), p);
        let refuse = |r: u8| encode_result(req.corr, Resolution::Rejected(r), &[]);
        let open = req.func == oep_core::FN && req.op == oep_core::op::OPEN;
        // The header first, in core §4.3's order: corr 0 is malformed, then an unknown fn or op,
        // then a locking fn 0 op without a session is session_required (other fns' go on as
        // they are, and the probe answers them).
        if req.corr == 0 {
            return Some(refuse(reject_reasons::MALFORMED));
        }
        if let Some(Err(reason)) = self.up.declares(req.func, req.op) {
            return Some(refuse(reason));
        }
        if req.func == oep_core::FN
            && req.session.is_none()
            && oep_core::OPS.iter().any(|o| o.code == req.op && o.lock)
        {
            return Some(refuse(reject_reasons::SESSION_REQUIRED));
        }
        // en: The session checks (core §4.3 steps 2-3). A ch32rv tool's shared session is the
        // broker's to keep; anyone else meets the one lock as at a probe: the resend check, then
        // the session table. ja: 相乗りの session はそのまま。ほかは probe と同じ 1 つのロック。
        let now = Instant::now();
        if let Some(c) = self.core.expire(now) {
            self.release_quiet(c, "lease lapse");
        }
        let shared = req
            .session
            .is_some_and(|sid| self.sessions.get(&id) == Some(&sid));
        let share_open = open && opens_shared(&req.payload);
        if let Some(sid) = req.session
            && !shared
            && !share_open
        {
            let bytes = req.encode();
            match self.core.replay(sid, req.corr, &bytes) {
                Replay::Stored(a) => return Some(a),
                Replay::Lost => return Some(refuse(reject_reasons::RESULT_LOST)),
                Replay::Malformed => return Some(refuse(reject_reasons::MALFORMED)),
                Replay::New => {}
            }
            if !open {
                match self.core.access(sid, now) {
                    Access::Owner => {}
                    Access::NoSession => return Some(refuse(reject_reasons::NO_SESSION)),
                    Access::Locked(ms, owner) => {
                        return Some(locked(req.corr, ms, owner.as_deref()));
                    }
                }
                self.core.note(req.corr, bytes);
                self.awaiting.insert((id, req.corr), sid);
            }
        }
        if req.func == oep_core::FN {
            let op = req.op;
            // The fn 0 requests the broker answers: their fixed part, then TLVs (core §2.3).
            let shape: Option<(usize, &[u8])> = match op {
                o if o == oep_core::op::CONFIRM => Some((6, &[])),
                o if o == oep_core::op::OPEN => Some((5, &[oep_core::tlvs::open::OWNER])),
                o if o == oep_core::op::END
                    || o == oep_core::op::KEEPALIVE
                    || o == oep_core::op::LOCK_STATE =>
                {
                    Some((0, &[]))
                }
                _ => None,
            };
            if let Some((fixed, known)) = shape {
                use ch32rv_oep::codec::{RequestTlvError, request_tlvs_named};
                let names = [ch32rv_oep::session::SHARE_EXTENSION];
                match req
                    .payload
                    .get(fixed..)
                    .map(|b| request_tlvs_named(b, known, &names))
                {
                    None | Some(Err(RequestTlvError::Malformed)) => {
                        return Some(refuse(reject_reasons::MALFORMED));
                    }
                    Some(Err(RequestTlvError::Unsupported(tag))) => {
                        return Some(encode_result(
                            req.corr,
                            Resolution::Rejected(reject_reasons::UNSUPPORTED),
                            &[tag],
                        ));
                    }
                    Some(Ok(_)) => {}
                }
            }
            return match op {
                o if o == oep_core::op::CONFIRM => {
                    let l = self.up.limits();
                    // "OEP?" min_rev max_rev (core §7.1): another magic or min > max is
                    // malformed; a range without the revision is unsupported, naming the range.
                    let (min, max) = (req.payload[4], req.payload[5]);
                    if &req.payload[..4] != registry::constants::CONFIRM_REQUEST_MAGIC.as_bytes()
                        || min > max
                    {
                        return Some(refuse(reject_reasons::MALFORMED));
                    }
                    if !(min..=max).contains(&l.revision) {
                        let mut p = vec![0x00];
                        ch32rv_oep::codec::put_tlv(
                            &mut p,
                            oep_core::tlvs::unsupported_payload::SUPPORTED,
                            false,
                            &[l.revision, l.revision],
                        );
                        return Some(encode_result(
                            req.corr,
                            Resolution::Rejected(reject_reasons::UNSUPPORTED),
                            &p,
                        ));
                    }
                    let mut p = registry::constants::CONFIRM_RESULT_MAGIC
                        .as_bytes()
                        .to_vec();
                    // "OEP!" revision max_frame window max_inflight boot_id (core §7.1).
                    p.push(l.revision);
                    p.extend_from_slice(&l.max_frame.to_le_bytes());
                    p.extend_from_slice(&l.window.to_le_bytes());
                    p.push(l.max_inflight);
                    p.extend_from_slice(&l.boot_id.to_le_bytes());
                    // The transport index: the broker's one endpoint, entry 0 of its describe.
                    ch32rv_oep::codec::put_tlv(
                        &mut p,
                        oep_core::tlvs::confirm_answer::TRANSPORT,
                        false,
                        &[0],
                    );
                    Some(ok(&p))
                }
                // lease_ms(u32) force(u8) [TLV owner] (core §6.4); the session id is the
                // header's. With SHARE_EXTENSION it joins the ch32rv tools' shared lock, unless
                // someone else holds the one lock; without, it meets that lock (host guide §7).
                o if o == oep_core::op::OPEN => {
                    let Some(sid) = req.session else {
                        return Some(refuse(reject_reasons::MALFORMED));
                    };
                    let p = &req.payload;
                    let lease = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                    let force = match p[4] {
                        0 => false,
                        1 => true,
                        _ => return Some(refuse(reject_reasons::MALFORMED)),
                    };
                    let owner = ch32rv_oep::codec::parse_tlvs(&p[5..])
                        .unwrap_or_default()
                        .into_iter()
                        .find(|t| t.tag & 0x7F == oep_core::tlvs::open::OWNER)
                        .map(|t| t.value);
                    if owner.as_ref().is_some_and(|o| !(1..=32).contains(&o.len())) {
                        return Some(refuse(reject_reasons::MALFORMED));
                    }
                    let boot = self.up.boot_id().to_le_bytes();
                    if share_open {
                        if let Some((ms, owner)) = self.core.state(now) {
                            return Some(locked(req.corr, ms, owner.as_deref()));
                        }
                        let lease = if lease == 0 { LEASE_MS } else { lease }.clamp(1000, 60_000);
                        self.sessions.insert(id, sid);
                        let mut a = lease.to_le_bytes().to_vec();
                        a.extend_from_slice(&boot);
                        return Some(ok(&a));
                    }
                    let busy = (!self.sessions.is_empty()).then_some(LEASE_MS);
                    let bytes = req.encode();
                    match self
                        .core
                        .open(sid, id, lease, force, owner, busy, req.corr, bytes, now)
                    {
                        Opened::Held { lease_ms, stolen } => {
                            if let Some(c) = stolen {
                                self.release_quiet(c, "force");
                            }
                            // A force took the lock from the ch32rv tools that shared it: their
                            // sessions end, and their next requests are told so (host guide §7).
                            if busy.is_some() {
                                let shared: Vec<u64> =
                                    self.sessions.drain().map(|(c, _)| c).collect();
                                for c in shared {
                                    self.release_quiet(c, "force");
                                }
                                broker_log(
                                    &self.key,
                                    &format!(
                                        "client {id} forced the lock: the shared sessions end"
                                    ),
                                );
                            }
                            self.awaiting.insert((id, req.corr), sid);
                            let mut a = lease_ms.to_le_bytes().to_vec();
                            a.extend_from_slice(&boot);
                            Some(ok(&a))
                        }
                        Opened::Refused(Access::Locked(ms, owner)) => {
                            Some(locked(req.corr, ms, owner.as_deref()))
                        }
                        Opened::Refused(_) => Some(refuse(reject_reasons::NO_SESSION)),
                    }
                }
                o if o == oep_core::op::KEEPALIVE => Some(ok(&[])),
                // locked(u8) remaining_ms(u32) [TLV owner]: the one lock, or the ch32rv tools'
                // shared one (the broker's own session to the probe is neither, core §6.4).
                o if o == oep_core::op::LOCK_STATE => {
                    let (held, ms, owner) = match self.core.state(now) {
                        Some((ms, owner)) => (true, ms, owner),
                        None if !self.sessions.is_empty() => (true, LEASE_MS, None),
                        None => (false, 0, None),
                    };
                    let mut p = vec![u8::from(held)];
                    p.extend_from_slice(&ms.to_le_bytes());
                    if let Some(o) = owner {
                        ch32rv_oep::codec::put_tlv(
                            &mut p,
                            oep_core::tlvs::lock_state_answer::OWNER,
                            false,
                            &o,
                        );
                    }
                    Some(ok(&p))
                }
                _ => None,
            };
        }
        // en: The broker shares one connection to the probe among its clients (an attach creates
        // a new connection on the probe, and a second one on the same pins is refused, oep-if-debug
        // §1): an attach on a wire where the broker holds one is answered here with that
        // connection, marked by the named extension SHARED_EXTENSION, with its halted flag and
        // dpc dropped (a client that wants it halted halts it).
        // ja: ブローカーは probe への接続を 1 本だけ持ち、client に貸す。既にある wire の attach は
        // ここでその接続を返す(名前つき拡張で印をつけ、halted は落とす)。
        if self.wires.contains(&req.func) && req.op == wire_rvswd::op::ATTACH {
            // en: The probe closes a connection on its own when the line is lost (oep-if-debug
            // §2): one still in the ledger is lent only while the wire's connections (lock-free,
            // required) list it; a gone one leaves the ledger and the attach goes to the probe.
            // ja: 線を失うと probe は接続を自分で閉じる。connections に載っている間だけ貸す。
            let held = self
                .ledger
                .conns
                .iter()
                .find(|(_, (func, _, _))| *func == req.func)
                .map(|(&conn, _)| conn);
            if let Some(conn) = held
                && !self.upstream_has(id, req.func, conn)
            {
                self.ledger.conns.remove(&conn);
            }
            let held = self
                .ledger
                .conns
                .iter_mut()
                .find(|(_, (func, _, _))| *func == req.func);
            if let Some((_, (_, users, answer))) = held {
                // One rest per connection (oep-if-debug §3.1): an attach naming another rest than
                // the held connection's is refused as the probe refuses one on live pins.
                let rest = |tlvs: &[ch32rv_oep::codec::Tlv], tag: u8| {
                    tlvs.iter()
                        .find(|t| t.tag & 0x7F == tag && t.value.len() == 1)
                        .map(|t| t.value[0])
                };
                // Answered here, so the request's TLVs are checked here as the probe would
                // (core §2.3): a broken list or an attach TLV twice is malformed.
                let known = [
                    wire_rvswd::tlvs::attach::IDLE_CLOCK,
                    wire_rvswd::tlvs::attach::RESET,
                ];
                let asked = match ch32rv_oep::codec::request_tlvs(req.payload.get(9..)?, &known) {
                    Ok(t) => rest(&t, wire_rvswd::tlvs::attach::IDLE_CLOCK),
                    Err(ch32rv_oep::codec::RequestTlvError::Malformed) => {
                        return Some(refuse(reject_reasons::MALFORMED));
                    }
                    Err(ch32rv_oep::codec::RequestTlvError::Unsupported(tag)) => {
                        return Some(encode_result(
                            req.corr,
                            Resolution::Rejected(reject_reasons::UNSUPPORTED),
                            &[tag],
                        ));
                    }
                };
                let held_rest = answer
                    .get(13..)
                    .and_then(|b| parse_tlvs(b).ok())
                    .and_then(|t| rest(&t, wire_rvswd::tlvs::attach_answer::IDLE_CLOCK));
                if asked.is_some() && asked != held_rest {
                    let cause = ch32rv_oep::registry::core::enums::unavailable_cause::PIN_IN_USE;
                    let mut v = Vec::new();
                    ch32rv_oep::codec::put_tlv(
                        &mut v,
                        ch32rv_oep::registry::core::tlvs::unavailable_payload::CAUSE,
                        false,
                        &[cause],
                    );
                    return Some(encode_result(
                        req.corr,
                        Resolution::Rejected(reject_reasons::UNAVAILABLE),
                        &v,
                    ));
                }
                users.insert(id);
                let mut p = answer.clone();
                let name = ch32rv_oep::target::SHARED_EXTENSION.as_bytes();
                let mut v = vec![name.len() as u8];
                v.extend_from_slice(name);
                ch32rv_oep::codec::put_tlv(
                    &mut p,
                    registry::constants::TAG_NAMED_EXTENSION,
                    false,
                    &v,
                );
                return Some(ok(&p));
            }
        }
        // A detach while another client still uses the connection only drops this client's use.
        if self.wires.contains(&req.func) && req.op == wire_rvswd::op::DETACH {
            let b = req.payload.get(..4)?;
            let conn = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            let (_, users, _) = self.ledger.conns.get_mut(&conn)?;
            if users.len() > 1 || !users.contains(&id) {
                users.remove(&id);
                return Some(ok(&[]));
            }
        }
        None
    }

    /// Whether the probe still lists `conn` among the connections of wire `func`.
    fn upstream_has(&mut self, id: u64, func: u16, conn: u32) -> bool {
        let call = Call {
            func,
            op: wire_rvswd::op::CONNECTIONS,
            session: None,
            payload: vec![0],
        };
        let Ok(r) = self.up.exchange(vec![(id, call)]) else {
            return true;
        };
        let Some(r) = r.first().filter(|r| r.succeeded()) else {
            return true;
        };
        // more count, count x (connection(u32) swdio swclk speed_hz tid_scheme tid_len tid)
        let a = &r.payload;
        let count = usize::from(*a.get(1).unwrap_or(&0));
        let mut at = 2;
        for _ in 0..count {
            let Some(e) = a.get(at..at + 14) else {
                return true;
            };
            if u32::from_le_bytes([e[0], e[1], e[2], e[3]]) == conn {
                return true;
            }
            at += 14 + usize::from(e[13]);
        }
        false
    }

    /// Record what a forwarded request changed in the ledger.
    fn note(&mut self, id: u64, req: &Request, r: &Reply) {
        if !r.succeeded() {
            return;
        }
        // en: A subscribe (0x01 / 0x02 in every interface's op space, core §10.3) goes to the
        // probe under the broker's one session, so the probe sees one subscriber per fn; the
        // broker keeps who asked and sends that fn's pushes there. Another client's subscribe
        // replaces it, as at the probe; an unsubscribe from anyone ends it.
        // ja: subscribe はブローカーの 1 つの session で probe へ行く。誰が頼んだかを覚え、その fn の
        // 通知をその client へ送る。別の client の subscribe は置き換え、unsubscribe は誰からでも外す。
        if req.func != oep_core::FN && req.op == registry::constants::OP_SUBSCRIBE {
            if let Some(prev) = self.ledger.subs.insert(req.func, id)
                && prev != id
            {
                broker_log(
                    &self.key,
                    &format!(
                        "fn {}: client {id}'s subscribe replaces client {prev}'s",
                        req.func
                    ),
                );
            }
            return;
        }
        if req.func != oep_core::FN && req.op == registry::constants::OP_UNSUBSCRIBE {
            self.ledger.subs.remove(&req.func);
            return;
        }
        if self.wires.contains(&req.func) {
            match req.op {
                o if o == wire_rvswd::op::ATTACH => {
                    // connection(u32) id(u32) flags(u8) speed_hz(u32) [TLV]: kept to lend the
                    // connection to the next client, without the halted flag and its dpc (its
                    // target_id and rest kept).
                    if r.payload.len() >= 13 {
                        let a = &r.payload;
                        let conn = u32::from_le_bytes([a[0], a[1], a[2], a[3]]);
                        let mut lent = a[..13].to_vec();
                        lent[8] &= !wire_rvswd::enums::attach_flags::HALTED;
                        if let Ok(tlvs) = parse_tlvs(&a[13..]) {
                            for t in tlvs.iter().filter(|t| {
                                t.tag == wire_rvswd::tlvs::attach_answer::TARGET_ID
                                    || t.tag == wire_rvswd::tlvs::attach_answer::IDLE_CLOCK
                            }) {
                                ch32rv_oep::codec::put_tlv(&mut lent, t.tag, false, &t.value);
                            }
                        }
                        self.ledger
                            .conns
                            .entry(conn)
                            .or_insert((req.func, BTreeSet::new(), lent))
                            .1
                            .insert(id);
                    }
                }
                o if o == wire_rvswd::op::DETACH && req.payload.len() >= 4 => {
                    let b = &req.payload;
                    let conn = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    self.ledger.conns.remove(&conn);
                }
                _ => {}
            }
        } else if Some(req.func) == self.plan_fn && req.op == registry::probe_plan::op::PLAN_APPLY {
            // role_assignment TLVs only (fn(u16) role(u8) channel(u16)); a named extension or
            // another TLV is not a fn.
            if let Ok(tlvs) = parse_tlvs(&req.payload) {
                let fns = self.ledger.plans.entry(id).or_default();
                for t in tlvs.iter().filter(|t| {
                    t.tag & 0x7F == registry::probe_plan::tlvs::plan_apply::ROLE_ASSIGNMENT
                        && t.value.len() >= 2
                }) {
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
        for (&conn, (func, users, _)) in self.ledger.conns.iter_mut() {
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
        // Its subscriptions end at the probe too, before anything else it held goes.
        let subscribed: Vec<u16> = self
            .ledger
            .subs
            .iter()
            .filter(|(_, c)| **c == id)
            .map(|(f, _)| *f)
            .collect();
        for func in subscribed {
            self.ledger.subs.remove(&func);
            calls.insert(
                0,
                (
                    id,
                    Call {
                        func,
                        op: registry::constants::OP_UNSUBSCRIBE,
                        session: Some(self.sid),
                        payload: Vec::new(),
                    },
                ),
            );
        }
        // No plan interface, nothing to release (op 0x11 on fn 0 would be the core's end).
        if let Some(fns) = self.ledger.plans.remove(&id)
            && !fns.is_empty()
            && let Some(plan_fn) = self.plan_fn
        {
            let mut p = vec![fns.len() as u8];
            for f in fns {
                p.extend_from_slice(&f.to_le_bytes());
            }
            calls.push((
                id,
                Call {
                    func: plan_fn,
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

/// en: Borrow the Link from its running broker. A broker of another ch32rv is waited for up to
/// `--lock-timeout`, then refused (`busy`, exit 13), as [`client_link_for`] does.
/// ja: 動いているブローカーから Link を借りる。別の版のブローカーは `--lock-timeout` 待って断る。
pub(crate) fn lend(target: &BrokerTarget) -> Result<Lend, ClientLinkError> {
    let key = target.key();
    let since = Instant::now();
    let link = loop {
        let Some(v) = read_endpoint(&key) else {
            return Err("the Link's broker is not running".into());
        };
        match connect_endpoint(&key, &v) {
            Found::Link(l) => break *l,
            Found::Other(who) if since.elapsed() >= crate::oep::lock_wait() => {
                return Err(other_version_busy(&who));
            }
            Found::Other(_) => std::thread::sleep(Duration::from_millis(100)),
            Found::Nothing => return Err("the Link's broker is not running".into()),
        }
    };
    Ok(lend_from(link)?)
}

fn lend_from(link: Link) -> Result<Lend, String> {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    /// A client that stops reading never blocks the broker: pushes past its backlog are
    /// dropped at once, and a result is still queued (it goes out once the client reads).
    #[test]
    fn a_client_that_stops_reading_does_not_block_the_broker() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (s, _) = l.accept().unwrap();
        let mut c = ClientOut::spawn(s);
        let push = vec![registry::roles::DATA; 64 << 10];
        let started = Instant::now();
        let mut queued = 0usize;
        for _ in 0..((CLIENT_BACKLOG >> 16) * 4) {
            if c.queue(push.clone(), true) {
                queued += 1;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "queueing blocked"
        );
        assert!(c.dropped > 0, "nothing dropped past the backlog");
        assert!(queued >= CLIENT_BACKLOG >> 16, "the backlog was not used");
        assert!(
            c.queue(vec![registry::roles::RESULT, 1, 0], false),
            "a result was dropped"
        );
        drop(peer);
    }

    #[test]
    fn only_a_broker_of_this_protocol_and_version_is_ours() {
        let ours = json!({"port": 1, "pid": 7, "protocol": BROKER_PROTOCOL, "version": VERSION});
        assert_eq!(other_version(&ours), None);
        let newer = json!({"port": 1, "pid": 7, "protocol": BROKER_PROTOCOL, "version": "9.9.9"});
        assert_eq!(
            other_version(&newer).as_deref(),
            Some("ch32rv 9.9.9 (pid 7)")
        );
        let protocol = json!({"port": 1, "pid": 7, "protocol": 99, "version": VERSION});
        assert!(other_version(&protocol).is_some());
        // A broker from before the check wrote neither field.
        let old = json!({"port": 1, "pid": 7, "time": 0, "transport": "serial"});
        assert!(other_version(&old).unwrap().contains("older"));
    }
}
