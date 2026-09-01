//! en: Exit codes (docs/cli.ja.md §3.6) and error classification.
//! 10-14 = entry device, 20-24 = target, 30-41 = transfer/verify, 50 = run confirmation.
//! New codes may only be added within the free numbers of each band.
//!
//! ja: exit code(docs/cli.ja.md §3.6)とエラー分類。
//! 10-14 = 入口 device、20-24 = target、30-41 = 転送・検証、50 = 実行確認。追加は各帯の空き番号のみ。

/// Process exit code. The values are part of the contract and never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    /// Argument / usage error (same as clap's 2)
    Usage = 2,
    /// Entry device (probe / ISP device / DFU / port) not found
    DeviceNotFound = 10,
    /// Device cannot be opened (permissions, driver binding)
    DeviceOpen = 11,
    /// Device firmware does not satisfy the operation (incl. known-bad versions)
    DeviceFirmware = 12,
    /// Device busy (lock acquisition failed)
    DeviceBusy = 13,
    /// Device does not resolve to exactly one (fail-closed)
    DeviceAmbiguous = 14,
    /// Target cannot be identified (no response / not in the DB)
    TargetUnidentified = 20,
    /// Target is protected (explicit unprotect required)
    TargetProtected = 21,
    /// Attach failed (wiring, power, BOOT)
    AttachFailed = 22,
    /// Target ambiguous (multiple candidates / conflicts with --chip)
    TargetAmbiguous = 23,
    /// Capability missing for this probe x firmware x target x operation
    Unsupported = 24,
    /// Verify mismatch / blank check failed
    VerifyMismatch = 30,
    /// Transport timeout / interrupted transfer
    TransportTimeout = 40,
    /// Probe is wedged; a USB re-plug is required
    ProbeWedged = 41,
    /// Programmed, but the target is not running (confirm-run failed)
    NotRunning = 50,
    /// Internal error (bug)
    Internal = 70,
}

impl ExitCode {
    pub fn code(self) -> u8 {
        self as u8
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(v: ExitCode) -> Self {
        std::process::ExitCode::from(v.code())
    }
}

/// en: Machine-readable error kind, serialized as kebab-case into `ErrorBody::kind`.
/// Every kind maps to exactly one [`ExitCode`].
/// ja: 機械可読なエラー種別。`ErrorBody::kind` に kebab-case で入る。必ず 1 つの exit code に写像される。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    Usage,
    DeviceNotFound,
    DeviceOpenFailed,
    DeviceFirmwareUnsupported,
    DeviceFirmwareKnownBad,
    DeviceBusy,
    DeviceAmbiguous,
    TargetNoResponse,
    TargetNotInDb,
    TargetProtected,
    AttachFailed,
    TargetAmbiguous,
    CapabilityUnsupported,
    VerifyMismatch,
    BlankCheckFailed,
    TransportTimeout,
    ProbeWedged,
    NotRunningAfterWrite,
    Internal,
    /// Scaffold only: command not implemented yet
    Unimplemented,
}

impl ErrorKind {
    /// Stable kebab-case name (`error.kind` in JSON).
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Usage => "usage",
            ErrorKind::DeviceNotFound => "device-not-found",
            ErrorKind::DeviceOpenFailed => "device-open-failed",
            ErrorKind::DeviceFirmwareUnsupported => "device-firmware-unsupported",
            ErrorKind::DeviceFirmwareKnownBad => "device-firmware-known-bad",
            ErrorKind::DeviceBusy => "device-busy",
            ErrorKind::DeviceAmbiguous => "device-ambiguous",
            ErrorKind::TargetNoResponse => "target-no-response",
            ErrorKind::TargetNotInDb => "target-not-in-db",
            ErrorKind::TargetProtected => "target-protected",
            ErrorKind::AttachFailed => "attach-failed",
            ErrorKind::TargetAmbiguous => "target-ambiguous",
            ErrorKind::CapabilityUnsupported => "capability-unsupported",
            ErrorKind::VerifyMismatch => "verify-mismatch",
            ErrorKind::BlankCheckFailed => "blank-check-failed",
            ErrorKind::TransportTimeout => "transport-timeout",
            ErrorKind::ProbeWedged => "probe-wedged",
            ErrorKind::NotRunningAfterWrite => "not-running-after-write",
            ErrorKind::Internal => "internal",
            ErrorKind::Unimplemented => "unimplemented",
        }
    }

    /// The exit code this kind maps to.
    pub fn exit_code(self) -> ExitCode {
        match self {
            ErrorKind::Usage => ExitCode::Usage,
            ErrorKind::DeviceNotFound => ExitCode::DeviceNotFound,
            ErrorKind::DeviceOpenFailed => ExitCode::DeviceOpen,
            ErrorKind::DeviceFirmwareUnsupported | ErrorKind::DeviceFirmwareKnownBad => {
                ExitCode::DeviceFirmware
            }
            ErrorKind::DeviceBusy => ExitCode::DeviceBusy,
            ErrorKind::DeviceAmbiguous => ExitCode::DeviceAmbiguous,
            ErrorKind::TargetNoResponse | ErrorKind::TargetNotInDb => ExitCode::TargetUnidentified,
            ErrorKind::TargetProtected => ExitCode::TargetProtected,
            ErrorKind::AttachFailed => ExitCode::AttachFailed,
            ErrorKind::TargetAmbiguous => ExitCode::TargetAmbiguous,
            ErrorKind::CapabilityUnsupported => ExitCode::Unsupported,
            ErrorKind::VerifyMismatch | ErrorKind::BlankCheckFailed => ExitCode::VerifyMismatch,
            ErrorKind::TransportTimeout => ExitCode::TransportTimeout,
            ErrorKind::ProbeWedged => ExitCode::ProbeWedged,
            ErrorKind::NotRunningAfterWrite => ExitCode::NotRunning,
            ErrorKind::Internal | ErrorKind::Unimplemented => ExitCode::Internal,
        }
    }
}
