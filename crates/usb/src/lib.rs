//! en: USB boundary layer. Owns enumeration, open, per-device locking, and transaction
//! capture via nusb, and never leaks nusb types to other crates (docs/architecture.ja.md §1.3:
//! a replaceable boundary).
//! Currently only the selector grammar ([`Selector`]) is implemented; enumeration, open,
//! locking, and capture are not implemented yet.
//!
//! ja: USB 境界層。nusb による列挙・open・lock・transaction capture を担い、
//! nusb の型を他 crate へ漏らさない(差し替え可能な境界)。
//! 現状は selector の文法のみ実装済み。

pub mod selector;

pub use selector::{Selector, SelectorParseError, SerialFilter};
