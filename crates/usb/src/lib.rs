//! en: USB boundary layer. Owns enumeration, open, per-device locking, and transaction
//! capture, and never leaks backend types to other crates (docs/architecture.ja.md §1.3:
//! a replaceable boundary). Enumeration is nusb; transfers are nusb with, on Windows, a
//! per-device fallback to WCH's stock vendor driver (`ch32rv-usb-wch-win`,
//! docs/windows-wch-driver.ja.md).
//! Implemented: selector grammar and resolution, enumeration, blocking bulk transfers,
//! per-device advisory lock ([`lock`]), transaction capture ([`capture`]). Not yet: replay.
//!
//! ja: USB 境界層。列挙・open・lock・transaction capture を担い、backend の型を他 crate へ
//! 漏らさない(差し替え可能な境界)。列挙は nusb。転送は nusb に加え、Windows のみ
//! device 単位で WCH 純正ドライバ経路(`ch32rv-usb-wch-win`)へフォールバックする。
//! 実装済み: selector 文法と解決、列挙、ブロッキング bulk 転送、per-device advisory lock、
//! transaction capture。未実装: replay。

pub mod capture;
pub mod device;
pub mod lock;
pub mod selector;

pub use device::{UsbDeviceInfo, UsbError, UsbInterface, enumerate};
pub use lock::{DeviceLock, LockError};
pub use selector::{ResolveError, Selector, SelectorParseError, SerialFilter, resolve};
