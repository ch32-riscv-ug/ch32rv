//! Clients for custom bootloaders.
//!
//! The first implemented route is the rv003usb/UIAPduino HID scratchpad
//! bootloader (`1209:b003`, UIAPduino `1209:b803`). The bootloader accepts a
//! 128-byte HID feature report containing a small RISC-V stub and executes it.

use std::thread;
use std::time::Duration;

use hidapi::{HidApi, HidDevice};
use thiserror::Error;

pub const DEFAULT_HID_IDS: [(u16, u16); 2] = [(0x1209, 0xb803), (0x1209, 0xb003)];
pub const V003_FLASH_BASE: u32 = 0x0800_0000;
pub const V003_FLASH_SIZE: usize = 16 * 1024;
const SECTOR_SIZE: usize = 64;
const REPORT_SIZE: usize = 128;
const REPORT_ID: u8 = 0xaa;

const FLASH_KEYR: u32 = 0x4002_2004;
const FLASH_OBKEYR: u32 = 0x4002_2008;
const FLASH_STATR: u32 = 0x4002_200c;
const FLASH_CTLR: u32 = 0x4002_2010;
const FLASH_ADDR: u32 = 0x4002_2014;
const FLASH_OBR: u32 = 0x4002_201c;
const FLASH_MODEKEYR: u32 = 0x4002_2024;

const CTLR_PAGE_PG: u32 = 0x0001_0000;
const CTLR_PAGE_ER: u32 = 0x0002_0000;
const CTLR_BUF_RST: u32 = 0x0008_0000;
const CTLR_START: u32 = 0x0000_0040;

const HALT_WAIT: &[u8] = &[0x81, 0x46, 0x94, 0xc1, 0xfd, 0x56, 0x14, 0xc1, 0x82, 0x80];

const WORD_READ: &[u8] = &[
    0x23, 0xa0, 0x05, 0x00, 0x13, 0x07, 0x45, 0x03, 0x0c, 0x43, 0x50, 0x43, 0x2e, 0x96, 0x21, 0x07,
    0x94, 0x41, 0x14, 0xc3, 0x91, 0x05, 0x11, 0x07, 0xe3, 0xcc, 0xc5, 0xfe, 0x93, 0x06, 0xf0, 0xff,
    0x14, 0xc1, 0x82, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

const WORD_WRITE: &[u8] = &[
    0x23, 0xa0, 0x05, 0x00, 0x13, 0x07, 0x45, 0x03, 0x0c, 0x43, 0x50, 0x43, 0x2e, 0x96, 0x21, 0x07,
    0x14, 0x43, 0x94, 0xc1, 0x91, 0x05, 0x11, 0x07, 0xe3, 0xcc, 0xc5, 0xfe, 0x93, 0x06, 0xf0, 0xff,
    0x14, 0xc1, 0x82, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

const WRITE64_FLASH: &[u8] = &[
    0x13, 0x07, 0x45, 0x03, 0x0c, 0x43, 0x13, 0x86, 0x05, 0x04, 0x5c, 0x43, 0x8c, 0xc7, 0x14, 0x47,
    0x94, 0xc1, 0xb7, 0x06, 0x05, 0x00, 0xd4, 0xc3, 0x94, 0x41, 0x91, 0x05, 0x11, 0x07, 0xe3, 0xc8,
    0xc5, 0xfe, 0xc1, 0x66, 0x93, 0x86, 0x06, 0x04, 0xd4, 0xc3, 0xfd, 0x56, 0x14, 0xc1, 0x82, 0x80,
];

const RUN_APP: &[u8] = &[
    0xb7, 0xf5, 0xff, 0x1f, 0x93, 0x87, 0xc5, 0x77, 0x03, 0xa7, 0x07, 0x00, 0x13, 0x57, 0x07, 0x01,
    0x83, 0x96, 0x07, 0x00, 0x93, 0xc7, 0xc6, 0x77, 0x63, 0x16, 0xf7, 0x00, 0x33, 0x87, 0xb6, 0x00,
    0x67, 0x00, 0x07, 0x00, 0xb7, 0x27, 0x02, 0x40, 0x93, 0x87, 0x87, 0x02, 0x37, 0x07, 0x67, 0x45,
    0x13, 0x07, 0x37, 0x12, 0x23, 0xa0, 0xe7, 0x00, 0xb7, 0x27, 0x02, 0x40, 0x93, 0x87, 0x87, 0x02,
    0x37, 0x97, 0xef, 0xcd, 0x13, 0x07, 0xb7, 0x9a, 0x23, 0xa0, 0xe7, 0x00, 0xb7, 0x27, 0x02, 0x40,
    0x93, 0x87, 0xc7, 0x00, 0x23, 0xa0, 0x07, 0x00, 0xb7, 0x27, 0x02, 0x40, 0x93, 0x87, 0x07, 0x01,
    0x13, 0x07, 0x00, 0x08, 0x23, 0xa0, 0xe7, 0x00, 0xb7, 0xf7, 0x00, 0xe0, 0x93, 0x87, 0x07, 0xd1,
    0x37, 0x07, 0x00, 0x80, 0x23, 0xa0, 0xe7, 0x00,
];

#[derive(Debug, Error)]
pub enum HidBootError {
    #[error("HID initialization failed: {0}")]
    Init(String),
    #[error("HID bootloader {0:04x}:{1:04x} was not found")]
    NotFound(u16, u16),
    #[error("HID transfer failed: {0}")]
    Transfer(String),
    #[error("HID stub did not complete")]
    Timeout,
    #[error("image is {actual} bytes; CH32V003 code flash is {limit} bytes")]
    ImageTooLarge { actual: usize, limit: usize },
    #[error("flash is locked after the unlock sequence (CTLR={0:#010x})")]
    FlashLocked(u32),
    #[error("target is read-protected (OBR={0:#010x})")]
    ReadProtected(u32),
    #[error("flash operation failed (STATR={0:#010x})")]
    FlashStatus(u32),
    #[error("read-back mismatch at {0:#010x}")]
    Verify(u32),
    #[error("invalid HID scratchpad payload")]
    Payload,
}

#[derive(Debug, Clone, Copy)]
pub struct FlashReport {
    pub bytes: usize,
    pub sectors_written: usize,
    pub vid: u16,
    pub pid: u16,
}

pub struct HidBoot {
    device: HidBackend,
    vid: u16,
    pid: u16,
}

enum HidBackend {
    Device(HidDevice),
    Replay,
}

impl HidBoot {
    pub fn open(usb_id: Option<(u16, u16)>) -> Result<Self, HidBootError> {
        let ids: &[(u16, u16)] = match usb_id.as_ref() {
            Some(id) => std::slice::from_ref(id),
            None => &DEFAULT_HID_IDS,
        };
        if ch32rv_usb::replay::active() {
            let recorded = ch32rv_usb::replay::device()
                .ok_or_else(|| HidBootError::Init("replay has no device".to_owned()))?;
            if ids.contains(&(recorded.vid, recorded.pid)) {
                let boot = Self {
                    device: HidBackend::Replay,
                    vid: recorded.vid,
                    pid: recorded.pid,
                };
                boot.commit(&payload(HALT_WAIT, &[], &[])?, false)?;
                return Ok(boot);
            }
            let (vid, pid) = usb_id.unwrap_or(DEFAULT_HID_IDS[0]);
            return Err(HidBootError::NotFound(vid, pid));
        }

        let api = HidApi::new().map_err(|e| HidBootError::Init(e.to_string()))?;
        for &(vid, pid) in ids {
            if let Some(info) = api
                .device_list()
                .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            {
                ch32rv_usb::capture::record_hid_device(
                    vid,
                    pid,
                    info.serial_number(),
                    &info.path().to_string_lossy(),
                    info.product_string(),
                );
                let device = api
                    .open_path(info.path())
                    .map_err(|e| HidBootError::Transfer(e.to_string()))?;
                let boot = Self {
                    device: HidBackend::Device(device),
                    vid,
                    pid,
                };
                boot.commit(&payload(HALT_WAIT, &[], &[])?, false)?;
                return Ok(boot);
            }
        }
        let (vid, pid) = usb_id.unwrap_or(DEFAULT_HID_IDS[0]);
        Err(HidBootError::NotFound(vid, pid))
    }

    pub fn flash(&self, image: &[u8], run: bool) -> Result<FlashReport, HidBootError> {
        if image.len() > V003_FLASH_SIZE {
            return Err(HidBootError::ImageTooLarge {
                actual: image.len(),
                limit: V003_FLASH_SIZE,
            });
        }
        let padded_len = image.len().div_ceil(SECTOR_SIZE) * SECTOR_SIZE;
        let mut padded = vec![0xff; padded_len];
        padded[..image.len()].copy_from_slice(image);
        self.unlock_flash()?;

        let mut sectors_written = 0;
        for sector in 0..padded_len / SECTOR_SIZE {
            let start = sector * SECTOR_SIZE;
            let expected = &padded[start..start + SECTOR_SIZE];
            let addr = V003_FLASH_BASE + (sector * SECTOR_SIZE) as u32;
            if self.read64(addr)? != expected {
                self.flash64(addr, expected)?;
                if self.read64(addr)? != expected {
                    return Err(HidBootError::Verify(addr));
                }
                sectors_written += 1;
            }
        }
        if run {
            self.commit(&payload(RUN_APP, &[], &[])?, true)?;
        }
        Ok(FlashReport {
            bytes: image.len(),
            sectors_written,
            vid: self.vid,
            pid: self.pid,
        })
    }

    fn unlock_flash(&self) -> Result<(), HidBootError> {
        let mut ctlr = self.read_word(FLASH_CTLR)?;
        if ctlr & 0x8080 != 0 {
            for (addr, value) in [
                (FLASH_KEYR, 0x4567_0123),
                (FLASH_KEYR, 0xcdef_89ab),
                (FLASH_OBKEYR, 0x4567_0123),
                (FLASH_OBKEYR, 0xcdef_89ab),
                (FLASH_MODEKEYR, 0x4567_0123),
                (FLASH_MODEKEYR, 0xcdef_89ab),
            ] {
                self.write_word(addr, value, false)?;
            }
            ctlr = self.read_word(FLASH_CTLR)?;
            if ctlr & 0x8080 != 0 {
                return Err(HidBootError::FlashLocked(ctlr));
            }
        }
        let obr = self.read_word(FLASH_OBR)?;
        if obr & 2 != 0 {
            return Err(HidBootError::ReadProtected(obr));
        }
        Ok(())
    }

    fn flash64(&self, addr: u32, data: &[u8]) -> Result<(), HidBootError> {
        self.write_word(FLASH_CTLR, CTLR_PAGE_ER, false)?;
        self.write_word(FLASH_ADDR, addr, false)?;
        self.write_word(FLASH_CTLR, CTLR_PAGE_ER | CTLR_START, true)?;
        self.wait_flash()?;
        self.write_word(FLASH_CTLR, CTLR_PAGE_PG, false)?;
        self.write_word(FLASH_CTLR, CTLR_PAGE_PG | CTLR_BUF_RST, false)?;
        self.commit(&payload(WRITE64_FLASH, &[addr, FLASH_STATR], data)?, false)?;
        self.wait_flash()
    }

    fn wait_flash(&self) -> Result<(), HidBootError> {
        for _ in 0..1000 {
            let status = self.read_word(FLASH_STATR)?;
            if status & 3 == 0 {
                return if status & 0x10 == 0 {
                    Ok(())
                } else {
                    Err(HidBootError::FlashStatus(status))
                };
            }
        }
        Err(HidBootError::Timeout)
    }

    fn read_word(&self, addr: u32) -> Result<u32, HidBootError> {
        let response = self.commit(&payload(WORD_READ, &[addr, 4], &[])?, false)?;
        let bytes: [u8; 4] = response[60..64]
            .try_into()
            .map_err(|_| HidBootError::Payload)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_word(&self, addr: u32, value: u32, long: bool) -> Result<(), HidBootError> {
        self.commit(
            &payload(WORD_WRITE, &[addr, 4], &value.to_le_bytes())?,
            long,
        )?;
        Ok(())
    }

    fn read64(&self, addr: u32) -> Result<Vec<u8>, HidBootError> {
        let response = self.commit(&payload(WORD_READ, &[addr, 64], &[])?, false)?;
        Ok(response[60..124].to_vec())
    }

    fn commit(
        &self,
        report: &[u8; REPORT_SIZE],
        no_reply: bool,
    ) -> Result<[u8; REPORT_SIZE], HidBootError> {
        let mut last_error = None;
        for _ in 0..=10 {
            match self.send_feature_report(report) {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) => {
                    last_error = Some(e.to_string());
                    thread::sleep(Duration::from_millis(2));
                }
            }
        }
        if let Some(error) = last_error {
            return Err(HidBootError::Transfer(error));
        }
        if no_reply {
            return Ok([0; REPORT_SIZE]);
        }

        let mut response = [0; REPORT_SIZE];
        for _ in 0..=200 {
            response.fill(0);
            response[0] = REPORT_ID;
            match self.get_feature_report(&mut response) {
                Ok(n) if n > 1 && response[1] == 0xff => return Ok(response),
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        Err(HidBootError::Timeout)
    }

    fn send_feature_report(&self, report: &[u8]) -> Result<(), String> {
        match &self.device {
            HidBackend::Device(device) => {
                let result = device
                    .send_feature_report(report)
                    .map_err(|e| e.to_string());
                ch32rv_usb::capture::record_hid(false, report, result.is_ok());
                result
            }
            HidBackend::Replay => {
                if ch32rv_usb::replay::consume_hid_write(report) {
                    Ok(())
                } else {
                    Err("recorded HID feature-report write failed".to_owned())
                }
            }
        }
    }

    fn get_feature_report(&self, response: &mut [u8]) -> Result<usize, String> {
        match &self.device {
            HidBackend::Device(device) => match device.get_feature_report(response) {
                Ok(n) => {
                    ch32rv_usb::capture::record_hid(true, &response[..n], true);
                    Ok(n)
                }
                Err(e) => {
                    ch32rv_usb::capture::record_hid(true, &[], false);
                    Err(e.to_string())
                }
            },
            HidBackend::Replay => {
                let data = ch32rv_usb::replay::serve_hid_read()
                    .ok_or_else(|| "recorded HID feature-report read failed".to_owned())?;
                let n = data.len().min(response.len());
                response[..n].copy_from_slice(&data[..n]);
                Ok(n)
            }
        }
    }
}

fn payload(stub: &[u8], words: &[u32], data: &[u8]) -> Result<[u8; REPORT_SIZE], HidBootError> {
    let used = 4 + stub.len() + words.len() * 4 + data.len();
    if used > REPORT_SIZE - 4 {
        return Err(HidBootError::Payload);
    }
    let mut report = [0; REPORT_SIZE];
    report[..4].copy_from_slice(&[REPORT_ID, 0, 0, 0]);
    let mut cursor = 4;
    report[cursor..cursor + stub.len()].copy_from_slice(stub);
    cursor += stub.len();
    for word in words {
        report[cursor..cursor + 4].copy_from_slice(&word.to_le_bytes());
        cursor += 4;
    }
    report[cursor..cursor + data.len()].copy_from_slice(data);
    report[REPORT_SIZE - 4..].copy_from_slice(&0x1234_abcd_u32.to_le_bytes());
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn write64_exactly_fills_report() {
        let report = payload(WRITE64_FLASH, &[V003_FLASH_BASE, FLASH_STATR], &[0x5a; 64])
            .expect("valid write64 payload");
        assert_eq!(report[0], 0xaa);
        assert_eq!(&report[52..56], &V003_FLASH_BASE.to_le_bytes());
        assert_eq!(&report[56..60], &FLASH_STATR.to_le_bytes());
        assert_eq!(&report[60..124], &[0x5a; 64]);
        assert_eq!(&report[124..128], &[0xcd, 0xab, 0x34, 0x12]);
    }

    #[test]
    fn rejects_oversized_payload() {
        assert!(matches!(
            payload(&[0; 121], &[], &[]),
            Err(HidBootError::Payload)
        ));
    }
}
