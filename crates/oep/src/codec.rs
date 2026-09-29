//! en: OEP v1 framing and message layer (oep-spec core §2-§4, docs/oep-host.ja.md §3).
//!
//! - Serial-visible transports (USB CDC, USB-Serial/JTAG, UART bridge): `COBS(message ‖ crc16_le)`
//!   delimited by 0x00; the host also sends a leading 0x00 (ArduinoCore-CH32 oep-workflow §4.2).
//!   Bytes outside a frame are noise (the port is shared with the target console); a frame whose
//!   CRC or COBS is wrong is dropped, and a lost answer shows up only as a timeout.
//! - Vendor bulk, HID's inner stream and TCP: `length(u16 LE) message`; length 0 is a keepalive.
//!
//! ja: OEP v1 の framing と message 層。serial に見える transport は COBS + CRC-16(0x00 区切り、
//! host は前にも 0x00)、フレームの外は雑音。vendor bulk / HID の中身 / TCP は `length(u16) message`。

use crate::registry;

// ---- checksums ----

/// CRC-16/CCITT-FALSE: poly 0x1021, init 0xFFFF, no reflection, no xor-out.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// CRC-32/IEEE (reflected, poly 0xEDB88320, init and xor-out 0xFFFFFFFF): the request fingerprint
/// the probe's resend table keys on, and the `oep.probe.config` hash.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// ---- COBS ----

/// Standard COBS (254-byte blocks). Emits no trailing empty block after a full block that ends the
/// data; [`cobs_decode`] accepts both that and the reference encoder's extra `0x01`.
pub fn cobs_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 254 + 2);
    let mut code_at = 0;
    out.push(0);
    let mut code: u8 = 1;
    // A block opened only because the previous one filled up (0xFF) and still empty.
    let mut after_full = false;
    for &b in data {
        if b == 0 {
            out[code_at] = code;
            code_at = out.len();
            out.push(0);
            code = 1;
            after_full = false;
        } else {
            out.push(b);
            code += 1;
            after_full = false;
            if code == 0xFF {
                out[code_at] = code;
                code_at = out.len();
                out.push(0);
                code = 1;
                after_full = true;
            }
        }
    }
    if after_full {
        out.pop();
    } else {
        out[code_at] = code;
    }
    out
}

/// Decode one COBS frame (without its 0x00 delimiter). `None` if the encoding is broken.
pub fn cobs_decode(enc: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(enc.len());
    let mut i = 0;
    while i < enc.len() {
        let code = usize::from(enc[i]);
        let end = i + code;
        if code == 0 || end > enc.len() {
            return None;
        }
        out.extend_from_slice(&enc[i + 1..end]);
        i = end;
        // A zero follows every block except a full (0xFF) one and the last one.
        if code != 0xFF && i < enc.len() {
            out.push(0);
        }
    }
    Some(out)
}

/// One serial frame, as the host sends it: `0x00 COBS(message ‖ crc16_le) 0x00`.
pub fn serial_frame(message: &[u8]) -> Vec<u8> {
    let mut body = message.to_vec();
    body.extend_from_slice(&crc16(message).to_le_bytes());
    let mut out = vec![0];
    out.extend(cobs_encode(&body));
    out.push(0);
    out
}

/// en: Deframer for serial transports. Feed it whatever the port gives; it returns the messages
/// whose COBS and CRC check out. Everything else - the target console's bytes on a shared port,
/// a frame cut short, a bad CRC - is counted and dropped.
/// ja: serial transport の deframer。COBS と CRC が合った message だけを返し、それ以外(共用 port の
/// console のバイト、途中で切れたフレーム、CRC 不一致)は数えて捨てる。
#[derive(Debug, Default)]
pub struct SerialDeframer {
    buf: Vec<u8>,
    in_frame: bool,
    /// Frames dropped for a bad COBS encoding or CRC.
    pub dropped: u64,
    max: usize,
}

impl SerialDeframer {
    /// `max_message` bounds a frame (the probe's max_frame, or 0xFFFF before confirm).
    pub fn new(max_message: usize) -> Self {
        // en: The start of the stream counts as a frame start: the reference probe sends only the
        // trailing delimiter, so its first frame after the port opens has none in front. Console
        // bytes in front of it just fail the CRC.
        // ja: stream の先頭もフレームの始まりとみなす(参照 probe は後ろの区切りしか送らず、開いた
        // 直後の最初のフレームには前の 0x00 が無い)。前に console のバイトがあれば CRC で落ちる。
        Self {
            max: max_message,
            in_frame: true,
            ..Self::default()
        }
    }

    pub fn set_max(&mut self, max_message: usize) {
        self.max = max_message;
    }

    /// Push received bytes; returns the complete messages in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        // COBS grows a message by 1 byte in 254 plus the code byte and the CRC.
        let cap = self.max + self.max / 254 + 4;
        for &b in bytes {
            if b == 0 {
                if self.in_frame && !self.buf.is_empty() {
                    match cobs_decode(&self.buf) {
                        Some(d) if d.len() >= 3 => {
                            let (msg, crc) = d.split_at(d.len() - 2);
                            if crc16(msg).to_le_bytes() == [crc[0], crc[1]] {
                                out.push(msg.to_vec());
                            } else {
                                self.dropped += 1;
                            }
                        }
                        _ => self.dropped += 1,
                    }
                }
                // A delimiter always (re)starts a frame; empty frames are ignored.
                self.buf.clear();
                self.in_frame = true;
            } else if self.in_frame {
                if self.buf.len() >= cap {
                    // Too long to be ours: console bytes after a lost delimiter. Wait for the next 0x00.
                    self.buf.clear();
                    self.in_frame = false;
                    self.dropped += 1;
                } else {
                    self.buf.push(b);
                }
            }
        }
        out
    }
}

// ---- length framing ----

/// One length-framed message: `length(u16 LE) message`.
pub fn length_frame(message: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(message.len() + 2);
    out.extend_from_slice(&(message.len() as u16).to_le_bytes());
    out.extend_from_slice(message);
    out
}

/// Why a length-framed stream lost its frame boundaries (the caller resyncs, core §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FramingLost {
    #[error("frame length {0} exceeds max_frame")]
    TooLong(usize),
}

/// Deframer for `length(u16) message` streams (vendor bulk, HID's inner stream, TCP).
#[derive(Debug, Default)]
pub struct LengthDeframer {
    buf: Vec<u8>,
    max: usize,
}

impl LengthDeframer {
    pub fn new(max_message: usize) -> Self {
        Self {
            buf: Vec::new(),
            max: max_message,
        }
    }

    pub fn set_max(&mut self, max_message: usize) {
        self.max = max_message;
    }

    /// Whether a frame is partly received (for the 200 ms stall rule).
    pub fn mid_frame(&self) -> bool {
        !self.buf.is_empty()
    }

    /// Drop a partial frame (after a stall, or when resyncing).
    pub fn reset(&mut self) {
        self.buf.clear();
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FramingLost> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 2 {
                break;
            }
            let len = usize::from(u16::from_le_bytes([self.buf[0], self.buf[1]]));
            if len == 0 {
                self.buf.drain(..2); // keepalive
                continue;
            }
            if len > self.max {
                self.buf.clear();
                return Err(FramingLost::TooLong(len));
            }
            if self.buf.len() < 2 + len {
                break;
            }
            out.push(self.buf[2..2 + len].to_vec());
            self.buf.drain(..2 + len);
        }
        Ok(out)
    }
}

// ---- TLV ----

/// One TLV item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlv {
    pub tag: u8,
    pub value: Vec<u8>,
}

/// Append a request TLV; `critical` sets bit 7 (the probe must honour it or refuse).
pub fn put_tlv(out: &mut Vec<u8>, tag: u8, critical: bool, value: &[u8]) {
    let t = if critical {
        tag | registry::constants::TAG_CRITICAL
    } else {
        tag
    };
    out.push(t);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
}

/// Parse a response TLV list. A truncated item means the whole result is broken (core §4.3).
pub fn parse_tlvs(mut b: &[u8]) -> Result<Vec<Tlv>, DecodeError> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if b.len() < 2 || b.len() < 2 + usize::from(b[1]) {
            return Err(DecodeError::TruncatedTlv);
        }
        let len = usize::from(b[1]);
        out.push(Tlv {
            tag: b[0],
            value: b[2..2 + len].to_vec(),
        });
        b = &b[2 + len..];
    }
    Ok(out)
}

// ---- messages ----

/// A request to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub corr: u16,
    pub func: u16,
    pub op: u8,
    /// `Some` sends role 0x81 with the session id after the header.
    pub session: Option<u32>,
    pub payload: Vec<u8>,
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let role = registry::roles::REQUEST
            | if self.session.is_some() {
                registry::constants::ROLE_SESSION_FLAG
            } else {
                0
            };
        let mut out = Vec::with_capacity(10 + self.payload.len());
        out.push(role);
        out.extend_from_slice(&self.corr.to_le_bytes());
        out.extend_from_slice(&self.func.to_le_bytes());
        out.push(self.op);
        if let Some(sid) = self.session {
            out.extend_from_slice(&sid.to_le_bytes());
        }
        out.extend_from_slice(&self.payload);
        out
    }
}

/// How the probe resolved a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Not accepted; the byte is the reject reason.
    Rejected(u8),
    /// Done; the byte is the outcome (success / failed / partial).
    Completed(u8),
    /// A resolution this host does not know (0x02 accepted is reserved in v1): a failure.
    Unknown(u8, u8),
}

/// A message from the probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Result {
        corr: u16,
        resolution: Resolution,
        payload: Vec<u8>,
    },
    Event {
        func: u16,
        seq: u16,
        kind: u8,
        payload: Vec<u8>,
    },
    Data {
        func: u16,
        seq: u16,
        payload: Vec<u8>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("message shorter than its header")]
    Short,
    #[error("unknown role 0x{0:02x}")]
    UnknownRole(u8),
    #[error("truncated TLV")]
    TruncatedTlv,
}

impl Incoming {
    /// Decode one message. Unknown roles are an error the caller drops (core §2.4).
    pub fn decode(m: &[u8]) -> Result<Self, DecodeError> {
        let role = *m.first().ok_or(DecodeError::Short)?;
        let u16at = |i: usize| u16::from_le_bytes([m[i], m[i + 1]]);
        match role {
            r if r == registry::roles::RESULT => {
                if m.len() < 5 {
                    return Err(DecodeError::Short);
                }
                let resolution = match m[3] {
                    x if x == registry::resolutions::REJECTED => Resolution::Rejected(m[4]),
                    x if x == registry::resolutions::COMPLETED => Resolution::Completed(m[4]),
                    x => Resolution::Unknown(x, m[4]),
                };
                Ok(Incoming::Result {
                    corr: u16at(1),
                    resolution,
                    payload: m[5..].to_vec(),
                })
            }
            r if r == registry::roles::EVENT => {
                if m.len() < 6 {
                    return Err(DecodeError::Short);
                }
                Ok(Incoming::Event {
                    func: u16at(1),
                    seq: u16at(3),
                    kind: m[5],
                    payload: m[6..].to_vec(),
                })
            }
            r if r == registry::roles::DATA => {
                if m.len() < 5 {
                    return Err(DecodeError::Short);
                }
                Ok(Incoming::Data {
                    func: u16at(1),
                    seq: u16at(3),
                    payload: m[5..].to_vec(),
                })
            }
            other => Err(DecodeError::UnknownRole(other)),
        }
    }
}

/// The next corr: +1 per request, 65535 wraps to 1, 0 is never used (core §4.1).
pub fn next_corr(c: u16) -> u16 {
    if c == u16::MAX { 1 } else { c + 1 }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn check_values() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn cobs_round_trip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![0, 0],
            vec![1, 2, 0, 3],
            (1..=254).collect(),
            (1..=255).collect(),
            (0..=255).collect(),
            vec![7; 600],
        ];
        for c in cases {
            let e = cobs_encode(&c);
            assert!(!e.contains(&0), "encoded contains zero for {c:?}");
            assert_eq!(cobs_decode(&e).unwrap(), c, "round trip {} bytes", c.len());
        }
    }

    #[test]
    fn cobs_accepts_the_reference_trailing_empty_block() {
        // 254 non-zero bytes: the reference encoder writes FF .. 01, this one FF ..
        let data: Vec<u8> = (1..=254).collect();
        let mut reference = vec![0xFF];
        reference.extend_from_slice(&data);
        reference.push(0x01);
        assert_eq!(cobs_decode(&reference).unwrap(), data);
        assert_eq!(cobs_encode(&data), reference[..255].to_vec());
    }

    #[test]
    fn serial_deframer_drops_noise_and_bad_crc() {
        let msg = Request {
            corr: 1,
            func: 0,
            op: 1,
            session: None,
            payload: b"OEP?\x01\x01".to_vec(),
        }
        .encode();
        let mut stream = b"hello console\r\n".to_vec();
        stream.extend(serial_frame(&msg));
        stream.extend(b"more text");
        let mut bad = serial_frame(&msg);
        let n = bad.len();
        bad[n - 3] ^= 0x55;
        stream.extend(bad);
        stream.extend(serial_frame(&msg));
        let mut d = SerialDeframer::new(1024);
        // Feed it byte by byte to cover every split point.
        let mut got = Vec::new();
        for b in &stream {
            got.extend(d.push(std::slice::from_ref(b)));
        }
        assert_eq!(got, vec![msg.clone(), msg]);
        assert!(d.dropped >= 2); // "more text" as a frame, and the bad CRC
    }

    #[test]
    fn length_deframer() {
        let mut s = vec![0, 0]; // keepalive
        s.extend(length_frame(b"abc"));
        s.extend(length_frame(b"de"));
        let mut d = LengthDeframer::new(1024);
        assert_eq!(d.push(&s[..4]).unwrap(), Vec::<Vec<u8>>::new());
        assert!(d.mid_frame());
        assert_eq!(
            d.push(&s[4..]).unwrap(),
            vec![b"abc".to_vec(), b"de".to_vec()]
        );
        assert_eq!(d.push(&[0xff, 0xff]), Err(FramingLost::TooLong(0xffff)));
    }

    #[test]
    fn request_and_result_layout() {
        let r = Request {
            corr: 0x0201,
            func: 0x0403,
            op: 5,
            session: Some(0x0908_0706),
            payload: vec![0xAA],
        };
        assert_eq!(r.encode(), vec![0x81, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xAA]);
        let m = Incoming::decode(&[0x02, 0x01, 0x02, 0x00, 0x08, 0x10, 0x27, 0, 0]).unwrap();
        assert_eq!(
            m,
            Incoming::Result {
                corr: 0x0201,
                resolution: Resolution::Rejected(0x08),
                payload: vec![0x10, 0x27, 0, 0]
            }
        );
        assert_eq!(
            Incoming::decode(&[0x03, 0, 0]),
            Err(DecodeError::UnknownRole(3))
        );
    }

    #[test]
    fn tlvs() {
        let mut b = Vec::new();
        put_tlv(&mut b, 0x01, true, &1_000_000u32.to_le_bytes());
        assert_eq!(b[0], 0x81);
        let t = parse_tlvs(&b).unwrap();
        assert_eq!(t[0].value, 1_000_000u32.to_le_bytes().to_vec());
        assert_eq!(parse_tlvs(&[0x10, 5, 1]), Err(DecodeError::TruncatedTlv));
    }

    #[test]
    fn corr_wraps_past_zero() {
        assert_eq!(next_corr(1), 2);
        assert_eq!(next_corr(u16::MAX), 1);
    }
}
