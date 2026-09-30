//! en: A byte stream over a vendor-class bulk pair (the OEP vendor bulk transport, oep-core §3.1 /
//! §3.3). IN is drained by a thread of its own, always, whatever the reader is doing: a probe's
//! send FIFO that nobody empties stops it taking OUT too (a P4 over usbip with a 4 KiB vendor FIFO
//! and a pipelined host deadlocked that way, E160), and a read never cancels a transfer (which
//! would drop what arrived with it). Writes whose length is a multiple of wMaxPacketSize are
//! followed by a zero-length transfer.
//! ja: vendor class の bulk の組の上のバイト列(OEP の vendor bulk の経路)。IN は専用の thread が、読み手が
//! 何をしていても常に汲む(誰も空けない probe の送信 FIFO は OUT も止める。usbip 越しの P4 と pipeline の
//! host がそれで詰まった、E160)。read は転送を cancel しない。長さが wMaxPacketSize の倍数の書き込みには
//! 長さ 0 の転送を続ける。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};

use crate::device::{UsbDeviceInfo, UsbError, classify_open_error};

/// en: `CH32RV_USB_TRACE=<file>`: one line per OUT write and IN completion (time, length, first
/// bytes), for finding where a probe stopped answering. Off by default.
/// ja: `CH32RV_USB_TRACE=<file>` で、OUT の書き込みと IN の完了ごとに 1 行(時刻、長さ、先頭)。既定は無し。
fn trace(dir: &str, data: &[u8]) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("CH32RV_USB_TRACE") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let head: String = data.iter().take(24).map(|b| format!("{b:02x}")).collect();
        let _ = writeln!(
            f,
            "{t:.4} {} {dir} {:5} {head}",
            std::process::id(),
            data.len()
        );
    }
}

/// How many IN transfers stay submitted.
const IN_FLIGHT: usize = 4;
/// Packets per IN transfer.
const IN_PACKETS: usize = 16;

/// Where a vendor bulk pair is: interface, OUT and IN endpoint addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorBulkPlace {
    pub interface: u8,
    pub ep_out: u8,
    pub ep_in: u8,
}

/// A claimed vendor bulk pair.
pub struct BulkPipe {
    _iface: nusb::Interface,
    out: nusb::Endpoint<Bulk, Out>,
    /// What the IN thread drained, in order; an error ends the stream.
    from_in: mpsc::Receiver<Result<Vec<u8>, UsbError>>,
    stop: Arc<AtomicBool>,
    rx: VecDeque<u8>,
    place: VendorBulkPlace,
}

impl Drop for BulkPipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// en: The IN side: keep [`IN_FLIGHT`] transfers submitted and hand every completion over, until
/// told to stop or the reader is gone.
/// ja: IN 側。[`IN_FLIGHT`] 本の転送を出したままにし、完了したものを渡し続ける(止めるか読み手が消えるまで)。
fn drain_in(
    mut inp: nusb::Endpoint<Bulk, In>,
    tx: mpsc::Sender<Result<Vec<u8>, UsbError>>,
    stop: Arc<AtomicBool>,
) {
    let size = inp.max_packet_size().max(1) * IN_PACKETS;
    while !stop.load(Ordering::Relaxed) {
        while inp.pending() < IN_FLIGHT {
            inp.submit(Buffer::new(size));
        }
        let Some(c) = inp.wait_next_complete(Duration::from_millis(100)) else {
            continue;
        };
        let item = match c.status {
            Ok(()) => {
                trace("in ", &c.buffer[..c.actual_len]);
                Ok(c.buffer[..c.actual_len].to_vec())
            }
            Err(e) => {
                trace("in!", e.to_string().as_bytes());
                Err(UsbError::Transfer(e.to_string()))
            }
        };
        let failed = item.is_err();
        if (item.as_ref().is_ok_and(|d| d.is_empty()) || tx.send(item).is_ok()) && !failed {
            continue;
        }
        break;
    }
    inp.cancel_all();
    while inp.pending() > 0 && inp.wait_next_complete(Duration::from_millis(100)).is_some() {}
}

impl UsbDeviceInfo {
    /// en: Open the first vendor-class interface (alt 0) that has a bulk OUT and a bulk IN
    /// endpoint. `Ok(None)` when the device has none.
    /// ja: bulk の OUT と IN を持つ最初の vendor class の interface(alt 0)を開く。無ければ `Ok(None)`。
    pub fn open_vendor_bulk(&self) -> Result<Option<BulkPipe>, UsbError> {
        let Some(info) = self.nusb_info() else {
            return Ok(None);
        };
        let device = info.open().wait().map_err(classify_open_error)?;
        let place = {
            let config = device
                .active_configuration()
                .map_err(|e| UsbError::Open(e.to_string()))?;
            config
                .interface_alt_settings()
                .filter(|i| i.alternate_setting() == 0 && i.class() == 0xFF)
                .find_map(|i| {
                    let bulk = |d: Direction| {
                        i.endpoints()
                            .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == d)
                            .map(|e| e.address())
                    };
                    Some(VendorBulkPlace {
                        interface: i.interface_number(),
                        ep_out: bulk(Direction::Out)?,
                        ep_in: bulk(Direction::In)?,
                    })
                })
        };
        let Some(place) = place else {
            return Ok(None);
        };
        let iface = device
            .claim_interface(place.interface)
            .wait()
            .map_err(classify_open_error)?;
        let out = iface
            .endpoint::<Bulk, Out>(place.ep_out)
            .map_err(|_| UsbError::Endpoint(place.ep_out))?;
        let inp = iface
            .endpoint::<Bulk, In>(place.ep_in)
            .map_err(|_| UsbError::Endpoint(place.ep_in))?;
        let (tx, from_in) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || drain_in(inp, tx, s));
        Ok(Some(BulkPipe {
            _iface: iface,
            out,
            from_in,
            stop,
            rx: VecDeque::new(),
            place,
        }))
    }
}

impl BulkPipe {
    pub fn place(&self) -> VendorBulkPlace {
        self.place
    }

    /// en: Send `data` as one transfer (plus a zero-length one when it ends on a packet boundary).
    /// ja: `data` を 1 回の転送で送る(packet の境目で終われば長さ 0 の転送を続ける)。
    pub fn write(&mut self, data: &[u8], timeout: Duration) -> Result<(), UsbError> {
        let mps = self.out.max_packet_size().max(1);
        self.send(data, timeout)?;
        if !data.is_empty() && data.len().is_multiple_of(mps) {
            self.send(&[], timeout)?;
        }
        Ok(())
    }

    fn send(&mut self, data: &[u8], timeout: Duration) -> Result<(), UsbError> {
        trace("out", data);
        let mut buf = Buffer::new(data.len());
        buf.extend_from_slice(data);
        self.out.submit(buf);
        let Some(c) = self.out.wait_next_complete(timeout) else {
            self.out.cancel_all();
            let _ = self.out.wait_next_complete(Duration::from_millis(100));
            return Err(UsbError::Timeout);
        };
        c.status.map_err(|e| UsbError::Transfer(e.to_string()))?;
        if c.actual_len != data.len() {
            return Err(UsbError::Transfer(format!(
                "sent {} of {} bytes",
                c.actual_len,
                data.len()
            )));
        }
        Ok(())
    }

    /// en: Up to `buf.len()` bytes; 0 when nothing arrived within `timeout`. Nothing is cancelled.
    /// ja: `buf.len()` まで読む。`timeout` の間に何も来なければ 0。何も cancel しない。
    pub fn read(&mut self, buf: &mut [u8], timeout: Duration) -> Result<usize, UsbError> {
        if self.rx.is_empty() {
            match self.from_in.recv_timeout(timeout) {
                Ok(d) => self.rx.extend(d?),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(0),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(UsbError::Transfer("the IN endpoint stopped".into()));
                }
            }
        }
        // Take whatever else the thread has already drained.
        while let Ok(d) = self.from_in.try_recv() {
            self.rx.extend(d?);
        }
        let n = buf.len().min(self.rx.len());
        for (d, s) in buf.iter_mut().zip(self.rx.drain(..n)) {
            *d = s;
        }
        Ok(n)
    }
}
