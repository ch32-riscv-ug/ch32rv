//! en: USB DFU 1.1 download (the DFU-mode half: DFU_DNLOAD / DFU_GETSTATUS / DFU_CLRSTATUS /
//! DFU_ABORT) for an application that reflashes itself, as ArduinoCore-CH32RV's sketches do on
//! a CH32H417 whose ROM ISP and SWD are out of reach. The run-time half (DFU_DETACH) lives in
//! [`crate::UsbDeviceInfo::dfu_detach`].
//! ja: USB DFU 1.1 のダウンロード(DFU mode 側)。自分を書き換えるアプリ向け(ArduinoCore-CH32RV の
//! H417 のスケッチ)。run-time 側の DFU_DETACH は `UsbDeviceInfo::dfu_detach`。

use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};

use crate::device::{UsbDeviceInfo, UsbError, classify_open_error};

const DFU_DNLOAD: u8 = 1;
const DFU_GETSTATUS: u8 = 3;
const DFU_CLRSTATUS: u8 = 4;
const DFU_ABORT: u8 = 6;
/// The DFU functional descriptor's type (DFU 1.1 §4.1.3).
const DFU_FUNCTIONAL: u8 = 0x21;
const TIMEOUT: Duration = Duration::from_secs(5);

/// DFU 1.1 states (§6.1.2) ch32rv acts on.
pub mod state {
    pub const DFU_IDLE: u8 = 2;
    pub const DNLOAD_SYNC: u8 = 3;
    pub const DNBUSY: u8 = 4;
    pub const DNLOAD_IDLE: u8 = 5;
    pub const MANIFEST_SYNC: u8 = 6;
    pub const MANIFEST: u8 = 7;
    pub const MANIFEST_WAIT_RESET: u8 = 8;
    pub const ERROR: u8 = 10;
}

/// What DFU_GETSTATUS answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// bStatus: 0 OK, else the error (DFU 1.1 §6.1.2).
    pub status: u8,
    /// bwPollTimeout: how long to wait before the next GETSTATUS.
    pub poll_ms: u32,
    pub state: u8,
}

/// A device in DFU mode, its DFU interface claimed.
pub struct DfuMode {
    iface: nusb::Interface,
    number: u8,
    /// wTransferSize from the functional descriptor (the block size of a DNLOAD).
    pub transfer_size: u16,
    /// bmAttributes bit 2: the device stays on the bus after manifestation.
    pub manifestation_tolerant: bool,
}

impl DfuMode {
    /// en: Open `dev`'s DFU-mode interface (detaching a kernel driver where one is bound) and read
    /// its functional descriptor. ja: DFU mode の interface を開き、機能記述子を読む。
    pub fn open(dev: &UsbDeviceInfo) -> Result<DfuMode, UsbError> {
        let number = dev
            .dfu_mode_interface()
            .ok_or_else(|| UsbError::Open("the device has no DFU-mode interface".into()))?;
        let info = dev
            .nusb_info()
            .ok_or_else(|| UsbError::Open("a replayed device has no DFU interface".into()))?;
        let device = info.open().wait().map_err(classify_open_error)?;
        let (mut transfer_size, mut tolerant) = (0u16, false);
        if let Ok(cfg) = device.active_configuration() {
            for alt in cfg.interface_alt_settings() {
                if alt.interface_number() != number {
                    continue;
                }
                for d in alt.descriptors() {
                    // bLength bDescriptorType bmAttributes wDetachTimeOut(2) wTransferSize(2) ...
                    if d.descriptor_type() == DFU_FUNCTIONAL && d.len() >= 7 {
                        tolerant = d[2] & 0x04 != 0;
                        transfer_size = u16::from_le_bytes([d[5], d[6]]);
                    }
                }
            }
        }
        let iface = device
            .detach_and_claim_interface(number)
            .wait()
            .map_err(classify_open_error)?;
        Ok(DfuMode {
            iface,
            number,
            transfer_size: if transfer_size == 0 {
                1024
            } else {
                transfer_size
            },
            manifestation_tolerant: tolerant,
        })
    }

    fn out(&self, request: u8, value: u16, data: &[u8]) -> Result<(), UsbError> {
        self.iface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value,
                    index: u16::from(self.number),
                    data,
                },
                TIMEOUT,
            )
            .wait()
            .map_err(|e| UsbError::Transfer(e.to_string()))
    }

    /// DFU_GETSTATUS.
    pub fn status(&self) -> Result<Status, UsbError> {
        let s = self
            .iface
            .control_in(
                ControlIn {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request: DFU_GETSTATUS,
                    value: 0,
                    index: u16::from(self.number),
                    length: 6,
                },
                TIMEOUT,
            )
            .wait()
            .map_err(|e| UsbError::Transfer(e.to_string()))?;
        if s.len() < 6 {
            return Err(UsbError::Transfer(format!(
                "DFU_GETSTATUS answered {} bytes",
                s.len()
            )));
        }
        Ok(Status {
            status: s[0],
            poll_ms: u32::from_le_bytes([s[1], s[2], s[3], 0]),
            state: s[4],
        })
    }

    /// DFU_CLRSTATUS (leave dfuERROR) and DFU_ABORT (back to dfuIDLE), for a fresh start.
    pub fn reset_state(&self) -> Result<(), UsbError> {
        let s = self.status()?;
        if s.state == state::ERROR {
            self.out(DFU_CLRSTATUS, 0, &[])?;
        } else if s.state != state::DFU_IDLE {
            self.out(DFU_ABORT, 0, &[])?;
        }
        Ok(())
    }

    /// en: DFU_DNLOAD block `block` with `data` (empty: the end of the download), then GETSTATUS
    /// until the device has taken it, waiting each bwPollTimeout as DFU 1.1 asks.
    /// ja: block を DNLOAD し、取り込むまで bwPollTimeout ずつ待って GETSTATUS する。
    pub fn download(&self, block: u16, data: &[u8]) -> Result<Status, UsbError> {
        self.out(DFU_DNLOAD, block, data)?;
        for _ in 0..1000 {
            let s = self.status()?;
            if s.status != 0 {
                return Err(UsbError::Transfer(format!(
                    "DFU block {block}: status {} in state {}",
                    s.status, s.state
                )));
            }
            match s.state {
                state::DNBUSY | state::DNLOAD_SYNC => {
                    std::thread::sleep(Duration::from_millis(u64::from(s.poll_ms.max(1))));
                }
                _ => return Ok(s),
            }
        }
        Err(UsbError::Timeout)
    }

    /// en: Finish: a zero-length DNLOAD, then one GETSTATUS to start manifestation. A device that
    /// is not manifestation tolerant resets now, so a lost answer here is not an error.
    /// ja: 終わり。長さ 0 の DNLOAD と GETSTATUS で manifestation を始める。リセットする device は
    /// ここで答えが無くてもよい。
    pub fn finish(&self, block: u16) -> Result<(), UsbError> {
        self.out(DFU_DNLOAD, block, &[])?;
        match self.status() {
            Ok(s) if s.status != 0 => Err(UsbError::Transfer(format!(
                "DFU manifestation: status {} in state {}",
                s.status, s.state
            ))),
            _ => Ok(()),
        }
    }
}
