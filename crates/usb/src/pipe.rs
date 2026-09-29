//! en: A byte stream over a vendor-class bulk pair (the OEP vendor bulk transport, oep-core §3.1 /
//! §3.3). Unlike [`crate::UsbInterface`], reads never cancel: IN transfers stay submitted between
//! reads, so bytes that arrive while nobody waits are kept, not dropped with a cancelled transfer.
//! Writes whose length is a multiple of wMaxPacketSize are followed by a zero-length transfer.
//! ja: vendor class の bulk の組の上のバイト列(OEP の vendor bulk の経路)。[`crate::UsbInterface`] と
//! 違い、read は cancel しない。IN の転送を出したままにするので、待っていない間に届いた分も落とさない。
//! 長さが wMaxPacketSize の倍数の書き込みには長さ 0 の転送を続ける。

use std::collections::VecDeque;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};

use crate::device::{UsbDeviceInfo, UsbError, classify_open_error};

/// How many IN transfers stay submitted.
const IN_FLIGHT: usize = 2;
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
    inp: nusb::Endpoint<Bulk, In>,
    rx: VecDeque<u8>,
    place: VendorBulkPlace,
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
        Ok(Some(BulkPipe {
            _iface: iface,
            out,
            inp,
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
        let size = self.inp.max_packet_size().max(1) * IN_PACKETS;
        while self.inp.pending() < IN_FLIGHT {
            self.inp.submit(Buffer::new(size));
        }
        if self.rx.is_empty() {
            let Some(c) = self.inp.wait_next_complete(timeout) else {
                return Ok(0);
            };
            c.status.map_err(|e| UsbError::Transfer(e.to_string()))?;
            self.rx.extend(&c.buffer[..c.actual_len]);
            self.inp.submit(Buffer::new(size));
        }
        let n = buf.len().min(self.rx.len());
        for (d, s) in buf.iter_mut().zip(self.rx.drain(..n)) {
            *d = s;
        }
        Ok(n)
    }
}
