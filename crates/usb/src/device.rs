//! en: Device enumeration and blocking bulk transfers over nusb. This module is the only
//! place that touches nusb types; everything else sees [`UsbDeviceInfo`] / [`UsbInterface`].
//!
//! ja: nusb による列挙とブロッキング bulk 転送。nusb の型に触るのはこの module だけで、
//! 外には [`UsbDeviceInfo`] / [`UsbInterface`] しか見せない。

use std::io;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, In, Out};
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum UsbError {
    #[error("failed to enumerate USB devices: {0}")]
    Enumerate(String),
    #[error("access denied opening device (permissions / driver binding): {0}")]
    AccessDenied(String),
    #[error("device busy (already claimed by another process): {0}")]
    Busy(String),
    #[error("failed to open device: {0}")]
    Open(String),
    #[error("bulk endpoint {0:#04x} not available")]
    Endpoint(u8),
    #[error("transfer timed out")]
    Timeout,
    #[error("transfer error: {0}")]
    Transfer(String),
}

fn classify_open_error(e: impl Into<io::Error>) -> UsbError {
    let e: io::Error = e.into();
    match e.kind() {
        io::ErrorKind::PermissionDenied => UsbError::AccessDenied(e.to_string()),
        io::ErrorKind::ResourceBusy => UsbError::Busy(e.to_string()),
        _ => UsbError::Open(e.to_string()),
    }
}

/// en: Information about one enumerated USB device. Wraps `nusb::DeviceInfo` without
/// exposing it.
/// ja: 列挙された USB device 1 つ分の情報。`nusb::DeviceInfo` を隠蔽して包む。
pub struct UsbDeviceInfo {
    inner: nusb::DeviceInfo,
}

impl std::fmt::Debug for UsbDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsbDeviceInfo")
            .field("vid", &format_args!("{:04x}", self.vid()))
            .field("pid", &format_args!("{:04x}", self.pid()))
            .field("serial", &self.serial())
            .field("topology", &self.topology())
            .finish()
    }
}

impl UsbDeviceInfo {
    pub fn vid(&self) -> u16 {
        self.inner.vendor_id()
    }

    pub fn pid(&self) -> u16 {
        self.inner.product_id()
    }

    pub fn serial(&self) -> Option<&str> {
        self.inner.serial_number()
    }

    pub fn product(&self) -> Option<&str> {
        self.inner.product_string()
    }

    pub fn manufacturer(&self) -> Option<&str> {
        self.inner.manufacturer_string()
    }

    /// "VID:PID" in lowercase hex.
    pub fn usb_id(&self) -> String {
        format!("{:04x}:{:04x}", self.vid(), self.pid())
    }

    /// en: Stable physical location: `<bus>-<port.port...>` (the `usb:` selector value).
    /// Falls back to the device address when the port chain is unknown.
    /// ja: 物理位置: `<bus>-<port.port...>`(`usb:` selector の値)。port chain が
    /// 取れない環境では device address で代替する。
    pub fn topology(&self) -> String {
        let bus = self.inner.bus_id().trim_start_matches('0');
        let bus = if bus.is_empty() { "0" } else { bus };
        let chain = self.inner.port_chain();
        if chain.is_empty() {
            format!("{bus}-addr{}", self.inner.device_address())
        } else {
            let ports: Vec<String> = chain.iter().map(u8::to_string).collect();
            format!("{bus}-{}", ports.join("."))
        }
    }

    /// en: Open the device and claim one interface with one bulk OUT/IN endpoint pair.
    /// ja: device を開き、interface 1 つと bulk OUT/IN endpoint の組を claim する。
    pub fn open_interface(
        &self,
        interface: u8,
        ep_out: u8,
        ep_in: u8,
    ) -> Result<UsbInterface, UsbError> {
        let device = self.inner.open().wait().map_err(classify_open_error)?;
        let iface = device
            .claim_interface(interface)
            .wait()
            .map_err(classify_open_error)?;
        let out = iface
            .endpoint::<Bulk, Out>(ep_out)
            .map_err(|_| UsbError::Endpoint(ep_out))?;
        let inp = iface
            .endpoint::<Bulk, In>(ep_in)
            .map_err(|_| UsbError::Endpoint(ep_in))?;
        Ok(UsbInterface {
            _iface: iface,
            out,
            inp,
        })
    }
}

/// en: Enumerate all USB devices. Callers filter by VID/PID.
/// ja: 全 USB device を列挙する。VID/PID での絞り込みは呼び出し側で行う。
pub fn enumerate() -> Result<Vec<UsbDeviceInfo>, UsbError> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|e| UsbError::Enumerate(io::Error::from(e).to_string()))?;
    Ok(devices.map(|inner| UsbDeviceInfo { inner }).collect())
}

/// en: A claimed interface with one bulk OUT/IN endpoint pair and blocking transfers.
/// On timeout the pending transfer is cancelled and drained so the endpoint stays clean
/// (same discipline as probe-rs's nusb wrapper).
/// ja: claim 済み interface と bulk OUT/IN endpoint の組。timeout 時は転送を cancel して
/// 排出し、endpoint に未完了転送を残さない。
pub struct UsbInterface {
    _iface: nusb::Interface,
    out: nusb::Endpoint<Bulk, Out>,
    inp: nusb::Endpoint<Bulk, In>,
}

impl UsbInterface {
    pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, UsbError> {
        let mut buf = Buffer::new(data.len());
        buf.extend_from_slice(data);
        self.out.submit(buf);
        let Some(completion) = self.out.wait_next_complete(timeout) else {
            self.out.cancel_all();
            let _ = self.out.wait_next_complete(Duration::from_millis(100));
            return Err(UsbError::Timeout);
        };
        completion
            .status
            .map_err(|e| UsbError::Transfer(e.to_string()))?;
        Ok(completion.actual_len)
    }

    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, UsbError> {
        let max_packet = self.inp.max_packet_size().max(1);
        let requested = buf.len().div_ceil(max_packet) * max_packet;
        self.inp.submit(Buffer::new(requested));
        let Some(completion) = self.inp.wait_next_complete(timeout) else {
            self.inp.cancel_all();
            let _ = self.inp.wait_next_complete(Duration::from_millis(100));
            return Err(UsbError::Timeout);
        };
        completion
            .status
            .map_err(|e| UsbError::Transfer(e.to_string()))?;
        let n = completion.actual_len;
        if n > buf.len() {
            return Err(UsbError::Transfer(format!(
                "device returned {n} bytes, buffer is {}",
                buf.len()
            )));
        }
        buf[..n].copy_from_slice(&completion.buffer[..n]);
        Ok(n)
    }
}
