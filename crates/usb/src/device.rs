//! en: Device enumeration and blocking bulk transfers. Enumeration is always nusb; a
//! transfer backend is chosen per device at open time: nusb (WinUSB on Windows) first,
//! and on Windows a fallback to WCH's stock vendor driver via `ch32rv-usb-wch-win` when
//! nusb cannot open the interface (docs/windows-wch-driver.ja.md §4). This module is the
//! only place that touches backend types; everything else sees [`UsbDeviceInfo`] /
//! [`UsbInterface`].
//!
//! ja: 列挙とブロッキング bulk 転送。列挙は常に nusb。転送 backend は open 時に device
//! 単位で選ぶ: まず nusb(Windows では WinUSB)、Windows で開けない場合のみ WCH 純正
//! ドライバ経路(`ch32rv-usb-wch-win`)へフォールバック(docs/windows-wch-driver.ja.md §4)。
//! backend の型に触るのはこの module だけで、外には [`UsbDeviceInfo`] / [`UsbInterface`]
//! しか見せない。

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

    /// en: Serial-port device nodes (CDC etc.) belonging to this USB device, e.g.
    /// "/dev/ttyACM5". Linux only for now (sysfs walk); other platforms return empty.
    /// ja: この USB device に属する serial port ノード(CDC 等)。当面 Linux のみ
    /// (sysfs 走査)。他 OS は空を返す(TODO M2: Windows COM / macOS cu.*)。
    pub fn serial_ports(&self) -> Vec<String> {
        #[cfg(target_os = "linux")]
        {
            let dev_path = match self.inner.sysfs_path().canonicalize() {
                Ok(p) => p,
                Err(_) => return Vec::new(),
            };
            let Ok(entries) = std::fs::read_dir("/sys/class/tty") else {
                return Vec::new();
            };
            let mut ports: Vec<String> = entries
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let real = e.path().canonicalize().ok()?;
                    real.starts_with(&dev_path).then(|| format!("/dev/{name}"))
                })
                .collect();
            ports.sort();
            ports
        }
        #[cfg(not(target_os = "linux"))]
        {
            Vec::new()
        }
    }

    /// en: Open the device and claim one interface with one bulk OUT/IN endpoint pair.
    /// nusb first; on Windows, when nusb cannot open (typically because WCH's stock
    /// driver owns the interface instead of WinUSB), fall back to the CH375 backend for
    /// the device with the same serial. The nusb error is kept when the fallback finds
    /// nothing, so non-driver failures stay diagnosable.
    /// ja: device を開き、interface 1 つと bulk OUT/IN endpoint の組を claim する。
    /// まず nusb。Windows で開けない場合(典型: WinUSB でなく WCH 純正ドライバが
    /// interface を所有)は、同一 serial の device に限り CH375 backend へフォールバック。
    /// フォールバック不成立時は nusb のエラーをそのまま返す。
    pub fn open_interface(
        &self,
        interface: u8,
        ep_out: u8,
        ep_in: u8,
    ) -> Result<UsbInterface, UsbError> {
        match self.open_nusb(interface, ep_out, ep_in) {
            Ok(backend) => Ok(UsbInterface {
                backend: Backend::Nusb(Box::new(backend)),
            }),
            #[cfg(windows)]
            Err(primary) => match self.open_ch375(interface, ep_out, ep_in) {
                Some(backend) => Ok(UsbInterface {
                    backend: Backend::Ch375(backend),
                }),
                None => Err(primary),
            },
            #[cfg(not(windows))]
            Err(e) => Err(e),
        }
    }

    fn open_nusb(&self, interface: u8, ep_out: u8, ep_in: u8) -> Result<NusbBackend, UsbError> {
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
        Ok(NusbBackend {
            iface,
            out,
            inp,
            data_out: None,
            data_in: None,
        })
    }

    /// en: Find this device on the CH375 (WCH stock driver) side by serial and open it.
    /// `None` on any miss or failure — the caller reports the primary nusb error instead.
    /// We deliberately fall back on every nusb open failure rather than matching the
    /// "incompatible driver" message text, which is not a stable API.
    /// The CH375 interface belongs to the vendor function (MI_00), so only interface 0
    /// is eligible; the endpoint pair is passed per transfer, not claimed.
    /// ja: CH375(WCH 純正ドライバ)側で同一 serial の device を探して開く。見つからない/
    /// 失敗時は `None`(呼び出し側が nusb の一次エラーを報告する)。nusb の
    /// 「incompatible driver」メッセージ文字列は安定 API ではないため、open 失敗全般で
    /// フォールバックを試す設計。CH375 interface は vendor function(MI_00)のものなので
    /// interface 0 のみ対象。endpoint は claim 不要で転送ごとに指定する。
    #[cfg(windows)]
    fn open_ch375(&self, interface: u8, ep_out: u8, ep_in: u8) -> Option<Ch375Backend> {
        use ch32rv_usb_wch_win::{GUID_CH375, list_interfaces};

        if interface != 0 {
            return None;
        }
        let serial = self.serial()?;
        for candidate in list_interfaces(&GUID_CH375).ok()? {
            let parent = match candidate.parent_instance_id() {
                Ok(p) => p,
                Err(_) => continue,
            };
            // Parent instance ID: `USB\VID_xxxx&PID_xxxx\<serial>`.
            let candidate_serial = parent.rsplit('\\').next().unwrap_or("");
            if candidate_serial.eq_ignore_ascii_case(serial) {
                let dev = candidate.open().ok()?;
                return Some(Ch375Backend {
                    dev,
                    ep_out,
                    ep_in,
                    data_eps: None,
                });
            }
        }
        None
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

/// en: An opened device with one bulk OUT/IN endpoint pair and blocking transfers,
/// backed by nusb or (Windows fallback) the WCH stock driver. On the nusb path a timeout
/// cancels and drains the pending transfer so the endpoint stays clean (same discipline
/// as probe-rs's nusb wrapper). The CH375 path has no timeout control (the driver
/// exposes none that is capture-verified) — transfers block, which is safe under the
/// request/reply pattern all callers use.
/// ja: open 済み device と bulk OUT/IN endpoint の組。backend は nusb または
/// (Windows フォールバックの)WCH 純正ドライバ。nusb 経路の timeout は転送を cancel
/// して排出し、endpoint に未完了転送を残さない。CH375 経路は timeout 制御なし
/// (検証済みの手段が無い)でブロックするが、全呼び出し元が request/reply パターンの
/// ため安全。
pub struct UsbInterface {
    backend: Backend,
}

enum Backend {
    // en: boxed — the nusb endpoints make this variant much larger than Ch375
    Nusb(Box<NusbBackend>),
    #[cfg(windows)]
    Ch375(Ch375Backend),
}

struct NusbBackend {
    iface: nusb::Interface,
    out: nusb::Endpoint<Bulk, Out>,
    inp: nusb::Endpoint<Bulk, In>,
    data_out: Option<nusb::Endpoint<Bulk, Out>>,
    data_in: Option<nusb::Endpoint<Bulk, In>>,
}

#[cfg(windows)]
struct Ch375Backend {
    dev: ch32rv_usb_wch_win::Ch375Device,
    /// Command endpoint pair from [`UsbDeviceInfo::open_interface`].
    ep_out: u8,
    ep_in: u8,
    /// Data endpoint pair once opened: `(ep_out, ep_in)`. No claim needed on this path.
    data_eps: Option<(u8, u8)>,
}

#[cfg(windows)]
fn ch375_err(e: ch32rv_usb_wch_win::Ch375Error) -> UsbError {
    UsbError::Transfer(e.to_string())
}

impl UsbInterface {
    pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<usize, UsbError> {
        let r = match &mut self.backend {
            Backend::Nusb(b) => write_ep(&mut b.out, data, timeout),
            #[cfg(windows)]
            Backend::Ch375(b) => b
                .dev
                .write_pipe(b.ep_out, data)
                .map(|()| data.len())
                .map_err(ch375_err),
        };
        crate::capture::record(
            crate::capture::Chan::Cmd,
            crate::capture::Dir::Out,
            data,
            r.is_ok(),
        );
        r
    }

    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, UsbError> {
        let r = match &mut self.backend {
            Backend::Nusb(b) => read_ep(&mut b.inp, buf, timeout),
            #[cfg(windows)]
            Backend::Ch375(b) => b.dev.read_pipe(b.ep_in, buf).map_err(ch375_err),
        };
        let n = *r.as_ref().unwrap_or(&0);
        crate::capture::record(
            crate::capture::Chan::Cmd,
            crate::capture::Dir::In,
            &buf[..n],
            r.is_ok(),
        );
        r
    }

    /// en: Open a second bulk endpoint pair (the WCH-Link flash data path, EP 0x02/0x82) on
    /// the same claimed interface. Idempotent.
    /// ja: 同じ interface 上に 2 つ目の bulk endpoint 組(WCH-Link の flash data 経路、
    /// EP 0x02/0x82)を開く。冪等。
    pub fn open_data_endpoints(&mut self, ep_out: u8, ep_in: u8) -> Result<(), UsbError> {
        match &mut self.backend {
            Backend::Nusb(b) => {
                if b.data_out.is_none() {
                    b.data_out = Some(
                        b.iface
                            .endpoint::<Bulk, Out>(ep_out)
                            .map_err(|_| UsbError::Endpoint(ep_out))?,
                    );
                }
                if b.data_in.is_none() {
                    b.data_in = Some(
                        b.iface
                            .endpoint::<Bulk, In>(ep_in)
                            .map_err(|_| UsbError::Endpoint(ep_in))?,
                    );
                }
            }
            #[cfg(windows)]
            Backend::Ch375(b) => {
                if b.data_eps.is_none() {
                    b.data_eps = Some((ep_out, ep_in));
                }
            }
        }
        Ok(())
    }

    /// Write to the data endpoint. Call [`open_data_endpoints`] first.
    pub fn write_data(&mut self, data: &[u8], timeout: Duration) -> Result<usize, UsbError> {
        let r = match &mut self.backend {
            Backend::Nusb(b) => {
                let ep = b.data_out.as_mut().ok_or(UsbError::Endpoint(0x02))?;
                write_ep(ep, data, timeout)
            }
            #[cfg(windows)]
            Backend::Ch375(b) => {
                let (ep_out, _) = b.data_eps.ok_or(UsbError::Endpoint(0x02))?;
                b.dev
                    .write_pipe(ep_out, data)
                    .map(|()| data.len())
                    .map_err(ch375_err)
            }
        };
        crate::capture::record(
            crate::capture::Chan::Data,
            crate::capture::Dir::Out,
            data,
            r.is_ok(),
        );
        r
    }

    /// Read from the data endpoint. Call [`open_data_endpoints`] first.
    pub fn read_data(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, UsbError> {
        let r = match &mut self.backend {
            Backend::Nusb(b) => {
                let ep = b.data_in.as_mut().ok_or(UsbError::Endpoint(0x82))?;
                read_ep(ep, buf, timeout)
            }
            #[cfg(windows)]
            Backend::Ch375(b) => {
                let (_, ep_in) = b.data_eps.ok_or(UsbError::Endpoint(0x82))?;
                b.dev.read_pipe(ep_in, buf).map_err(ch375_err)
            }
        };
        let n = *r.as_ref().unwrap_or(&0);
        crate::capture::record(
            crate::capture::Chan::Data,
            crate::capture::Dir::In,
            &buf[..n],
            r.is_ok(),
        );
        r
    }
}

fn write_ep(
    ep: &mut nusb::Endpoint<Bulk, Out>,
    data: &[u8],
    timeout: Duration,
) -> Result<usize, UsbError> {
    let mut buf = Buffer::new(data.len());
    buf.extend_from_slice(data);
    ep.submit(buf);
    let Some(completion) = ep.wait_next_complete(timeout) else {
        ep.cancel_all();
        let _ = ep.wait_next_complete(Duration::from_millis(100));
        return Err(UsbError::Timeout);
    };
    completion
        .status
        .map_err(|e| UsbError::Transfer(e.to_string()))?;
    Ok(completion.actual_len)
}

fn read_ep(
    ep: &mut nusb::Endpoint<Bulk, In>,
    buf: &mut [u8],
    timeout: Duration,
) -> Result<usize, UsbError> {
    let max_packet = ep.max_packet_size().max(1);
    let requested = buf.len().div_ceil(max_packet) * max_packet;
    ep.submit(Buffer::new(requested));
    let Some(completion) = ep.wait_next_complete(timeout) else {
        ep.cancel_all();
        let _ = ep.wait_next_complete(Duration::from_millis(100));
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
