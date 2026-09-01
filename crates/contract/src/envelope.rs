//! en: Result envelope printed to stdout with `--json` (docs/contract/result.schema.json).
//! ja: `--json` 時に stdout へ出る result envelope(docs/contract/result.schema.json)。

use serde::{Deserialize, Serialize};

use crate::exit::ErrorKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultEnvelope {
    /// Contract version ([`crate::CONTRACT_VERSION`]).
    pub contract: String,
    pub ok: bool,
    /// Canonical command name (e.g. "flash", "probe.firmware.check").
    pub cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetReport>,
    /// Command-specific result. Per-command schemas are added to the contract over time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<Warning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl ResultEnvelope {
    pub fn success(cmd: impl Into<String>) -> Self {
        Self {
            contract: crate::CONTRACT_VERSION.to_owned(),
            ok: true,
            cmd: cmd.into(),
            probe: None,
            target: None,
            result: None,
            warnings: Vec::new(),
            error: None,
        }
    }

    pub fn failure(cmd: impl Into<String>, kind: ErrorKind, msg: impl Into<String>) -> Self {
        Self {
            contract: crate::CONTRACT_VERSION.to_owned(),
            ok: false,
            cmd: cmd.into(),
            probe: None,
            target: None,
            result: None,
            warnings: Vec::new(),
            error: Some(ErrorBody::new(kind, msg)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// e.g. "WCH-LinkE" / "WCH-Link(CH549)".
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// "VID:PID".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb: Option<String>,
    /// Bus-port form; the selector for devices without a serial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ProbeMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<FirmwareVersion>,
    /// Serial-port nodes belonging to this probe (UART bridge / SDI output), e.g. "/dev/ttyACM5".
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub ports: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProbeMode {
    Riscv,
    Dap,
    Iap,
    Isp,
    Unknown,
}

/// en: Probe firmware version. Always carries raw / normalized / WCH notations side by side;
/// comparisons use `norm` only (docs/protocol/wch-link.ja.md §6 - the notation triplet, and the
/// structural answer to probe-rs's version-comparison bug).
///
/// ja: probe firmware 版。raw / 正規化 / WCH 表示を常に併記し、比較は `norm` で行う
/// (docs/protocol/wch-link.ja.md §6: 表記の三重性と probe-rs の版比較バグへの構造的回答)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareVersion {
    /// Raw bytes as hex (e.g. "020c").
    pub raw: String,
    /// Normalized notation (e.g. "2.12").
    pub norm: String,
    /// WCH notation (e.g. "v32").
    pub wch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_bad: Option<bool>,
    /// Firmware mode ("riscv" / "arm"); separate firmware exists only on the CH549 Link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

impl FirmwareVersion {
    /// Build the three notations from the (major, minor) of a GetProbeInfo response.
    pub fn from_major_minor(major: u8, minor: u8) -> Self {
        Self {
            raw: format!("{major:02x}{minor:02x}"),
            norm: format!("{major}.{minor}"),
            wch: format!("v{}", u32::from(major) * 10 + u32::from(minor)),
            known_bad: None,
            mode: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sku: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Hex with 0x prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    /// Whether the SKU is verified on real silicon in the DB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// Whether the DB entry comes from the provisional overlay (docs/architecture.ja.md §3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected: Option<bool>,
    /// Flash size in KiB as reported by the probe (ChipInfo).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_kb: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    /// Stable warning code (e.g. fw-known-bad, target-unverified).
    pub code: String,
    pub msg: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Same value as the exit code (docs/cli.ja.md §3.6).
    pub code: u8,
    /// Machine-readable name ([`ErrorKind::as_str`]).
    pub kind: String,
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Candidate list for ambiguity errors (14/23).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<serde_json::Value>>,
    /// Reason for code=24 (probe x firmware x target x operation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<serde_json::Value>,
}

impl ErrorBody {
    pub fn new(kind: ErrorKind, msg: impl Into<String>) -> Self {
        Self {
            code: kind.exit_code().code(),
            kind: kind.as_str().to_owned(),
            msg: msg.into(),
            hint: None,
            candidates: None,
            capability: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn firmware_version_triple() {
        let fw = FirmwareVersion::from_major_minor(2, 12);
        assert_eq!(fw.raw, "020c");
        assert_eq!(fw.norm, "2.12");
        assert_eq!(fw.wch, "v32");
    }

    #[test]
    fn failure_envelope_has_matching_code() {
        let env = ResultEnvelope::failure(
            "flash",
            ErrorKind::NotRunningAfterWrite,
            "target halted after reset",
        );
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"]["code"], serde_json::json!(50));
        assert_eq!(
            v["error"]["kind"],
            serde_json::json!("not-running-after-write")
        );
        assert_eq!(v["contract"], serde_json::json!(crate::CONTRACT_VERSION));
    }

    #[test]
    fn success_envelope_omits_empty_fields() {
        let env = ResultEnvelope::success("version");
        let json = serde_json::to_string(&env).unwrap();
        assert!(!json.contains("warnings"));
        assert!(!json.contains("error"));
        assert!(!json.contains("probe"));
    }
}
