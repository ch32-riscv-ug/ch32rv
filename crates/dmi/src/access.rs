//! en: Transport-neutral target operations a probe may do faster than one DMI access at a time
//! (docs/oep-host.ja.md §4.2): word blocks, loader runs, and hart control with the probe's own
//! reset. The flash loader path and (next) the gdb server use this, so an OEP probe and a
//! WCH-Link meet the debug code at the same boundary.
//! ja: probe が DMI 1 回ずつより速くできる、transport に依らない target の操作(word の block、
//! loader の run、probe の reset を含む hart 制御)。flash の loader 経路と(次の)gdb server が使う。

use std::time::Duration;

use crate::{DmiError, DtmAccess};

/// How a reset leaves the hart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// Reset and run.
    Run,
    /// Reset, run, and confirm the hart is running (the probe samples the pc).
    RunVerified,
    /// Reset and hold the hart at its first instruction.
    HaltAtReset,
}

/// What a reset reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetResult {
    /// The pc the probe saw (dpc when halted at reset).
    pub pc: u32,
}

/// The result of a loader run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// The hart reached a halt (ebreak) before the timeout. When false the probe halted it.
    pub stopped: bool,
    /// Where it stopped. Still the start pc means the run never started.
    pub dpc: u32,
    pub elapsed_us: u32,
    /// The registers asked for, in order.
    pub outs: Vec<u32>,
}

/// Abstract register numbers (Debug Spec): GPR `x` is `0x1000 + x`, CSRs by number.
pub mod regno {
    pub const fn gpr(x: u16) -> u16 {
        0x1000 + x
    }
    pub const SP: u16 = gpr(2);
    pub const A0: u16 = gpr(10);
    pub const A1: u16 = gpr(11);
    pub const A2: u16 = gpr(12);
    pub const A3: u16 = gpr(13);
    pub const A4: u16 = gpr(14);
    pub const A5: u16 = gpr(15);
    pub const MSTATUS: u16 = 0x0300;
    pub const DCSR: u16 = 0x07B0;
    pub const DPC: u16 = 0x07B1;
}

pub trait TargetAccess: DtmAccess {
    /// The most words one block request carries.
    fn max_block_words(&self) -> usize;
    /// Read `count` words at `addr` (hart halted).
    fn read_words(&mut self, addr: u32, count: usize) -> Result<Vec<u32>, DmiError>;
    /// Write words at `addr` (hart halted).
    fn write_words(&mut self, addr: u32, words: &[u32]) -> Result<(), DmiError>;
    /// Start at `pc` with `regs` set, wait for the halt (ebreak) up to `timeout`, read `outs`.
    /// A timeout is not an error: `stopped` is false and the rest is valid.
    fn run_until_halt(
        &mut self,
        pc: u32,
        regs: &[(u16, u32)],
        outs: &[u16],
        timeout: Duration,
    ) -> Result<RunResult, DmiError>;
    fn halt(&mut self) -> Result<(), DmiError>;
    /// One resume request. The CH32 retry rule (V006 misses, L103 has no allresumeack) is the
    /// caller's: see [`resume_ch32`].
    fn resume_once(&mut self) -> Result<(), DmiError>;
    fn reset(&mut self, mode: ResetMode) -> Result<ResetResult, DmiError>;
}

/// en: Resume a CH32 hart: a resume that does not take is re-issued only while dpc has not moved
/// (the CH32V006 now and then misses resumereq; the CH32L103 never raises allresumeack, so an
/// answer of "state" alone does not mean it stayed halted). Up to 8 tries.
/// ja: CH32 の resume。取りこぼし(V006)は dpc が動いていない間だけ出し直す(L103 は allresumeack を
/// 立てないので「state」だけでは止まったままと言えない)。最大 8 回。
pub fn resume_ch32<T: TargetAccess + ?Sized>(
    t: &mut T,
    read_dpc: impl Fn(&mut T) -> Result<u32, DmiError>,
) -> Result<bool, DmiError> {
    let before = read_dpc(t)?;
    for _ in 0..8 {
        match t.resume_once() {
            Ok(()) => return Ok(true),
            Err(DmiError::OperationFailed(_)) => {
                // Halted again (a breakpoint) at a new pc means it ran.
                if read_dpc(t)? != before {
                    return Ok(true);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(false)
}
