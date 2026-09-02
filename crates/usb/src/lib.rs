//! en: USB boundary layer. Owns enumeration, open, per-device locking, and transaction
//! capture via nusb, and never leaks nusb types to other crates (docs/architecture.ja.md §1.3:
//! a replaceable boundary).
//! Implemented: selector grammar and resolution, enumeration, blocking bulk transfers,
//! per-device advisory lock ([`lock`]). Not yet: transaction capture/replay.
//!
//! ja: USB 境界層。nusb による列挙・open・lock・transaction capture を担い、
//! nusb の型を他 crate へ漏らさない(差し替え可能な境界)。
//! 実装済み: selector 文法と解決、列挙、ブロッキング bulk 転送、per-device advisory lock。
//! 未実装: transaction capture/replay。

pub mod device;
pub mod lock;
pub mod selector;

pub use device::{UsbDeviceInfo, UsbError, UsbInterface, enumerate};
pub use lock::{DeviceLock, LockError};
pub use selector::{ResolveError, Selector, SelectorParseError, SerialFilter, resolve};
