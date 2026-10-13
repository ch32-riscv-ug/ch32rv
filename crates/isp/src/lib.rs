//! en: Factory ISP route (docs/cli.ja.md §4.7): the bootloader WCH keeps in the chip's BOOT area,
//! spoken over USB bulk (EP `0x02` OUT / `0x82` IN). The protocol is implemented in-house
//! (wchisp is GPL-2.0 and is not vendored; references are minichlink's `pgm-wch-isp.c` (MIT),
//! wch-protocols `protocols/pc-to-device-isp.ja.md`, and our own bench runs). ISP devices
//! (`4348:55e0` / `1a86:55e0`) carry no USB serial, so several devices are told apart by USB
//! position or refused. WCH-Link IAP mode shares `4348:55e0`; it does not answer Identify the
//! factory way, which is how it is told apart.
//!
//! A request is `cmd len(2 LE) payload`; the reply is `cmd 00 len(2 LE) payload`.
//!
//! ja: factory ISP 経路。チップの BOOT 領域にある WCH の bootloader と USB bulk(EP `0x02` /
//! `0x82`)で話す。protocol は自前実装(wchisp は GPL-2.0 のため取り込まない)。ISP device は
//! USB serial を持たないので、複数台は USB の位置で選ぶか断る。WCH-Link の IAP は同じ
//! `4348:55e0` だが、Identify に factory の形で答えないので区別できる。

use std::time::Duration;

use ch32rv_usb::device::{UsbDeviceInfo, UsbError, UsbInterface};
use thiserror::Error;

/// USB IDs the factory bootloader enumerates with (both are seen in the field).
pub const ISP_IDS: [(u16, u16); 2] = [(0x1a86, 0x55e0), (0x4348, 0x55e0)];

const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x82;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

const CMD_IDENTIFY: u8 = 0xa1;
const CMD_END: u8 = 0xa2;
const CMD_KEY: u8 = 0xa3;
const CMD_ERASE: u8 = 0xa4;
const CMD_PROGRAM: u8 = 0xa5;
const CMD_VERIFY: u8 = 0xa6;
const CMD_READ_CONFIG: u8 = 0xa7;

/// en: Data bytes per Program / Verify request (the request is 64 bytes on the wire with its
/// headers: `cmd len(2) addr(4) pad(1)` + 56).
/// ja: Program / Verify 1 回のデータ長(ヘッダ込みで 64 byte)。
pub const CHUNK: usize = 56;
/// The bootloader erases in 1 KiB sectors.
pub const SECTOR: usize = 1024;
/// Images are padded with `0xff` to this boundary before they are sent.
const PAD_TO: usize = 256;
/// Fewer sectors than this are never erased (the minimum the reference implementations use).
const MIN_ERASE_SECTORS: u32 = 8;

const IDENTIFY_MAGIC: &[u8; 16] = b"MCU ISP & WCH.CN";

#[derive(Debug, Error)]
pub enum IspError {
    #[error(transparent)]
    Usb(#[from] UsbError),
    #[error("short write: {written} of {expected} byte(s)")]
    ShortWrite { written: usize, expected: usize },
    #[error("unexpected reply to 0x{cmd:02x}: {reply:02x?}")]
    UnexpectedReply { cmd: u8, reply: Vec<u8> },
    #[error("not a factory ISP bootloader (Identify answered {0:02x?})")]
    NotFactoryIsp(Vec<u8>),
    #[error(
        "the bootloader's key check does not match ours (sent 0x{ours:02x}, got 0x{theirs:02x})"
    )]
    KeyMismatch { ours: u8, theirs: u8 },
    #[error("0x{cmd:02x} failed with status 0x{status:02x}")]
    Status { cmd: u8, status: u8 },
    #[error("verify mismatch in the chunk at offset 0x{0:06x}")]
    VerifyMismatch(u32),
    #[error("image is {actual} bytes; the chip's code flash is {limit} bytes")]
    ImageTooLarge { actual: usize, limit: usize },
    #[error("no key yet: read the config (UID) before programming")]
    NoKey,
}

/// What Identify answers: the chip within its series and the series itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub chip_id: u8,
    pub device_type: u8,
}

/// What ReadConfig answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IspConfig {
    /// The two bytes before the config block (meaning not pinned down).
    pub head: [u8; 2],
    /// The option bytes as the bootloader presents them (12 bytes).
    pub config: [u8; 12],
    /// Bootloader version, `[_, major, minor_hi, minor_lo]` (shown as `major.minor_hi minor_lo`).
    pub btver: [u8; 4],
    pub uid: [u8; 8],
}

impl IspConfig {
    /// `2.31` style, as WCH's tools print it.
    pub fn btver_text(&self) -> String {
        format!("{}.{}{}", self.btver[1], self.btver[2], self.btver[3])
    }

    /// Read protection is off when RDPR/nRDPR read `a5 5a`.
    pub fn read_protected(&self) -> bool {
        self.config[0] != 0xa5
    }
}

/// en: The 8-byte XOR key for Program / Verify data, for an all-zero IspKey seed: every byte is
/// the UID's byte sum, and the last one adds the chip id. ja: seed を全 0 で送ったときの key。
pub fn xor_key(uid: &[u8; 8], chip_id: u8) -> [u8; 8] {
    let sum = uid.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    let mut key = [sum; 8];
    key[7] = sum.wrapping_add(chip_id);
    key
}

/// en: Code flash size by Identify, for the series ch32rv knows (`None` otherwise; the DB is
/// the authority where the chip is also known by its debug chip id).
/// ja: Identify から code flash の容量(知っている系列のみ)。
pub fn flash_size(id: Identity) -> Option<usize> {
    match id.device_type {
        // CH32X03x: chip ids ending in 7 are the 48 KiB parts; the others have 62 KiB usable.
        0x23 => Some(if id.chip_id & 0x0f == 7 {
            48 * 1024
        } else {
            62 * 1024
        }),
        _ => None,
    }
}

/// en: Series name by Identify's device type, for display. ja: 表示用の系列名。
pub fn series_name(device_type: u8) -> Option<&'static str> {
    Some(match device_type {
        0x14 => "CH32F10x",
        0x15 => "CH32V10x",
        0x17 => "CH32V30x",
        0x18 => "CH32V20x",
        0x19 => "CH32F20x",
        0x20 => "CH32V00x",
        0x21 => "CH32V00x",
        0x23 => "CH32X03x",
        0x24 => "CH32L10x",
        0x25 => "CH32V00x",
        _ => return None,
    })
}

/// en: ISP-mode USB devices present now (factory bootloaders and WCH-Link IAP alike; which one a
/// device is shows only when it is asked to Identify). ja: いま居る ISP mode の USB device。
pub fn list() -> Result<Vec<UsbDeviceInfo>, UsbError> {
    Ok(ch32rv_usb::enumerate()?
        .into_iter()
        .filter(|d| ISP_IDS.contains(&(d.vid(), d.pid())))
        .collect())
}

/// An opened factory ISP bootloader.
pub struct IspDevice {
    iface: UsbInterface,
    timeout: Duration,
    identity: Option<Identity>,
    key: Option<[u8; 8]>,
}

impl IspDevice {
    pub fn open(dev: &UsbDeviceInfo) -> Result<Self, IspError> {
        let iface = dev.open_interface(0, EP_OUT, EP_IN)?;
        Ok(Self {
            iface,
            timeout: DEFAULT_TIMEOUT,
            identity: None,
            key: None,
        })
    }

    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Send one request and return the reply payload (after checking its `cmd 00 len` header).
    fn transact(&mut self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>, IspError> {
        let mut tx = Vec::with_capacity(3 + payload.len());
        tx.push(cmd);
        tx.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        tx.extend_from_slice(payload);
        let written = self.iface.write(&tx, self.timeout)?;
        if written != tx.len() {
            return Err(IspError::ShortWrite {
                written,
                expected: tx.len(),
            });
        }
        let mut rx = [0u8; 64];
        let n = self.iface.read(&mut rx, self.timeout)?;
        let rx = &rx[..n];
        let len = rx
            .get(2..4)
            .map(|l| u16::from_le_bytes([l[0], l[1]]) as usize);
        match len {
            Some(len) if rx[0] == cmd && rx.len() == 4 + len => Ok(rx[4..].to_vec()),
            _ => Err(IspError::UnexpectedReply {
                cmd,
                reply: rx.to_vec(),
            }),
        }
    }

    /// A request whose reply is `status 00`; non-zero status is an error.
    fn status(&mut self, cmd: u8, payload: &[u8]) -> Result<(), IspError> {
        let r = self.transact(cmd, payload)?;
        match r.first() {
            Some(0) => Ok(()),
            Some(&status) => Err(IspError::Status { cmd, status }),
            None => Err(IspError::UnexpectedReply { cmd, reply: r }),
        }
    }

    /// en: Identify (`0xa1`). The chip id / device type sent are ignored by the bootloader; the
    /// reply names the real ones. ja: Identify。送る値は無視され、応答が実物を名乗る。
    pub fn identify(&mut self) -> Result<Identity, IspError> {
        let mut p = vec![0x00, 0x00];
        p.extend_from_slice(IDENTIFY_MAGIC);
        let r = match self.transact(CMD_IDENTIFY, &p) {
            Ok(r) => r,
            Err(IspError::UnexpectedReply { reply, .. }) => {
                return Err(IspError::NotFactoryIsp(reply));
            }
            Err(e) => return Err(e),
        };
        let [chip_id, device_type] = r[..] else {
            return Err(IspError::NotFactoryIsp(r));
        };
        let id = Identity {
            chip_id,
            device_type,
        };
        self.identity = Some(id);
        Ok(id)
    }

    /// en: ReadConfig (`0xa7`, every field). Also derives the data key from the UID, since every
    /// later write needs it. ja: ReadConfig。UID から data の key も作る。
    pub fn read_config(&mut self) -> Result<IspConfig, IspError> {
        let r = self.transact(CMD_READ_CONFIG, &[0x1f, 0x00])?;
        if r.len() < 26 {
            return Err(IspError::UnexpectedReply {
                cmd: CMD_READ_CONFIG,
                reply: r,
            });
        }
        let mut cfg = IspConfig {
            head: [r[0], r[1]],
            config: [0; 12],
            btver: [0; 4],
            uid: [0; 8],
        };
        cfg.config.copy_from_slice(&r[2..14]);
        cfg.btver.copy_from_slice(&r[14..18]);
        cfg.uid.copy_from_slice(&r[18..26]);
        Ok(cfg)
    }

    /// en: IspKey (`0xa3`) with an all-zero seed; checks the bootloader's key sum against ours.
    /// Needs Identify and ReadConfig first. ja: seed 全 0 の IspKey。応答の key の和を照合する。
    pub fn set_key(&mut self, cfg: &IspConfig) -> Result<(), IspError> {
        let id = self.identity.ok_or(IspError::NoKey)?;
        let key = xor_key(&cfg.uid, id.chip_id);
        let r = self.transact(CMD_KEY, &[0u8; 30])?;
        let ours = key.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        let theirs = *r.first().ok_or(IspError::UnexpectedReply {
            cmd: CMD_KEY,
            reply: r.clone(),
        })?;
        if ours != theirs {
            return Err(IspError::KeyMismatch { ours, theirs });
        }
        self.key = Some(key);
        Ok(())
    }

    /// Erase `sectors` 1 KiB sectors from the start of code flash (at least the minimum).
    pub fn erase(&mut self, sectors: u32) -> Result<(), IspError> {
        let n = sectors.max(MIN_ERASE_SECTORS);
        self.status(CMD_ERASE, &n.to_le_bytes())
    }

    /// One Program / Verify request: `addr(4 LE) pad(1) data ^ key`.
    fn data_cmd(&mut self, cmd: u8, offset: u32, chunk: &[u8]) -> Result<(), IspError> {
        let key = self.key.ok_or(IspError::NoKey)?;
        let mut p = Vec::with_capacity(5 + chunk.len());
        p.extend_from_slice(&offset.to_le_bytes());
        p.push(0);
        p.extend(chunk.iter().enumerate().map(|(i, b)| b ^ key[i % 8]));
        self.status(cmd, &p)
    }

    /// en: Program `image` from the start of code flash (offset 0 = `0x0800_0000`), padded with
    /// `0xff` to 256 bytes, then the empty request that commits the last page. `progress(done)`
    /// is called after each chunk. ja: code flash 先頭から書き、最後に空の要求で最終 page を確定。
    pub fn program(
        &mut self,
        image: &[u8],
        mut progress: impl FnMut(usize),
    ) -> Result<(), IspError> {
        let padded = pad(image);
        let mut off = 0usize;
        for chunk in padded.chunks(CHUNK) {
            self.data_cmd(CMD_PROGRAM, off as u32, chunk)?;
            off += chunk.len();
            progress(off.min(image.len()));
        }
        self.data_cmd(CMD_PROGRAM, off as u32, &[])
    }

    /// en: Verify `image` against code flash: the bootloader compares each chunk and answers a
    /// non-zero status on a difference. ja: chunk ごとに bootloader が照合する。
    pub fn verify(
        &mut self,
        image: &[u8],
        mut progress: impl FnMut(usize),
    ) -> Result<(), IspError> {
        let padded = pad(image);
        let mut off = 0usize;
        for chunk in padded.chunks(CHUNK) {
            match self.data_cmd(CMD_VERIFY, off as u32, chunk) {
                Ok(()) => {}
                Err(IspError::Status { .. }) => return Err(IspError::VerifyMismatch(off as u32)),
                Err(e) => return Err(e),
            }
            off += chunk.len();
            progress(off.min(image.len()));
        }
        Ok(())
    }

    /// en: Leave the bootloader (`0xa2`); with `run`, reset into the application. The bootloader
    /// answers `a2 00 02 00 00 00` and resets only once that answer has been read (~200 ms later on
    /// CH32X035, BTVER 2.60) - a host that does not read it leaves the chip in the bootloader,
    /// which then refuses the next session's ReadConfig with a stall. The read is best effort:
    /// the answer can race the reset.
    /// ja: bootloader を抜ける(`run` なら app へ reset)。応答 `a2 00 02 00 00 00` が読まれて
    /// はじめて reset する(X035 で約 200 ms 後)。読まないと bootloader に残り、次の session の
    /// ReadConfig を stall で断る。応答は reset と競りうるので読むのは best effort。
    pub fn end(mut self, run: bool) -> Result<(), IspError> {
        let tx = [CMD_END, 0x01, 0x00, u8::from(run)];
        let written = self.iface.write(&tx, self.timeout)?;
        if written != tx.len() {
            return Err(IspError::ShortWrite {
                written,
                expected: tx.len(),
            });
        }
        let mut rx = [0u8; 64];
        let _ = self.iface.read(&mut rx, Duration::from_millis(500));
        Ok(())
    }
}

/// The image padded with `0xff` to the bootloader's 256-byte boundary.
fn pad(image: &[u8]) -> Vec<u8> {
    let mut v = image.to_vec();
    v.resize(image.len().div_ceil(PAD_TO) * PAD_TO, 0xff);
    v
}

/// Sectors to erase for an image of `len` bytes.
pub fn sectors_for(len: usize) -> u32 {
    len.div_ceil(SECTOR) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_from_uid() {
        let uid = [0xcd, 0xab, 0x16, 0xc0, 0x5c, 0xbc, 0x3f, 0x28];
        let sum = uid.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        let key = xor_key(&uid, 0x11);
        assert_eq!(key[..7], [sum; 7]);
        assert_eq!(key[7], sum.wrapping_add(0x11));
    }

    #[test]
    fn pads_to_256() {
        assert_eq!(pad(&[1; 10]).len(), 256);
        assert_eq!(pad(&[1; 256]).len(), 256);
        assert_eq!(pad(&[1; 257]).len(), 512);
        assert_eq!(pad(&[1; 10])[10], 0xff);
    }

    #[test]
    fn sectors() {
        assert_eq!(sectors_for(1), 1);
        assert_eq!(sectors_for(1024), 1);
        assert_eq!(sectors_for(4097), 5);
    }
}
