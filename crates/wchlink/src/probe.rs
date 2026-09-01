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
const CMD_SET_SPEED: u8 = 0x0c;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// en: Debug speed. WCH-Link supports exactly three steps; the wire encoding is inverted
/// (High=0x01 ... Low=0x03).
/// ja: debug 速度。WCH-Link は 3 段階のみで、wire 上の符号は逆順(High=0x01 ... Low=0x03)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Speed {
    /// 400 kHz
    Low = 0x03,
    /// 4 MHz
    Medium = 0x02,
    /// 6 MHz
    #[default]
    High = 0x01,
}

/// en: Family byte reported by AttachChip (docs/protocol/wch-link.ja.md §5, attested from
/// probe-rs). Returns None for unknown values — including, until measured, the 7 gap series.
/// ja: AttachChip が返す family byte の名前(attested)。未知値(gap 7 series を含む)は None。
pub fn family_name(byte: u8) -> Option<&'static str> {
    Some(match byte {
        0x01 => "CH32V103",
        0x02 => "CH57x",
        0x03 => "CH56x",
        0x04 => "CH32F10x",
        0x05 => "CH32V20x",
        0x06 => "CH32V30x",
        0x07 => "CH58x",
        0x09 => "CH32V003",
        0x0A => "CH8571",
        0x0B => "CH59x",
        0x0C => "CH643",
        0x0D => "CH32X035",
        0x0E => "CH32L103",
        0x49 => "CH641",
        0x4E => "CH32V00X",
        0x86 => "CH32V317",
        0x8B => "CH570/572",
        0xC6 => "CH32H4",
        _ => return None,
    })
}

/// AttachChip result: family byte and the 32-bit chip id (big-endian on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachInfo {
    pub family_byte: u8,
    pub chip_id: u32,
}

/// en: ChipInfo (`0x81 0x11 0x01 0x05`) result. The reply is a raw 20-byte block with no
/// frame header: `flash_kb(be16 at [2:4]) | uuid[4:12] | protection[12:16] | chip_id[16:20]`.
/// Source: board-identify `wch_link.py` (measured) + wlink.
/// ja: ChipInfo の結果。応答はフレームヘッダ無しの生 20 byte。出典は board-identify(実測)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipInfo {
    pub flash_kb: u16,
    pub uuid: [u8; 8],
    /// Interpretation not yet established; exposed raw (docs/protocol/wch-link.ja.md).
    pub protection_raw: [u8; 4],
    pub chip_id_echo: u32,
}

/// en: ChipInfo outcomes that are data, not transport errors.
/// ja: transport エラーではない ChipInfo の結果分類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipInfoStatus {
    Ok(ChipInfo),
    /// UUID is all-zero / all-ff: the part did not answer or refused.
    NoAnswer,
    /// en: The whole reply is one repeating 32-bit word: the probe (LinkE) holds a corrupted
    /// readback of its target. Persists across attach cycles and target power cycles;
    /// recovery is RedetectChip + detach + re-attach (board-identify, measured).
    /// ja: 応答全体が同一 32bit word の繰り返し。probe(LinkE)側が壊れた読み値を保持する
    /// 既知バグ。attach し直しや target の電源断では直らず、RedetectChip+detach+再attach で
    /// 復旧する(board-identify 実測)。
    CorruptedReadback,
}

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
    /// WCH-DAPLink (value 4; ch32-device-data read_link_version.py / MRS extension.js).
    DapLink,
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
            4 => Variant::DapLink,
            5 | 0x85 => Variant::LinkW,
            other => Variant::Unknown(other),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Variant::Ch549 => "WCH-Link(CH549)".to_owned(),
            Variant::LinkE => "WCH-LinkE".to_owned(),
            Variant::LinkS => "WCH-LinkS".to_owned(),
            Variant::DapLink => "WCH-DAPLink".to_owned(),
            Variant::LinkW => "WCH-LinkW".to_owned(),
            Variant::Unknown(v) => format!("WCH-Link(unknown variant {v:#04x})"),
        }
    }
}

/// en: Firmware mode from GetProbeInfo payload byte 3 (0=RISC-V, 1=ARM). Only the CH549
/// Link ships separate RV/ARM firmware; source: ch32-device-data read_link_version.py.
/// ja: GetProbeInfo payload 4 byte 目の firmware mode(0=RISC-V, 1=ARM)。RV/ARM で
/// 別 firmware を持つのは CH549 のみ。出典は ch32-device-data の実測スクリプト。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FwMode {
    RiscV,
    Arm,
    Unknown(u8),
}

impl FwMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => FwMode::RiscV,
            1 => FwMode::Arm,
            other => FwMode::Unknown(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            FwMode::RiscV => "riscv",
            FwMode::Arm => "arm",
            FwMode::Unknown(_) => "unknown",
        }
    }
}

/// GetProbeInfo result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeInfo {
    pub variant: Variant,
    pub fw_major: u8,
    pub fw_minor: u8,
    /// Present when the probe reports the 4-byte payload form (`major minor variant mode`).
    pub fw_mode: Option<FwMode>,
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
    /// Status: verified against live Link(CH549) + LinkE on 2026-09-01.
    /// ja: GetProbeInfo。firmware 版と型番。2026-09-01 に実機 Link + LinkE で検証済み。
    pub fn probe_info(&mut self) -> Result<ProbeInfo, WchLinkError> {
        let payload = self.command(CMD_CONTROL, &[0x01])?;
        if payload.len() < 3 {
            return Err(WchLinkError::UnexpectedResponse(payload));
        }
        Ok(ProbeInfo {
            fw_major: payload[0],
            fw_minor: payload[1],
            variant: Variant::from_u8(payload[2]),
            fw_mode: payload.get(3).copied().map(FwMode::from_u8),
        })
    }

    /// en: SetSpeed (`0x81 0x0c 0x02 family speed`). Before the first attach the family is
    /// unknown; probe-rs sends 0x01 (CH32V103) as the placeholder and so do we.
    /// ja: SetSpeed。初回 attach 前は family 不明のため、probe-rs と同じく 0x01 を送る。
    pub fn set_speed(&mut self, family_byte: u8, speed: Speed) -> Result<(), WchLinkError> {
        let _ack = self.command(CMD_SET_SPEED, &[family_byte, speed as u8])?;
        Ok(())
    }

    /// en: AttachChip (`0x81 0x0d 0x01 0x02`): response payload is
    /// `[family, chip_id_be32]` (5 bytes). Bits [7:4] of the chip id are the silicon
    /// revision (don't-care when matching).
    /// ja: AttachChip。応答 payload は `[family, chip_id_be32]` の 5 byte。chip id の
    /// [7:4] は silicon revision(照合時 don't-care)。
    pub fn attach_chip(&mut self) -> Result<AttachInfo, WchLinkError> {
        let payload = self.command(CMD_CONTROL, &[0x02])?;
        if payload.len() != 5 {
            return Err(WchLinkError::UnexpectedResponse(payload));
        }
        Ok(AttachInfo {
            family_byte: payload[0],
            chip_id: u32::from_be_bytes([payload[1], payload[2], payload[3], payload[4]]),
        })
    }

    /// en: DetachChip (`0x81 0x0d 0x01 0xff`), aka OptEnd / clear-state. Releases the held
    /// target core; also used to clear probe state before a session (board-identify).
    /// ja: DetachChip。掴んでいる target core を解放する。セッション前の状態クリアにも使う。
    pub fn detach_chip(&mut self) -> Result<(), WchLinkError> {
        let _ = self.command(CMD_CONTROL, &[0xff])?;
        Ok(())
    }

    /// en: RedetectChip (`0x81 0x0d 0x01 0x03`): makes the probe re-establish its target
    /// WITHOUT resetting it (verified via the debug module's sticky havereset bits by
    /// board-identify). Clears the LinkE corrupted-readback state.
    /// ja: RedetectChip。target を reset せずに probe の把握し直しを行わせる(havereset の
    /// sticky bit で board-identify が実測確認)。LinkE の壊れ読み値の解消に使う。
    pub fn redetect_chip(&mut self) -> Result<(), WchLinkError> {
        let _ = self.command(CMD_CONTROL, &[0x03])?;
        Ok(())
    }

    /// en: ChipInfo (`0x81 0x11 0x01 0x05`): flash size, factory UUID, protection flags.
    /// The reply is raw (no `0x82` frame), so this bypasses the framed reader.
    /// ja: ChipInfo。flash 容量・工場 UUID・保護フラグ。応答はフレーム無しの生データ。
    pub fn chip_info(&mut self) -> Result<ChipInfoStatus, WchLinkError> {
        let tx = [0x81, 0x11, 0x01, 0x05];
        let written = self.iface.write(&tx, self.timeout)?;
        if written != tx.len() {
            return Err(WchLinkError::ShortWrite {
                written,
                expected: tx.len(),
            });
        }
        let mut rx = [0u8; 64];
        let n = self.iface.read(&mut rx, self.timeout)?;
        let reply = &rx[..n];
        if n != 20 || reply[0] == 0x00 {
            return Err(WchLinkError::UnexpectedResponse(reply.to_vec()));
        }
        // en: One repeating 32-bit word across the whole reply = corrupted probe readback.
        // ja: 応答全体が同一 32bit word の繰り返しなら probe 側の壊れ読み値。
        if reply.chunks(4).all(|c| c == &reply[0..4]) {
            return Ok(ChipInfoStatus::CorruptedReadback);
        }
        let mut uuid = [0u8; 8];
        uuid.copy_from_slice(&reply[4..12]);
        if uuid == [0u8; 8] || uuid == [0xff; 8] {
            return Ok(ChipInfoStatus::NoAnswer);
        }
        let mut protection_raw = [0u8; 4];
        protection_raw.copy_from_slice(&reply[12..16]);
        Ok(ChipInfoStatus::Ok(ChipInfo {
            flash_kb: u16::from_be_bytes([reply[2], reply[3]]),
            uuid,
            protection_raw,
            chip_id_echo: u32::from_be_bytes([reply[16], reply[17], reply[18], reply[19]]),
        }))
    }
}
