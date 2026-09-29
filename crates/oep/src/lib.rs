//! en: OEP v1 (Open Embedded Probe) host side for ch32rv (docs/oep-host.ja.md). The probe knows
//! nothing about the target; this crate speaks the wire protocol and nothing CH32-specific.
//!
//! - [`registry`]: every number on the wire, generated from oep-spec's `registry/oep-v1.toml`
//!   (`cargo xtask oep-gen`).
//! - [`codec`]: framing (COBS + CRC-16 for serial transports, `length(u16)` for vendor bulk / HID /
//!   TCP) and the message layer (headers, TLV).
//! - [`link`]: one framed byte stream to one probe with the core §5 rules (corr, pipeline, resend,
//!   resync).
//! - [`session`]: discovery, open / end / keepalive / lock_state, typed rejections.
//! - [`target`]: `oep.wire.*` attach / detach, `oep.target.riscv-dm`, and [`target::OepDtm`]
//!   behind `ch32rv-dmi`'s `DtmAccess` / `TargetAccess`.
//!
//! ja: ch32rv の OEP v1 host 側。probe は target を知らず、この crate も CH32 固有のことは持たない。
//! [`registry`] は台帳から生成した番号、[`codec`] は framing と message 層。

pub mod codec;
pub mod link;
pub mod session;
pub mod target;

#[rustfmt::skip]
pub mod registry;

/// One operation of an interface, as the registry lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpInfo {
    pub code: u8,
    pub name: &'static str,
    /// The op needs the session lock.
    pub lock: bool,
    /// The response ends with a list of unknown length, so nothing can follow it.
    pub closed_tail: bool,
}
