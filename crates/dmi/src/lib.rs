//! en: RISC-V Debug Module (DM) driver, per the RISC-V Debug Spec (0.13.2 / 1.0), with
//! WCH-specific deviations isolated in a quirk layer (docs/architecture.ja.md §2). It reaches a
//! probe only through the [`DtmAccess`] trait (DMI register read/write), so it knows nothing
//! about USB - any transport implementing `DtmAccess` (e.g. `ch32rv_wchlink::WchLink`) can drive
//! it.
//!
//! [`DebugModule::new`] wraps a `DtmAccess` and provides hart control ([`DebugModule::halt`],
//! [`DebugModule::resume`], [`DebugModule::step`], [`DebugModule::is_halted`]), register access
//! ([`DebugModule::read_reg`] / [`DebugModule::write_reg`] over [`RegName`]), memory access
//! ([`DebugModule::read_mem`], [`DebugModule::write_mem`] / `write_mem32` / `write_mem16`),
//! the SerialDMDATA mailbox ([`DebugModule::dmdata_poll`]), hardware-trigger discovery, direct
//! FLASH-controller page erase/program ([`DebugModule::flash_page_erase`],
//! [`DebugModule::flash_program_page`], keyed by [`FlashProgMode`]), and option-byte writes.
//!
//! ```no_run
//! use ch32rv_dmi::{DebugModule, RegName};
//! # fn go(dtm: &mut impl ch32rv_dmi::DtmAccess) -> Result<(), ch32rv_dmi::DmiError> {
//! let mut dm = DebugModule::new(dtm);
//! dm.halt()?;
//! let pc = dm.read_reg(RegName::Pc)?;
//! let word = dm.read_mem32(0x2000_0000)?;   // first SRAM word
//! dm.resume()?;
//! # let _ = (pc, word); Ok(())
//! # }
//! ```
//!
//! ja: RISC-V Debug Module ドライバ。Debug Spec(0.13.2 / 1.0)準拠で WCH 固有差分は quirk 層に
//! 隔離。probe へは [`DtmAccess`] trait(DMI read/write)越しにのみ触れ USB を知らない。
//! [`DebugModule::new`] が `DtmAccess` を包み、hart 制御(halt/resume/step)・レジスタ・メモリ
//! 読み書き・SerialDMDATA mailbox([`DebugModule::dmdata_poll`])・HW trigger 探索・直接
//! FLASH controller の page erase/program・option byte 書込を提供する。

pub mod access;
pub mod dm;
pub mod dmseq;

pub use access::{DmTarget, ResetMode, ResetResult, RunResult, TargetAccess, resume_ch32};
pub use dm::{DebugModule, DmdataPoll, FlashProgMode, RegName};
pub use dmseq::{DmSeq, DmSeqPoll};

use thiserror::Error;

/// en: Minimal access to the DTM (Debug Transport Module), implemented by probe backends.
/// On WCH-Link this maps to USB command `0x08 DmiOp` (docs/protocol/wch-link.ja.md §4.1).
///
/// ja: DTM への最小アクセス。probe backend が実装する。WCH-Link では USB コマンド
/// `0x08 DmiOp` に対応する。
pub trait DtmAccess {
    /// DMI register read.
    fn dmi_read(&mut self, addr: u8) -> Result<u32, DmiError>;
    /// DMI register write.
    fn dmi_write(&mut self, addr: u8, value: u32) -> Result<(), DmiError>;
    /// en: DMI nop. Note: WCH-Link firmware reportedly returns the previous read result on
    /// nop (docs/protocol/wch-link.ja.md §7). Absorbing that quirk is the backend's job;
    /// this trait keeps Debug Spec semantics.
    ///
    /// ja: DMI nop。WCH-Link firmware には「nop が直前の read 結果を返す」quirk が報告
    /// されている。quirk の吸収は backend 側の責務とし、この trait の意味論は Debug Spec に従う。
    fn dmi_nop(&mut self) -> Result<(), DmiError>;

    /// en: Run `ops` as one sequence and return the values the reads and polls produced, in order.
    /// A probe that can take the whole list in one request (OEP `dmi`) overrides this, which keeps
    /// an abstract-command sequence from being split by the probe's own console polling between
    /// requests. The default runs them one by one. A poll that never sees its bits clear is
    /// [`DmiError::Timeout`].
    /// ja: `ops` を 1 つの並びとして実行し、read と poll の値を順に返す。1 要求で受けられる probe
    /// (OEP の `dmi`)は上書きする(abstract command の一連が probe の console の poll で割られない)。
    /// 既定は 1 つずつ。poll が最後まで満たされなければ Timeout。
    fn dmi_sequence(&mut self, ops: &[DmiOp]) -> Result<Vec<u32>, DmiError> {
        let mut out = Vec::new();
        for op in ops {
            match *op {
                DmiOp::Write(addr, value) => self.dmi_write(addr, value)?,
                DmiOp::Read(addr) => out.push(self.dmi_read(addr)?),
                DmiOp::PollClear {
                    addr,
                    mask,
                    max_reads,
                } => {
                    let mut last = 0;
                    let mut clear = false;
                    for _ in 0..max_reads.max(1) {
                        last = self.dmi_read(addr)?;
                        if last & mask == 0 {
                            clear = true;
                            break;
                        }
                    }
                    out.push(last);
                    if !clear {
                        return Err(DmiError::Timeout);
                    }
                }
            }
        }
        Ok(out)
    }
}

/// One step of a [`DtmAccess::dmi_sequence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmiOp {
    Write(u8, u32),
    Read(u8),
    /// Read `addr` until `value & mask == 0`, at most `max_reads` times; yields the last value.
    PollClear {
        addr: u8,
        mask: u32,
        max_reads: u16,
    },
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DmiError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("dmi operation failed (op state: {0})")]
    OperationFailed(String),
    /// A reset finished but the hart is not in the state asked for (not running after a verified
    /// run, not halted at reset).
    #[error("the target did not reach the requested state after reset ({0})")]
    NotReached(String),
    #[error("timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
}
