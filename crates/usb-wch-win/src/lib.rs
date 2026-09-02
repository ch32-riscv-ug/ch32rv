//! en: Access USB devices bound to WCH's stock Windows vendor drivers (the CH375 driver
//! family: `WCHLinkW64.SYS` for WCH-Link probes and compatible CH37x function drivers)
//! via `DeviceIoControl` — from 64-bit Rust, without replacing the driver with WinUSB
//! (Zadig) and without the 32-bit-only `WCHLinkDLL.dll`.
//!
//! ja: WCH 純正 Windows ドライバ(CH375 系: WCH-Link の `WCHLinkW64.SYS` ほか)に
//! バインドされた USB device を `DeviceIoControl` で直接叩く。WinUSB 置換(Zadig)も
//! 32bit 限定の `WCHLinkDLL.dll` も不要で、64bit Rust から使える。
//!
//! This crate is deliberately protocol-agnostic: it exposes device-interface enumeration,
//! open, and endpoint-addressed bulk transfers, nothing WCH-Link specific. Windows-only;
//! on other platforms it compiles to an empty crate — gate it with
//! `[target.'cfg(windows)'.dependencies]`.

#[cfg_attr(not(windows), allow(dead_code))]
mod proto;
#[cfg(windows)]
mod sys;

#[cfg(windows)]
pub use sys::{Ch375Device, DeviceInterface, list_interfaces};

use thiserror::Error;

/// en: A device interface class GUID, independent of `windows-sys` types so the public
/// API stays stable across `windows-sys` major bumps.
/// ja: device interface class GUID。public API を `windows-sys` の版から切り離すため独自型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// en: `{F8D5EDCA-B647-4E9C-9BD3-A5BD2328D55C}` — the interface GUID hardcoded in WCH's
/// CH375-family function drivers (`WCHLinkW64.SYS` etc.). Verified live against WCH-Link
/// and WCH-LinkE probes (2026-09-02).
/// ja: WCH の CH375 系ドライバがハードコードする interface GUID。実機検証済み。
pub const GUID_CH375: InterfaceGuid = InterfaceGuid {
    data1: 0xF8D5_EDCA,
    data2: 0xB647,
    data3: 0x4E9C,
    data4: [0x9B, 0xD3, 0xA5, 0xBD, 0x23, 0x28, 0xD5, 0x5C],
};

/// en: `{CDB3B5AD-293B-4663-AA36-1AAE46463776}` — the second GUID `wchlinkwdm.inf`
/// registers via the `DeviceInterfaceGUIDs` registry value. [`GUID_CH375`] is the one
/// verified to work; this is provided for completeness.
/// ja: `wchlinkwdm.inf` がレジストリ経由で登録するもう 1 つの GUID。動作検証済みなのは
/// [`GUID_CH375`] の方で、こちらは補完用。
pub const GUID_WCHLINK_INF: InterfaceGuid = InterfaceGuid {
    data1: 0xCDB3_B5AD,
    data2: 0x293B,
    data3: 0x4663,
    data4: [0xAA, 0x36, 0x1A, 0xAE, 0x46, 0x46, 0x37, 0x76],
};

/// Errors from enumeration, open, and transfers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Ch375Error {
    /// A configuration manager (cfgmgr32) call failed with the given CONFIGRET code.
    #[error("{op} failed (CONFIGRET {code})")]
    Cm { op: &'static str, code: u32 },
    /// A Win32 call failed with the given `GetLastError` code.
    #[error("{op} failed (Win32 error {code})")]
    Win32 { op: &'static str, code: u32 },
    /// en: Endpoint address maps to no CH375 pipe (the number bits must be 1..=15).
    /// ja: endpoint 番号が CH375 の pipe に対応しない(番号部は 1..=15)。
    #[error("invalid endpoint address {0:#04x} (endpoint number must be 1..=15)")]
    InvalidEndpoint(u8),
    /// The driver accepted fewer bytes than requested for one write chunk.
    #[error("short write: driver accepted {accepted} of {requested} bytes")]
    ShortWrite { accepted: usize, requested: usize },
    /// The driver's ioctl reply was shorter than the 8-byte WIN32_COMMAND header.
    #[error("malformed ioctl reply ({0} bytes, expected at least 8)")]
    MalformedReply(u32),
}
