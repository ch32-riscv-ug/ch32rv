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
    pub connection: u16,
    pub dmstatus: u32,
    /// A pending havereset was acknowledged.
    pub acked_havereset: bool,
    /// The line already had a connection; this is it.
    pub existing: bool,
    pub speed_hz: u32,
    /// DMI 0x7F of a WCH debug module (target_id scheme 1), when the probe read one.
    pub wch_chip_id: Option<u32>,
    /// The hart is halted (flags bit3), and its dpc (TLV 0x11).
    pub halted: Option<u32>,
}

/// Attach options.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttachOptions {
    /// Halt the hart (method 1) instead of leaving it running.
    pub halt: bool,
    /// Never faster than this (sent critical, always: the probe requires one). `None`: the
    /// wire's declared `max_clock_hz`, else 1 MHz.
    pub max_speed_hz: Option<u32>,
    /// The (swdio, swclk) pair; swclk 0xFFFF on one wire (sent critical).
    pub pins: Option<(u16, u16)>,
    /// How the line idles (`wire_rvswd::enums::idle_clock`; rvswd only, sent critical). `None`:
    /// the probe's default (high). A slot's value is the host's to pass (oep-spec 5bfe052).
    pub idle_clock: Option<u8>,
    /// Hold the target's reset line (channel, ms) through the attach (TLV reset, critical): with
    /// `halt`, the hart is stopped as early as the probe can after the release (oep-if-debug §3).
    pub reset: Option<(u16, u16)>,
}

/// `attach` on `kind`.
pub fn attach(p: &mut Probe, kind: WireKind, o: AttachOptions) -> Result<Attached, OepError> {
    let func = p.interface(kind.interface())?.func;
    let method = if o.halt {
        wire::enums::attach_method::HALT
    } else {
        wire::enums::attach_method::RUN
    };
    let mut pl = vec![method];
    // Required on attach (oep-if-debug §1); without the caller's, the wire's own ceiling.
    let hz = match o.max_speed_hz {
        Some(hz) => hz,
        None => p
            .describe(func)?
            .iter()
            .find(|t| t.tag == describe_common::MAX_CLOCK_HZ && t.value.len() == 4)
            .map_or(1_000_000, |t| le32(&t.value, 0)),
    };
    put_tlv(
        &mut pl,
        wire::tlvs::attach::MAX_SPEED,
        true,
        &hz.to_le_bytes(),
    );
    if let Some((d, c)) = o.pins {
        let mut v = d.to_le_bytes().to_vec();
        v.extend_from_slice(&c.to_le_bytes());
        put_tlv(&mut pl, wire::tlvs::attach::PINS, true, &v);
    }
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
    if a.len() < 11 {
        return Err(OepError::Malformed(
            "attach answer shorter than 11 bytes".into(),
        ));
    }
    let tail = parse_tlvs(&a[11..]).map_err(|e| OepError::Malformed(e.to_string()))?;
    let wch_chip_id = tail
        .iter()
        .find(|t| t.tag == wire::tlvs::attach_answer::TARGET_ID)
        .and_then(|t| {
            (t.value.len() == 5
                && t.value[0] == crate::registry::common::enum_::target_id_scheme::WCH_DMI_7F)
                .then(|| le32(&t.value, 1))
        });
    // connection(u16), id(u32: DMSTATUS), flags(u8: attach_flags), speed_hz(u32), [TLV]
    use wire::enums::attach_flags as f;
    let halted = (a[6] & f::HALTED != 0).then(|| {
        tail.iter()
            .find(|t| t.tag == wire::tlvs::attach_answer::DPC && t.value.len() >= 4)
            .map_or(0, |t| le32(&t.value, 0))
    });
    Ok(Attached {
        connection: le16(&a, 0),
        dmstatus: le32(&a, 2),
        acked_havereset: a[6] & f::HAVERESET_ACKED != 0,
        existing: a[6] & f::EXISTING != 0,
        speed_hz: le32(&a, 7),
        wch_chip_id,
        halted,
    })
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

/// en: `scan` every pair the probe allows (count 0), going on with `skip` until a scan tries none
/// (oep-if-debug §1: at most 255 pairs a scan). The pairs found go to attach as they are.
/// ja: probe が許すすべての組を scan する(count 0)。1 回は 255 組までなので、skip で続け、何も試さ
/// なくなったら終わり。見つかった組はそのまま attach の pins に渡せる。
pub fn scan_all(p: &mut Probe, kind: WireKind) -> Result<Vec<Found>, OepError> {
    let func = p.interface(kind.interface())?.func;
    let mut found = Vec::new();
    let mut skip: u32 = 0;
    // A probe offers far fewer pairs than this; the bound only stops a probe that never says 0.
    for _ in 0..512 {
        let mut pl = vec![0u8];
        if skip > 0 {
            let s = u16::try_from(skip).unwrap_or(u16::MAX);
            put_tlv(&mut pl, wire::tlvs::scan::SKIP, false, &s.to_le_bytes());
        }
        let a = check(p.call(func, wire::op::SCAN, pl)?)?;
        let (Some(&tried), Some(&count)) = (a.first(), a.get(1)) else {
            return Err(OepError::Malformed(
                "scan answer shorter than 2 bytes".into(),
            ));
        };
        // count x (len(u8), kind, swdio, swclk, DMSTATUS); an unknown tail is skipped.
        let mut at = 2;
        for _ in 0..count {
            let len = usize::from(
                *a.get(at)
                    .ok_or_else(|| OepError::Malformed("scan entry cut short".into()))?,
            );
            let e = a
                .get(at + 1..at + 1 + len)
                .ok_or_else(|| OepError::Malformed("scan entry cut short".into()))?;
            at += 1 + len;
            if e.len() < 9 {
                return Err(OepError::Malformed(
                    "scan entry shorter than 9 bytes".into(),
                ));
            }
            found.push(Found {
                kind: e[0],
                swdio: u16::from_le_bytes([e[1], e[2]]),
                swclk: u16::from_le_bytes([e[3], e[4]]),
                status: u32::from_le_bytes([e[5], e[6], e[7], e[8]]),
            });
        }
        if tried == 0 {
            break;
        }
        skip += u32::from(tried);
    }
    Ok(found)
}

/// `detach`: drop this session's use of the connection (`force` closes it for everyone).
pub fn detach(p: &mut Probe, kind: WireKind, connection: u16, force: bool) -> Result<(), OepError> {
    let func = p.interface(kind.interface())?.func;
    let mut pl = connection.to_le_bytes().to_vec();
    if force {
        put_tlv(&mut pl, wire::tlvs::detach::FORCE, true, &[]);
    }
    check(p.call(func, wire::op::DETACH, pl)?).map(|_| ())
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
    connection: u16,
    max_words: usize,
    /// The probe's `max_op_ms`: a run's timeout stays at or under it.
    max_op_ms: u32,
}

/// en: The most words one block request may carry: the describe `max_length` (bytes; the probe
/// declares it so that requests and answers fit its max_frame, oep-if-debug §4.5), not computed
/// from max_frame. Only a probe that does not declare it (it must) falls back to what fits
/// max_frame (session header 10, connection + address + count 8) and the reference 256 words.
/// ja: 1 回の block の語数の上限は describe の max_length(byte 数)。max_frame からは計算しない。宣言の
/// 無い probe(宣言は必須)だけ、max_frame に入る数と 256 語に落とす。
fn block_words(max_frame: u16, max_length: Option<u16>) -> usize {
    match max_length {
        Some(b) => (usize::from(b) / 4).max(1),
        None => ((usize::from(max_frame).saturating_sub(18)) / 4).clamp(1, 256),
    }
}

impl<'a> OepDtm<'a> {
    /// Use `connection` (from [`attach`]) through `probe`.
    pub fn new(probe: &'a mut Probe, connection: u16) -> Result<Self, OepError> {
        let func = probe.interface(dm::NAME)?.func;
        let max_length = probe
            .describe(func)?
            .iter()
            .find(|t| t.tag == describe_common::MAX_LENGTH && t.value.len() == 2)
            .map(|t| le16(&t.value, 0));
        let max_words = block_words(probe.limits().max_frame, max_length);
        let max_op_ms = probe.max_op_ms()?;
        Ok(OepDtm {
            probe,
            func,
            connection,
            max_words,
            max_op_ms,
        })
    }

    pub fn connection(&self) -> u16 {
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
    pub fn parts(&self) -> (u16, usize, u32) {
        (self.func, self.max_words, self.max_op_ms)
    }

    pub fn from_parts(
        probe: &'a mut Probe,
        connection: u16,
        (func, max_words, max_op_ms): (u16, usize, u32),
    ) -> Self {
        OepDtm {
            probe,
            func,
            connection,
            max_words,
            max_op_ms,
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

    /// A status-only answer (halt / resume): ok, or the status as the error.
    fn status_op(&mut self, op: u8, what: &str) -> Result<(), DmiError> {
        let body = self.body();
        let a = completed(self.call(op, body)?)?;
        match a.first() {
            Some(&s) if s == status::OK => Ok(()),
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

impl TargetAccess for OepDtm<'_> {
    fn max_block_words(&self) -> usize {
        self.max_words
    }

    fn read_words(&mut self, addr: u32, count: usize) -> Result<Vec<u32>, DmiError> {
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
            // en: The answer was lost on the way and the probe did not keep it for the resend
            // (core §5.2 lets it skip large answers: `result_lost`). A read changes nothing, so it
            // is asked again as a new request (seen on the V003 jig's CP2102, 2026-10-01).
            // ja: 答えが途中で失われ、probe は再送用に覚えていなかった(大きな応答は覚えなくてよい)。
            // read は何も変えないので、新しい要求としてもう一度聞く。
            let r = if r.resolution
                == Resolution::Rejected(crate::registry::reject_reasons::RESULT_LOST)
            {
                self.probe
                    .exchange(vec![call])
                    .map_err(transport)?
                    .pop()
                    .ok_or_else(|| short("read_block"))?
            } else {
                r
            };
            let a = completed(r)?;
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
            if a[2] != status::OK {
                return Err(failed("read_block", a[2]));
            }
        }
        Ok(out)
    }

    fn write_words(&mut self, addr: u32, words: &[u32]) -> Result<(), DmiError> {
        let calls = words
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
        for r in self.probe.exchange(calls).map_err(transport)? {
            let a = completed(r)?;
            match a.get(2) {
                Some(&s) if s == status::OK => {}
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
        self.status_op(dm::op::HALT, "halt")
    }

    fn resume_once(&mut self) -> Result<(), DmiError> {
        self.status_op(dm::op::RESUME, "resume")
    }

    fn reset(&mut self, mode: ResetMode) -> Result<ResetResult, DmiError> {
        let mut pl = self.body();
        pl.push(match mode {
            ResetMode::Run => dm::enums::reset_mode::RUN,
            ResetMode::RunVerified => dm::enums::reset_mode::RUN_VERIFIED,
            ResetMode::HaltAtReset => dm::enums::reset_mode::HALT_AT_RESET,
        });
        let r = self.call(dm::op::RESET, pl)?;
        // en: A reset that did not reach the mode is completed failed with the full answer
        // (oep-if-debug §4.3); only the outcome tells it from success.
        // ja: mode の状態に達しない reset は、同じ形の completed failed(§4.3)。outcome でだけ分かる。
        let reached = r.succeeded();
        let a = completed(r)?;
        if a.len() == 1 {
            return Err(failed("reset", a[0]));
        }
        if a.len() < 7 {
            return Err(short("reset"));
        }
        if a[0] != status::OK {
            return Err(failed("reset", a[0]));
        }
        if !reached {
            return Err(DmiError::NotReached(format!(
                "flags 0x{:02x}, {} attempt(s)",
                a[1], a[2]
            )));
        }
        Ok(ResetResult { pc: le32(&a, 3) })
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
    fn block_size_is_the_declared_max_length() {
        // Declared: taken as it is (oep-if-debug §4.5), whatever max_frame says.
        assert_eq!(block_words(1024, Some(1000)), 250);
        assert_eq!(block_words(4096, Some(2048)), 512);
        // Not declared (it must be): what fits max_frame, at most 256 words.
        assert_eq!(block_words(1024, None), 251);
        assert_eq!(block_words(4096, None), 256);
        assert_eq!(block_words(64, None), 11);
    }
}
