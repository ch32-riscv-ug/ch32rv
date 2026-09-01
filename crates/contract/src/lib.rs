//! en: Shared vocabulary (contract) for the ch32rv tool suite.
//! This crate implements `docs/contract/` (result.schema.json / events.schema.json) and gives
//! the CLI, GUIs, and CI the same types to talk with. The contract version is
//! [`CONTRACT_VERSION`]; fields may be added without bumping it, breaking changes bump the
//! major (docs/contract/README.ja.md).
//!
//! ja: ch32rv の共通語彙(contract)。`docs/contract/` の schema の実装であり、
//! CLI・GUI・CI が同じ型で会話するための語彙を提供する。契約版は [`CONTRACT_VERSION`]。
//! field の追加は契約版を変えずに行い、破壊変更でのみ major を上げる。

pub mod envelope;
pub mod event;
pub mod exit;
pub mod policy;
pub mod progress;

/// en: JSON contract version, carried in `ResultEnvelope::contract`.
/// ja: JSON contract の版。`ResultEnvelope::contract` に入る。
pub const CONTRACT_VERSION: &str = "1";

pub use envelope::{
    ErrorBody, FirmwareVersion, ProbeMode, ProbeReport, ResultEnvelope, TargetReport, Warning,
};
pub use event::{Event, LogLevel};
pub use exit::{ErrorKind, ExitCode};
pub use progress::{CancelToken, NullSink, ProgressSink};
