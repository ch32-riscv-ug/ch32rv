//! en: RISC-V Debug Module layer. Implemented against the RISC-V Debug Spec (0.13.2 / 1.0),
//! with WCH-specific deviations isolated in a quirk layer (docs/architecture.ja.md §2).
//! Transports (WCH-Link / compatible probes) are reached only through the [`DtmAccess`] trait;
//! this crate knows nothing about USB.
//! Currently only the boundary trait is defined; Debug Module operations (halt / resume /
//! abstract commands / progbuf) are not implemented yet.
//!
//! ja: RISC-V Debug Module 層。Debug Spec(0.13.2 / 1.0)に沿って実装し、WCH 固有の差分は
//! quirk 層に隔離する。transport は [`DtmAccess`] trait 越しにのみ扱い、この crate は USB を知らない。
//! 現状は境界 trait の定義のみ。

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
