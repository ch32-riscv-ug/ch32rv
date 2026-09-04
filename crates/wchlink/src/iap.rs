//! en: IAP mode - rewriting the probe's *own* firmware. The probe leaves RISC-V mode on
//! `0x81 0x0f 0x01 0x01` ([`crate::WchLink::enter_iap`]), re-enumerates as a different USB
//! device (`4348:55e0`, vendor class `ff/80/55`, four 64-byte bulk endpoints) and accepts an
//! application image on the bulk pair `0x02`/`0x82`. Nothing here touches the target.
//!
//! Frames are `cmd | len | off_lo | off_hi | data`, every one of them answered by a 2-byte
//! `00 00` ack except the two that make the device leave (IAP entry and [`IapDevice::end`]).
//! The image is sent twice - a write pass (`0x80`) and a verify pass (`0x82`) - in 60-byte
//! chunks from offset 0; `off` is the low 16 bits of the running offset, so a transfer stream
//! must stay strictly sequential (the probe, not the host, keeps the upper bits).
//!
//! ja: IAP mode。probe 自身の firmware を書き換える。`0x81 0x0f 0x01 0x01` で RISC-V mode を
//! 抜け、別 device(`4348:55e0`)として再列挙し、bulk `0x02`/`0x82` で app image を受ける。
//! target には一切触れない。frame は `cmd | len | off_lo | off_hi | data` で、device が消える
//! 2 つ(IAP entry と [`IapDevice::end`])以外はすべて 2 byte の `00 00` ack が返る。image は
//! 書込 pass(`0x80`)と照合 pass(`0x82`)の 2 回、offset 0 から 60 byte ずつ送る。`off` は
//! 累積 offset の下位 16 bit なので、転送は厳密に順送りでなければならない(上位を持つのは
//! host ではなく probe 側)。
//!
//! Reference: `docs/protocol/wch-link.ja.md` §6.1 (verified against a live
//! WCH-LinkE updated in both directions, 2.22 <-> 2.13).

use std::time::Duration;

use ch32rv_usb::{UsbDeviceInfo, UsbError, UsbInterface};
use thiserror::Error;

use crate::{PID_IAP, VID_IAP};

/// en: Bulk endpoints used in IAP mode. The `0x01`/`0x81` pair exists but is never used.
/// ja: IAP mode で使う bulk endpoint。`0x01`/`0x81` の組は存在するが使われない。
const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x82;

/// Payload bytes per transfer: the 64-byte packet minus the 4-byte header.
pub const CHUNK: usize = 60;

const CMD_START: u8 = 0x81;
const CMD_WRITE: u8 = 0x80;
const CMD_VERIFY: u8 = 0x82;
const CMD_END: u8 = 0x83;
const ACK: [u8; 2] = [0x00, 0x00];

/// en: The probe stalls ~170 ms every 64 write packets (it programs 3,840 B at a time), so the
/// ack timeout has to sit well above that - a 100 ms timeout fails on every 64th transfer.
/// ja: probe は書込 64 packet ごとに ~170 ms 止まる(3,840 B ずつ焼く)ので、ack の timeout は
/// それより十分大きく取る。100 ms では 64 転送ごとに必ず失敗する。
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(3000);

/// Bootloader sizes seen in front of an application image inside a `*_APP_IAP.bin` file.
const BOOTLOADER_SIZES: [usize; 2] = [0x2000, 0x0c00];

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum IapError {
    #[error("not an IAP-mode device: {0} (expected 4348:55e0)")]
    NotIapMode(String),
    #[error(transparent)]
    Usb(#[from] UsbError),
    #[error("short write: {written} of {expected} bytes")]
    ShortWrite { written: usize, expected: usize },
    #[error("unexpected reply at offset {offset:#x}: {reply:02x?} (expected 00 00)")]
    UnexpectedReply { offset: usize, reply: Vec<u8> },
    #[error("image is empty")]
    EmptyImage,
}

/// en: What can be told about a firmware image without a probe attached. The version and the
/// USB identity come from the USB device descriptors the image carries verbatim; a WCH probe
/// firmware embeds one per mode (RISC-V `1a86:8010`, DAP `1a86:8012`).
/// ja: probe 無しで image から分かること。版と USB 識別は、image がそのまま抱えている USB
/// device descriptor から読む(WCH の probe firmware は mode ごとに 1 つ埋めている)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageInfo {
    pub len: usize,
    /// Firmware version decoded from `bcdDevice` (BCD, e.g. `0x0222` -> `(2, 22)`).
    pub version: Option<(u8, u8)>,
    /// Product IDs the image will enumerate as, in the order found.
    pub pids: Vec<u16>,
    /// en: `Some(n)` when the file looks like `bootloader + app` (a `*_APP_IAP.bin`): `n` bytes
    /// of bootloader, all-`0xff` padded up to the application at offset `n`. Such a file must
    /// NOT be sent over IAP - it is meant for an external programmer at `0x08000000`.
    /// ja: `Some(n)` なら BL + app 形式(`*_APP_IAP.bin`)。IAP に流してはいけない。
    pub bootloader_prefix: Option<usize>,
    /// en: The image starts with a RISC-V `jal` (opcode `0x6f`), as every CH32V-based probe
    /// firmware does. False for the 8051 images of the CH549 Link (`0x02` = LJMP).
    /// ja: 先頭が RISC-V `jal`(opcode `0x6f`)か。CH549 Link の 8051 image は `0x02`(LJMP)。
    pub looks_riscv: bool,
}

impl ImageInfo {
    /// The offset of the application within the file (0 unless this is a bootloader+app image).
    pub fn app_offset(&self) -> usize {
        self.bootloader_prefix.unwrap_or(0)
    }
}

/// en: Read what the image says about itself. Never fails: an unrecognised blob simply reports
/// no version and no PIDs, and the caller decides.
/// ja: image の自己申告を読む。失敗はしない(素性が読めなければ版も PID も空で返す)。
pub fn inspect(image: &[u8]) -> ImageInfo {
    let mut info = ImageInfo {
        len: image.len(),
        ..Default::default()
    };
    // A USB device descriptor is 18 bytes: 12 01 bcdUSB(2) class sub proto mps
    // idVendor(2) idProduct(2) bcdDevice(2) i* x3 bNumConfigurations.
    for off in 0..image.len().saturating_sub(18) {
        let d = &image[off..off + 18];
        if d[0] != 0x12 || d[1] != 0x01 {
            continue;
        }
        if u16::from_le_bytes([d[8], d[9]]) != crate::VID_WCH {
            continue;
        }
        let pid = u16::from_le_bytes([d[10], d[11]]);
        if !info.pids.contains(&pid) {
            info.pids.push(pid);
        }
        if info.version.is_none() {
            info.version = Some((bcd(d[13]), bcd(d[12])));
        }
    }
    info.looks_riscv = image.first() == Some(&0x6f);
    info.bootloader_prefix = BOOTLOADER_SIZES.into_iter().find(|&bl| {
        image.len() > bl + 0x400
            && image[bl - 64..bl].iter().all(|&b| b == 0xff)
            && image.get(bl).is_some_and(|&b| b != 0xff)
    });
    info
}

/// One packed BCD byte to decimal (`0x22` -> 22).
fn bcd(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0x0f)
}

/// en: An opened probe in IAP mode. Dropping it without [`IapDevice::end`] leaves the probe in
/// IAP mode, which is recoverable: re-open and write again (no IAP entry needed).
/// ja: IAP mode で開いた probe。[`IapDevice::end`] せずに落とすと IAP mode のまま残るが、
/// 開き直して書き直せば復旧できる(entry は不要)。
pub struct IapDevice {
    iface: UsbInterface,
    timeout: Duration,
}

impl IapDevice {
    /// Open a probe that has already re-enumerated into IAP mode (`4348:55e0`).
    pub fn open(dev: &UsbDeviceInfo) -> Result<Self, IapError> {
        if dev.vid() != VID_IAP || dev.pid() != PID_IAP {
            return Err(IapError::NotIapMode(dev.usb_id()));
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

    /// Send one frame and require the `00 00` ack.
    fn frame(&mut self, cmd: u8, len: u8, off: usize, data: &[u8]) -> Result<(), IapError> {
        let mut tx = Vec::with_capacity(4 + data.len());
        tx.push(cmd);
        tx.push(len);
        tx.extend_from_slice(&(off as u16).to_le_bytes());
        tx.extend_from_slice(data);
        let written = self.iface.write(&tx, self.timeout)?;
        if written != tx.len() {
            return Err(IapError::ShortWrite {
                written,
                expected: tx.len(),
            });
        }
        let mut rx = [0u8; 64];
        let n = self.iface.read(&mut rx, self.timeout)?;
        if rx[..n] != ACK {
            return Err(IapError::UnexpectedReply {
                offset: off,
                reply: rx[..n].to_vec(),
            });
        }
        Ok(())
    }

    /// Begin an update (`81 02 0000`). The ack takes ~10 ms - too short to be a mass erase.
    pub fn start(&mut self) -> Result<(), IapError> {
        self.frame(CMD_START, 0x02, 0, &[])
    }

    /// en: Stream the whole image with `cmd`, 60 bytes per transfer from offset 0.
    /// `progress(done)` is called after each acked chunk.
    /// ja: image 全体を `cmd` で offset 0 から 60 byte ずつ送る。
    fn pass(
        &mut self,
        cmd: u8,
        image: &[u8],
        progress: &mut impl FnMut(u64),
    ) -> Result<(), IapError> {
        let mut off = 0usize;
        for chunk in image.chunks(CHUNK) {
            self.frame(cmd, chunk.len() as u8, off, chunk)?;
            off += chunk.len();
            progress(off as u64);
        }
        Ok(())
    }

    /// Write pass (`0x80`): the transfers that actually program the probe's flash.
    pub fn write_pass(
        &mut self,
        image: &[u8],
        mut progress: impl FnMut(u64),
    ) -> Result<(), IapError> {
        self.pass(CMD_WRITE, image, &mut progress)
    }

    /// en: Verify pass (`0x82`): the same bytes again. The stock utility always sends it, and the
    /// probe flushes its last partial write buffer on the first frame of this pass.
    /// ja: 照合 pass(`0x82`)。純正も必ず送る。書込 pass の端数はこの pass の最初の frame で
    /// flush される。
    pub fn verify_pass(
        &mut self,
        image: &[u8],
        mut progress: impl FnMut(u64),
    ) -> Result<(), IapError> {
        self.pass(CMD_VERIFY, image, &mut progress)
    }

    /// en: End (`83 02 0000`). No reply: the probe jumps to the new application and re-enumerates.
    /// ja: 終了(`83 02 0000`)。応答は無く、probe は新しい app へ jump して再列挙する。
    pub fn end(&mut self) -> Result<(), IapError> {
        let tx = [CMD_END, 0x02, 0x00, 0x00];
        let _ = self.iface.write(&tx, self.timeout);
        Ok(())
    }

    /// en: The whole update: start -> write pass -> verify pass -> end. `progress(phase, done)`
    /// reports bytes done within each pass.
    /// ja: 更新一式。`progress(phase, done)` が pass ごとの進捗を返す。
    pub fn update(
        &mut self,
        image: &[u8],
        mut progress: impl FnMut(Pass, u64),
    ) -> Result<(), IapError> {
        if image.is_empty() {
            return Err(IapError::EmptyImage);
        }
        self.start()?;
        self.write_pass(image, |done| progress(Pass::Write, done))?;
        self.verify_pass(image, |done| progress(Pass::Verify, done))?;
        self.end()
    }
}

/// Which of the two identical passes an update is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Write,
    Verify,
}

impl Pass {
    pub fn as_str(self) -> &'static str {
        match self {
            Pass::Write => "iap-write",
            Pass::Verify => "iap-verify",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bcd_decodes_version_bytes() {
        assert_eq!(bcd(0x22), 22);
        assert_eq!(bcd(0x13), 13);
        assert_eq!(bcd(0x02), 2);
    }

    /// A synthetic image carrying one RISC-V-mode device descriptor.
    fn image_with_descriptor(len: usize, bcd_device: u16) -> Vec<u8> {
        let mut img = vec![0xa5u8; len];
        img[0] = 0x6f; // RISC-V jal, as every CH32V-based probe firmware starts
        let d = [
            0x12,
            0x01,
            0x10,
            0x01,
            0xef,
            0x02,
            0x01,
            0x40,
            0x86,
            0x1a,
            0x10,
            0x80,
            bcd_device as u8,
            (bcd_device >> 8) as u8,
            0x01,
            0x02,
            0x03,
            0x01,
        ];
        img[len / 2..len / 2 + 18].copy_from_slice(&d);
        img
    }

    #[test]
    fn inspect_rejects_a_non_riscv_image() {
        let mut img = image_with_descriptor(4096, 0x0212);
        img[0] = 0x02; // 8051 LJMP, as in the CH549 Link firmware
        assert!(!inspect(&img).looks_riscv);
    }

    #[test]
    fn inspect_reads_version_and_pid() {
        let info = inspect(&image_with_descriptor(4096, 0x0222));
        assert_eq!(info.version, Some((2, 22)));
        assert_eq!(info.pids, vec![0x8010]);
        assert_eq!(info.bootloader_prefix, None);
        assert_eq!(info.app_offset(), 0);
    }

    #[test]
    fn inspect_flags_a_bootloader_plus_app_image() {
        // 8 KB of bootloader (0xff-padded tail) followed by an application.
        let mut img = vec![0x11u8; 0x1000];
        img.resize(0x2000, 0xff);
        img.extend_from_slice(&image_with_descriptor(4096, 0x0222));
        let info = inspect(&img);
        assert_eq!(info.bootloader_prefix, Some(0x2000));
        assert_eq!(info.app_offset(), 0x2000);
    }

    #[test]
    fn inspect_does_not_flag_a_plain_app() {
        let info = inspect(&image_with_descriptor(0x20000, 0x0213));
        assert_eq!(info.version, Some((2, 13)));
        assert_eq!(info.bootloader_prefix, None);
    }
}
