//! en: The OEP HID transport's reports (oep-transports §1): the length-framed byte stream goes into
//! vendor-defined reports as `[report id] count(u16) first(u16) bytes... zero padding`, the report
//! size being what the report descriptor declares. `first` is where the first frame starting in
//! the report begins (`hid_no_frame_start` when none starts there): a reader whose own reading puts
//! that start elsewhere has lost a report and reads on from `first`, and one just opened or with a
//! broken stream reads from the next report that has one. No 200 ms gap rule on HID. Opening the
//! HID device is the caller's (hidapi).
//! ja: OEP の HID の経路の report。長さ見出しのバイト列を `[report ID] count(u16) first(u16) バイト
//! 0 埋め` で詰める。first はその report で最初に始まるフレームの位置(無ければ 0xFFFF)。読む側は
//! 自分の読みと first が違えば読みかけを捨てて first から読み直し、開いた直後や壊れた後は first を
//! 持つ次の report から読む。HID は 200 ms の途切れの規則を使わない。HID を開くのは呼び出し側。

use crate::registry::usb::{HID_NO_FRAME_START, HID_REPORT_HEADER_BYTES};

const HEADER: usize = HID_REPORT_HEADER_BYTES as usize;

/// The vendor report OEP uses, as the report descriptor declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportShape {
    /// The report ID, when the descriptor declares IDs.
    pub id: Option<u8>,
    /// Input report bytes (after the ID).
    pub input: usize,
    /// Output report bytes (after the ID).
    pub output: usize,
}

impl ReportShape {
    /// Stream bytes one output report carries.
    pub fn out_capacity(&self) -> usize {
        self.output.saturating_sub(HEADER)
    }
}

/// en: Where the length-framed stream stands: at a frame's first byte, between its two length
/// bytes, or inside its body. ja: 長さつきの列のどこにいるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    Start,
    Length(u8),
    Body(usize),
}

impl At {
    fn step(self, b: u8) -> At {
        match self {
            At::Start => At::Length(b),
            At::Length(lo) => match usize::from(u16::from_le_bytes([lo, b])) {
                0 => At::Start,
                n => At::Body(n),
            },
            At::Body(1) => At::Start,
            At::Body(n) => At::Body(n - 1),
        }
    }

    /// The offset in `bytes` of the first frame that starts there, reading on from `self`.
    fn first_start(mut self, bytes: &[u8]) -> Option<usize> {
        for (i, &b) in bytes.iter().enumerate() {
            if self == At::Start {
                return Some(i);
            }
            self = self.step(b);
        }
        None
    }
}

/// en: Packs the host's stream into output reports, keeping where it stands across writes (a frame
/// may span writes), so that each report's `first` is right.
/// ja: host の列を output の report に詰める。書き込みをまたいで位置を覚え、first を正しく付ける。
#[derive(Debug, Clone)]
pub struct HidWriter {
    shape: ReportShape,
    at: At,
}

impl HidWriter {
    pub fn new(shape: ReportShape) -> Self {
        HidWriter {
            shape,
            at: At::Start,
        }
    }

    /// en: `data` as output reports, each with the ID byte first (0 when the descriptor has no IDs,
    /// which is how hidapi takes a write).
    /// ja: `data` を output の report に詰める。先頭は ID(ID が無ければ hidapi の書き方どおり 0)。
    pub fn pack(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        let cap = self.shape.out_capacity().max(1);
        data.chunks(cap)
            .map(|c| {
                let first = self
                    .at
                    .first_start(c)
                    .map_or(HID_NO_FRAME_START, |i| i as u16);
                for &b in c {
                    self.at = self.at.step(b);
                }
                let mut r = Vec::with_capacity(1 + self.shape.output);
                r.push(self.shape.id.unwrap_or(0));
                r.extend_from_slice(&(c.len() as u16).to_le_bytes());
                r.extend_from_slice(&first.to_le_bytes());
                r.extend_from_slice(c);
                r.resize(1 + self.shape.output, 0);
                r
            })
            .collect()
    }
}

/// en: Reads input reports back into whole frames (`length(u16) message`, a length 0 frame read
/// past), re-reading from a report's `first` when its own reading disagrees: a report was lost.
/// It starts as a reader that has just opened the transport (from the next report with a first).
/// ja: input の report から、まるごとのフレーム(長さ 0 は読み飛ばす)を取り出す。自分の読みと first
/// が違えば report が欠けたので first から読み直す。開いた直後の読み手として始まる。
#[derive(Debug, Clone)]
pub struct HidReader {
    shape: ReportShape,
    /// Where the stream stands; `None` until a report with a first (just opened, or broken).
    at: Option<At>,
    frame: Vec<u8>,
    /// Reports dropped as broken, and re-reads from a `first` (a report lost before it).
    pub broken: u64,
    pub resyncs: u64,
}

impl HidReader {
    pub fn new(shape: ReportShape) -> Self {
        HidReader {
            shape,
            at: None,
            frame: Vec::new(),
            broken: 0,
            resyncs: 0,
        }
    }

    /// en: One input report as hidapi returns it (the ID first when the descriptor declares IDs):
    /// the frames it completes. A report of another ID is ignored; one whose count or first does
    /// not fit is dropped and the stream is broken till the next first.
    /// ja: input の report 1 つから、完成したフレームを返す。別の ID は無視、count / first が
    /// 合わないものは捨て、次の first まで壊れたものとして扱う。
    pub fn push(&mut self, report: &[u8]) -> Vec<Vec<u8>> {
        let body = match self.shape.id {
            Some(id) => match report.strip_prefix(&[id]) {
                Some(b) => b,
                None => return Vec::new(),
            },
            None => report,
        };
        let header = |i: usize| body.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let (Some(count), Some(first)) = (header(0), header(2)) else {
            return Vec::new();
        };
        let (count, room) = (usize::from(count), self.shape.input.saturating_sub(HEADER));
        let first = (first != HID_NO_FRAME_START).then_some(usize::from(first));
        let Some(bytes) = body.get(HEADER..HEADER + count).filter(|_| count <= room) else {
            self.break_stream();
            return Vec::new();
        };
        if first.is_some_and(|f| f >= count) {
            self.break_stream();
            return Vec::new();
        }
        if count == 0 {
            return Vec::new();
        }
        let from = match self.at {
            Some(at) if at.first_start(bytes) == first => Some(0),
            Some(_) => {
                // Our reading and the sender's disagree: a report was lost before this one.
                self.resyncs += 1;
                self.restart(first)
            }
            None => self.restart(first),
        };
        let Some(from) = from else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut at = self.at.unwrap_or(At::Start);
        for &b in &bytes[from..] {
            self.frame.push(b);
            at = at.step(b);
            if at == At::Start {
                if self.frame.len() > 2 {
                    out.push(std::mem::take(&mut self.frame));
                } else {
                    self.frame.clear();
                }
            }
        }
        self.at = Some(at);
        out
    }

    /// Drop the frame read so far and read on from `first` (or wait for a report with one).
    fn restart(&mut self, first: Option<usize>) -> Option<usize> {
        self.frame.clear();
        self.at = first.map(|_| At::Start);
        first
    }

    fn break_stream(&mut self) {
        self.broken += 1;
        self.frame.clear();
        self.at = None;
    }
}

/// en: The first report of a vendor usage page (0xFF00..) that has both an input and an output,
/// read from a HID report descriptor. Only the items needed for sizes are followed.
/// ja: HID の report 記述子から、vendor の usage page(0xFF00 以上)で input と output の両方を持つ
/// 最初の report を読む。大きさに要る item だけを追う。
pub fn vendor_report(desc: &[u8]) -> Option<ReportShape> {
    #[derive(Default, Clone, Copy)]
    struct Bits {
        input: usize,
        output: usize,
    }
    let mut page: u32 = 0;
    let mut size: usize = 0;
    let mut count: usize = 0;
    let mut id: Option<u8> = None;
    // (id, vendor page, bits) in the order first seen.
    let mut reports: Vec<(Option<u8>, bool, Bits)> = Vec::new();
    let mut stack: Vec<(u32, usize, usize, Option<u8>)> = Vec::new();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: size, tag, data.
            let n = usize::from(*desc.get(i + 1)?);
            i += 3 + n;
            continue;
        }
        let n = match prefix & 0x03 {
            3 => 4,
            s => usize::from(s),
        };
        let data = desc.get(i + 1..i + 1 + n)?;
        let value = data
            .iter()
            .rev()
            .fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
        i += 1 + n;
        match prefix & 0xFC {
            0x04 => page = value,
            0x74 => size = value as usize,
            0x94 => count = value as usize,
            0x84 => id = Some(value as u8),
            0xA4 => stack.push((page, size, count, id)),
            0xB4 => {
                if let Some(s) = stack.pop() {
                    (page, size, count, id) = s;
                }
            }
            0x80 | 0x90 => {
                let vendor = page >= 0xFF00;
                let at = match reports.iter().position(|r| r.0 == id && r.1 == vendor) {
                    Some(p) => p,
                    None => {
                        reports.push((id, vendor, Bits::default()));
                        reports.len() - 1
                    }
                };
                let bits = size * count;
                if prefix & 0xFC == 0x80 {
                    reports[at].2.input += bits;
                } else {
                    reports[at].2.output += bits;
                }
            }
            _ => {}
        }
    }
    reports
        .into_iter()
        .find(|r| r.1 && r.2.input >= 24 && r.2.output >= 24)
        .map(|(id, _, b)| ReportShape {
            id,
            input: b.input.div_ceil(8),
            output: b.output.div_ceil(8),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ESP32-P4 OEP probe's descriptor (usage page 0xFF00, report 6, 511 bytes each way).
    const P4: [u8; 32] = [
        0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x06, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
        0x08, 0x96, 0xff, 0x01, 0x09, 0x01, 0x81, 0x02, 0x09, 0x01, 0x91, 0x02, 0x09, 0x01, 0xb1,
        0x02, 0xc0,
    ];

    #[test]
    fn reads_the_p4_descriptor() {
        assert_eq!(
            vendor_report(&P4),
            Some(ReportShape {
                id: Some(6),
                input: 511,
                output: 511
            })
        );
    }

    #[test]
    fn skips_a_non_vendor_report() {
        // A keyboard-like report (page 1) with no vendor report: nothing.
        let kb = [
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x08, 0x95, 0x08, 0x81, 0x02, 0x91, 0x02,
            0xc0,
        ];
        assert_eq!(vendor_report(&kb), None);
    }

    fn shape(room: usize) -> ReportShape {
        ReportShape {
            id: None,
            input: room,
            output: room,
        }
    }

    #[test]
    fn a_frame_spanning_writes_keeps_its_first() {
        // An 8-byte frame (length 6) written in two halves, then a 3-byte frame: room 8 (4 bytes).
        let mut w = HidWriter::new(shape(8));
        let a = w.pack(&[6, 0, 1, 2]);
        let b = w.pack(&[3, 4, 5, 6, 1, 0, 9]);
        assert_eq!(a, vec![vec![0, 4, 0, 0, 0, 6, 0, 1, 2]]);
        assert_eq!(b[0], [0, 4, 0, 0xff, 0xff, 3, 4, 5, 6]);
        assert_eq!(b[1], [0, 3, 0, 0, 0, 1, 0, 9, 0]);
        let mut r = HidReader::new(shape(8));
        let got: Vec<Vec<u8>> = a.iter().chain(&b).flat_map(|x| r.push(&x[1..])).collect();
        assert_eq!(got, vec![vec![6, 0, 1, 2, 3, 4, 5, 6], vec![1, 0, 9]]);
    }

    #[test]
    fn a_report_id_comes_first_and_another_is_ignored() {
        let s = ReportShape {
            id: Some(6),
            input: 8,
            output: 8,
        };
        let r = HidWriter::new(s).pack(&[1, 0, 7]);
        assert_eq!(r, vec![vec![6, 3, 0, 0, 0, 1, 0, 7, 0]]);
        let mut rd = HidReader::new(s);
        assert!(rd.push(&[5, 3, 0, 0, 0, 1, 0, 7, 0]).is_empty());
        assert_eq!(rd.push(&r[0]), vec![vec![1, 0, 7]]);
    }
}
