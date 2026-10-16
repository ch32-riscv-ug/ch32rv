//! en: A WCH-Link behind the broker (docs/oep-host.ja.md §7.2): the broker holds the Link's debug
//! interface and shows its clients OEP, mapping the standard interfaces onto the Link -
//! `oep.wire.rvswd` / `oep.wire.swio` attach onto AttachChip (a `Session`, with the probe lock and
//! the target DB), `oep.target.riscv-dm` onto DmiOp / the Debug Module / the Link's fast read, and
//! `oep.target.console` onto the dmdata / dmseq mailboxes, which the broker polls itself into a
//! position stream. So several ch32rv commands (a monitor and, next, a gdb server) can use one
//! Link at once. The uart / sdi monitors use the Link's CDC and stay outside.
//!
//! ja: ブローカーの裏の WCH-Link。ブローカーが Link の debug の口を持ち、client には OEP を見せる。
//! wire の attach は AttachChip(`Session`)、riscv-dm は DmiOp / Debug Module / 高速 read、console は
//! dmdata / dmseq の mailbox をブローカーが自分で poll して位置付きのストリームにする。

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use ch32rv_contract::policy::MonitorSource;
use ch32rv_dmi::{DmTarget, DtmAccess, ResetMode, TargetAccess};
use ch32rv_oep::codec::{Resolution, parse_tlvs};
use ch32rv_oep::link::{Call, Limits, Reply};
use ch32rv_oep::registry::{
    self, core as oep_core, outcomes, reject_reasons, status, target_console as console,
    target_riscv_dm as dm, wire_rvswd as wire,
};
use ch32rv_wchlink::Speed;

use crate::cmd_probe::Entry;
use crate::session::Session;
use crate::source::DmiSource;

const FN_RVSWD: u16 = 1;
const FN_SWIO: u16 = 2;
const FN_DM: u16 = 3;
const FN_CONSOLE: u16 = 4;
/// ch32rv's own interface: lend the Link to a client for its direct (WCH stub) flash.
const FN_WCHLINK: u16 = 5;
/// The name of ch32rv's own interface (reverse DNS, core §12).
pub(crate) const WCHLINK_IF: &str = "io.github.ch32-riscv-ug.wchlink";
/// Its ops (0x01-0x0F are the common range of every interface, core §3: subscribe, ...).
pub(crate) const OP_LEND: u8 = 0x10;
pub(crate) const OP_RECLAIM: u8 = 0x11;
/// The one connection a Link has (resource ids are u32, core §9).
const CONN: u32 = 1;
/// The Link's two debug lines as channels (fn 0 describe `channels`): SWDIO/DAT and SWCLK.
const CH_SWDIO: u16 = 0;
const CH_SWCLK: u16 = 1;
/// Streams listed per STREAMS answer (9 bytes each).
const STREAMS_PER_ANSWER: usize = 32;
/// A console keeps this much output for its readers.
const CONSOLE_KEEP: usize = 64 * 1024;

/// What list answers: fn 0 (the core) is never listed (core §7.2).
const INTERFACES: [(u16, &str); 5] = [
    (FN_RVSWD, registry::wire_rvswd::NAME),
    (FN_SWIO, registry::wire_swio::NAME),
    (FN_DM, dm::NAME),
    (FN_CONSOLE, console::NAME),
    (FN_WCHLINK, WCHLINK_IF),
];

/// The console mechanisms this broker offers, in describe order: the wire value is the place
/// (from 1) in this list (a named list, core §7.7).
const MECHANISMS: [&str; 2] = [
    console::names::mechanism::DMDATA,
    console::names::mechanism::DMSEQ,
];

struct Console {
    src: DmiSource,
    /// The mechanism's place in [`MECHANISMS`] (1-based).
    mech: u8,
    buf: VecDeque<u8>,
    /// Stream position of `buf[0]`.
    base: u64,
    /// Host input waiting to reach the target.
    input: Vec<u8>,
}

pub(crate) struct WchUpstream {
    entry: Entry,
    session: Option<Session>,
    /// Something was written (block, dmi write, run) since the attach: the Link's fast read may
    /// then return stale bytes, so reads go through the Debug Module.
    dirty: bool,
    consoles: HashMap<u32, Console>,
    next_stream: u32,
    lock_timeout: Duration,
    /// The client the Link is lent to (its session dropped so the client can open the Link
    /// directly), and whether a connection existed to be restored.
    lent: Option<(u64, bool)>,
    /// What confirm reports: new for every broker (a restart of it is a new "boot").
    boot_id: u32,
    /// The broker's "boot", for `clock`'s uptime_ns.
    started: std::time::Instant,
}

/// The `max_op_ms` this broker declares (fn 0 describe): a run of longer is unsupported.
const MAX_OP_MS: u32 = 10_000;

/// The riscv-dm `max_length` this broker declares (bytes): a block of more is unsupported.
const BLOCK_MAX_LENGTH: usize = 1024;

/// `rejected unsupported` for a fixed-part value (payload `tag 0x00`, oep-core §4.3).
fn rejected_unsupported_fixed() -> Reply {
    Reply {
        resolution: Resolution::Rejected(reject_reasons::UNSUPPORTED),
        payload: vec![0x00],
    }
}

/// Unsupported, naming the TLV whose value cannot be done (core §2.4: the number received).
fn unsupported_tlv(tag: u8) -> Reply {
    Reply {
        resolution: Resolution::Rejected(reject_reasons::UNSUPPORTED),
        payload: vec![tag],
    }
}

fn ok(payload: Vec<u8>) -> Reply {
    Reply {
        resolution: Resolution::Completed(outcomes::SUCCESS),
        payload,
    }
}

fn failed(payload: Vec<u8>) -> Reply {
    Reply {
        resolution: Resolution::Completed(outcomes::FAILED),
        payload,
    }
}

fn rejected(reason: u8) -> Reply {
    Reply {
        resolution: Resolution::Rejected(reason),
        payload: Vec::new(),
    }
}

/// `unavailable` with its cause TLV (core §4.3).
fn unavailable(cause: u8) -> Reply {
    Reply {
        resolution: Resolution::Rejected(reject_reasons::UNAVAILABLE),
        payload: tlv(oep_core::tlvs::unavailable_payload::CAUSE, &[cause]),
    }
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    ch32rv_oep::codec::put_tlv(&mut v, tag, false, value);
    v
}

/// The describe `ops` tag for `ops` (core §7.4: base(u8) bitmap).
fn ops_tlv(ops: &[u8]) -> Vec<u8> {
    let base = ops.iter().copied().min().unwrap_or(0);
    let top = ops.iter().copied().max().unwrap_or(0);
    let mut v = vec![base];
    v.resize(1 + usize::from(top - base) / 8 + 1, 0);
    for &op in ops {
        let i = usize::from(op - base);
        v[1 + i / 8] |= 1 << (i % 8);
    }
    tlv(registry::describe_common::OPS, &v)
}

/// What a console stream's send queue holds (the probe sizes it, oep-if-console §2).
const SEND_QUEUE: usize = 256;

impl WchUpstream {
    pub(crate) fn new(entry: Entry, lock_timeout: Duration) -> Self {
        WchUpstream {
            entry,
            session: None,
            dirty: false,
            consoles: HashMap::new(),
            // One number space for connections and streams (core §9): the connection is CONN.
            next_stream: CONN + 1,
            lock_timeout,
            lent: None,
            boot_id: ch32rv_oep::session::random_session_id().max(1),
            started: std::time::Instant::now(),
        }
    }

    pub(crate) fn limits(&self) -> Limits {
        Limits {
            revision: 1,
            max_frame: 4096,
            window: 1 << 16,
            max_inflight: 16,
            boot_id: self.boot_id,
        }
    }

    pub(crate) fn wires(&self) -> Vec<u16> {
        vec![FN_RVSWD, FN_SWIO]
    }

    /// Serve one request from client `id`.
    pub(crate) fn handle(&mut self, id: u64, c: &Call) -> Reply {
        if c.func == FN_WCHLINK {
            return self.lending(id, c.op, &c.payload);
        }
        // While lent, only what needs no Link is served: the consoles' kept output, discovery.
        if self.lent.is_some() {
            let readable = c.func == oep_core::FN
                || (c.func == FN_CONSOLE
                    && [
                        console::op::READ,
                        console::op::MARKS,
                        console::op::WRITE,
                        console::op::STREAMS,
                    ]
                    .contains(&c.op));
            if !readable {
                return rejected(reject_reasons::UNAVAILABLE);
            }
        }
        match c.func {
            f if f == oep_core::FN => self.core(c),
            FN_RVSWD | FN_SWIO => self.wire(c),
            FN_DM => self.riscv_dm(c),
            FN_CONSOLE => self.console(c),
            _ => rejected(reject_reasons::UNKNOWN_FUNCTION),
        }
    }

    fn core(&mut self, c: &Call) -> Reply {
        let p = &c.payload;
        match c.op {
            // boot_id(u32) uptime_ns(u64): this broker is the probe here, its clock since it started.
            o if o == oep_core::op::CLOCK => {
                let mut out = self.boot_id.to_le_bytes().to_vec();
                let ns = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                out.extend_from_slice(&ns.to_le_bytes());
                ok(out)
            }
            o if o == oep_core::op::LIST => {
                // first(u16): every interface from there (core §7.2; the host filters).
                let Some(first) = le16(p, 0) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let hits: Vec<&(u16, &str)> = INTERFACES.iter().collect();
                let mut out = (hits.len() as u16).to_le_bytes().to_vec();
                let page = &hits[usize::from(first).min(hits.len())..];
                out.push(page.len() as u8);
                // Each entry: fn, instance, revision, name_len, name (core §7.2; no element length).
                for (func, name) in page {
                    out.extend_from_slice(&func.to_le_bytes());
                    out.extend_from_slice(&0u16.to_le_bytes()); // instance
                    out.extend_from_slice(&[1, name.len() as u8]);
                    out.extend_from_slice(name.as_bytes());
                }
                ok(out)
            }
            o if o == oep_core::op::DESCRIBE => {
                // fn(u16) first(u16): the TLVs from index `first` (core §7.3).
                let (Some(func), Some(first)) = (le16(p, 0), le16(p, 2)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let tlvs: Vec<Vec<u8>> = match func {
                    f if f == oep_core::FN => {
                        let mut v = vec![tlv(
                            oep_core::tlvs::describe::MODEL,
                            format!("{} via ch32rv broker", self.model()).as_bytes(),
                        )];
                        // unit_id (required): the Link's serial, a-z 0-9 - only.
                        v.push(tlv(
                            oep_core::tlvs::describe::UNIT_ID,
                            self.unit_id().as_bytes(),
                        ));
                        // transport (required): index 0, the broker's local endpoint (kind TCP).
                        v.push(tlv(
                            oep_core::tlvs::describe::TRANSPORT,
                            &[0, oep_core::enums::transport_kind::TCP, 0xFF],
                        ));
                        v.push(tlv(oep_core::tlvs::describe::CHANNELS, &2u16.to_le_bytes()));
                        v.push(tlv(
                            oep_core::tlvs::describe::MAX_OP_MS,
                            &MAX_OP_MS.to_le_bytes(),
                        ));
                        use oep_core::op as o;
                        v.push(ops_tlv(&[
                            o::CONFIRM,
                            o::LIST,
                            o::DESCRIBE,
                            o::OPEN,
                            o::END,
                            o::KEEPALIVE,
                            o::LOCK_STATE,
                            o::CLOCK,
                        ]));
                        v
                    }
                    FN_RVSWD | FN_SWIO => {
                        // The Link's lines are fixed: one channel group (role 1 SWDIO, and role 2
                        // SWCLK on two wires), and attach may halt (features attach_halt).
                        let mut group = vec![0u8, 1, wire::enums::pin_role::SWDIO];
                        group.extend_from_slice(&CH_SWDIO.to_le_bytes());
                        if func == FN_RVSWD {
                            group[1] = 2;
                            group.push(wire::enums::pin_role::SWCLK);
                            group.extend_from_slice(&CH_SWCLK.to_le_bytes());
                        }
                        vec![
                            tlv(wire::tlvs::describe::MAX_CONNECTIONS, &[1]),
                            tlv(registry::describe_common::CHANNEL_GROUP, &group),
                            tlv(
                                registry::describe_common::FEATURES,
                                &[wire::enums::features::ATTACH_HALT],
                            ),
                            tlv(
                                registry::describe_common::MAX_CLOCK_HZ,
                                &6_000_000u32.to_le_bytes(),
                            ),
                            ops_tlv(&[wire::op::ATTACH, wire::op::DETACH, wire::op::CONNECTIONS]),
                        ]
                    }
                    FN_DM => {
                        let mut v = vec![tlv(
                            registry::describe_common::MAX_LENGTH,
                            &(BLOCK_MAX_LENGTH as u16).to_le_bytes(),
                        )];
                        v.push(ops_tlv(&[
                            dm::op::DMI,
                            dm::op::HALT,
                            dm::op::RESUME,
                            dm::op::RESET,
                            dm::op::READ_BLOCK,
                            dm::op::WRITE_BLOCK,
                            dm::op::RUN,
                        ]));
                        v
                    }
                    FN_CONSOLE => {
                        // Mechanisms: a named list, one text TLV each (place 1 dmdata, 2 dmseq).
                        // No marks: marks / clear / mark are optional and not offered.
                        let mut v: Vec<Vec<u8>> = MECHANISMS
                            .iter()
                            .map(|m| tlv(console::tlvs::describe::MECHANISM, m.as_bytes()))
                            .collect();
                        v.push(ops_tlv(&[
                            console::op::OPEN,
                            console::op::READ,
                            console::op::WRITE,
                            console::op::CLOSE,
                            console::op::STREAMS,
                        ]));
                        v
                    }
                    FN_WCHLINK => vec![ops_tlv(&[OP_LEND, OP_RECLAIM])],
                    _ => return rejected(reject_reasons::UNKNOWN_FUNCTION),
                };
                // more(u8) then the TLVs from `first`.
                let mut out = vec![0u8];
                for t in tlvs.into_iter().skip(usize::from(first)) {
                    out.extend(t);
                }
                ok(out)
            }
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    /// en: Lend the Link to client `id` (drop the session: the core is detached and the probe lock
    /// and vendor interface are free for that client's direct flash), or take it back (attach again
    /// and reopen the consoles, which keep their positions).
    /// ja: Link を client `id` に貸す(session を手放し、直接の flash のために lock と vendor の口を
    /// 空ける)か、取り返す(attach し直し、console を開き直す。位置はそのまま)。
    fn lending(&mut self, id: u64, op: u8, payload: &[u8]) -> Reply {
        match op {
            OP_LEND => {
                if self.lent.is_some() {
                    return rejected(reject_reasons::UNAVAILABLE);
                }
                let had = self.session.take().is_some();
                self.lent = Some((id, had));
                ok(Vec::new())
            }
            OP_RECLAIM => {
                if self.lent.map(|(who, _)| who) != Some(id) {
                    return rejected(reject_reasons::UNAVAILABLE);
                }
                if !self.reclaim() {
                    return failed(vec![status::LINE]);
                }
                // A reset asked with the reclaim runs now, with the consoles open again, so the
                // next tick polls them while the firmware starts.
                if payload.first() == Some(&1) {
                    if self.session.is_none() {
                        let mut warnings = Vec::new();
                        match Session::attach(
                            &self.entry,
                            Speed::High,
                            Duration::from_millis(1000),
                            self.lock_timeout,
                            None,
                            None,
                            &mut warnings,
                        ) {
                            Ok(s) => self.session = Some(s),
                            Err(_) => return failed(vec![status::LINE]),
                        }
                    }
                    let Some(s) = self.session.as_mut() else {
                        return failed(vec![status::LINE]);
                    };
                    if crate::cmd_flash::soft_reset_and_run(s).is_err() {
                        return failed(vec![status::FAULT]);
                    }
                    let running = s.dm().is_running().unwrap_or(false);
                    return ok(vec![u8::from(running)]);
                }
                ok(Vec::new())
            }
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    /// Take the Link back (after a lend); true when it is attached again or was not attached.
    fn reclaim(&mut self) -> bool {
        let Some((_, had)) = self.lent.take() else {
            return true;
        };
        if !had && self.consoles.is_empty() {
            return true;
        }
        let mut warnings = Vec::new();
        let Ok(mut s) = Session::attach(
            &self.entry,
            Speed::High,
            Duration::from_millis(1000),
            self.lock_timeout,
            None,
            None,
            &mut warnings,
        ) else {
            return false;
        };
        self.dirty = false;
        for x in self.consoles.values_mut() {
            let source = if MECHANISMS.get(usize::from(x.mech).wrapping_sub(1))
                == Some(&console::names::mechanism::DMSEQ)
            {
                MonitorSource::Dmseq
            } else {
                MonitorSource::Dmdata
            };
            if let Ok(src) = DmiSource::open(&mut s, source, &mut warnings) {
                x.src = src;
            }
        }
        let _ = s.dm().resume();
        self.session = Some(s);
        true
    }

    /// A client left: a Link it still borrowed comes back.
    pub(crate) fn client_gone(&mut self, id: u64) {
        if self.lent.is_some_and(|(who, _)| who == id) {
            let _ = self.reclaim();
        }
    }

    fn model(&self) -> &'static str {
        "WCH-Link"
    }

    /// The unit id this broker declares: the Link's USB serial, lowercased (a-z 0-9 - only).
    fn unit_id(&self) -> String {
        let id: String = self
            .entry
            .dev
            .port_id()
            .unwrap_or_else(|| "wch-link".to_owned())
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        id.chars().take(32).collect()
    }

    fn wire(&mut self, c: &Call) -> Reply {
        let p = &c.payload;
        match c.op {
            o if o == wire::op::ATTACH => {
                // method(u8) swdio(u16) swclk(u16) max_speed(u32) [TLV] (oep-if-debug §3).
                let (Some(&method), Some(swdio), Some(swclk), Some(hz)) =
                    (p.first(), le16(p, 1), le16(p, 3), le32(p, 5))
                else {
                    return rejected(reject_reasons::MALFORMED);
                };
                if hz == 0 {
                    return rejected(reject_reasons::MALFORMED);
                }
                if method != wire::enums::attach_method::RUN
                    && method != wire::enums::attach_method::HALT
                {
                    return rejected_unsupported_fixed();
                }
                // The pins must be the declared group.
                let want_clk = if c.func == FN_RVSWD { CH_SWCLK } else { 0xFFFF };
                if swdio != CH_SWDIO || swclk != want_clk {
                    return rejected_unsupported_fixed();
                }
                // en: The WCH-Link rests the line as its firmware does for the chip; the rest
                // answered is the one oep-if-debug §3.1 gives for the chip id. An attach naming
                // another rest is one this probe cannot make (unsupported, the TLV's number).
                // ja: 休ませ方は WCH-Link のファームが決める。答えは §3.1 の規則で chip id から。
                let asked = parse_tlvs(&p[9..]).ok().and_then(|t| {
                    t.iter()
                        .find(|t| t.tag & 0x7F == wire::tlvs::attach::IDLE_CLOCK)
                        .map(|t| t.value.clone())
                });
                let asked = match asked.as_deref() {
                    None => None,
                    Some(&[v]) if c.func == FN_RVSWD && v <= 1 => Some(v),
                    Some(_) => return unsupported_tlv(wire::tlvs::attach::IDLE_CLOCK),
                };
                let max_speed = Some(hz);
                let existing = self.session.is_some();
                if !existing {
                    let speed = match max_speed {
                        Some(hz) if hz <= 400_000 => Speed::Low,
                        Some(hz) if hz <= 4_000_000 => Speed::Medium,
                        _ => Speed::High,
                    };
                    let mut warnings = Vec::new();
                    match Session::attach(
                        &self.entry,
                        speed,
                        Duration::from_millis(1000),
                        self.lock_timeout,
                        None,
                        None,
                        &mut warnings,
                    ) {
                        Ok(s) => {
                            self.session = Some(s);
                            self.dirty = false;
                        }
                        // A failed attach answers its status alone (oep-if-common §3).
                        Err(_) => return failed(vec![status::LINE]),
                    }
                }
                let Some(chip_id) = self.session.as_ref().map(|s| s.attach.chip_id) else {
                    return failed(vec![status::LINE]);
                };
                let rest = ch32rv_oep::target::auto_rest(chip_id);
                if asked.is_some_and(|a| a != rest) {
                    if !existing {
                        self.session = None;
                    }
                    return unsupported_tlv(wire::tlvs::attach::IDLE_CLOCK);
                }
                let Some(s) = self.session.as_mut() else {
                    return failed(vec![status::LINE]);
                };
                if method == wire::enums::attach_method::HALT && s.dm().halt().is_err() {
                    return failed(vec![status::TIMEOUT]);
                }
                let dmstatus = s.dm().dmstatus().unwrap_or(0);
                // flags (attach_flags): bit3 halted with the dpc TLV.
                let dpc = if s.dm().is_halted().unwrap_or(false) {
                    s.dm().read_reg(ch32rv_dmi::RegName::Pc).ok()
                } else {
                    None
                };
                // connection(u32) id(u32) flags(u8) speed_hz(u32)
                let mut out = CONN.to_le_bytes().to_vec();
                out.extend_from_slice(&dmstatus.to_le_bytes());
                let mut flags = 0;
                if dpc.is_some() {
                    flags |= wire::enums::attach_flags::HALTED;
                }
                out.push(flags);
                let hz = match max_speed {
                    Some(hz) if hz <= 400_000 => 400_000u32,
                    Some(hz) if hz <= 4_000_000 => 4_000_000,
                    _ => 6_000_000,
                };
                out.extend_from_slice(&hz.to_le_bytes());
                if chip_id != 0 && chip_id != u32::MAX {
                    let mut v = vec![registry::common::enum_::target_id_scheme::DMI_7F];
                    v.extend_from_slice(&chip_id.to_le_bytes());
                    out.extend(tlv(wire::tlvs::attach_answer::TARGET_ID, &v));
                }
                if let Some(pc) = dpc {
                    out.extend(tlv(wire::tlvs::attach_answer::DPC, &pc.to_le_bytes()));
                }
                if c.func == FN_RVSWD {
                    out.extend(tlv(wire::tlvs::attach_answer::IDLE_CLOCK, &[rest]));
                }
                ok(out)
            }
            o if o == wire::op::DETACH => {
                if le32(p, 0) != Some(CONN) || self.session.is_none() {
                    return rejected(reject_reasons::NO_RESOURCE);
                }
                self.consoles.clear();
                self.session = None; // Session's drop detaches the core and frees the lock
                ok(Vec::new())
            }
            // first(u8) -> more count entries (oep-if-debug §2.1, lock-free): the one connection.
            o if o == wire::op::CONNECTIONS => {
                let Some(&first) = p.first() else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let mut out = vec![0u8];
                match self.session.as_ref().filter(|_| first == 0) {
                    Some(s) => {
                        out.push(1);
                        out.extend_from_slice(&CONN.to_le_bytes());
                        out.extend_from_slice(&CH_SWDIO.to_le_bytes());
                        let clk = if c.func == FN_RVSWD { CH_SWCLK } else { 0xFFFF };
                        out.extend_from_slice(&clk.to_le_bytes());
                        out.extend_from_slice(&6_000_000u32.to_le_bytes());
                        let id = s.attach.chip_id;
                        if id != 0 && id != u32::MAX {
                            out.extend_from_slice(&[
                                registry::common::enum_::target_id_scheme::DMI_7F,
                                4,
                            ]);
                            out.extend_from_slice(&id.to_le_bytes());
                        } else {
                            out.extend_from_slice(&[0, 0]);
                        }
                        if c.func == FN_RVSWD {
                            let rest = ch32rv_oep::target::auto_rest(id);
                            out.extend(tlv(wire::tlvs::connections_answer::IDLE_CLOCK, &[0, rest]));
                        }
                    }
                    None => out.push(0),
                }
                ok(out)
            }
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    fn session_for(&mut self, p: &[u8]) -> Result<&mut Session, Reply> {
        if le32(p, 0) != Some(CONN) {
            return Err(rejected(reject_reasons::NO_RESOURCE));
        }
        self.session
            .as_mut()
            .ok_or_else(|| rejected(reject_reasons::NO_RESOURCE))
    }

    fn riscv_dm(&mut self, c: &Call) -> Reply {
        let p = c.payload.clone();
        let dirty = self.dirty;
        if [dm::op::DMI, dm::op::WRITE_BLOCK, dm::op::RUN].contains(&c.op) {
            self.dirty = true;
        }
        let s = match self.session_for(&p) {
            Ok(s) => s,
            Err(r) => return r,
        };
        let st = |r: Result<(), ch32rv_dmi::DmiError>, bad: u8| match r {
            Ok(()) => ok(vec![status::OK]),
            Err(_) => failed(vec![bad]),
        };
        match c.op {
            o if o == dm::op::DMI => dmi_steps(s, &p[4..]),
            o if o == dm::op::HALT => st(s.dm().halt(), status::TIMEOUT),
            o if o == dm::op::RESUME => st(s.dm().resume(), status::STATE),
            o if o == dm::op::RESET => {
                // Mode 0 run (checked running), 1 halt at reset; anything else unsupported
                // (oep-if-debug §4.3).
                let mode = match p.get(4) {
                    Some(&m) if m == dm::enums::reset_mode::HALT_AT_RESET => ResetMode::HaltAtReset,
                    Some(&m) if m == dm::enums::reset_mode::RUN => ResetMode::RunVerified,
                    Some(_) => return rejected_unsupported_fixed(),
                    None => return rejected(reject_reasons::MALFORMED),
                };
                match DmTarget::new(s.link()).reset(mode) {
                    Ok(r) => {
                        // flags bit0 = reached: mode 0 running seen, mode 1 halted with dpc read.
                        let reached = if mode == ResetMode::RunVerified {
                            s.dm().is_running().unwrap_or(false)
                        } else {
                            true
                        };
                        // status(u8) flags(u8) pc(u32): pc is the dpc for mode 1 only.
                        let pc = if mode == ResetMode::HaltAtReset {
                            r.pc
                        } else {
                            0
                        };
                        let mut out = vec![
                            if reached { status::OK } else { status::TIMEOUT },
                            u8::from(reached),
                        ];
                        out.extend_from_slice(&pc.to_le_bytes());
                        if reached { ok(out) } else { failed(out) }
                    }
                    Err(_) => failed(vec![status::FAULT, 0, 0, 0, 0, 0]),
                }
            }
            o if o == dm::op::READ_BLOCK => {
                let (Some(addr), Some(count)) = (le32(&p, 4), le16(&p, 8)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                if addr % 4 != 0 {
                    return rejected(reject_reasons::MALFORMED);
                }
                if usize::from(count) * 4 > BLOCK_MAX_LENGTH {
                    return rejected_unsupported_fixed();
                }
                let len = u32::from(count) * 4;
                // en: The Link's fast read serves code flash only: on a peripheral register
                // (FLASH_CTLR at 0x40022010) it returned the last word written there (a key), and
                // SRAM just written over DMI read back stale, both measured on a CH32V003; those go
                // through the Debug Module. Flash falls back to DMI when the fast read refuses.
                // ja: Link の高速 read は code flash 専用。周辺の register(FLASH_CTLR)では直前に書いた
                // 語(鍵)を返し、DMI で書いた直後の SRAM は古い値を返した(どちらも CH32V003 で実測)ので、
                // それらは Debug Module で読む。
                // SRAM just written over DMI read back stale through the fast read, and so did code
                // flash just programmed by a loader run (CH32V003): once anything was written in
                // this attach, every read goes through the Debug Module.
                let memory = !dirty && (0x0800_0000..0x1000_0000).contains(&addr);
                let fast = if memory {
                    s.link()
                        .read_mem(addr, len)
                        .ok()
                        .filter(|d| d.len() == len as usize)
                } else {
                    None
                };
                let data = fast.or_else(|| s.dm().read_mem(addr, len).ok());
                match data {
                    Some(d) => {
                        let mut out = count.to_le_bytes().to_vec();
                        out.push(status::OK);
                        out.extend(d);
                        ok(out)
                    }
                    None => failed(vec![0, 0, status::FAULT]),
                }
            }
            o if o == dm::op::WRITE_BLOCK => {
                let (Some(addr), Some(count)) = (le32(&p, 4), le16(&p, 8)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                if addr % 4 != 0 {
                    return rejected(reject_reasons::MALFORMED);
                }
                if usize::from(count) * 4 > BLOCK_MAX_LENGTH {
                    return rejected_unsupported_fixed();
                }
                let words: Vec<u32> = (0..usize::from(count))
                    .filter_map(|i| le32(&p, 10 + 4 * i))
                    .collect();
                if words.len() != usize::from(count) {
                    return rejected(reject_reasons::MALFORMED);
                }
                let mut dmx = s.dm();
                for (i, w) in words.iter().enumerate() {
                    if dmx.write_mem32(addr + 4 * i as u32, *w).is_err() {
                        let mut out = (i as u16).to_le_bytes().to_vec();
                        out.push(status::FAULT);
                        return failed(out);
                    }
                }
                let mut out = count.to_le_bytes().to_vec();
                out.push(status::OK);
                ok(out)
            }
            o if o == dm::op::RUN => run(s, &p[4..]),
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    fn console(&mut self, c: &Call) -> Reply {
        let p = &c.payload;
        match c.op {
            // conn(u32) mech(u8: the place in MECHANISMS) -> stream(u32) (oep-if-console §1).
            o if o == console::op::OPEN => {
                let Some(&mech) = p.get(4) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                // The broker shares one connection among its clients: a stream already open for
                // this mechanism is lent to the next client too (its readers keep their own
                // positions; reads never consume).
                if let Some((&id, _)) = self.consoles.iter().find(|(_, x)| x.mech == mech) {
                    return ok(id.to_le_bytes().to_vec());
                }
                let source = match MECHANISMS.get(usize::from(mech).wrapping_sub(1)) {
                    Some(&m) if m == console::names::mechanism::DMDATA => MonitorSource::Dmdata,
                    Some(&m) if m == console::names::mechanism::DMSEQ => MonitorSource::Dmseq,
                    _ => return rejected_unsupported_fixed(),
                };
                if self.next_stream == u32::MAX {
                    return unavailable(oep_core::enums::unavailable_cause::LIMIT);
                }
                let s = match self.session_for(p) {
                    Ok(s) => s,
                    Err(r) => return r,
                };
                let mut warnings = Vec::new();
                let Ok(src) = DmiSource::open(s, source, &mut warnings) else {
                    return rejected(reject_reasons::UNAVAILABLE);
                };
                // The mailboxes only move while the core runs.
                let _ = s.dm().resume();
                // Numbers are never reused within a boot (core §9).
                let id = self.next_stream;
                self.next_stream += 1;
                self.consoles.insert(
                    id,
                    Console {
                        src,
                        mech,
                        buf: VecDeque::new(),
                        base: 0,
                        input: Vec::new(),
                    },
                );
                ok(id.to_le_bytes().to_vec())
            }
            // stream(u32) from(u8) arg(u64) max(u16) (oep-if-common §1.2).
            o if o == console::op::READ => {
                let (Some(id), Some(&from), Some(arg), Some(max)) = (
                    le32(p, 0),
                    p.get(4),
                    p.get(5..13)
                        .map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))),
                    le16(p, 13),
                ) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                use registry::common::enum_::read_from as f;
                // No marks are offered: from 3 (last mark) is unsupported, like any other.
                if from != f::POSITION && from != f::OLDEST && from != f::NOW {
                    return rejected_unsupported_fixed();
                }
                let Some(x) = self.consoles.get(&id) else {
                    return rejected(reject_reasons::NO_RESOURCE);
                };
                let end = x.base + x.buf.len() as u64;
                let want = match from {
                    f::POSITION => arg,
                    f::OLDEST => x.base,
                    _ => end,
                };
                let start = want.clamp(x.base, end);
                let skip = (start - x.base) as usize;
                let n = usize::from(max).min(x.buf.len() - skip).min(4096 - 16);
                let mut out = start.to_le_bytes().to_vec();
                let more = skip + n < x.buf.len();
                let gap = want < x.base;
                out.push(u8::from(more) | (u8::from(gap) << 1));
                out.extend_from_slice(&(n as u16).to_le_bytes());
                out.extend(x.buf.iter().skip(skip).take(n));
                ok(out)
            }
            // stream(u32) count(u16) data (oep-if-common §1.4).
            o if o == console::op::WRITE => {
                let (Some(id), Some(n)) = (le32(p, 0), le16(p, 4)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let Some(data) = p.get(6..6 + usize::from(n)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let Some(x) = self.consoles.get_mut(&id) else {
                    return rejected(reject_reasons::NO_RESOURCE);
                };
                // accepted = min(count, free room in the send queue): all taken success, some
                // partial, none (with something to take) failed.
                let room = SEND_QUEUE.saturating_sub(x.input.len());
                let taken = &data[..data.len().min(room)];
                x.input.extend_from_slice(taken);
                let out = (taken.len() as u16).to_le_bytes().to_vec();
                if taken.len() == data.len() {
                    ok(out)
                } else if taken.is_empty() {
                    failed(out)
                } else {
                    Reply {
                        resolution: Resolution::Completed(outcomes::PARTIAL),
                        payload: out,
                    }
                }
            }
            o if o == console::op::CLOSE => {
                let Some(id) = le32(p, 0) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                match self.consoles.remove(&id) {
                    Some(_) => ok(Vec::new()),
                    None => rejected(reject_reasons::NO_RESOURCE),
                }
            }
            // en: The live streams from index `first` (oep-if-console §1, required): more(u8)
            // count(u8), then count x (stream u32, connection u32, mechanism u8). A closed stream
            // leaves `consoles`. ja: 生きているストリームの一覧。
            o if o == console::op::STREAMS => {
                let Some(first) = le16(p, 0) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let mut ids: Vec<(u32, u8)> =
                    self.consoles.iter().map(|(&id, x)| (id, x.mech)).collect();
                ids.sort_unstable();
                let rest = ids.get(usize::from(first)..).unwrap_or(&[]);
                let page = &rest[..rest.len().min(STREAMS_PER_ANSWER)];
                let mut out = vec![u8::from(page.len() < rest.len()), page.len() as u8];
                for &(id, mech) in page {
                    out.extend_from_slice(&id.to_le_bytes());
                    out.extend_from_slice(&CONN.to_le_bytes());
                    out.push(mech);
                }
                ok(out)
            }
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    /// en: Poll the open consoles (the broker calls this between requests); returns how soon to
    /// call again. ja: 開いている console を poll する(ブローカーが要求の合間に呼ぶ)。次に呼ぶまでの時間を返す。
    pub(crate) fn tick(&mut self) -> Duration {
        let Some(s) = self.session.as_mut() else {
            return Duration::from_millis(250);
        };
        let mut next = Duration::from_millis(250);
        for x in self.consoles.values_mut() {
            if let Ok(bytes) = x.src.poll(s, &mut x.input) {
                x.buf.extend(bytes);
                while x.buf.len() > CONSOLE_KEEP {
                    x.buf.pop_front();
                    x.base += 1;
                }
            }
            next = next.min(x.src.idle());
        }
        next
    }
}

/// `dmi`: the steps in order (oep-if-debug §2.1).
fn dmi_steps(s: &mut Session, b: &[u8]) -> Reply {
    use dm::enums::dmi_step as k;
    let Some(n) = le16(b, 0) else {
        return rejected(reject_reasons::MALFORMED);
    };
    let mut at = 2;
    let (mut done, mut st, mut values) = (0u16, status::OK, Vec::new());
    let link = s.link();
    for _ in 0..n {
        let Some(&kind) = b.get(at) else {
            return rejected(reject_reasons::MALFORMED);
        };
        let r: Result<Option<u32>, ()> = match kind {
            x if x == k::WRITE => {
                let (Some(&a), Some(v)) = (b.get(at + 1), le32(b, at + 2)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                at += 6;
                link.dmi_write(a, v).map(|_| None).map_err(|_| ())
            }
            x if x == k::READ => {
                let Some(&a) = b.get(at + 1) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                at += 2;
                link.dmi_read(a).map(Some).map_err(|_| ())
            }
            x if x == k::POLL_READS => {
                let (Some(&a), Some(mask), Some(want), Some(max)) = (
                    b.get(at + 1),
                    le32(b, at + 2),
                    le32(b, at + 6),
                    le16(b, at + 10),
                ) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                at += 12;
                let mut last = 0;
                let mut hit = false;
                let mut err = false;
                for _ in 0..max.max(1) {
                    match link.dmi_read(a) {
                        Ok(v) => {
                            last = v;
                            if v & mask == want {
                                hit = true;
                                break;
                            }
                        }
                        Err(_) => {
                            err = true;
                            break;
                        }
                    }
                }
                if err {
                    Err(())
                } else {
                    values.push(last);
                    if !hit {
                        st = status::TIMEOUT;
                        break;
                    }
                    Ok(None)
                }
            }
            x if x == k::WAIT_US => {
                let Some(us) = le32(b, at + 1) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                at += 5;
                std::thread::sleep(Duration::from_micros(u64::from(us)));
                Ok(None)
            }
            _ => return rejected(reject_reasons::MALFORMED),
        };
        match r {
            Ok(Some(v)) => values.push(v),
            Ok(None) => {}
            Err(()) => {
                st = status::LINE;
                break;
            }
        }
        done += 1;
    }
    // done(u16), status(u8), nvals(u16), nvals x value(u32)
    let mut out = done.to_le_bytes().to_vec();
    out.push(st);
    out.extend_from_slice(&(values.len() as u16).to_le_bytes());
    for v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    if st == status::OK {
        ok(out)
    } else if done > 0 {
        Reply {
            resolution: Resolution::Completed(outcomes::PARTIAL),
            payload: out,
        }
    } else {
        failed(out)
    }
}

/// `run`: pc, timeout_ms, regs, outs -> the loader run over plain DMI.
fn run(s: &mut Session, b: &[u8]) -> Reply {
    let (Some(pc), Some(timeout), Some(&n)) = (le32(b, 0), le32(b, 4), b.get(8)) else {
        return rejected(reject_reasons::MALFORMED);
    };
    // 1..=max_op_ms (core §7.5, oep-if-debug §4.4).
    if timeout == 0 {
        return rejected(reject_reasons::MALFORMED);
    }
    if timeout > MAX_OP_MS {
        return rejected(reject_reasons::UNSUPPORTED);
    }
    let mut at = 9;
    let mut regs = Vec::new();
    for _ in 0..n {
        let (Some(r), Some(v)) = (le16(b, at), le32(b, at + 2)) else {
            return rejected(reject_reasons::MALFORMED);
        };
        regs.push((r, v));
        at += 6;
    }
    let Some(&n_out) = b.get(at) else {
        return rejected(reject_reasons::MALFORMED);
    };
    let outs: Vec<u16> = (0..usize::from(n_out))
        .filter_map(|i| le16(b, at + 1 + 2 * i))
        .collect();
    match DmTarget::new(s.link()).run_until_halt(
        pc,
        &regs,
        &outs,
        Duration::from_millis(u64::from(timeout)),
    ) {
        Ok(r) => {
            let mut out = vec![
                if r.stopped {
                    status::OK
                } else {
                    status::TIMEOUT
                },
                u8::from(r.stopped),
            ];
            out.extend_from_slice(&r.dpc.to_le_bytes());
            out.extend_from_slice(&r.elapsed_us.to_le_bytes());
            out.push(r.outs.len() as u8);
            for v in r.outs {
                out.extend_from_slice(&v.to_le_bytes());
            }
            if r.stopped { ok(out) } else { failed(out) }
        }
        // Could not run or halt it: stopped 2, dpc and values mean nothing (oep-if-debug §4.4).
        Err(_) => {
            let mut out = vec![status::FAULT, dm::enums::run_stopped::NOT_HALTED];
            out.extend_from_slice(&[0; 8]);
            out.push(0);
            failed(out)
        }
    }
}
