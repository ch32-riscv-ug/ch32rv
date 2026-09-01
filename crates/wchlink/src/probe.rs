//! en: WCH-Link probe session over the command endpoint pair (OUT 0x01 / IN 0x81).
//! Only capture-verified or attested commands are implemented; byte layouts come from
//! docs/protocol/wch-link.ja.md and are verified against live probes as they land.
//!
//! ja: WCH-Link の probe セッション(command endpoint OUT 0x01 / IN 0x81)。
//! capture 済み/attested のコマンドのみ実装する。byte 配列は docs/protocol/wch-link.ja.md
//! に従い、実装のたびに実機で裏を取る。

use std::time::Duration;

use ch32rv_usb::{UsbDeviceInfo, UsbError, UsbInterface};
use thiserror::Error;

use crate::{PID_LINK_RISCV, VID_WCH};

const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;
const CMD_CONTROL: u8 = 0x0d;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WchLinkError {
    #[error(transparent)]
    Usb(#[from] UsbError),
    #[error("not a WCH-Link in RISC-V mode ({0})")]
    NotRiscvMode(String),
    #[error("probe replied with error (reason {reason:#04x}): {raw:02x?}")]
    Protocol { reason: u8, raw: Vec<u8> },
    #[error("unexpected response: {0:02x?}")]
    UnexpectedResponse(Vec<u8>),
    #[error("short write: {written} of {expected} bytes")]
    ShortWrite { written: usize, expected: usize },
    #[error("payload too long: {0} bytes")]
    PayloadTooLong(usize),
}

/// WCH-Link hardware variant, reported by GetProbeInfo (byte 2 of the payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// WCH-Link (CH549). No 1-wire SWIO support.
    Ch549,
    /// WCH-LinkE (CH32V305). The full-featured probe.
    LinkE,
    /// WCH-LinkS (CH32V203).
    LinkS,
    /// WCH-LinkW (CH32V208, wireless).
    LinkW,
    /// en: Unknown variant byte: reported as-is instead of failing, so listing keeps working
    /// on new hardware (the CLI attaches a warning).
    /// ja: 未知の variant byte。失敗にせずそのまま報告する(CLI が警告を付ける)。
    Unknown(u8),
}

impl Variant {
    /// en: Values 1 / 2 (0x12) / 3 / 5 (0x85) per probe-rs and wlink (attested).
    /// ja: 値は probe-rs / wlink による 1 / 2(0x12) / 3 / 5(0x85)(attested)。
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Variant::Ch549,
            2 | 0x12 => Variant::LinkE,
            3 => Variant::LinkS,
            5 | 0x85 => Variant::LinkW,
            other => Variant::Unknown(other),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Variant::Ch549 => "WCH-Link(CH549)".to_owned(),
            Variant::LinkE => "WCH-LinkE".to_owned(),
            Variant::LinkS => "WCH-LinkS".to_owned(),
            Variant::LinkW => "WCH-LinkW".to_owned(),
            Variant::Unknown(v) => format!("WCH-Link(unknown variant {v:#04x})"),
        }
    }
}

/// GetProbeInfo result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeInfo {
    pub variant: Variant,
    pub fw_major: u8,
    pub fw_minor: u8,
}

/// en: Known-bad firmware table. Returns the defect description.
/// Source: measured on ArduinoCore-CH32 (upload-and-fixture); hash mapping lives in
/// ch32-device-data `evidence/link_firmware.csv`.
/// ja: 既知不良 firmware 表。不良内容を返す。出典は ArduinoCore-CH32 の実測。
pub fn known_bad_firmware(major: u8, minor: u8) -> Option<&'static str> {
    match (major, minor) {
        (2, 11) => Some(
            "firmware 2.11 (v31) has a known defect: the target is not started after flashing (reset does not take effect)",
        ),
        _ => None,
    }
}

/// An opened WCH-Link (RISC-V mode) session.
pub struct WchLink {
    iface: UsbInterface,
    timeout: Duration,
}

impl WchLink {
    /// en: Open a WCH-Link in RISC-V mode (VID 1a86, PID 8010), claiming interface 0 and the
    /// command endpoints. DAP/IAP-mode devices are rejected with `NotRiscvMode`.
    /// ja: RISC-V mode の WCH-Link を開く(interface 0 と command endpoint を claim)。
    /// DAP/IAP mode の device は `NotRiscvMode` で拒否する。
    pub fn open(dev: &UsbDeviceInfo) -> Result<Self, WchLinkError> {
        if dev.vid() != VID_WCH || dev.pid() != PID_LINK_RISCV {
            return Err(WchLinkError::NotRiscvMode(dev.usb_id()));
        }
        let iface = dev.open_interface(0, EP_OUT, EP_IN)?;
        Ok(Self {
            iface,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// en: Send one framed command (`0x81 cmd len payload`) and parse the reply
    /// (`0x82 cmd len payload`; an `0x81`-headed reply is a probe-side error).
    /// ja: フレーム化コマンドを 1 往復する。応答先頭 `0x81` は probe 側エラー。
    fn command(&mut self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>, WchLinkError> {
        if payload.len() > 60 {
            return Err(WchLinkError::PayloadTooLong(payload.len()));
        }
        let mut tx = Vec::with_capacity(payload.len() + 3);
        tx.push(0x81);
        tx.push(cmd);
        tx.push(payload.len() as u8);
        tx.extend_from_slice(payload);
        let written = self.iface.write(&tx, self.timeout)?;
        if written != tx.len() {
            return Err(WchLinkError::ShortWrite {
                written,
                expected: tx.len(),
            });
        }
        let mut rx = [0u8; 64];
        let n = self.iface.read(&mut rx, self.timeout)?;
        if n < 3 || n != rx[2] as usize + 3 {
            return Err(WchLinkError::UnexpectedResponse(rx[..n].to_vec()));
        }
        if rx[0] == 0x81 {
            return Err(WchLinkError::Protocol {
                reason: rx[1],
                raw: rx[..n].to_vec(),
            });
        }
        if rx[0] != 0x82 || rx[1] != cmd {
            return Err(WchLinkError::UnexpectedResponse(rx[..n].to_vec()));
        }
        Ok(rx[3..n].to_vec())
    }

    /// en: GetProbeInfo (`0x81 0x0d 0x01 0x01`): firmware version and hardware variant.
    /// Status: attested (probe-rs / wlink); verified against live Link + LinkE on 2026-09-01.
    /// ja: GetProbeInfo。firmware 版と型番。attested、2026-09-01 に実機 Link + LinkE で検証。
    pub fn probe_info(&mut self) -> Result<ProbeInfo, WchLinkError> {
        let payload = self.command(CMD_CONTROL, &[0x01])?;
        if payload.len() < 3 {
            return Err(WchLinkError::UnexpectedResponse(payload));
        }
        Ok(ProbeInfo {
            fw_major: payload[0],
            fw_minor: payload[1],
            variant: Variant::from_u8(payload[2]),
        })
    }
}
