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
}

/// Attach options.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttachOptions {
    /// Halt the hart (method 1) instead of leaving it running.
    pub halt: bool,
    /// Never faster than this (sent critical).
    pub max_speed_hz: Option<u32>,
    /// The (swdio, swclk) pair; swclk 0xFFFF on one wire (sent critical).
    pub pins: Option<(u16, u16)>,
}

/// `attach` on `kind`.
pub fn attach(p: &mut Probe, kind: WireKind, o: AttachOptions) -> Result<Attached, OepError> {
    let func = p.interface(kind.interface())?.func;
    let mut pl = vec![u8::from(o.halt)];
    if let Some(hz) = o.max_speed_hz {
        put_tlv(
            &mut pl,
            wire::tlvs::attach::MAX_SPEED,
            true,
            &hz.to_le_bytes(),
        );
    }
    if let Some((d, c)) = o.pins {
        let mut v = d.to_le_bytes().to_vec();
        v.extend_from_slice(&c.to_le_bytes());
        put_tlv(&mut pl, wire::tlvs::attach::PINS, true, &v);
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
            (t.value.len() == 5 && t.value[0] == wire::enums::target_id_scheme::WCH_DMI_7F)
                .then(|| le32(&t.value, 1))
        });
    Ok(Attached {
        connection: le16(&a, 0),
        dmstatus: le32(&a, 2),
        acked_havereset: a[6] & 1 != 0,
        existing: a[6] & 2 != 0,
        speed_hz: le32(&a, 7),
        wch_chip_id,
    })
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
}

/// en: The most words one block request may carry: what fits `max_frame` (session header 10,
/// connection + address + count 8, or on the answer 5 + done/status 3), the describe `max_length`
/// (bytes, oep-if-debug) if declared, and the reference probe's 256-word buffer.
/// ja: 1 回の block の語数の上限: max_frame に入る数、describe の max_length(byte 数)、参照 probe の 256 語。
fn block_words(max_frame: u16, max_length: Option<u16>) -> usize {
    let by_frame = (usize::from(max_frame).saturating_sub(18)) / 4;
    let by_decl = max_length.map_or(usize::MAX, |b| usize::from(b) / 4);
    by_frame.min(by_decl).clamp(1, 256)
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
        Ok(OepDtm {
            probe,
            func,
            connection,
            max_words,
        })
    }

    pub fn connection(&self) -> u16 {
        self.connection
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
        if a.len() < 3 {
            return Err(short("dmi"));
        }
        let values = a[3..]
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
        for r in self.probe.exchange(calls).map_err(transport)? {
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
        // Always finite: the probe refuses "no limit".
        let ms = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX - 1)
            .max(1);
        pl.extend_from_slice(&ms.min(u32::MAX - 1).to_le_bytes());
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
        if a.len() == 1 {
            // Could not halt the hart at all: status alone.
            return Err(failed("run", a[0]));
        }
        if a.len() < 10 + 4 * outs.len() {
            return Err(short("run"));
        }
        let s = a[0];
        if s != status::OK && s != status::TIMEOUT {
            return Err(failed("run", s));
        }
        Ok(RunResult {
            stopped: a[1] != 0,
            dpc: le32(&a, 2),
            elapsed_us: le32(&a, 6),
            outs: (0..outs.len()).map(|i| le32(&a, 10 + 4 * i)).collect(),
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
        let a = completed(self.call(dm::op::RESET, pl)?)?;
        if a.len() == 1 {
            return Err(failed("reset", a[0]));
        }
        if a.len() < 7 {
            return Err(short("reset"));
        }
        if a[0] != status::OK {
            return Err(failed("reset", a[0]));
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
    fn block_size_follows_the_smallest_limit() {
        assert_eq!(block_words(1024, None), 251);
        assert_eq!(block_words(1024, Some(1000)), 250);
        assert_eq!(block_words(4096, None), 256);
        assert_eq!(block_words(64, None), 11);
    }
}
