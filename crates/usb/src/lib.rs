//! en: USB boundary layer. Owns enumeration, open, per-device locking, and transaction
//! capture via nusb, and never leaks nusb types to other crates (docs/architecture.ja.md §1.3:
//! a replaceable boundary).
//! Implemented: selector grammar and resolution, enumeration, blocking bulk transfers.
//! Not yet: per-device lock, capture/replay.
//!
//! ja: USB 境界層。nusb による列挙・open・lock・transaction capture を担い、
//! nusb の型を他 crate へ漏らさない(差し替え可能な境界)。
//! 実装済み: selector 文法と解決、列挙、ブロッキング bulk 転送。未実装: lock、capture/replay。

pub mod device;
pub mod selector;

pub use device::{UsbDeviceInfo, UsbError, UsbInterface, enumerate};
pub use selector::{ResolveError, Selector, SelectorParseError, SerialFilter, resolve};
