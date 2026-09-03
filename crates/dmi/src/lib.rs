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

pub mod dm;

pub use dm::{DebugModule, FlashProgMode, RegName};

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
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DmiError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("dmi operation failed (op state: {0})")]
    OperationFailed(String),
    #[error("timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
}
