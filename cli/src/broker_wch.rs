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
/// Its ops.
pub(crate) const OP_LEND: u8 = 0x01;
pub(crate) const OP_RECLAIM: u8 = 0x02;
/// The one connection a Link has.
const CONN: u16 = 1;
/// A console keeps this much output for its readers.
const CONSOLE_KEEP: usize = 64 * 1024;

const INTERFACES: [(u16, &str); 6] = [
    (oep_core::FN, oep_core::NAME),
    (FN_RVSWD, registry::wire_rvswd::NAME),
    (FN_SWIO, registry::wire_swio::NAME),
    (FN_DM, dm::NAME),
    (FN_CONSOLE, console::NAME),
    (FN_WCHLINK, WCHLINK_IF),
];

struct Console {
    src: DmiSource,
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
    consoles: HashMap<u16, Console>,
    next_stream: u16,
    lock_timeout: Duration,
    /// The client the Link is lent to (its session dropped so the client can open the Link
    /// directly), and whether a connection existed to be restored.
    lent: Option<(u64, bool)>,
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

/// Label-boundary prefix match (core §7.2).
fn name_matches(name: &str, prefix: &str, exact: bool) -> bool {
    if exact {
        return name == prefix;
    }
    prefix.is_empty() || name == prefix || name.starts_with(&format!("{prefix}."))
}

fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut v = vec![tag, value.len() as u8];
    v.extend_from_slice(value);
    v
}

impl WchUpstream {
    pub(crate) fn new(entry: Entry, lock_timeout: Duration) -> Self {
        WchUpstream {
            entry,
            session: None,
            dirty: false,
            consoles: HashMap::new(),
            next_stream: 1,
            lock_timeout,
            lent: None,
        }
    }

    pub(crate) fn limits(&self) -> Limits {
        Limits {
            revision: 1,
            max_frame: 4096,
            window: 1 << 16,
            max_inflight: 16,
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
                    && [console::op::READ, console::op::MARKS, console::op::WRITE].contains(&c.op));
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
            o if o == oep_core::op::LIST => {
                let (Some(&flags), Some(first), Some(&n)) = (p.first(), le16(p, 1), p.get(3))
                else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let prefix = String::from_utf8_lossy(p.get(4..4 + usize::from(n)).unwrap_or(&[]))
                    .into_owned();
                let hits: Vec<&(u16, &str)> = INTERFACES
                    .iter()
                    .filter(|(_, name)| name_matches(name, &prefix, flags & 1 != 0))
                    .collect();
                let mut out = (hits.len() as u16).to_le_bytes().to_vec();
                let page = &hits[usize::from(first).min(hits.len())..];
                out.push(page.len() as u8);
                for (func, name) in page {
                    out.extend_from_slice(&func.to_le_bytes());
                    out.extend_from_slice(&0u16.to_le_bytes()); // instance
                    out.extend_from_slice(&[1, 0, name.len() as u8]);
                    out.extend_from_slice(name.as_bytes());
                }
                ok(out)
            }
            o if o == oep_core::op::DESCRIBE => {
                let Some(func) = le16(p, 0) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let tlvs: Vec<u8> = match func {
                    f if f == oep_core::FN => tlv(
                        oep_core::tlvs::describe::MODEL,
                        format!("{} via ch32rv broker", self.model()).as_bytes(),
                    ),
                    FN_RVSWD | FN_SWIO => tlv(wire::tlvs::describe::MAX_CONNECTIONS, &[1]),
                    FN_DM => {
                        let mut v = tlv(registry::describe_common::FEATURES, &0x7u32.to_le_bytes());
                        v.extend(tlv(
                            registry::describe_common::MAX_LENGTH,
                            &1024u16.to_le_bytes(),
                        ));
                        v
                    }
                    FN_CONSOLE => tlv(
                        console::tlvs::describe::MECHANISMS,
                        &[
                            console::enums::mechanism::DMDATA,
                            console::enums::mechanism::DMSEQ,
                        ],
                    ),
                    _ => return rejected(reject_reasons::UNAVAILABLE),
                };
                let mut out = vec![0u8];
                out.extend(tlvs);
                ok(out)
            }
            _ => rejected(reject_reasons::UNSUPPORTED),
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
            let source = if x.mech == console::enums::mechanism::DMSEQ {
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

    fn wire(&mut self, c: &Call) -> Reply {
        let p = &c.payload;
        match c.op {
            o if o == wire::op::ATTACH => {
                let Some(&method) = p.first() else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let tlvs = parse_tlvs(&p[1..]).unwrap_or_default();
                let max_speed = tlvs
                    .iter()
                    .find(|t| t.tag & 0x7F == wire::tlvs::attach::MAX_SPEED)
                    .and_then(|t| le32(&t.value, 0));
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
                let Some(s) = self.session.as_mut() else {
                    return failed(vec![status::LINE]);
                };
                if method == wire::enums::attach_method::HALT && s.dm().halt().is_err() {
                    return failed(vec![status::TIMEOUT]);
                }
                let dmstatus = s.dm().dmstatus().unwrap_or(0);
                let chip_id = s.attach.chip_id;
                let mut out = CONN.to_le_bytes().to_vec();
                out.extend_from_slice(&dmstatus.to_le_bytes());
                out.push(if existing { 2 } else { 0 });
                let hz = match max_speed {
                    Some(hz) if hz <= 400_000 => 400_000u32,
                    Some(hz) if hz <= 4_000_000 => 4_000_000,
                    _ => 6_000_000,
                };
                out.extend_from_slice(&hz.to_le_bytes());
                if chip_id != 0 && chip_id != u32::MAX {
                    let mut v = vec![wire::enums::target_id_scheme::WCH_DMI_7F];
                    v.extend_from_slice(&chip_id.to_le_bytes());
                    out.extend(tlv(wire::tlvs::attach_answer::TARGET_ID, &v));
                }
                ok(out)
            }
            o if o == wire::op::DETACH => {
                if le16(p, 0) != Some(CONN) || self.session.is_none() {
                    return rejected(reject_reasons::NO_CONNECTION);
                }
                self.consoles.clear();
                self.session = None; // Session's drop detaches the core and frees the lock
                ok(Vec::new())
            }
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    fn session_for(&mut self, p: &[u8]) -> Result<&mut Session, Reply> {
        if le16(p, 0) != Some(CONN) {
            return Err(rejected(reject_reasons::NO_CONNECTION));
        }
        self.session
            .as_mut()
            .ok_or_else(|| rejected(reject_reasons::NO_CONNECTION))
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
            o if o == dm::op::DMI => dmi_steps(s, &p[2..]),
            o if o == dm::op::HALT => st(s.dm().halt(), status::TIMEOUT),
            o if o == dm::op::RESUME => st(s.dm().resume(), status::STATE),
            o if o == dm::op::RESET => {
                let mode = match p.get(2) {
                    Some(&m) if m == dm::enums::reset_mode::HALT_AT_RESET => ResetMode::HaltAtReset,
                    Some(&m) if m == dm::enums::reset_mode::RUN_VERIFIED => ResetMode::RunVerified,
                    Some(_) => ResetMode::Run,
                    None => return rejected(reject_reasons::MALFORMED),
                };
                match DmTarget::new(s.link()).reset(mode) {
                    Ok(r) => {
                        // en: flags bit0 = reached the mode, bit1 = checked it runs (mode 1 only,
                        // and success needs it; oep-if-debug §4.3).
                        // ja: bit0 = mode の状態に達した、bit1 = 走っているのを確かめた(mode 1 だけ)。
                        let (reached, flags) = if mode == ResetMode::RunVerified {
                            let running = s.dm().is_running().unwrap_or(false);
                            (running, if running { 0b11 } else { 0 })
                        } else {
                            (true, 0b01)
                        };
                        let mut out = vec![status::OK, flags, 1];
                        out.extend_from_slice(&r.pc.to_le_bytes());
                        if reached { ok(out) } else { failed(out) }
                    }
                    Err(_) => failed(vec![status::FAULT]),
                }
            }
            o if o == dm::op::READ_BLOCK => {
                let (Some(addr), Some(count)) = (le32(&p, 2), le16(&p, 6)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
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
                let (Some(addr), Some(count)) = (le32(&p, 2), le16(&p, 6)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let words: Vec<u32> = (0..usize::from(count))
                    .filter_map(|i| le32(&p, 8 + 4 * i))
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
            o if o == dm::op::RUN => run(s, &p[2..]),
            _ => rejected(reject_reasons::UNKNOWN_OPERATION),
        }
    }

    fn console(&mut self, c: &Call) -> Reply {
        let p = &c.payload;
        match c.op {
            o if o == console::op::OPEN => {
                let Some(&mech) = p.get(2) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                if let Some((&id, _)) = self.consoles.iter().find(|(_, x)| x.mech == mech) {
                    let mut out = id.to_le_bytes().to_vec();
                    out.push(1);
                    return ok(out);
                }
                let source = match mech {
                    m if m == console::enums::mechanism::DMDATA => MonitorSource::Dmdata,
                    m if m == console::enums::mechanism::DMSEQ => MonitorSource::Dmseq,
                    _ => return rejected(reject_reasons::UNSUPPORTED),
                };
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
                let id = self.next_stream;
                self.next_stream = self.next_stream.wrapping_add(1).max(1);
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
                let mut out = id.to_le_bytes().to_vec();
                out.push(0);
                ok(out)
            }
            o if o == console::op::READ => {
                let (Some(id), Some(&from), Some(arg), Some(max)) = (
                    le16(p, 0),
                    p.get(2),
                    p.get(3..11)
                        .map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]))),
                    le16(p, 11),
                ) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let Some(x) = self.consoles.get(&id) else {
                    return rejected(reject_reasons::UNAVAILABLE);
                };
                let end = x.base + x.buf.len() as u64;
                let want = match from {
                    0 => arg,
                    2 => end,
                    _ => x.base, // oldest; no marks are kept here, so the last mark is the oldest
                };
                let start = want.clamp(x.base, end);
                let skip = (start - x.base) as usize;
                let n = usize::from(max).min(x.buf.len() - skip);
                let mut out = start.to_le_bytes().to_vec();
                let more = skip + n < x.buf.len();
                let gap = want < x.base;
                out.push(u8::from(more) | (u8::from(gap) << 1));
                out.extend(x.buf.iter().skip(skip).take(n));
                ok(out)
            }
            o if o == console::op::MARKS => ok(vec![0, 0]),
            o if o == console::op::WRITE => {
                let (Some(id), Some(n)) = (le16(p, 0), le16(p, 2)) else {
                    return rejected(reject_reasons::MALFORMED);
                };
                let Some(x) = self.consoles.get_mut(&id) else {
                    return rejected(reject_reasons::UNAVAILABLE);
                };
                let data = p.get(4..4 + usize::from(n)).unwrap_or(&[]);
                x.input.extend_from_slice(data);
                ok((data.len() as u16).to_le_bytes().to_vec())
            }
            o if o == console::op::CLOSE => {
                if let Some(id) = le16(p, 0) {
                    self.consoles.remove(&id);
                }
                ok(Vec::new())
            }
            o if o == console::op::CLEAR || o == console::op::MARK => ok(Vec::new()),
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
    let mut out = done.to_le_bytes().to_vec();
    out.push(st);
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
            for v in r.outs {
                out.extend_from_slice(&v.to_le_bytes());
            }
            if r.stopped { ok(out) } else { failed(out) }
        }
        Err(_) => failed(vec![status::FAULT]),
    }
}
