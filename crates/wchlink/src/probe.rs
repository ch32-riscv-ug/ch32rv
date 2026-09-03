//! en: WCH-Link probe session over the command endpoint pair (OUT 0x01 / IN 0x81).
//! Only capture-verified or attested commands are implemented; byte layouts come from
//! docs/protocol/wch-link.ja.md and are verified against live probes as they land.
//!
//! ja: WCH-Link の probe セッション(command endpoint OUT 0x01 / IN 0x81)。
//! capture 済み/attested のコマンドのみ実装する。byte 配列は docs/protocol/wch-link.ja.md
//! に従い、実装のたびに実機で裏を取る。

use std::time::Duration;

use ch32rv_dmi::{DmiError, DtmAccess};
use ch32rv_usb::{UsbDeviceInfo, UsbError, UsbInterface};
use thiserror::Error;

use crate::{PID_LINK_RISCV, VID_WCH};

const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;
const CMD_CONTROL: u8 = 0x0d;
const CMD_SET_SPEED: u8 = 0x0c;
const CMD_DMI_OP: u8 = 0x08;
const CMD_SET_MEM_REGION: u8 = 0x01;
const CMD_SET_READ_MEM_REGION: u8 = 0x03;
const CMD_PROGRAM: u8 = 0x02;
const CMD_CONFIG_CHIP: u8 = 0x06;
const CMD_RESET: u8 = 0x0b;
const DATA_EP_OUT: u8 = 0x02;
const DATA_EP_IN: u8 = 0x82;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// en: Flash parameters that vary by chip family (docs/protocol/wch-link.ja.md, from wlink).
/// ja: chip family ごとに変わる flash パラメータ(wlink 由来)。
#[derive(Debug, Clone, Copy)]
pub struct FlashParams {
    /// Flash loader stub run in target RAM.
    pub stub: &'static [u8],
    /// Data-endpoint packet size (the stub and each chunk are padded to this).
    pub data_packet_size: usize,
    /// fastprogram chunk size.
    pub write_pack_size: usize,
    /// Whether this family accepts the flash-protection command group.
    pub supports_protect: bool,
    /// Whether this family accepts power-off / RST special erase.
    pub supports_special_erase: bool,
}

/// en: DMI operation status from the probe (byte 5 of the DmiOp reply).
/// ja: DmiOp 応答の byte 5 が返す DMI 操作の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmiStatus {
    Success,
    Failed,
    Busy,
    Other(u8),
}

impl DmiStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => DmiStatus::Success,
            2 => DmiStatus::Failed,
            3 => DmiStatus::Busy,
            other => DmiStatus::Other(other),
        }
    }
}

/// One DmiOp reply: `[addr, data_be32, op]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmiReply {
    pub addr: u8,
    pub data: u32,
    pub status: DmiStatus,
}

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
/// frame header: `flash_kib(be16 at [2:4]) | uuid[4:12] | protection[12:16] | chip_id[16:20]`
/// (the KiB value is widened to bytes in `flash_bytes`).
/// Source: board-identify `wch_link.py` (measured) + wlink.
/// ja: ChipInfo の結果。応答はフレームヘッダ無しの生 20 byte。出典は board-identify(実測)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChipInfo {
    /// Flash size in bytes (the probe reports KiB; converted at the boundary for a uniform unit).
    pub flash_bytes: u32,
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
    /// `[family, chip_id_be32]` (5 bytes). Bits `[7:4]` of the chip id are the silicon
    /// revision (don't-care when matching).
    /// ja: AttachChip。応答 payload は `[family, chip_id_be32]` の 5 byte。chip id の
    /// `[7:4]` は silicon revision(照合時 don't-care)。
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

    /// en: DmiOp (`0x81 0x08 0x06 addr data_be32 op`): one DMI transaction on the target's
    /// debug transport. op = 0 nop / 1 read / 2 write. Reply is `[addr, data_be32, status]`.
    /// Status: attested (probe-rs / wlink); verified against a live target on 2026-09-01.
    /// ja: DmiOp。target debug transport への 1 トランザクション。op=0 nop/1 read/2 write。
    /// 応答は `[addr, data_be32, status]`。実機で検証済み。
    pub fn dmi_op(&mut self, addr: u8, data: u32, op: u8) -> Result<DmiReply, WchLinkError> {
        let mut payload = [0u8; 6];
        payload[0] = addr;
        payload[1..5].copy_from_slice(&data.to_be_bytes());
        payload[5] = op;
        let resp = self.command(CMD_DMI_OP, &payload)?;
        if resp.len() != 6 {
            return Err(WchLinkError::UnexpectedResponse(resp));
        }
        Ok(DmiReply {
            addr: resp[0],
            data: u32::from_be_bytes([resp[1], resp[2], resp[3], resp[4]]),
            status: DmiStatus::from_u8(resp[5]),
        })
    }

    /// en: DetachChip (`0x81 0x0d 0x01 0xff`), aka OptEnd / clear-state. Releases the held
    /// target core; also used to clear probe state before a session (board-identify).
    /// ja: DetachChip。掴んでいる target core を解放する。セッション前の状態クリアにも使う。
    pub fn detach_chip(&mut self) -> Result<(), WchLinkError> {
        let _ = self.command(CMD_CONTROL, &[0xff])?;
        Ok(())
    }

    /// en: Set the target debug speed (family placeholder 0x01 for a pre-attach session).
    /// ja: target debug 速度を設定する(attach 前は family placeholder 0x01)。
    pub fn set_speed_default(&mut self, speed: Speed) -> Result<(), WchLinkError> {
        self.set_speed(0x01, speed)
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

    // ---- Flash programming (docs/protocol/wch-link.ja.md §4.2 flash path, from wlink) ----

    /// Send a command and return the first payload byte (Program/ConfigChip replies).
    fn command_u8(&mut self, cmd: u8, payload: &[u8]) -> Result<u8, WchLinkError> {
        let resp = self.command(cmd, payload)?;
        resp.first()
            .copied()
            .ok_or(WchLinkError::UnexpectedResponse(resp))
    }

    /// en: Check read-protection state (ConfigChip 0x01). 1 = protected, 2 = unprotected.
    /// ja: 読み出し保護状態を確認(ConfigChip 0x01)。1=保護、2=非保護。
    pub fn check_read_protect(&mut self) -> Result<u8, WchLinkError> {
        self.command_u8(CMD_CONFIG_CHIP, &[0x01])
    }

    /// en: Unprotect flash (ConfigChip 0x02). Only meaningful when currently protected: the
    /// probe firmware mass-erases the option-byte page, so wlink skips it otherwise.
    /// ja: 保護解除(ConfigChip 0x02)。保護時のみ実行する。
    pub fn unprotect_if_needed(&mut self) -> Result<(), WchLinkError> {
        if self.check_read_protect()? == 0x01 {
            let _ = self.command_u8(CMD_CONFIG_CHIP, &[0x02])?;
            self.detach_chip()?;
            let _ = self.attach_chip()?;
        }
        Ok(())
    }

    /// en: Whole-chip flash erase (Program 0x01) followed by a fresh attach. The caller must
    /// have attached first.
    /// ja: chip 全体の flash 消去(Program 0x01)後に再 attach。
    pub fn erase_flash(&mut self) -> Result<(), WchLinkError> {
        let _ = self.command_u8(CMD_PROGRAM, &[0x01])?;
        let _ = self.attach_chip()?;
        Ok(())
    }

    /// en: Program `data` to `address` via the flash loader stub (wlink `write_flash`).
    /// Sequence: SetWriteMemoryRegion -> WriteFlashOP -> upload stub on the data EP ->
    /// confirm 0x07 -> WriteFlash -> stream chunks on the data EP, checking each ack ->
    /// End. `progress(done)` is called after each chunk.
    /// ja: flash loader stub 経由で `data` を `address` へ書く(wlink `write_flash`)。
    pub fn write_flash(
        &mut self,
        addr: u32,
        data: &[u8],
        params: &FlashParams,
        mut progress: impl FnMut(u64),
    ) -> Result<(), WchLinkError> {
        self.iface.open_data_endpoints(DATA_EP_OUT, DATA_EP_IN)?;
        if params.supports_protect {
            self.unprotect_if_needed()?;
        }

        // SetWriteMemoryRegion (cmd 0x01): start_addr BE32 + len BE32.
        let mut region = Vec::with_capacity(8);
        region.extend_from_slice(&addr.to_be_bytes());
        region.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let _ = self.command(CMD_SET_MEM_REGION, &region)?;

        // WriteFlashOP (Program 0x05), then upload the stub on the data endpoint.
        let _ = self.command_u8(CMD_PROGRAM, &[0x05])?;
        self.write_data_padded(params.stub, params.data_packet_size)?;

        // Confirm (Program 0x07) — must echo 0x07.
        let n = self.command_u8(CMD_PROGRAM, &[0x07])?;
        if n != 0x07 {
            return Err(WchLinkError::UnexpectedResponse(vec![n]));
        }

        // WriteFlash (Program 0x02), then stream data chunks, checking each ack.
        let _ = self.command_u8(CMD_PROGRAM, &[0x02])?;
        let mut done = 0u64;
        for chunk in data.chunks(params.write_pack_size) {
            self.write_data_padded(chunk, params.data_packet_size)?;
            let mut ack = [0u8; 4];
            let got = self.iface.read_data(&mut ack, self.timeout)?;
            // Ack looks like `41 01 01 04`; byte 3 == 0x04 means the chunk landed.
            if got < 4 || ack[3] != 0x04 {
                return Err(WchLinkError::UnexpectedResponse(ack[..got].to_vec()));
            }
            done += chunk.len() as u64;
            progress(done);
        }

        let _ = self.command_u8(CMD_PROGRAM, &[0x08])?; // End
        Ok(())
    }

    /// Write `data` to the data endpoint, padding the final packet to `packet_size` with 0xff.
    fn write_data_padded(&mut self, data: &[u8], packet_size: usize) -> Result<(), WchLinkError> {
        for chunk in data.chunks(packet_size) {
            if chunk.len() == packet_size {
                self.iface.write_data(chunk, self.timeout)?;
            } else {
                let mut padded = vec![0xffu8; packet_size];
                padded[..chunk.len()].copy_from_slice(chunk);
                self.iface.write_data(&padded, self.timeout)?;
            }
        }
        Ok(())
    }

    /// en: Fast bulk memory read via the WCH-Link (SetReadMemoryRegion + Program ReadMemory +
    /// bulk from the data endpoint) - the read counterpart of [`Self::write_flash`], far faster than
    /// word-by-word DMI reads over a high-latency link (usbipd, the Windows WCH-driver ioctl path).
    /// `len` is rounded up to 4 bytes. Works for any readable address (flash / system / RAM). The
    /// chip must be attached. The link returns each 32-bit word byte-reversed; this restores LE.
    /// ja: WCH-Link の高速バルク read(SetReadMemoryRegion + ReadMemory + data EP からバルク)。
    /// word 単位 DMI read より桁違いに速い(usbipd / Windows の ioctl 経路で顕著)。len は 4 に切上げ。
    pub fn read_mem(&mut self, addr: u32, len: u32) -> Result<Vec<u8>, WchLinkError> {
        let len4 = len.div_ceil(4) * 4;
        self.iface.open_data_endpoints(DATA_EP_OUT, DATA_EP_IN)?;
        // SetReadMemoryRegion (cmd 0x03): start_addr BE32 + len BE32.
        let mut region = Vec::with_capacity(8);
        region.extend_from_slice(&addr.to_be_bytes());
        region.extend_from_slice(&len4.to_be_bytes());
        let _ = self.command(CMD_SET_READ_MEM_REGION, &region)?;
        // Program ReadMemory (0x0c), then stream len4 bytes from the data endpoint.
        let _ = self.command(CMD_PROGRAM, &[0x0c])?;
        let mut buf = vec![0u8; len4 as usize];
        let mut got = 0usize;
        while got < buf.len() {
            let n = self.iface.read_data(&mut buf[got..], self.timeout)?;
            if n == 0 {
                return Err(WchLinkError::UnexpectedResponse(Vec::new()));
            }
            got += n;
        }
        // Each 32-bit word comes back byte-reversed; swap to little-endian in place.
        let mut i = 0;
        while i + 4 <= buf.len() {
            buf.swap(i, i + 3);
            buf.swap(i + 1, i + 2);
            i += 4;
        }
        buf.truncate(len as usize);
        Ok(buf)
    }

    /// en: Control the WCH-LinkE target-power output (SetPower, cmd 0x0d): 3.3V on=`09`/off=`0a`,
    /// 5V on=`0b`/off=`0c`. WCH-LinkE only - the caller must check the variant (the CH549 Link has
    /// no power output). ja: WCH-LinkE の target 給電出力を制御(SetPower)。LinkE 限定。
    pub fn set_power(&mut self, rail_5v: bool, on: bool) -> Result<(), WchLinkError> {
        let payload: u8 = match (rail_5v, on) {
            (false, true) => 0x09,
            (false, false) => 0x0a,
            (true, true) => 0x0b,
            (true, false) => 0x0c,
        };
        let _ = self.command(CMD_CONTROL, &[payload])?;
        Ok(())
    }

    /// en: Switch a WCH-LinkE from RISC-V mode to DAP/ARM mode (`81 ff 01 41`). The probe
    /// re-enumerates as PID 0x8012, so this is fire-and-forget (no response is read). LinkE only -
    /// the caller must check the variant first. To switch back, send `81 ff 01 52` to the 0x8012
    /// device's OUT endpoint 0x02. Ref: wlink `switch_from_rv_to_dap` / cjacker/wchlinke-mode-switch.
    /// ja: WCH-LinkE を RISC-V→DAP に切替(`81 ff 01 41`)。probe は PID 0x8012 へ再列挙するので
    /// 応答は読まない。LinkE 限定(呼び出し側で variant 確認)。戻すのは 0x8012 の EP0x02 へ `81 ff 01 52`。
    pub fn switch_to_dap(&mut self) -> Result<(), WchLinkError> {
        let _ = self.iface.write(&[0x81, 0xff, 0x01, 0x41], self.timeout);
        Ok(())
    }

    /// en: Soft reset and run (Reset 0x01). This is `wlink reset` / the run-after-flash reset.
    /// ja: soft reset して実行(Reset 0x01)。`wlink reset` 相当。
    pub fn soft_reset(&mut self) -> Result<(), WchLinkError> {
        let _ = self.command(CMD_RESET, &[0x01])?;
        Ok(())
    }

    /// en: "Clear All Code Flash - By Power off" (EraseCodeFlash `0x0f`). Power-cycles the
    /// target through the probe and erases in the boot window before the app can reconfigure
    /// the debug pins - the recovery for a target whose SWDIO/SWCLK were repurposed. Requires
    /// SetSpeed(family) first. The probe must power the target (LinkE/LinkW).
    /// ja: 「Clear All Code Flash - By Power off」。probe が target を電源再投入し、app が
    /// debug ピンを再構成する前の boot 窓で消去する。SWDIO/SWCLK を他用途に使った target の
    /// 復旧手段。SetSpeed(family) が先に要る。target を probe 給電していること(LinkE/LinkW)。
    pub fn erase_code_flash_by_power_off(&mut self, family_byte: u8) -> Result<(), WchLinkError> {
        self.set_speed(family_byte, Speed::default())?;
        let _ = self.command(CMD_CONTROL, &[0x0f, family_byte])?;
        Ok(())
    }

    /// en: Enable/disable SDI-print forwarding. Payload is `ee 00` to ENABLE, `ee 01` to
    /// DISABLE (the flag is inverted vs intuition; verified against wlink and a usbmon capture:
    /// `sdi-print enable` sends `81 0d 02 ee 00`). Response byte 0x00 = ok, 0xff = unsupported.
    /// The LinkE then polls the target's DM data registers and forwards to its own CDC. LinkE only.
    /// ja: SDI print forward の有効/無効。payload は enable=`ee 00`、disable=`ee 01`(直感と逆。
    /// wlink と usbmon capture で確認)。応答 byte 0x00=成功、0xff=非対応。LinkE 専用。
    pub fn set_sdi_print_enabled(&mut self, enable: bool) -> Result<(), WchLinkError> {
        let flag = if enable { 0x00 } else { 0x01 };
        let resp = self.command(CMD_CONTROL, &[0xee, flag])?;
        if resp.first() == Some(&0xff) {
            return Err(WchLinkError::UnexpectedResponse(resp));
        }
        Ok(())
    }

    /// en: "Clear All Code Flash - By RST pin" (EraseCodeFlash `0x08`). Same idea but toggles
    /// NRST instead of power; requires the RST pin wired.
    /// ja: 「Clear All Code Flash - By RST pin」。電源でなく NRST を使う。RST 配線が要る。
    pub fn erase_code_flash_by_rst(&mut self, family_byte: u8) -> Result<(), WchLinkError> {
        self.set_speed(family_byte, Speed::default())?;
        let _ = self.command(CMD_CONTROL, &[0x08, family_byte])?;
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
            flash_bytes: u32::from(u16::from_be_bytes([reply[2], reply[3]])) * 1024,
            uuid,
            protection_raw,
            chip_id_echo: u32::from_be_bytes([reply[16], reply[17], reply[18], reply[19]]),
        }))
    }
}

/// en: [`WchLink`] speaks the DMI transport (docs/architecture.ja.md §2.1: the transport
/// implements DtmAccess, the DM layer stays transport-agnostic). Retries on BUSY.
/// ja: [`WchLink`] を DMI transport として使う。DM 層は transport 非依存のまま。BUSY は再試行。
impl DtmAccess for WchLink {
    fn dmi_read(&mut self, addr: u8) -> Result<u32, DmiError> {
        for _ in 0..8 {
            let r = self
                .dmi_op(addr, 0, 1)
                .map_err(|e| DmiError::Transport(e.to_string()))?;
            match r.status {
                DmiStatus::Busy => continue,
                DmiStatus::Failed => {
                    return Err(DmiError::OperationFailed("dmi read failed".into()));
                }
                _ => return Ok(r.data),
            }
        }
        Err(DmiError::Timeout)
    }

    fn dmi_write(&mut self, addr: u8, value: u32) -> Result<(), DmiError> {
        for _ in 0..8 {
            let r = self
                .dmi_op(addr, value, 2)
                .map_err(|e| DmiError::Transport(e.to_string()))?;
            match r.status {
                DmiStatus::Busy => continue,
                DmiStatus::Failed => {
                    return Err(DmiError::OperationFailed("dmi write failed".into()));
                }
                _ => return Ok(()),
            }
        }
        Err(DmiError::Timeout)
    }

    fn dmi_nop(&mut self) -> Result<(), DmiError> {
        self.dmi_op(0, 0, 0)
            .map(|_| ())
            .map_err(|e| DmiError::Transport(e.to_string()))
    }
}
