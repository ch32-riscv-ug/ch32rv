//! en: The OEP HID transport's report packing (oep-core §3.1): the length-framed byte stream goes
//! into vendor-defined reports as `[report id] count(u16) bytes... zero padding`, the report size
//! being what the report descriptor declares. Opening the HID device is the caller's (hidapi).
//! ja: OEP の HID の経路の report の詰め方。長さ見出しのバイト列を vendor 定義の report に
//! `[report ID] count(u16) バイト 0 埋め` で詰める。report の大きさは記述子のとおり。HID を開くのは呼び出し側。

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
        self.output.saturating_sub(2)
    }

    /// en: `data` as output reports, each with the ID byte first (0 when the descriptor has no IDs,
    /// which is how hidapi takes a write).
    /// ja: `data` を output の report に詰める。先頭は ID(ID が無ければ hidapi の書き方どおり 0)。
    pub fn pack(&self, data: &[u8]) -> Vec<Vec<u8>> {
        let cap = self.out_capacity().max(1);
        data.chunks(cap)
            .map(|c| {
                let mut r = Vec::with_capacity(1 + self.output);
                r.push(self.id.unwrap_or(0));
                r.extend_from_slice(&(c.len() as u16).to_le_bytes());
                r.extend_from_slice(c);
                r.resize(1 + self.output, 0);
                r
            })
            .collect()
    }

    /// en: The stream bytes in one input report as hidapi returns it (the ID first when the
    /// descriptor declares IDs). `None` for a report of another ID or a count past its end.
    /// ja: hidapi が返す input の report 1 つの中のバイト列(ID があれば先頭が ID)。別の ID の report
    /// や、count が report をはみ出すものは `None`。
    pub fn unpack<'a>(&self, report: &'a [u8]) -> Option<&'a [u8]> {
        let body = match self.id {
            Some(id) => report.strip_prefix(&[id])?,
            None => report,
        };
        let count = usize::from(u16::from_le_bytes([*body.first()?, *body.get(1)?]));
        body.get(2..2 + count)
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

    #[test]
    fn packs_and_unpacks() {
        let s = ReportShape {
            id: Some(6),
            input: 8,
            output: 8,
        };
        let data: Vec<u8> = (1..=13).collect();
        let r = s.pack(&data);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], [6, 6, 0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(r[2], [6, 1, 0, 13, 0, 0, 0, 0, 0]);
        let back: Vec<u8> = r
            .iter()
            .flat_map(|x| s.unpack(x).map(<[u8]>::to_vec).unwrap_or_default())
            .collect();
        assert_eq!(back, data);
        // Another report ID, or a count past the end, is not ours.
        assert_eq!(s.unpack(&[5, 1, 0, 9]), None);
        assert_eq!(s.unpack(&[6, 9, 0, 1]), None);
    }

    #[test]
    fn no_report_ids() {
        let s = ReportShape {
            id: None,
            input: 4,
            output: 4,
        };
        assert_eq!(s.pack(&[7, 8]), vec![vec![0, 2, 0, 7, 8]]);
        assert_eq!(s.unpack(&[1, 0, 9, 0]), Some(&[9][..]));
    }
}
