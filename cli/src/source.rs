//! en: DMI-backed runtime I/O sources shared by `monitor`, `run` and `arduino monitor`
//! (docs/cli.ja.md §4.5): `dmdata` (the ch32fun/minichlink DM data0/data1 mailbox) and `rtt`
//! (a SEGGER-format ring buffer in target RAM). Both are bidirectional: one [`DmiSource::poll`]
//! drains the target's output and hands over pending host input. Also here: the output [`Sink`]
//! (raw bytes on stdout, or `output` NDJSON events on stderr under `--json`, where stdout is
//! reserved for the result envelope) and the reader thread that feeds host input (stdin or a
//! socket) into a channel.
//!
//! `rtt` reads and writes target RAM over the Debug Module, which needs the hart halted, so a
//! poll briefly halts the core (unless it already is - a `run` servicing semihosting leaves a
//! halted core alone). `dmdata` only touches the DM data registers and never halts.
//!
//! ja: `monitor`/`run`/`arduino monitor` が共用する DMI 経由の実行時 I/O source(dmdata / rtt)。
//! どちらも双方向で、1 回の [`DmiSource::poll`] で target 出力を汲み host 入力を渡す。出力先
//! [`Sink`](生 stdout。`--json` 時は stdout を envelope に譲り stderr の `output` event)と、
//! host 入力(stdin / socket)を channel に流す reader thread もここに置く。`rtt` は RAM の
//! 読み書きに halt が要るので poll ごとに一瞬 halt する(既に halt 中ならそのまま)。

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ch32rv_contract::event::Event;
use ch32rv_contract::policy::MonitorSource;
use ch32rv_contract::{ErrorKind, Warning};
use ch32rv_dmi::DmiError;

use crate::args::Cli;
use crate::session::Session;

// ---- RTT control block layout (SEGGER format; ArduinoCore-CH32 SerialRTT publishes the same) ----

/// All CH32 parts map SRAM at this base.
const RTT_RAM_BASE: u32 = 0x2000_0000;
/// The control-block id string the target publishes once its RTT channel is up.
const RTT_MAGIC: &[u8] = b"SEGGER RTT";
/// Sanity cap on a ring-buffer size read out of RAM (reject a half-initialized / garbage block).
const RTT_MAX_BUF: u32 = 0x1_0000;
/// Sanity cap on the channel counts in the header.
const RTT_MAX_CHANNELS: u32 = 16;
/// Scan length when the target's SRAM size is unknown (the `_SEGGER_RTT` block lives in early .bss).
const RTT_DEFAULT_SCAN: u32 = 8 * 1024;
/// The WCH-Link's bulk read rejects/times-out on a very large single region, so read the scan
/// window in transfers this size (8 KiB is proven to work well within the transport timeout).
const RTT_READ_CHUNK: u32 = 8192;
/// Header: id[16] + max_up(4) + max_down(4); then `max_up` up descriptors, then the down ones.
const RTT_HEADER_LEN: u32 = 24;
/// One ring descriptor: name(+0) buffer(+4) size(+8) write_off(+12) read_off(+16) flags(+20).
const RTT_DESC_LEN: u32 = 24;
const RTT_DESC_WRITE_OFF: u32 = 12;
const RTT_DESC_READ_OFF: u32 = 16;

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// A control block found in a RAM snapshot: its offset and channel counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RttHeader {
    offset: usize,
    max_up: u32,
    max_down: u32,
}

/// The live fields of one ring descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingDesc {
    buffer: u32,
    size: u32,
    wr: u32,
    rd: u32,
}

impl RingDesc {
    /// Parse and validate a 24-byte descriptor (buffer in RAM, sane size, offsets in range).
    fn parse(d: &[u8]) -> Option<Self> {
        if d.len() < RTT_DESC_LEN as usize {
            return None;
        }
        let desc = RingDesc {
            buffer: le32(d, 4),
            size: le32(d, 8),
            wr: le32(d, 12),
            rd: le32(d, 16),
        };
        let ok = desc.size > 0
            && desc.size <= RTT_MAX_BUF
            && desc.wr < desc.size
            && desc.rd < desc.size
            && desc.buffer >= RTT_RAM_BASE;
        ok.then_some(desc)
    }

    fn used(&self) -> u32 {
        if self.wr >= self.rd {
            self.wr - self.rd
        } else {
            self.size - self.rd + self.wr
        }
    }
}

/// Find a SEGGER RTT control block in a RAM snapshot: the magic, a header with sane channel
/// counts, and an up[0] descriptor that validates. Validating rejects a stray copy of the magic
/// that lives in a ring buffer's own contents, not in a real block.
fn find_control_block(snap: &[u8]) -> Option<RttHeader> {
    let mut from = 0usize;
    while let Some(rel) = snap[from..]
        .windows(RTT_MAGIC.len())
        .position(|w| w == RTT_MAGIC)
    {
        let pos = from + rel;
        let hdr_end = pos + RTT_HEADER_LEN as usize;
        if hdr_end + RTT_DESC_LEN as usize <= snap.len() {
            let max_up = le32(snap, pos + 16);
            let max_down = le32(snap, pos + 20);
            if (1..=RTT_MAX_CHANNELS).contains(&max_up)
                && max_down <= RTT_MAX_CHANNELS
                && RingDesc::parse(&snap[hdr_end..]).is_some()
            {
                return Some(RttHeader {
                    offset: pos,
                    max_up,
                    max_down,
                });
            }
        }
        from = pos + 1;
    }
    None
}

// ---- sources ----

/// Target addresses of the RTT ring descriptors this session streams (channel 0 each way).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RttChannels {
    up: u32,
    down: Option<u32>,
}

/// An opened runtime output source over the Debug Module.
pub(crate) enum DmiSource {
    Dmdata,
    Rtt(RttChannels),
}

pub(crate) enum OpenError {
    /// The source is not a DMI one (uart/sdi go through the probe's CDC port).
    NotDmi,
    /// No RTT control block appeared in the scanned RAM window.
    NoControlBlock {
        scan_len: u32,
    },
    Dmi(DmiError),
}

impl From<DmiError> for OpenError {
    fn from(e: DmiError) -> Self {
        OpenError::Dmi(e)
    }
}

fn transport(e: ch32rv_wchlink::WchLinkError) -> DmiError {
    DmiError::Transport(e.to_string())
}

/// Read `len` bytes of target memory into one buffer, chunked so each transfer stays small. Stops
/// early (returning what it has) if a chunk fails, so a short read still lets the scan try.
fn read_region(session: &mut Session, base: u32, len: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len as usize);
    let mut off = 0u32;
    while off < len {
        let want = RTT_READ_CHUNK.min(len - off);
        match session.link().read_mem(base + off, want) {
            Ok(mut chunk) => {
                buf.append(&mut chunk);
                off += want;
            }
            Err(_) => break,
        }
    }
    buf
}

impl DmiSource {
    /// en: Open `source` on an attached session. `rtt` scans RAM for the control block (the
    /// target has usually been running since power-on, so begin() has published it; a few
    /// retries cover a very early attach) and leaves the core HALTED - the caller resumes it.
    /// `dmdata` needs no setup. A block with more than one channel per direction is reported as
    /// a warning: only channel 0 is streamed.
    /// ja: attach 済み session 上で source を開く。`rtt` は RAM を走査して control block を見つけ、
    /// core を halt のまま返す(resume は呼び出し側)。channel が複数ある block は警告して 0 番だけ流す。
    pub(crate) fn open(
        session: &mut Session,
        source: MonitorSource,
        warnings: &mut Vec<Warning>,
    ) -> Result<Self, OpenError> {
        match source {
            MonitorSource::Dmdata => Ok(DmiSource::Dmdata),
            MonitorSource::Rtt => Self::open_rtt(session, warnings),
            MonitorSource::Uart | MonitorSource::Sdi => Err(OpenError::NotDmi),
        }
    }

    fn open_rtt(session: &mut Session, warnings: &mut Vec<Warning>) -> Result<Self, OpenError> {
        // How much RAM to scan for the control block: the target's SRAM (from the DB) or a default.
        let scan_len = {
            let db = ch32rv_target::Db::builtin();
            match db.resolve_by_chip_id(session.attach.chip_id) {
                ch32rv_target::Resolution::Sku(s) if s.sram_bytes > 0 => {
                    s.sram_bytes.min(64 * 1024)
                }
                _ => RTT_DEFAULT_SCAN,
            }
        };
        let mut found = None;
        for _ in 0..10 {
            session.dm().halt()?;
            let snap = read_region(session, RTT_RAM_BASE, scan_len);
            if let Some(h) = find_control_block(&snap) {
                found = Some(h);
                break;
            }
            session.dm().resume()?;
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(h) = found else {
            return Err(OpenError::NoControlBlock { scan_len });
        };
        if h.max_up > 1 || h.max_down > 1 {
            warnings.push(Warning {
                code: "rtt-channels".to_owned(),
                msg: format!(
                    "the RTT control block has {} up / {} down channels; only channel 0 is streamed",
                    h.max_up, h.max_down
                ),
            });
        }
        let base = RTT_RAM_BASE + h.offset as u32;
        let up = base + RTT_HEADER_LEN;
        let down = (h.max_down >= 1).then_some(up + RTT_DESC_LEN * h.max_up);
        Ok(DmiSource::Rtt(RttChannels { up, down }))
    }

    pub(crate) fn name(&self) -> &'static str {
        match self {
            DmiSource::Dmdata => "dmdata",
            DmiSource::Rtt(_) => "rtt",
        }
    }

    /// How long to wait between polls that returned nothing.
    pub(crate) fn idle(&self) -> Duration {
        match self {
            DmiSource::Dmdata => Duration::from_millis(2),
            DmiSource::Rtt(_) => Duration::from_millis(50),
        }
    }

    /// One-line description for the human "monitor: ..." banner.
    pub(crate) fn describe(&self) -> String {
        match self {
            DmiSource::Dmdata => "dmdata (DMI mailbox, core runs)".to_owned(),
            DmiSource::Rtt(ch) => format!(
                "rtt (RAM ring @ 0x{:08x}, core briefly halts per poll)",
                ch.up - RTT_HEADER_LEN
            ),
        }
    }

    /// en: One exchange: returns the target's output since the last poll and removes from the
    /// front of `input` whatever was handed to the target (dmdata: up to 3 bytes per target
    /// frame; rtt: as much as the down ring has room for).
    /// ja: 1 回の交換。前回以降の target 出力を返し、target へ渡せた分だけ `input` の先頭を消す。
    pub(crate) fn poll(
        &mut self,
        session: &mut Session,
        input: &mut Vec<u8>,
    ) -> Result<Vec<u8>, DmiError> {
        match self {
            DmiSource::Dmdata => {
                let r = session.dm().dmdata_poll(&input[..input.len().min(3)])?;
                input.drain(..r.sent);
                Ok(r.received)
            }
            DmiSource::Rtt(ch) => {
                let ch = *ch;
                // RAM access needs the hart halted. Leave an already-halted core (a semihosting
                // stop being serviced by `run`) alone; otherwise halt for the exchange and resume.
                let was_halted = session.dm().is_halted()?;
                if !was_halted {
                    session.dm().halt()?;
                }
                let result = rtt_exchange(session, ch, input);
                if !was_halted {
                    let _ = session.dm().resume();
                }
                result
            }
        }
    }
}

/// Drain up[0] (acknowledging by moving its read offset) and push host input into down[0].
fn rtt_exchange(
    session: &mut Session,
    ch: RttChannels,
    input: &mut Vec<u8>,
) -> Result<Vec<u8>, DmiError> {
    let raw = session
        .link()
        .read_mem(ch.up, RTT_DESC_LEN)
        .map_err(transport)?;
    let Some(up) = RingDesc::parse(&raw) else {
        // Not ready or garbage: let the target run and retry next poll.
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    if up.wr != up.rd {
        let link = session.link();
        if up.wr > up.rd {
            out = link
                .read_mem(up.buffer + up.rd, up.wr - up.rd)
                .map_err(transport)?;
        } else {
            // Wrapped: [rd, size) then [0, wr).
            out = link
                .read_mem(up.buffer + up.rd, up.size - up.rd)
                .map_err(transport)?;
            if up.wr > 0 {
                out.extend(link.read_mem(up.buffer, up.wr).map_err(transport)?);
            }
        }
        // Tell the target we drained: up[0].read_off = write_off.
        session.dm().write_mem32(ch.up + RTT_DESC_READ_OFF, up.wr)?;
    }
    if let Some(down_addr) = ch.down
        && !input.is_empty()
    {
        let raw = session
            .link()
            .read_mem(down_addr, RTT_DESC_LEN)
            .map_err(transport)?;
        if let Some(down) = RingDesc::parse(&raw) {
            // A ring spends one slot telling full from empty.
            let room = (down.size - 1 - down.used()) as usize;
            let n = input.len().min(room);
            if n > 0 {
                let first = n.min((down.size - down.wr) as usize);
                let mut dm = session.dm();
                dm.write_mem(down.buffer + down.wr, &input[..first])?;
                if n > first {
                    dm.write_mem(down.buffer, &input[first..n])?;
                }
                // Publish the offset only after the bytes are in place (the target reads it).
                dm.write_mem32(
                    down_addr + RTT_DESC_WRITE_OFF,
                    (down.wr + n as u32) % down.size,
                )?;
                input.drain(..n);
            }
        }
    }
    Ok(out)
}

/// Exit code class for a DMI failure while streaming (docs/cli.ja.md §3.6: 40 either way, the
/// JSON `kind` tells a true timeout from a failed transfer).
pub(crate) fn dmi_error_kind(e: &DmiError) -> ErrorKind {
    match e {
        DmiError::Timeout => ErrorKind::TransportTimeout,
        _ => ErrorKind::TransferFailed,
    }
}

// ---- output sink ----

/// en: Where the target's output goes. Human mode: raw bytes straight to stdout. `--json`: one
/// `output` NDJSON event per chunk on stderr, split on UTF-8 character boundaries so a
/// multi-byte character straddling two 7-byte dmdata frames is not mangled.
/// ja: target 出力の行き先。human は生 byte を stdout へ。`--json` は stderr へ `output` event
/// (UTF-8 文字境界で分割。dmdata の 7 byte frame をまたぐ多バイト文字を壊さない)。
pub(crate) enum Sink {
    Raw,
    Json {
        source: &'static str,
        pending: Vec<u8>,
    },
}

impl Sink {
    pub(crate) fn new(cli: &Cli, source: &'static str) -> Self {
        if cli.json {
            Sink::Json {
                source,
                pending: Vec::new(),
            }
        } else {
            Sink::Raw
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match self {
            Sink::Raw => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(bytes);
                let _ = out.flush();
            }
            Sink::Json { source, pending } => {
                pending.extend_from_slice(bytes);
                let text = take_text(pending);
                emit_output(source, text);
            }
        }
    }

    /// Flush an incomplete trailing sequence (lossily) when the stream ends.
    pub(crate) fn finish(&mut self) {
        if let Sink::Json { source, pending } = self
            && !pending.is_empty()
        {
            let text = String::from_utf8_lossy(pending).into_owned();
            pending.clear();
            emit_output(source, text);
        }
    }
}

fn emit_output(source: &str, data: String) {
    if data.is_empty() {
        return;
    }
    let ev = Event::Output {
        source: source.to_owned(),
        data,
    };
    if let Ok(line) = serde_json::to_string(&ev) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{line}");
    }
}

/// Take the longest run of complete UTF-8 characters off the front of `buf`, replacing invalid
/// sequences with U+FFFD. An incomplete multi-byte sequence at the end (at most 3 bytes) stays in
/// `buf` for the next chunk to complete.
fn take_text(buf: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    loop {
        match std::str::from_utf8(&buf[i..]) {
            Ok(s) => {
                out.push_str(s);
                i = buf.len();
                break;
            }
            Err(e) => {
                let ok = e.valid_up_to();
                out.push_str(&String::from_utf8_lossy(&buf[i..i + ok]));
                i += ok;
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        i += bad;
                    }
                    // Incomplete sequence at the end: keep it for the next chunk.
                    None => break,
                }
            }
        }
    }
    buf.drain(..i);
    out
}

// ---- host input ----

/// en: Read `r` (stdin, a socket) on its own thread and hand chunks to the returned channel;
/// ends quietly at EOF or error. The main loop pulls from it between polls so a blocking read
/// never stalls the target exchange.
/// ja: `r`(stdin / socket)を別 thread で読み、chunk を channel に渡す。EOF/エラーで静かに終わる。
pub(crate) fn spawn_reader(mut r: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 256];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    rx
}

/// Move everything the reader has produced so far into `into` (non-blocking).
pub(crate) fn drain_input(rx: &Receiver<Vec<u8>>, into: &mut Vec<u8>) {
    while let Ok(chunk) = rx.try_recv() {
        into.extend_from_slice(&chunk);
    }
}

/// en: Stream `src` to `sink` and `input` to the target until `deadline` (None = until Ctrl-C).
/// The caller has resumed the core. Errors are DMI failures the caller turns into an exit code.
/// ja: `deadline` まで(None なら Ctrl-C まで)`src`→`sink`、`input`→target を流す。
pub(crate) fn stream(
    session: &mut Session,
    src: &mut DmiSource,
    sink: &mut Sink,
    input: &Receiver<Vec<u8>>,
    deadline: Option<Instant>,
) -> Result<(), DmiError> {
    let mut pending = Vec::new();
    loop {
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            return Ok(());
        }
        drain_input(input, &mut pending);
        let out = src.poll(session, &mut pending)?;
        if out.is_empty() {
            std::thread::sleep(src.idle());
        } else {
            sink.write(&out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a RAM snapshot with a control block at `at` (`max_up`/`max_down` channels) whose
    /// up[0] descriptor is valid and whose down[0], if any, sits after all the up descriptors.
    fn snapshot(at: usize, max_up: u32, max_down: u32, up0: RingDesc) -> Vec<u8> {
        let descs = (max_up + max_down) as usize;
        let mut ram = vec![0u8; at + 24 + 24 * descs + 8];
        ram[at..at + RTT_MAGIC.len()].copy_from_slice(RTT_MAGIC);
        ram[at + 16..at + 20].copy_from_slice(&max_up.to_le_bytes());
        ram[at + 20..at + 24].copy_from_slice(&max_down.to_le_bytes());
        let d = at + 24;
        ram[d + 4..d + 8].copy_from_slice(&up0.buffer.to_le_bytes());
        ram[d + 8..d + 12].copy_from_slice(&up0.size.to_le_bytes());
        ram[d + 12..d + 16].copy_from_slice(&up0.wr.to_le_bytes());
        ram[d + 16..d + 20].copy_from_slice(&up0.rd.to_le_bytes());
        ram
    }

    fn valid_up0() -> RingDesc {
        RingDesc {
            buffer: RTT_RAM_BASE + 0x100,
            size: 256,
            wr: 10,
            rd: 0,
        }
    }

    #[test]
    fn finds_valid_control_block_with_counts() {
        let ram = snapshot(64, 1, 1, valid_up0());
        assert_eq!(
            find_control_block(&ram),
            Some(RttHeader {
                offset: 64,
                max_up: 1,
                max_down: 1
            })
        );
    }

    #[test]
    fn down_descriptor_follows_all_up_descriptors() {
        // With 2 up channels, down[0] is the third descriptor after the header.
        let h = RttHeader {
            offset: 0,
            max_up: 2,
            max_down: 1,
        };
        let base = RTT_RAM_BASE + h.offset as u32;
        let up = base + RTT_HEADER_LEN;
        let down = up + RTT_DESC_LEN * h.max_up;
        assert_eq!(down, RTT_RAM_BASE + 24 + 48);
    }

    #[test]
    fn rejects_bogus_channel_counts() {
        // max_up = 0 (no up channel to stream) and an absurd count are both garbage.
        assert_eq!(find_control_block(&snapshot(0, 0, 1, valid_up0())), None);
        assert_eq!(find_control_block(&snapshot(0, 1000, 1, valid_up0())), None);
    }

    #[test]
    fn skips_magic_with_bogus_descriptor() {
        // A stray "SEGGER RTT" in buffer contents: the descriptor after it is garbage (size huge),
        // so it must not be mistaken for a real block.
        let bogus = RingDesc {
            buffer: 0,
            size: 0xFFFF_FFFF,
            wr: 0,
            rd: 0,
        };
        assert_eq!(find_control_block(&snapshot(64, 1, 1, bogus)), None);
        assert_eq!(find_control_block(&[0u8; 64]), None);
    }

    #[test]
    fn ring_used_handles_wrap() {
        let d = RingDesc {
            buffer: RTT_RAM_BASE,
            size: 16,
            wr: 2,
            rd: 14,
        };
        assert_eq!(d.used(), 4);
        assert_eq!(RingDesc { wr: 9, rd: 3, ..d }.used(), 6);
    }

    #[test]
    fn take_text_keeps_incomplete_tail_across_chunks() {
        // "こ" is e3 81 93; a 7-byte dmdata frame can end after e3 81.
        let mut buf = b"ab\xe3\x81".to_vec();
        assert_eq!(take_text(&mut buf), "ab");
        assert_eq!(buf, b"\xe3\x81");
        buf.extend_from_slice(b"\x93!");
        assert_eq!(take_text(&mut buf), "こ!");
        assert!(buf.is_empty());
    }

    #[test]
    fn take_text_replaces_invalid_bytes() {
        let mut buf = b"x\xffy".to_vec();
        assert_eq!(take_text(&mut buf), "x\u{FFFD}y");
        assert!(buf.is_empty());
    }

    #[test]
    fn le32_reads_little_endian() {
        assert_eq!(le32(&[0x78, 0x56, 0x34, 0x12], 0), 0x1234_5678);
    }
}
