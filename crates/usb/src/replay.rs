//! en: Offline replay of a captured USB session (the counterpart of [`crate::capture`]). With
//! `--replay <file>`, the CLI runs against the transfers recorded in a capture NDJSON instead of
//! real hardware: `enumerate()` returns the recorded device, and every bulk read/write is served
//! from / matched against the recorded log. This lets the protocol layers (wchlink / dmi) be
//! exercised, regression-tested, and bug-reproduced with no probe attached - e.g. a capture of a
//! failing operation sent from another machine can be analysed and re-run here.
//!
//! Replay serves each channel (cmd / data) and direction (in / out) as an independent FIFO in the
//! order it was recorded, so it is faithful for the deterministic request/reply exchanges the
//! callers use. A write whose bytes differ from the recorded out-transfer is counted as a
//! divergence (the code produced a different protocol than the capture) but does not abort; a read
//! past the end of a channel's queue yields an empty transfer.
//!
//! ja: capture した USB セッションの offline 再生([`crate::capture`] の対。`--replay <file>` で、
//! CLI は実機でなく capture NDJSON に記録された転送に対して動く。`enumerate()` は記録された device
//! を返し、bulk read/write は記録ログから供給/照合される。probe 無しで protocol 層(wchlink/dmi)を
//! 動かし、回帰テスト・バグ再現ができる。各 channel×方向を記録順の独立 FIFO として供給する。

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use crate::capture::Chan;

/// The identity of a recorded device, enough to reconstruct one [`crate::UsbDeviceInfo`].
#[derive(Clone, Debug)]
pub struct ReplayDevice {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub topology: String,
    pub product: Option<String>,
    pub ports: Vec<String>,
}

struct ReplayLog {
    device: ReplayDevice,
    cmd_in: VecDeque<Vec<u8>>,
    cmd_out: VecDeque<Vec<u8>>,
    data_in: VecDeque<Vec<u8>>,
    data_out: VecDeque<Vec<u8>>,
    /// Count of writes whose bytes did not match the recorded out-transfer (protocol divergence).
    divergences: u32,
    /// Count of reads served past the end of a channel's queue (log exhausted).
    underruns: u32,
}

static LOG: OnceLock<Mutex<ReplayLog>> = OnceLock::new();

/// Whether replay mode is active (a fixture was loaded).
pub fn active() -> bool {
    LOG.get().is_some()
}

/// The recorded device, when replay is active (for `enumerate()`).
pub fn device() -> Option<ReplayDevice> {
    let cell = LOG.get()?;
    let guard = cell.lock().ok()?;
    Some(guard.device.clone())
}

/// en: Load a capture NDJSON fixture and enter replay mode. Call once from `main`, before any
/// device I/O. Errors if the file is missing/unreadable or carries no `_device` line.
/// ja: capture NDJSON fixture を読み込み replay モードに入る。`main` から device I/O 前に一度呼ぶ。
pub fn start(path: &Path) -> io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut device: Option<ReplayDevice> = None;
    let (mut cmd_in, mut cmd_out) = (VecDeque::new(), VecDeque::new());
    let (mut data_in, mut data_out) = (VecDeque::new(), VecDeque::new());

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // tolerate malformed lines
        };
        if let Some(d) = v.get("_device") {
            if device.is_none() {
                device = parse_device(d);
            }
            continue;
        }
        // A transfer line: chan / dir / data(hex).
        let (Some(chan), Some(dir), Some(hex)) = (
            v.get("chan").and_then(|c| c.as_str()),
            v.get("dir").and_then(|d| d.as_str()),
            v.get("data").and_then(|d| d.as_str()),
        ) else {
            continue;
        };
        let bytes = decode_hex(hex);
        match (chan, dir) {
            ("cmd", "in") => cmd_in.push_back(bytes),
            ("cmd", "out") => cmd_out.push_back(bytes),
            ("data", "in") => data_in.push_back(bytes),
            ("data", "out") => data_out.push_back(bytes),
            _ => {}
        }
    }

    let device = device.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "capture has no `_device` line (recorded with an older ch32rv); cannot replay",
        )
    })?;

    let _ = LOG.set(Mutex::new(ReplayLog {
        device,
        cmd_in,
        cmd_out,
        data_in,
        data_out,
        divergences: 0,
        underruns: 0,
    }));
    Ok(())
}

/// A short human summary of how the replay went (divergences / underruns), for the caller to warn.
pub fn summary() -> Option<(u32, u32)> {
    let cell = LOG.get()?;
    let guard = cell.lock().ok()?;
    Some((guard.divergences, guard.underruns))
}

/// Serve the next recorded IN transfer for `chan` (empty vec when the queue is exhausted).
pub(crate) fn serve_read(chan: Chan) -> Vec<u8> {
    let Some(cell) = LOG.get() else {
        return Vec::new();
    };
    let Ok(mut guard) = cell.lock() else {
        return Vec::new();
    };
    let q = match chan {
        Chan::Cmd => &mut guard.cmd_in,
        Chan::Data => &mut guard.data_in,
    };
    match q.pop_front() {
        Some(bytes) => bytes,
        None => {
            guard.underruns += 1;
            Vec::new()
        }
    }
}

/// Consume the next recorded OUT transfer for `chan`, counting a divergence if `data` differs.
pub(crate) fn consume_write(chan: Chan, data: &[u8]) {
    let Some(cell) = LOG.get() else {
        return;
    };
    let Ok(mut guard) = cell.lock() else {
        return;
    };
    let q = match chan {
        Chan::Cmd => &mut guard.cmd_out,
        Chan::Data => &mut guard.data_out,
    };
    match q.pop_front() {
        Some(recorded) if recorded == data => {}
        Some(_) => guard.divergences += 1,
        None => guard.underruns += 1,
    }
}

fn parse_device(d: &serde_json::Value) -> Option<ReplayDevice> {
    Some(ReplayDevice {
        vid: d.get("vid")?.as_u64()? as u16,
        pid: d.get("pid")?.as_u64()? as u16,
        serial: d.get("serial").and_then(|s| s.as_str()).map(str::to_owned),
        topology: d.get("topology").and_then(|s| s.as_str())?.to_owned(),
        product: d.get("product").and_then(|s| s.as_str()).map(str::to_owned),
        ports: d
            .get("ports")
            .and_then(|p| p.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn decode_hex(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 2 <= bytes.len() {
        let hi = (bytes[i] as char).to_digit(16);
        let lo = (bytes[i + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            _ => break,
        }
        i += 2;
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn decode_hex_pairs() {
        assert_eq!(decode_hex("810d01ff"), vec![0x81, 0x0d, 0x01, 0xff]);
        assert_eq!(decode_hex(""), Vec::<u8>::new());
        assert_eq!(decode_hex("00"), vec![0x00]);
    }

    #[test]
    fn parse_device_line() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"vid":6790,"pid":32784,"serial":"ABC","topology":"1-2","product":"WCH-Link","ports":["/dev/ttyACM0"]}"#,
        )
        .unwrap();
        let d = parse_device(&v).unwrap();
        assert_eq!(d.vid, 6790);
        assert_eq!(d.pid, 32784);
        assert_eq!(d.serial.as_deref(), Some("ABC"));
        assert_eq!(d.topology, "1-2");
        assert_eq!(d.ports, vec!["/dev/ttyACM0"]);
    }
}
