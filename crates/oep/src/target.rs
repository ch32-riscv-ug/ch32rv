//! en: `oep.wire.*` (attach / detach) and `oep.target.riscv-dm`, and [`OepDtm`], which puts a
//! connection behind `ch32rv-dmi`'s [`DtmAccess`] and [`TargetAccess`] (docs/oep-host.ja.md §4.2)
//! so the existing Debug Module code and the loader path run on an OEP probe unchanged.
//!
//! Failure shapes (oep-if-common §3): a completed failed / partial answer carries the success
//! layout with `done` and `status`, except that attach / scan and a run that cannot halt answer
//! `status(u8)` alone - both are accepted here.
//! ja: `oep.wire.*` と `oep.target.riscv-dm`、接続を `DtmAccess` / `TargetAccess` の裏に置く
//! [`OepDtm`]。失敗の応答は成功と同じ形か、status 1 byte だけの形(attach / scan / halt できない run)。

use std::time::Duration;

use ch32rv_dmi::{DmiError, DmiOp, DtmAccess, ResetMode, ResetResult, RunResult, TargetAccess};

use crate::codec::{Resolution, parse_tlvs, put_tlv};
use crate::link::Reply;
use crate::registry::{describe_common, status, target_riscv_dm as dm, wire_rvswd as wire};
use crate::session::{OepError, Probe, check};

/// Which wire the target hangs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireKind {
    /// Two-wire RVSWD (most CH32).
    Rvswd,
    /// One-wire SWIO (CH32V003 / V00x).
    Swio,
}

impl WireKind {
    pub fn interface(self) -> &'static str {
        match self {
            WireKind::Rvswd => crate::registry::wire_rvswd::NAME,
            WireKind::Swio => crate::registry::wire_swio::NAME,
        }
    }
}

/// What attach answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attached {
    /// A new connection, never shared (oep-if-debug §1; resource ids are u32, core §9).
    pub connection: u32,
    pub dmstatus: u32,
    /// A pending havereset was acknowledged.
    pub acked_havereset: bool,
    pub speed_hz: u32,
    /// DMI 0x7F of a WCH debug module (target_id scheme 1), when the probe read one.
    pub wch_chip_id: Option<u32>,
    /// The hart is halted (flags bit3), and its dpc (TLV dpc; 0 when the probe gave none).
    pub halted: Option<u32>,
    /// en: A ch32rv broker lent a connection it already holds (named extension
    /// [`SHARED_EXTENSION`]); a probe always makes a new one.
    /// ja: ch32rv のブローカーが既に持つ接続を貸した(名前つき拡張)。probe は常に新しい接続。
    pub existing: bool,
}

/// en: The named extension (core §2.2, TLV 0x7F) a ch32rv broker puts in an attach answer when
/// it lends a connection it already holds to another client (the broker shares one connection to
/// the probe among its clients). No data. ja: ブローカーが既存の接続を貸したときの名前つき拡張。
pub const SHARED_EXTENSION: &str = "io.github.ch32-riscv-ug.shared";

/// Whether `tail` carries the named extension `name` (value `name_len(u8) name data`).
pub fn has_named_extension(tail: &[crate::codec::Tlv], name: &str) -> bool {
    tail.iter().any(|t| {
        t.tag & 0x7F == crate::registry::constants::TAG_NAMED_EXTENSION
            && t.value
                .first()
                .is_some_and(|&n| t.value.get(1..1 + usize::from(n)) == Some(name.as_bytes()))
    })
}

/// Attach options.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttachOptions {
    /// Halt the hart: attach method 1 where the wire offers it (describe features
    /// `attach_halt`), else attach running and halt right after (oep-if-debug §3).
    pub halt: bool,
    /// Never faster than this (attach's fixed part, never 0). `None`: the wire's declared
    /// `max_clock_hz`, else 1 MHz. Raised to the wire's `min_clock_hz` (a lower one is refused).
    pub max_speed_hz: Option<u32>,
    /// The (swdio, swclk) pair; swclk 0xFFFF on one wire. `None`: the one pair the wire's
    /// describe allows (an error when it allows several: name the port).
    pub pins: Option<(u16, u16)>,
    /// How the line idles (`wire_rvswd::enums::idle_clock`; rvswd only, sent critical). `None`:
    /// the probe's default (high).
    pub idle_clock: Option<u8>,
    /// Hold the target's reset line (channel, ms) through the attach (TLV reset, critical; the
    /// channel must be one of describe role_channels role 3): with `halt`, the hart is stopped
    /// as early as the probe can after the release (oep-if-debug §3).
    pub reset: Option<(u16, u16)>,
}

/// en: The (swdio, swclk) pairs a wire's describe allows (core §7.4, oep-if-debug §1): per
/// channel_group its role 1 / role 2 (a role the group lists is that group's channel, kept when
/// role_channels lists the role too and has the channel; a role the group does not list comes
/// from role_channels), else role_channels' role 1 x role 2 without a channel used twice. One
/// wire: swclk 0xFFFF. ja: wire の describe が許すピンの組。
pub fn candidate_pairs(p: &mut Probe, kind: WireKind) -> Result<Vec<(u16, u16)>, OepError> {
    let func = p.interface(kind.interface())?.func;
    let tlvs = p.describe(func)?;
    let mut roles: std::collections::BTreeMap<u8, Vec<u16>> = Default::default();
    let mut groups: std::collections::BTreeMap<u8, Vec<(u8, u16)>> = Default::default();
    for t in &tlvs {
        let v = &t.value;
        if t.tag == describe_common::ROLE_CHANNELS && v.len() >= 3 {
            // role(u8) base(u16) bitmap: bit i = channel base + i.
            let base = le16(v, 1);
            let list = roles.entry(v[0]).or_default();
            for (byte, b) in v[3..].iter().enumerate() {
                for bit in 0..8 {
                    if b & (1 << bit) != 0 {
                        list.push(base + (byte * 8 + bit) as u16);
                    }
                }
            }
        } else if t.tag == describe_common::CHANNEL_GROUP && v.len() >= 2 {
            // group(u8) n(u8) n x (role(u8) channel(u16)).
            let n = usize::from(v[1]);
            let pins = (0..n)
                .filter_map(|k| v.get(2 + 3 * k..5 + 3 * k).map(|e| (e[0], le16(e, 1))))
                .collect();
            groups.insert(v[0], pins);
        }
    }
    for list in roles.values_mut() {
        list.sort_unstable();
        list.dedup();
    }
    let one_wire = kind == WireKind::Swio;
    let options = |group: &[(u8, u16)], role: u8| -> Vec<u16> {
        match group.iter().find(|(r, _)| *r == role) {
            Some(&(_, ch)) => match roles.get(&role) {
                Some(list) if !list.contains(&ch) => Vec::new(),
                _ => vec![ch],
            },
            None => roles.get(&role).cloned().unwrap_or_default(),
        }
    };
    let group_list: Vec<Vec<(u8, u16)>> = if groups.is_empty() {
        vec![Vec::new()]
    } else {
        groups.into_values().collect()
    };
    let mut out = Vec::new();
    for g in &group_list {
        let dios = options(g, wire::enums::pin_role::SWDIO);
        let clks = if one_wire {
            vec![0xFFFF]
        } else {
            options(g, wire::enums::pin_role::SWCLK)
        };
        for &d in &dios {
            for &c in &clks {
                if (one_wire || d != c) && !out.contains(&(d, c)) {
                    out.push((d, c));
                }
            }
        }
    }
    Ok(out)
}

/// The first copy of describe tag `tag` on `func` as a u32 (core §2.3), if it is one.
fn describe_u32(p: &mut Probe, func: u16, tag: u8) -> Result<Option<u32>, OepError> {
    Ok(p.describe(func)?
        .iter()
        .find(|t| t.tag == tag)
        .filter(|t| t.value.len() == 4)
        .map(|t| le32(&t.value, 0)))
}

/// `attach` on `kind`: a new connection (oep-if-debug §1, §3).
pub fn attach(p: &mut Probe, kind: WireKind, o: AttachOptions) -> Result<Attached, OepError> {
    let func = p.interface(kind.interface())?.func;
    let pins = match o.pins {
        Some(pins) => pins,
        None => {
            let pairs = candidate_pairs(p, kind)?;
            match pairs[..] {
                [one] => one,
                _ => {
                    return Err(OepError::Malformed(format!(
                        "name the pins: {} allows {} pairs",
                        kind.interface(),
                        pairs.len()
                    )));
                }
            }
        }
    };
    // Method 1 only where the wire offers it (describe features bit0 attach_halt).
    let features = p
        .describe(func)?
        .iter()
        .find(|t| t.tag == describe_common::FEATURES)
        .and_then(|t| t.value.first().copied())
        .unwrap_or(0);
    let halt_in_attach = o.halt && features & wire::enums::features::ATTACH_HALT != 0;
    let method = if halt_in_attach {
        wire::enums::attach_method::HALT
    } else {
        wire::enums::attach_method::RUN
    };
    let declared_max = describe_u32(p, func, describe_common::MAX_CLOCK_HZ)?;
    let min_hz = describe_u32(p, func, describe_common::MIN_CLOCK_HZ)?.unwrap_or(1);
    let hz = o
        .max_speed_hz
        .or(declared_max)
        .unwrap_or(1_000_000)
        .max(min_hz)
        .max(1);
    // method(u8) swdio(u16) swclk(u16) max_speed(u32), then TLVs.
    let mut pl = vec![method];
    pl.extend_from_slice(&pins.0.to_le_bytes());
    pl.extend_from_slice(&pins.1.to_le_bytes());
    pl.extend_from_slice(&hz.to_le_bytes());
    // Low only: high is the default, so the TLV says something only when it is low.
    if kind == WireKind::Rvswd && o.idle_clock == Some(wire::enums::idle_clock::LOW) {
        put_tlv(
            &mut pl,
            wire::tlvs::attach::IDLE_CLOCK,
            true,
            &[wire::enums::idle_clock::LOW],
        );
    }
    if let Some((channel, hold_ms)) = o.reset {
        let mut v = channel.to_le_bytes().to_vec();
        v.extend_from_slice(&hold_ms.to_le_bytes());
        put_tlv(&mut pl, wire::tlvs::attach::RESET, true, &v);
    }
    let a = check(p.call(func, wire::op::ATTACH, pl)?)?;
    // connection(u32) id(u32: DMSTATUS) flags(u8: attach_flags) speed_hz(u32), [TLV]
    if a.len() < 13 {
        return Err(OepError::Malformed(
            "attach answer shorter than 13 bytes".into(),
        ));
    }
    let tail = parse_tlvs(&a[13..]).map_err(|e| OepError::Malformed(e.to_string()))?;
    let wch_chip_id = tail
        .iter()
        .find(|t| t.tag == wire::tlvs::attach_answer::TARGET_ID)
        .and_then(|t| {
            (t.value.len() == 5
                && t.value[0] == crate::registry::common::enum_::target_id_scheme::DMI_7F)
                .then(|| le32(&t.value, 1))
        });
    use wire::enums::attach_flags as f;
    let flags = a[8];
    let mut halted = (flags & f::HALTED != 0).then(|| {
        tail.iter()
            .find(|t| t.tag == wire::tlvs::attach_answer::DPC && t.value.len() == 4)
            .map_or(0, |t| le32(&t.value, 0))
    });
    let connection = le32(&a, 0);
    let existing = has_named_extension(&tail, SHARED_EXTENSION);
    if o.halt && halted.is_none() {
        // The wire has no halting attach: halt now, the riscv-dm halt op if offered, else
        // haltreq through dmi in one request (oep-if-debug §4.1).
        halt_after_attach(p, connection)?;
        halted = Some(0);
    }
    Ok(Attached {
        connection,
        dmstatus: le32(&a, 4),
        acked_havereset: flags & f::HAVERESET_ACKED != 0,
        speed_hz: le32(&a, 9),
        wch_chip_id,
        halted,
        existing,
    })
}

/// Halt the hart on `connection` right after a running attach.
fn halt_after_attach(p: &mut Probe, connection: u32) -> Result<(), OepError> {
    let func = p.interface(dm::NAME)?.func;
    let body = connection.to_le_bytes().to_vec();
    if p.offers(func, dm::op::HALT)? {
        let a = check(p.call(func, dm::op::HALT, body)?)?;
        return match a.first() {
            Some(&s) if s == status::OK => Ok(()),
            _ => Err(OepError::Malformed("halt after attach failed".into())),
        };
    }
    // haltreq, wait for allhalted (DMSTATUS bit 9), then drop haltreq (dmactive kept).
    use crate::registry::target_riscv_dm::enums::dmi_step as k;
    let mut pl = body;
    pl.extend_from_slice(&3u16.to_le_bytes());
    pl.extend_from_slice(&[k::WRITE, 0x10]);
    pl.extend_from_slice(&0x8000_0001u32.to_le_bytes());
    pl.extend_from_slice(&[k::POLL_READS, 0x11]);
    pl.extend_from_slice(&0x200u32.to_le_bytes());
    pl.extend_from_slice(&0x200u32.to_le_bytes());
    pl.extend_from_slice(&64u16.to_le_bytes());
    pl.extend_from_slice(&[k::WRITE, 0x10]);
    pl.extend_from_slice(&1u32.to_le_bytes());
    let a = check(p.call(func, dm::op::DMI, pl)?)?;
    match a.get(2) {
        Some(&s) if s == status::OK => Ok(()),
        _ => Err(OepError::Malformed("halt after attach (dmi) failed".into())),
    }
}

/// One pair a scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// 0x01 riscv-dm.
    pub kind: u8,
    pub swdio: u16,
    /// 0xFFFF on one wire.
    pub swclk: u16,
    /// The debug module's DMSTATUS as read there.
    pub status: u32,
}

/// en: `scan` the pairs the wire's describe allows (the host builds the list; scan has no
/// implicit one, oep-if-debug §1): up to 255 pairs a request, the probe answering how many it
/// tried from the front; the rest go in the next request. A scan of an rvswd line may wake and
/// reset a target: it is an explicit tool action, never part of resolving an address.
/// ja: describe が許す組を scan する(1 回 255 組まで、tried だけ進めて残りを次へ)。
pub fn scan_all(p: &mut Probe, kind: WireKind) -> Result<Vec<Found>, OepError> {
    let func = p.interface(kind.interface())?.func;
    let mut pairs = candidate_pairs(p, kind)?;
    let mut found = Vec::new();
    while !pairs.is_empty() {
        let chunk = &pairs[..pairs.len().min(255)];
        let mut pl = vec![chunk.len() as u8];
        for (d, c) in chunk {
            pl.extend_from_slice(&d.to_le_bytes());
            pl.extend_from_slice(&c.to_le_bytes());
        }
        let a = check(p.call(func, wire::op::SCAN, pl)?)?;
        let (Some(&tried), Some(&count)) = (a.first(), a.get(1)) else {
            return Err(OepError::Malformed(
                "scan answer shorter than 2 bytes".into(),
            ));
        };
        // count x (kind, swdio, swclk, id): 9 bytes, no element length (debug §1, core §2.3).
        for k in 0..usize::from(count) {
            let e = a
                .get(2 + k * 9..2 + (k + 1) * 9)
                .ok_or_else(|| OepError::Malformed("scan entry cut short".into()))?;
            found.push(Found {
                kind: e[0],
                swdio: u16::from_le_bytes([e[1], e[2]]),
                swclk: u16::from_le_bytes([e[3], e[4]]),
                status: u32::from_le_bytes([e[5], e[6], e[7], e[8]]),
            });
        }
        if tried == 0 {
            return Err(OepError::Malformed(format!(
                "scan tried none of {} pairs",
                chunk.len()
            )));
        }
        pairs.drain(..usize::from(tried).min(pairs.len()));
    }
    Ok(found)
}

/// One live connection of a wire (oep-if-debug §2.1 `connections`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub connection: u32,
    pub swdio: u16,
    pub swclk: u16,
    pub speed_hz: u32,
    /// The target id read at attach: (scheme, value).
    pub target_id: Option<(u8, Vec<u8>)>,
}

/// en: `connections` of the wire at `func` (lock-free), following `more`: each entry is
/// connection(u32) swdio(u16) swclk(u16) speed_hz(u32) tid_scheme(u8) tid_len(u8) tid.
/// ja: wire の connections(ロック不要)。
pub fn connections(p: &mut Probe, func: u16) -> Result<Vec<Connection>, OepError> {
    let mut out = Vec::new();
    for _ in 0..16 {
        let first = u8::try_from(out.len()).unwrap_or(u8::MAX);
        let a = check(p.link().call(crate::link::Call {
            func,
            op: wire::op::CONNECTIONS,
            session: None,
            payload: vec![first],
        })?)?;
        let (Some(&more), Some(&count)) = (a.first(), a.get(1)) else {
            return Err(OepError::Malformed("connections answer too short".into()));
        };
        let mut at = 2;
        for _ in 0..count {
            let e = a
                .get(at..at + 14)
                .ok_or_else(|| OepError::Malformed("connections entry cut short".into()))?;
            let len = usize::from(e[13]);
            let tid = a
                .get(at + 14..at + 14 + len)
                .ok_or_else(|| OepError::Malformed("connections tid cut short".into()))?
                .to_vec();
            out.push(Connection {
                connection: le32(e, 0),
                swdio: le16(e, 4),
                swclk: le16(e, 6),
                speed_hz: le32(e, 8),
                target_id: (e[12] != 0).then_some((e[12], tid)),
            });
            at += 14 + len;
        }
        if more == 0 || count == 0 {
            break;
        }
    }
    Ok(out)
}

/// `detach`: close the connection and its streams (oep-if-debug §2).
pub fn detach(p: &mut Probe, kind: WireKind, connection: u32) -> Result<(), OepError> {
    let func = p.interface(kind.interface())?.func;
    check(p.call(func, wire::op::DETACH, connection.to_le_bytes().to_vec())?).map(|_| ())
}

/// One DMI step (oep-if-debug §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmiStep {
    Write {
        addr: u8,
        value: u32,
    },
    Read {
        addr: u8,
    },
    /// Read until `value & mask == want`, at most `max_reads` times.
    PollReads {
        addr: u8,
        mask: u32,
        want: u32,
        max_reads: u16,
    },
    WaitUs(u32),
}

/// What a dmi request did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmiResult {
    pub done: u16,
    pub status: u8,
    pub values: Vec<u32>,
}

/// An attached `oep.target.riscv-dm` connection, driven through a borrowed [`Probe`].
pub struct OepDtm<'a> {
    probe: &'a mut Probe,
    func: u16,
    connection: u32,
    max_words: usize,
    /// The probe's `max_op_ms`: a run's timeout stays at or under it.
    max_op_ms: u32,
    /// The optional ops the fn offers (only dmi is required, oep-if-debug §4).
    offered: Offered,
}

/// The riscv-dm ops beyond dmi that the fn declares in describe ops.
#[derive(Debug, Clone, Copy, Default)]
pub struct Offered {
    pub halt: bool,
    pub resume: bool,
    pub reset: bool,
    /// read_block and write_block (declared as a pair).
    pub blocks: bool,
    pub run: bool,
}

/// en: The most words one block request may carry (oep-if-debug §4.5): within the describe
/// `max_length` (bytes) and within max_frame - a write's request carries 20 bytes besides the
/// words (header 10, connection 4, address 4, count 2), a read's answer 8 - so the write's
/// bound, the smaller, is used for both. ja: max_length と max_frame の小さい方(write の 20 byte)。
fn block_words(max_frame: u16, max_length: Option<u16>) -> usize {
    let by_frame = usize::from(max_frame).saturating_sub(20) / 4;
    match max_length {
        Some(b) => (usize::from(b) / 4).min(by_frame).max(1),
        None => by_frame.clamp(1, 256),
    }
}

impl<'a> OepDtm<'a> {
    /// Use `connection` (from [`attach`]) through `probe`.
    pub fn new(probe: &'a mut Probe, connection: u32) -> Result<Self, OepError> {
        let func = probe.interface(dm::NAME)?.func;
        let max_length = probe
            .describe(func)?
            .iter()
            .find(|t| t.tag == describe_common::MAX_LENGTH && t.value.len() == 2)
            .map(|t| le16(&t.value, 0));
        let max_words = block_words(probe.limits().max_frame, max_length);
        let max_op_ms = probe.max_op_ms()?;
        let ops = probe.ops(func)?.unwrap_or_default();
        let offered = Offered {
            halt: ops.contains(&dm::op::HALT),
            resume: ops.contains(&dm::op::RESUME),
            reset: ops.contains(&dm::op::RESET),
            blocks: ops.contains(&dm::op::READ_BLOCK) && ops.contains(&dm::op::WRITE_BLOCK),
            run: ops.contains(&dm::op::RUN),
        };
        Ok(OepDtm {
            probe,
            func,
            connection,
            max_words,
            max_op_ms,
            offered,
        })
    }

    /// The optional riscv-dm ops this probe offers.
    pub fn offered(&self) -> Offered {
        self.offered
    }

    pub fn connection(&self) -> u32 {
        self.connection
    }

    /// The probe this connection goes through (for its other interfaces' describes).
    pub fn probe(&mut self) -> &mut Probe {
        self.probe
    }

    /// en: What [`Self::new`] learned (the riscv-dm fn and the block size), so a caller that makes
    /// an `OepDtm` per poll can skip the describe round trip with [`Self::from_parts`].
    /// ja: [`Self::new`] が調べた値(riscv-dm の fn と block の大きさ)。poll ごとに作る呼び出し側は
    /// [`Self::from_parts`] で describe の往復を省ける。
    pub fn parts(&self) -> (u16, usize, u32, Offered) {
        (self.func, self.max_words, self.max_op_ms, self.offered)
    }

    pub fn from_parts(
        probe: &'a mut Probe,
        connection: u32,
        (func, max_words, max_op_ms, offered): (u16, usize, u32, Offered),
    ) -> Self {
        OepDtm {
            probe,
            func,
            connection,
            max_words,
            max_op_ms,
            offered,
        }
    }

    fn body(&self) -> Vec<u8> {
        self.connection.to_le_bytes().to_vec()
    }

    fn call(&mut self, op: u8, payload: Vec<u8>) -> Result<Reply, DmiError> {
        self.probe.call(self.func, op, payload).map_err(transport)
    }

    /// Several DMI steps in one request (keep an abstract-command sequence together: the probe
    /// may poll the console between requests).
    pub fn dmi_batch(&mut self, steps: &[DmiStep]) -> Result<DmiResult, DmiError> {
        let mut pl = self.body();
        pl.extend_from_slice(&(steps.len() as u16).to_le_bytes());
        use crate::registry::target_riscv_dm::enums::dmi_step as k;
        for s in steps {
            match *s {
                DmiStep::Write { addr, value } => {
                    pl.extend_from_slice(&[k::WRITE, addr]);
                    pl.extend_from_slice(&value.to_le_bytes());
                }
                DmiStep::Read { addr } => pl.extend_from_slice(&[k::READ, addr]),
                DmiStep::PollReads {
                    addr,
                    mask,
                    want,
                    max_reads,
                } => {
                    pl.extend_from_slice(&[k::POLL_READS, addr]);
                    pl.extend_from_slice(&mask.to_le_bytes());
                    pl.extend_from_slice(&want.to_le_bytes());
                    pl.extend_from_slice(&max_reads.to_le_bytes());
                }
                DmiStep::WaitUs(us) => {
                    pl.push(k::WAIT_US);
                    pl.extend_from_slice(&us.to_le_bytes());
                }
            }
        }
        let r = self.call(dm::op::DMI, pl)?;
        let a = completed(r)?;
        // done(u16), status(u8), nvals(u16), nvals x value(u32), [TLV]
        if a.len() < 5 {
            return Err(short("dmi"));
        }
        let nvals = usize::from(le16(&a, 3));
        let Some(vals) = a.get(5..5 + 4 * nvals) else {
            return Err(short("dmi"));
        };
        let values = vals
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        Ok(DmiResult {
            done: le16(&a, 0),
            status: a[2],
            values,
        })
    }

    /// dpc through the Debug Module, with DATA1 / DATA0 read first and written back after.
    fn dpc_keeping_mailbox(&mut self) -> Result<u32, DmiError> {
        const DATA0: u8 = 0x04;
        const DATA1: u8 = 0x05;
        let kept =
            self.dmi_batch(&[DmiStep::Read { addr: DATA1 }, DmiStep::Read { addr: DATA0 }])?;
        let dpc = ch32rv_dmi::DebugModule::new(self).read_reg(ch32rv_dmi::RegName::Pc);
        if let [d1, d0] = kept.values[..] {
            // The order the target writes them in (dmseq: DATA1 before DATA0).
            self.dmi_batch(&[
                DmiStep::Write {
                    addr: DATA1,
                    value: d1,
                },
                DmiStep::Write {
                    addr: DATA0,
                    value: d0,
                },
            ])?;
        }
        dpc
    }

    /// A status-only answer (halt / resume): success and ok, or the status as the error.
    fn status_op(&mut self, op: u8, what: &str) -> Result<(), DmiError> {
        let body = self.body();
        let r = self.call(op, body)?;
        let ok = r.succeeded();
        let a = completed(r)?;
        match a.first() {
            Some(&s) if s == status::OK && ok => Ok(()),
            Some(&s) => Err(failed(what, s)),
            None => Err(short(what)),
        }
    }
}

impl DtmAccess for OepDtm<'_> {
    /// en: The probe's own resume (with the CH32 retry rule): it is what lets the probe poll its
    /// console again after the host drove the debug module through raw DMI. The rule reads dpc
    /// with an abstract command, which goes through DATA0; the probe gives nothing back on its
    /// resume (oep-if-debug §4: the host restores what it used), so the target's DATA0 / DATA1 -
    /// a dmseq frame waiting there - are put back around each read.
    /// ja: probe 自身の resume(CH32 の出し直しの規則つき)。規則の dpc の読みは DATA0 を通る abstract
    /// command で、probe は resume で何も戻さない(host が使ったものは host が戻す)。だから読むたびに
    /// target の DATA0 / DATA1(dmseq のフレームが待っている)を戻す。
    fn resume_hart(&mut self) -> Option<Result<(), DmiError>> {
        let r = ch32rv_dmi::resume_ch32(self, |t| t.dpc_keeping_mailbox());
        Some(match r {
            Ok(true) => Ok(()),
            Ok(false) => Err(DmiError::OperationFailed("hart did not resume".to_owned())),
            Err(e) => Err(e),
        })
    }

    fn dmi_read(&mut self, addr: u8) -> Result<u32, DmiError> {
        let r = self.dmi_batch(&[DmiStep::Read { addr }])?;
        match (r.status, r.values.first()) {
            (s, Some(&v)) if s == status::OK => Ok(v),
            (s, _) => Err(failed("dmi read", s)),
        }
    }

    fn dmi_write(&mut self, addr: u8, value: u32) -> Result<(), DmiError> {
        let r = self.dmi_batch(&[DmiStep::Write { addr, value }])?;
        if r.status == status::OK {
            Ok(())
        } else {
            Err(failed("dmi write", r.status))
        }
    }

    fn dmi_nop(&mut self) -> Result<(), DmiError> {
        // The probe's dmi steps are whole accesses; there is no bare nop to send.
        Ok(())
    }

    /// The whole sequence in one `dmi` request (oep-if-debug §2.1: the probe may poll the
    /// console between requests, never inside one).
    fn dmi_sequence(&mut self, ops: &[DmiOp]) -> Result<Vec<u32>, DmiError> {
        let steps: Vec<DmiStep> = ops
            .iter()
            .map(|op| match *op {
                DmiOp::Write(addr, value) => DmiStep::Write { addr, value },
                DmiOp::Read(addr) => DmiStep::Read { addr },
                DmiOp::PollClear {
                    addr,
                    mask,
                    max_reads,
                } => DmiStep::PollReads {
                    addr,
                    mask,
                    want: 0,
                    max_reads,
                },
            })
            .collect();
        let r = self.dmi_batch(&steps)?;
        match r.status {
            s if s == status::OK => Ok(r.values),
            s if s == status::TIMEOUT => Err(DmiError::Timeout),
            s => Err(failed("dmi", s)),
        }
    }
}

/// How often a read_block that came back `line` / `fault`, or whose answer was lost, is sent again.
const BLOCK_TRIES: usize = 4;
/// The pause before sending it again (a link drop on rvswd takes a few ms to clear).
const BLOCK_PAUSE: Duration = Duration::from_millis(10);

impl OepDtm<'_> {
    /// en: A block answer as (succeeded, payload). A read_block is sent again (as a new request,
    /// after a short pause) while it is `result_lost` or reports `line` / `fault`: reading the
    /// same words again changes nothing. A write_block is never sent again: a store that may
    /// have reached the target is not repeated under a new corr (core §5, oep-if-common §3; a
    /// FLASH key written twice jams the controller) - its failure goes to the caller, who
    /// checks the connection and starts over.
    /// ja: read_block は result_lost / line / fault で送り直す。write_block は送り直さない
    /// (届いたかもしれない書き込みは新しい corr で繰り返さない)。
    fn block_again(
        &mut self,
        mut r: Reply,
        call: (u16, u8, Vec<u8>),
    ) -> Result<(bool, Vec<u8>), DmiError> {
        let lost = Resolution::Rejected(crate::registry::reject_reasons::RESULT_LOST);
        if call.1 == dm::op::READ_BLOCK {
            for _ in 1..BLOCK_TRIES {
                let again = r.resolution == lost
                    || (matches!(r.resolution, Resolution::Completed(_))
                        && matches!(r.payload.get(2), Some(&s) if s == status::LINE || s == status::FAULT));
                if !again {
                    break;
                }
                std::thread::sleep(BLOCK_PAUSE);
                r = self
                    .probe
                    .exchange(vec![call.clone()])
                    .map_err(transport)?
                    .pop()
                    .ok_or_else(|| short("block"))?;
            }
        }
        let ok = r.succeeded();
        Ok((ok, completed(r)?))
    }
}

impl TargetAccess for OepDtm<'_> {
    fn max_block_words(&self) -> usize {
        self.max_words
    }

    fn read_words(&mut self, addr: u32, count: usize) -> Result<Vec<u32>, DmiError> {
        if !self.offered.blocks {
            // No block ops (optional, oep-if-debug §4.5): word by word through the Debug Module.
            let bytes = ch32rv_dmi::DebugModule::new(self).read_mem(addr, 4 * count as u32)?;
            return Ok(bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect());
        }
        let mut out = Vec::with_capacity(count);
        let mut calls = Vec::new();
        let mut at = 0;
        while at < count {
            let n = (count - at).min(self.max_words);
            let mut pl = self.body();
            pl.extend_from_slice(&(addr + 4 * at as u32).to_le_bytes());
            pl.extend_from_slice(&(n as u16).to_le_bytes());
            calls.push((self.func, dm::op::READ_BLOCK, pl));
            at += n;
        }
        let replies = self.probe.exchange(calls.clone()).map_err(transport)?;
        for (r, call) in replies.into_iter().zip(calls) {
            let (ok, a) = self.block_again(r, call)?;
            if a.len() < 3 {
                return Err(short("read_block"));
            }
            let done = usize::from(le16(&a, 0));
            out.extend(
                a[3..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .take(done)
                    .map(|c| u32::from_le_bytes(*c)),
            );
            if a[2] != status::OK || !ok {
                return Err(failed("read_block", a[2]));
            }
        }
        Ok(out)
    }

    fn write_words(&mut self, addr: u32, words: &[u32]) -> Result<(), DmiError> {
        if !self.offered.blocks {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            return ch32rv_dmi::DebugModule::new(self).write_mem(addr, &bytes);
        }
        let calls: Vec<(u16, u8, Vec<u8>)> = words
            .chunks(self.max_words)
            .enumerate()
            .map(|(i, chunk)| {
                let mut pl = self.body();
                pl.extend_from_slice(&(addr + (4 * i * self.max_words) as u32).to_le_bytes());
                pl.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
                for w in chunk {
                    pl.extend_from_slice(&w.to_le_bytes());
                }
                (self.func, dm::op::WRITE_BLOCK, pl)
            })
            .collect();
        let replies = self.probe.exchange(calls.clone()).map_err(transport)?;
        for (r, call) in replies.into_iter().zip(calls) {
            let (ok, a) = self.block_again(r, call)?;
            match a.get(2) {
                Some(&s) if s == status::OK && ok => {}
                Some(&s) => return Err(failed("write_block", s)),
                None => return Err(short("write_block")),
            }
        }
        Ok(())
    }

    fn run_until_halt(
        &mut self,
        pc: u32,
        regs: &[(u16, u32)],
        outs: &[u16],
        timeout: Duration,
    ) -> Result<RunResult, DmiError> {
        let mut pl = self.body();
        pl.extend_from_slice(&pc.to_le_bytes());
        // 1..=max_op_ms (core §7.5): 0 is malformed and more is unsupported.
        let ms = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX)
            .clamp(1, self.max_op_ms.max(1));
        pl.extend_from_slice(&ms.to_le_bytes());
        pl.push(regs.len() as u8);
        for (r, v) in regs {
            pl.extend_from_slice(&r.to_le_bytes());
            pl.extend_from_slice(&v.to_le_bytes());
        }
        pl.push(outs.len() as u8);
        for r in outs {
            pl.extend_from_slice(&r.to_le_bytes());
        }
        let a = completed(self.call(dm::op::RUN, pl)?)?;
        // status, stopped (run_stopped), dpc, elapsed_us, nvals(u8), nvals x value, [TLV]
        if a.len() < 11 {
            return Err(short("run"));
        }
        let s = a[0];
        if a[1] == dm::enums::run_stopped::NOT_RUN {
            // The preparation failed: the code did not run (oep-if-debug §4.4).
            return Err(failed("run (not run)", s));
        }
        if a[1] == dm::enums::run_stopped::NOT_HALTED {
            // Could not halt the hart after the timeout: dpc and values mean nothing.
            return Err(failed("run", s));
        }
        if s != status::OK && s != status::TIMEOUT {
            return Err(failed("run", s));
        }
        let nvals = usize::from(a[10]);
        if nvals < outs.len() || a.len() < 11 + 4 * nvals {
            return Err(short("run"));
        }
        Ok(RunResult {
            stopped: a[1] == dm::enums::run_stopped::STOPPED,
            dpc: le32(&a, 2),
            elapsed_us: le32(&a, 6),
            outs: (0..outs.len()).map(|i| le32(&a, 11 + 4 * i)).collect(),
        })
    }

    fn halt(&mut self) -> Result<(), DmiError> {
        if self.offered.halt {
            self.status_op(dm::op::HALT, "halt")
        } else {
            ch32rv_dmi::DebugModule::new(self).halt()
        }
    }

    fn resume_once(&mut self) -> Result<(), DmiError> {
        if self.offered.resume {
            self.status_op(dm::op::RESUME, "resume")
        } else {
            ch32rv_dmi::DebugModule::new(self).resume()
        }
    }

    fn reset(&mut self, mode: ResetMode) -> Result<ResetResult, DmiError> {
        // Mode 0 (run) always checks that the hart runs; mode 1 halts it at the reset vector
        // (oep-if-debug §4.3: halt_at_reset is 1 since oep-spec 675288a).
        let mut pl = self.body();
        pl.push(match mode {
            ResetMode::Run | ResetMode::RunVerified => dm::enums::reset_mode::RUN,
            ResetMode::HaltAtReset => dm::enums::reset_mode::HALT_AT_RESET,
        });
        let r = self.call(dm::op::RESET, pl)?;
        // en: A reset that did not reach the mode is completed failed with the same answer and
        // status timeout; only the outcome and flags bit0 (reached) tell it from success.
        // ja: mode の状態に達しない reset は、同じ形の completed failed(status timeout)。
        let succeeded = r.succeeded();
        let a = completed(r)?;
        // status(u8) flags(u8) pc(u32) [TLV] (oep-if-debug §4.3).
        if a.len() < 6 {
            return Err(short("reset"));
        }
        let reached = a[1] & dm::enums::reset_flags::REACHED != 0;
        if !succeeded && a[0] == status::TIMEOUT && !reached {
            return Err(DmiError::NotReached(format!("flags 0x{:02x}", a[1])));
        }
        if a[0] != status::OK || !succeeded {
            return Err(failed("reset", a[0]));
        }
        Ok(ResetResult { pc: le32(&a, 2) })
    }
}

/// The payload of a completed answer (success, failed or partial - the status inside says which).
fn completed(r: Reply) -> Result<Vec<u8>, DmiError> {
    match r.resolution {
        Resolution::Completed(_) => Ok(r.payload),
        _ => Err(transport(check(r).err().unwrap_or(OepError::Malformed(
            "answer neither completed nor rejected".into(),
        )))),
    }
}

fn transport(e: OepError) -> DmiError {
    DmiError::Transport(e.to_string())
}

fn failed(what: &str, s: u8) -> DmiError {
    let name = match s {
        x if x == status::WAIT => "wait",
        x if x == status::LINE => "line",
        x if x == status::FAULT => "fault",
        x if x == status::TIMEOUT => "timeout",
        x if x == status::STATE => "state",
        _ => "unknown status",
    };
    DmiError::OperationFailed(format!("{what}: {name} (0x{s:02x})"))
}

fn short(what: &str) -> DmiError {
    DmiError::Transport(format!("{what}: answer shorter than its fixed part"))
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::block_words;

    #[test]
    fn block_size_fits_max_length_and_max_frame() {
        // Declared: within it and within max_frame (a write's 20 bytes besides the words).
        assert_eq!(block_words(1024, Some(1000)), 250);
        assert_eq!(block_words(4096, Some(2048)), 512);
        assert_eq!(block_words(1024, Some(2048)), 251);
        // Not declared (it must be): what fits max_frame, at most 256 words.
        assert_eq!(block_words(1024, None), 251);
        assert_eq!(block_words(4096, None), 256);
        assert_eq!(block_words(64, None), 11);
    }
}
