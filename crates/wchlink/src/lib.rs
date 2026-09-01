//! en: WCH-Link USB protocol implementation (bulk protocol + IAP).
//! The primary protocol document is `docs/protocol/wch-link.ja.md`; commands that are not
//! capture-verified are not implemented (per that document's rule). This crate will provide
//! [`ch32rv_dmi::DtmAccess`] plus probe services (power / mode / SDI / firmware).
//! Currently only USB identifier constants (status: attested; docs/protocol/wch-link.ja.md §1).
//!
//! ja: WCH-Link USB protocol 実装(bulk protocol + IAP)。protocol の一次文書は
//! `docs/protocol/wch-link.ja.md`。capture で verified になっていないコマンドは実装しない。
//! この crate は DtmAccess と probe サービス(power / mode / SDI / firmware)を提供する予定。
//! 現状は USB 識別子の定数と probe セッション(GetProbeInfo / SetSpeed / AttachChip /
//! ChipInfo / RedetectChip / DetachChip)。

pub mod probe;

pub use probe::{
    AttachInfo, ChipInfo, ChipInfoStatus, FwMode, ProbeInfo, Speed, Variant, WchLink, WchLinkError,
    family_name, known_bad_firmware,
};

/// WCH USB Vendor ID.
pub const VID_WCH: u16 = 0x1a86;
/// WCH-Link RISC-V mode Product ID.
pub const PID_LINK_RISCV: u16 = 0x8010;
/// en: Second RISC-V mode Product ID (observed by ch32-device-data's read_link_version.py).
/// ja: RISC-V mode のもう 1 つの Product ID(ch32-device-data の実測スクリプトが対応)。
pub const PID_LINK_RISCV2: u16 = 0x8011;
/// WCH-Link ARM/DAP mode Product ID.
pub const PID_LINK_DAP: u16 = 0x8012;
/// en: IAP mode VID (identical to the factory ISP device; disambiguation logic required -
/// docs/requirements.ja.md §3.7).
/// ja: IAP mode の VID(factory ISP device と同一。判別ロジックが必要)。
pub const VID_IAP: u16 = 0x4348;
/// IAP mode PID.
pub const PID_IAP: u16 = 0x55e0;
