//! en: Exit codes (docs/cli.ja.md §3.6) and error classification.
//! 10-14 = entry device, 20-24 = target, 30-41 = transfer/verify, 50 = run confirmation,
//! 60-69 = `run` (the target's own outcome under HIL), 70-79 = the tool itself.
//! New codes may only be added within the free numbers of each band.
//!
//! ja: exit code(docs/cli.ja.md §3.6)とエラー分類。
//! 10-14 = 入口 device、20-24 = target、30-41 = 転送・検証、50 = 実行確認、60-69 = `run`(HIL での target
//! 自身の結果)、70-79 = tool 自身。追加は各帯の空き番号のみ。

/// Process exit code. The values are part of the contract and never change; new ones may be added
/// (hence `non_exhaustive`, so a caller's `match` keeps compiling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    Success = 0,
    /// Argument / usage error (same as clap's 2)
    Usage = 2,
    /// Entry device (probe / ISP device / DFU / port) not found
    DeviceNotFound = 10,
    /// Device cannot be opened (permissions, driver binding)
    DeviceOpenFailed = 11,
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
    CapabilityUnsupported = 24,
    /// Verify mismatch / blank check failed
    VerifyMismatch = 30,
    /// Any transfer / DMI operation failure, including a genuine transport timeout
    TransferFailed = 40,
    /// Probe is wedged; a USB re-plug is required (reserved; not currently emitted)
    ProbeWedged = 41,
    /// Programmed, but the target is not running (confirm-run failed)
    NotRunningAfterWrite = 50,
    /// `run`: the target exited with a non-zero code (the code is in `result.exit`)
    TargetExit = 60,
    /// `run`: the target neither exited nor stopped within `--duration`
    RunTimeout = 61,
    /// `run`: the target stopped other than by a semihosting exit (breakpoint, exception)
    TargetHalted = 62,
    /// Internal error (bug)
    Internal = 70,
    /// The command is reserved but not implemented yet
    Unimplemented = 71,
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
    /// A genuine transport timeout (transfer stalled/interrupted).
    TransportTimeout,
    /// A flash/DMI operation failed for a non-timeout reason (program/erase/readback/option/reset).
    TransferFailed,
    ProbeWedged,
    NotRunningAfterWrite,
    /// `run`: the target exited with a non-zero code.
    TargetExit,
    /// `run`: `--duration` ran out.
    RunTimeout,
    /// `run`: the target stopped other than by a semihosting exit.
    TargetHalted,
    Internal,
    /// A reserved command that is not implemented yet.
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
            ErrorKind::TransferFailed => "transfer-failed",
            ErrorKind::ProbeWedged => "probe-wedged",
            ErrorKind::NotRunningAfterWrite => "not-running-after-write",
            ErrorKind::TargetExit => "target-exit",
            ErrorKind::RunTimeout => "run-timeout",
            ErrorKind::TargetHalted => "target-halted",
            ErrorKind::Internal => "internal",
            ErrorKind::Unimplemented => "unimplemented",
        }
    }

    /// The exit code this kind maps to.
    pub fn exit_code(self) -> ExitCode {
        match self {
            ErrorKind::Usage => ExitCode::Usage,
            ErrorKind::DeviceNotFound => ExitCode::DeviceNotFound,
            ErrorKind::DeviceOpenFailed => ExitCode::DeviceOpenFailed,
            ErrorKind::DeviceFirmwareUnsupported | ErrorKind::DeviceFirmwareKnownBad => {
                ExitCode::DeviceFirmware
            }
            ErrorKind::DeviceBusy => ExitCode::DeviceBusy,
            ErrorKind::DeviceAmbiguous => ExitCode::DeviceAmbiguous,
            ErrorKind::TargetNoResponse | ErrorKind::TargetNotInDb => ExitCode::TargetUnidentified,
            ErrorKind::TargetProtected => ExitCode::TargetProtected,
            ErrorKind::AttachFailed => ExitCode::AttachFailed,
            ErrorKind::TargetAmbiguous => ExitCode::TargetAmbiguous,
            ErrorKind::CapabilityUnsupported => ExitCode::CapabilityUnsupported,
            ErrorKind::VerifyMismatch | ErrorKind::BlankCheckFailed => ExitCode::VerifyMismatch,
            ErrorKind::TransportTimeout | ErrorKind::TransferFailed => ExitCode::TransferFailed,
            ErrorKind::ProbeWedged => ExitCode::ProbeWedged,
            ErrorKind::NotRunningAfterWrite => ExitCode::NotRunningAfterWrite,
            ErrorKind::TargetExit => ExitCode::TargetExit,
            ErrorKind::RunTimeout => ExitCode::RunTimeout,
            ErrorKind::TargetHalted => ExitCode::TargetHalted,
            ErrorKind::Internal => ExitCode::Internal,
            ErrorKind::Unimplemented => ExitCode::Unimplemented,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full set of every `ErrorKind`, so the mapping/naming tests are exhaustive. If a variant
    /// is added, this array (and the assertions below) must be updated - that is the point.
    const ALL_KINDS: &[ErrorKind] = &[
        ErrorKind::Usage,
        ErrorKind::DeviceNotFound,
        ErrorKind::DeviceOpenFailed,
        ErrorKind::DeviceFirmwareUnsupported,
        ErrorKind::DeviceFirmwareKnownBad,
        ErrorKind::DeviceBusy,
        ErrorKind::DeviceAmbiguous,
        ErrorKind::TargetNoResponse,
        ErrorKind::TargetNotInDb,
        ErrorKind::TargetProtected,
        ErrorKind::AttachFailed,
        ErrorKind::TargetAmbiguous,
        ErrorKind::CapabilityUnsupported,
        ErrorKind::VerifyMismatch,
        ErrorKind::BlankCheckFailed,
        ErrorKind::TransportTimeout,
        ErrorKind::TransferFailed,
        ErrorKind::ProbeWedged,
        ErrorKind::NotRunningAfterWrite,
        ErrorKind::TargetExit,
        ErrorKind::RunTimeout,
        ErrorKind::TargetHalted,
        ErrorKind::Internal,
        ErrorKind::Unimplemented,
    ];

    /// The frozen exit-code numbers (docs/cli.ja.md §3.6). These are a public contract and must
    /// never change; this pins them so a refactor cannot silently renumber.
    #[test]
    fn exit_code_numbers_are_frozen() {
        assert_eq!(ExitCode::Success.code(), 0);
        assert_eq!(ExitCode::Usage.code(), 2);
        assert_eq!(ExitCode::DeviceNotFound.code(), 10);
        assert_eq!(ExitCode::DeviceOpenFailed.code(), 11);
        assert_eq!(ExitCode::DeviceFirmware.code(), 12);
        assert_eq!(ExitCode::DeviceBusy.code(), 13);
        assert_eq!(ExitCode::DeviceAmbiguous.code(), 14);
        assert_eq!(ExitCode::TargetUnidentified.code(), 20);
        assert_eq!(ExitCode::TargetProtected.code(), 21);
        assert_eq!(ExitCode::AttachFailed.code(), 22);
        assert_eq!(ExitCode::TargetAmbiguous.code(), 23);
        assert_eq!(ExitCode::CapabilityUnsupported.code(), 24);
        assert_eq!(ExitCode::VerifyMismatch.code(), 30);
        assert_eq!(ExitCode::TransferFailed.code(), 40);
        assert_eq!(ExitCode::ProbeWedged.code(), 41);
        assert_eq!(ExitCode::NotRunningAfterWrite.code(), 50);
        assert_eq!(ExitCode::TargetExit.code(), 60);
        assert_eq!(ExitCode::RunTimeout.code(), 61);
        assert_eq!(ExitCode::TargetHalted.code(), 62);
        assert_eq!(ExitCode::Internal.code(), 70);
        assert_eq!(ExitCode::Unimplemented.code(), 71);
    }

    /// Every kind maps to a code in the documented band set; `exit_code` is total (compile-checked)
    /// and never yields a code outside the contract's table.
    #[test]
    fn every_kind_maps_into_the_documented_band() {
        const VALID: &[u8] = &[
            2, 10, 11, 12, 13, 14, 20, 21, 22, 23, 24, 30, 40, 41, 50, 60, 61, 62, 70, 71,
        ];
        for &k in ALL_KINDS {
            assert!(
                VALID.contains(&k.exit_code().code()),
                "{k:?} maps to an undocumented exit code {}",
                k.exit_code().code()
            );
        }
    }

    /// The intentional many-to-one groupings (a finer JSON `kind`, one coarse exit code).
    #[test]
    fn documented_many_to_one_groupings() {
        assert_eq!(
            ErrorKind::TransportTimeout.exit_code(),
            ErrorKind::TransferFailed.exit_code()
        );
        assert_eq!(
            ErrorKind::TargetNoResponse.exit_code(),
            ErrorKind::TargetNotInDb.exit_code()
        );
        assert_eq!(
            ErrorKind::VerifyMismatch.exit_code(),
            ErrorKind::BlankCheckFailed.exit_code()
        );
        assert_eq!(
            ErrorKind::DeviceFirmwareUnsupported.exit_code(),
            ErrorKind::DeviceFirmwareKnownBad.exit_code()
        );
    }

    /// `as_str` is kebab-case, non-empty, and unique across kinds (the stable JSON `error.kind`).
    #[test]
    fn as_str_is_kebab_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for &k in ALL_KINDS {
            let s = k.as_str();
            assert!(!s.is_empty());
            assert!(
                s.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{k:?} -> {s:?} is not kebab-case"
            );
            assert!(seen.insert(s), "duplicate as_str {s:?}");
        }
    }
}
