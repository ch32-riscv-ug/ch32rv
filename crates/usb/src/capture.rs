//! en: USB transaction capture (docs/cli.ja.md §3.7, ArduinoCore-CH32 request A-3). When the CLI is
//! run with `--capture <file>`, every bulk transfer on the probe (the WCH-Link command channel
//! 0x01/0x81 and data channel 0x02/0x82) is appended to the file as one NDJSON line, so a
//! protocol problem hit on the bench can be reported as a replay fixture instead of "reproduce it
//! on real hardware first". The sink is a process-global set once from `main`; `record` is a no-op
//! until then, so the transfer paths pay nothing when capture is off.
//! ja: USB transaction capture(cli.ja.md §3.7、ArduinoCore-CH32 依頼 A-3)。`--capture <file>` 時に
//! probe の全 bulk 転送(WCH-Link の command 0x01/0x81・data 0x02/0x82)を NDJSON 1 行ずつ追記する。
//! ベンチで踏んだ protocol 問題を「実機で再現待ち」でなく replay fixture として報告できる。sink は
//! `main` が一度だけ設定するプロセスグローバルで、未設定なら `record` は no-op(off 時のコストなし)。

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The transfer channel: the WCH-Link command endpoints vs the flash-data endpoints.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Chan {
    /// Command channel (EP 0x01 out / 0x81 in).
    Cmd,
    /// Flash data channel (EP 0x02 out / 0x82 in).
    Data,
}

impl Chan {
    fn as_str(self) -> &'static str {
        match self {
            Chan::Cmd => "cmd",
            Chan::Data => "data",
        }
    }
}

/// The transfer direction (host's point of view).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Dir {
    /// Host -> probe.
    Out,
    /// Probe -> host.
    In,
}

impl Dir {
    fn as_str(self) -> &'static str {
        match self {
            Dir::Out => "out",
            Dir::In => "in",
        }
    }
}

struct Sink {
    writer: BufWriter<File>,
    start: Instant,
    seq: u64,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// en: Begin capturing to `path`, overwriting it. Writes a `_meta` header line. Idempotent-ish: a
/// second call is ignored (the first sink stays). Call once from `main` before any device I/O.
/// ja: `path` へ capture 開始(上書き)。`_meta` ヘッダ行を書く。`main` から device I/O 前に一度呼ぶ。
pub fn start(path: &Path) -> std::io::Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    writeln!(writer, r#"{{"_meta":{{"format":1,"unit":"us"}}}}"#)?;
    writer.flush()?;
    let _ = SINK.set(Mutex::new(Sink {
        writer,
        start: Instant::now(),
        seq: 0,
    }));
    Ok(())
}

/// en: Record one bulk transfer. No-op when capture was not started. `data` is the bytes actually
/// transferred (the slice written, or the received prefix); `ok` is whether the transfer succeeded.
/// ja: bulk 転送を 1 件記録。capture 未開始なら no-op。`data` は実際に転送されたバイト、`ok` は成否。
pub(crate) fn record(ep: u8, chan: Chan, dir: Dir, data: &[u8], ok: bool) {
    let Some(cell) = SINK.get() else {
        return;
    };
    let Ok(mut guard) = cell.lock() else {
        return;
    };
    let sink = &mut *guard;
    let t_us = sink.start.elapsed().as_micros();
    let seq = sink.seq;
    sink.seq += 1;
    let line = encode_line(seq, t_us, ep, chan, dir, data, ok);
    let _ = sink.writer.write_all(line.as_bytes());
    let _ = sink.writer.write_all(b"\n");
    // Flush each line so a crash mid-operation still leaves the trace up to that point.
    let _ = sink.writer.flush();
}

/// en: Record the identity of the device being opened, so a replay fixture can reconstruct
/// `enumerate()` offline. Written as a `{"_device":{...}}` line. No-op when capture is off.
/// ja: 開く device の識別子を記録し、replay fixture が offline で `enumerate()` を再構成できる
/// ようにする。`{"_device":{...}}` 行で書く。capture off なら no-op。
pub(crate) fn record_device(
    vid: u16,
    pid: u16,
    serial: Option<&str>,
    topology: &str,
    product: Option<&str>,
    ports: &[String],
) {
    let Some(cell) = SINK.get() else {
        return;
    };
    let Ok(mut guard) = cell.lock() else {
        return;
    };
    let val = serde_json::json!({
        "_device": {
            "vid": vid,
            "pid": pid,
            "serial": serial,
            "topology": topology,
            "product": product,
            "ports": ports,
        }
    });
    let _ = writeln!(guard.writer, "{val}");
    let _ = guard.writer.flush();
}

/// en: Format one NDJSON transaction line (pure, so it can be unit-tested). `ep` is the raw
/// endpoint address: `chan` alone is ambiguous once a session spans several enumerations (in the
/// WCH-Link IAP mode the `0x02`/`0x82` pair is the device's only pair and carries commands), so
/// the endpoint is recorded explicitly and is the authoritative field.
/// ja: NDJSON 1 行を組み立てる(純関数なので単体テストできる)。`ep` は生の endpoint 番地。
/// 複数の列挙をまたぐ capture では `chan` だけでは役割が決まらない(IAP mode の `0x02`/`0x82` は
/// その device 唯一の組で command を運ぶ)ため、endpoint を明示的に記録し、そちらを正とする。
fn encode_line(
    seq: u64,
    t_us: u128,
    ep: u8,
    chan: Chan,
    dir: Dir,
    data: &[u8],
    ok: bool,
) -> String {
    let mut hex = String::with_capacity(data.len() * 2);
    for b in data {
        let _ = write!(hex, "{b:02x}");
    }
    format!(
        r#"{{"seq":{seq},"t_us":{t_us},"ep":"{ep:#04x}","chan":"{}","dir":"{}","len":{},"ok":{ok},"data":"{hex}"}}"#,
        chan.as_str(),
        dir.as_str(),
        data.len(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn encode_line_is_ndjson_with_lowercase_hex() {
        let line = encode_line(
            3,
            1234,
            0x01,
            Chan::Cmd,
            Dir::Out,
            &[0x81, 0x0d, 0x01],
            true,
        );
        assert_eq!(
            line,
            r#"{"seq":3,"t_us":1234,"ep":"0x01","chan":"cmd","dir":"out","len":3,"ok":true,"data":"810d01"}"#
        );
    }

    #[test]
    fn encode_line_empty_data_and_failure() {
        let line = encode_line(0, 0, 0x82, Chan::Data, Dir::In, &[], false);
        assert_eq!(
            line,
            r#"{"seq":0,"t_us":0,"ep":"0x82","chan":"data","dir":"in","len":0,"ok":false,"data":""}"#
        );
    }

    #[test]
    fn record_is_noop_when_capture_not_started() {
        // No unit test calls start(), so the global sink stays unset and this must not panic.
        record(0x81, Chan::Cmd, Dir::In, &[0xff, 0x00], true);
    }
}
