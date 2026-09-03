//! en: Shared vocabulary (contract) for the ch32rv tool suite.
//! This crate implements `docs/contract/` (result.schema.json / events.schema.json) and gives
//! the CLI, GUIs, and CI the same types to talk with. The contract version is
//! [`CONTRACT_VERSION`]; fields may be added without bumping it, breaking changes bump the
//! major (docs/contract/README.ja.md).
//!
//! Key types: [`ResultEnvelope`] (the `--json` object), [`ErrorKind`] / [`ExitCode`] (every
//! failure maps to exactly one exit code), [`Event`] (the NDJSON progress/log stream), and the
//! policy value-enums in [`policy`].
//!
//! ```
//! use ch32rv_contract::{ResultEnvelope, ErrorKind};
//!
//! let ok = ResultEnvelope::success("flash");
//! assert!(ok.ok && ok.error.is_none());
//!
//! // A failure carries the machine-readable kind and its exit code (here verify-mismatch = 30).
//! let err = ResultEnvelope::failure("verify", ErrorKind::VerifyMismatch, "readback differs");
//! assert_eq!(err.error.unwrap().code, 30);
//! assert_eq!(ErrorKind::VerifyMismatch.exit_code().code(), 30);
//! ```
//!
//! ja: ch32rv の共通語彙(contract)。`docs/contract/` の schema の実装であり、
//! CLI・GUI・CI が同じ型で会話するための語彙を提供する。契約版は [`CONTRACT_VERSION`]。
//! field の追加は契約版を変えずに行い、破壊変更でのみ major を上げる。主な型: [`ResultEnvelope`]
//! (`--json` object)、[`ErrorKind`]/[`ExitCode`](各失敗は 1 つの exit code へ写像)、
//! [`Event`](NDJSON progress/log)、[`policy`] の value enum 群。

pub mod envelope;
pub mod event;
pub mod exit;
pub mod policy;
pub mod progress;

/// en: JSON contract version, carried in `ResultEnvelope::contract`.
/// ja: JSON contract の版。`ResultEnvelope::contract` に入る。
pub const CONTRACT_VERSION: &str = "2";

pub use envelope::{
    ErrorBody, FirmwareVersion, ProbeMode, ProbeReport, ResultEnvelope, TargetReport, Warning,
};
pub use event::{Event, LogLevel};
pub use exit::{ErrorKind, ExitCode};
pub use progress::{CancelToken, NullSink, ProgressSink};
