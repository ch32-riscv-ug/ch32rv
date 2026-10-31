//! en: WCH-Link probe driver - the WCH-Link / WCH-LinkE USB bulk protocol.
//!
//! [`WchLink`] opens a probe ([`WchLink::open`]) and speaks its command channel: the probe
//! session ([`WchLink::probe_info`], [`WchLink::attach_chip`], [`WchLink::chip_info`],
//! [`WchLink::detach_chip`]), bulk memory read ([`WchLink::read_mem`]) and stub-driven flash
//! programming ([`WchLink::write_flash`], with parameters from
//! `ch32rv_flash::params_for_family`), whole-chip and power-off/RST erase, target power
//! ([`WchLink::set_power`]), RISC-V/DAP mode switching, and SDI-print forwarding. It also
//! implements [`ch32rv_dmi::DtmAccess`] (USB command `0x08 DmiOp`), so a
//! [`ch32rv_dmi::DebugModule`] can drive the Debug Module over it.
//!
//! The protocol facts live in wch-protocols (`protocols/pc-to-link.ja.md`, command table source
//! `protocols/data/wchlink-commands.csv`); every request frame is built by [`Request`], and a unit
//! test compares those frames with the CSV.
//!
//! ```no_run
//! use ch32rv_wchlink::WchLink;
//! # fn go(dev: &ch32rv_usb::UsbDeviceInfo) -> Result<(), ch32rv_wchlink::WchLinkError> {
//! let mut link = WchLink::open(dev)?;
//! let info = link.probe_info()?;               // firmware / variant
//! let attach = link.attach_chip()?;            // family byte + chip id
//! let head = link.read_mem(0x0800_0000, 16)?;  // first 16 bytes of flash
//! link.detach_chip()?;
//! # let _ = (info, attach, head); Ok(())
//! # }
//! ```
//!
//! ja: WCH-Link probe ドライバ。WCH-Link / WCH-LinkE の USB bulk protocol。[`WchLink`] が probe を
//! 開き、probe セッション・バルクメモリ読み([`WchLink::read_mem`])・stub 経由 flash 書込
//! ([`WchLink::write_flash`]、パラメータは `ch32rv_flash::params_for_family`)・chip/power-off
//! 消去・target 給電・RISC-V/DAP 切替・SDI print を扱う。[`ch32rv_dmi::DtmAccess`](USB `0x08
//! DmiOp`)も実装するので [`ch32rv_dmi::DebugModule`] を載せられる。protocol の事実は
//! wch-protocols(`protocols/pc-to-link.ja.md`、コマンド表の正本は `protocols/data/wchlink-commands.csv`)
//! にある。request frame はすべて [`Request`] が作り、unit test がその frame を CSV と比べる。

pub mod iap;
pub mod probe;

pub use iap::{IapDevice, IapError, ImageInfo, Pass};
pub use probe::{
    AttachInfo, ChipInfo, ChipInfoStatus, DmiReply, DmiStatus, FlashParams, FwMode, ProbeInfo,
    Request, Speed, Variant, WchLink, WchLinkError, family_name, frame, known_bad_firmware,
};

#[cfg(test)]
mod wch_protocols_frames;

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
