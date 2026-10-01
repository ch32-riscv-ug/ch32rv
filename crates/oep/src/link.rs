//! en: The OEP link: one byte stream to one probe, framed, with the core §5 rules - corr per
//! request, one write per frame (and per pipelined batch), results routed by role and corr,
//! pushes queued, `max_inflight` / `window` respected, a lost or broken answer resent once with
//! the same corr, and the length-framing resync (quiet, then confirm). docs/oep-host.ja.md §3.4.
//!
//! ja: OEP の link。1 本の byte stream に framing を掛け、core §5 の規則(要求ごとの corr、1 フレーム
//! 1 write、role と corr での振り分け、push の保留、max_inflight / window、失われた応答は同じ corr で
//! 1 回だけ送り直す、長さ見出しの resync)を守る。

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::codec::{
    FramingLost, Incoming, LengthDeframer, Request, Resolution, SerialDeframer, length_frame,
    next_corr, serial_frame,
};
use crate::registry::{self, constants, core, timing};

/// A byte stream to a probe. `read` returns 0 bytes when nothing arrived within `timeout`.
pub trait ByteStream: Send {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

impl ByteStream for TcpStream {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        Write::write_all(self, data)
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        match Read::read(self, buf) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed",
            )),
            Ok(n) => Ok(n),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

/// How messages are framed on the stream (docs/oep-host.ja.md §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Serial-visible transports: COBS + CRC-16, 0x00-delimited. Needs no resync.
    Cobs,
    /// Vendor bulk, HID's inner stream, TCP: `length(u16) message`.
    Length,
}

/// What confirm reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub revision: u8,
    /// The largest message the probe accepts (and sends).
    pub max_frame: u16,
    /// The most message bytes that may be outstanding.
    pub window: u32,
    /// The most requests that may be outstanding.
    pub max_inflight: u8,
    /// Changes on every boot of the probe (core §5.2): a host without the lock learns a restart.
    pub boot_id: u32,
}

/// One request to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub func: u16,
    pub op: u8,
    pub session: Option<u32>,
    pub payload: Vec<u8>,
}

/// The probe's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub resolution: Resolution,
    pub payload: Vec<u8>,
}

impl Reply {
    /// Completed with outcome success.
    pub fn succeeded(&self) -> bool {
        self.resolution == Resolution::Completed(registry::outcomes::SUCCESS)
    }
}

/// A push from the probe (event or data), kept until the caller takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Push {
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

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("transport: {0}")]
    Io(#[from] io::Error),
    #[error("no answer from the probe within {0:?}")]
    Timeout(Duration),
    #[error("the probe does not speak OEP v1: {0}")]
    NotOep(String),
    #[error("request of {0} bytes exceeds the probe's max_frame {1}")]
    TooLarge(usize, u16),
}

/// Reply wait before confirm and for ordinary requests.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

pub struct Link {
    stream: Box<dyn ByteStream>,
    framing: Framing,
    cobs: SerialDeframer,
    length: LengthDeframer,
    corr: u16,
    limits: Option<Limits>,
    timeout: Duration,
    pushes: VecDeque<Push>,
    /// Messages decoded but not yet consumed.
    inbox: VecDeque<Vec<u8>>,
    /// Frames dropped as broken or unknown (diagnostics).
    pub dropped: u64,
    /// Resyncs performed (diagnostics).
    pub resyncs: u64,
}

impl Link {
    pub fn new(stream: Box<dyn ByteStream>, framing: Framing) -> Self {
        Link {
            stream,
            framing,
            cobs: SerialDeframer::new(0xFFFF),
            length: LengthDeframer::new(0xFFFF),
            corr: 0,
            limits: None,
            timeout: DEFAULT_TIMEOUT,
            pushes: VecDeque::new(),
            inbox: VecDeque::new(),
            dropped: 0,
            resyncs: 0,
        }
    }

    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
    }

    pub fn limits(&self) -> Option<Limits> {
        self.limits
    }

    /// Restart corr numbering (the probe drops its resend table on every open, core §5.2).
    pub fn reset_corr(&mut self) {
        self.corr = 0;
    }

    /// Take the pushes received so far.
    pub fn take_pushes(&mut self) -> Vec<Push> {
        self.pushes.drain(..).collect()
    }

    /// `confirm`: learn the revision and the limits. Refuses a probe that is not v1.
    pub fn confirm(&mut self) -> Result<Limits, LinkError> {
        let mut payload = constants::CONFIRM_REQUEST_MAGIC.as_bytes().to_vec();
        payload.extend_from_slice(&[registry::PROTOCOL_REVISION, registry::PROTOCOL_REVISION]);
        let r = self.call(Call {
            func: core::FN,
            op: core::op::CONFIRM,
            session: None,
            payload,
        })?;
        let p = &r.payload;
        if !r.succeeded() {
            return Err(LinkError::NotOep(format!(
                "confirm answered {:?} (a probe that refuses the revision range is not v1)",
                r.resolution
            )));
        }
        // "OEP!", revision, flags, max_frame(u16), window(u32), max_inflight, boot_id(u32), [TLV]
        if p.len() < 17 || &p[..4] != constants::CONFIRM_RESULT_MAGIC.as_bytes() {
            return Err(LinkError::NotOep(
                "confirm answer is not \"OEP!\"".to_owned(),
            ));
        }
        let limits = Limits {
            revision: p[4],
            max_frame: u16::from_le_bytes([p[6], p[7]]),
            window: u32::from_le_bytes([p[8], p[9], p[10], p[11]]),
            max_inflight: p[12].max(1),
            boot_id: u32::from_le_bytes([p[13], p[14], p[15], p[16]]),
        };
        if limits.revision < 1 {
            return Err(LinkError::NotOep(format!(
                "revision {} (ch32rv needs 1)",
                limits.revision
            )));
        }
        self.cobs.set_max(usize::from(limits.max_frame));
        self.length.set_max(usize::from(limits.max_frame));
        self.limits = Some(limits);
        Ok(limits)
    }

    /// One request, one answer.
    pub fn call(&mut self, call: Call) -> Result<Reply, LinkError> {
        let mut r = self.exchange(vec![call])?;
        r.pop().ok_or(LinkError::Timeout(self.timeout))
    }

    /// en: Several requests pipelined (the probe processes them in order); answers in the same
    /// order. Admitted requests go out in one write. An answer that does not come is waited for,
    /// then the unanswered tail is sent once more with the same corrs (after a resync on length
    /// framing); a second loss is a timeout.
    /// ja: 複数の要求を pipeline で送り、同じ順で答えを返す。答えが来なければ、答えの無い後ろを同じ
    /// corr で 1 回だけ送り直す(長さ見出しでは resync の後)。2 回目も失えば timeout。
    pub fn exchange(&mut self, calls: Vec<Call>) -> Result<Vec<Reply>, LinkError> {
        let max_frame = self.limits.map_or(0xFFFF, |l| l.max_frame);
        let mut reqs = Vec::with_capacity(calls.len());
        for c in calls {
            self.corr = next_corr(self.corr);
            let req = Request {
                corr: self.corr,
                func: c.func,
                op: c.op,
                session: c.session,
                payload: c.payload,
            };
            let len = req.encode().len();
            if len > usize::from(max_frame) {
                return Err(LinkError::TooLarge(len, max_frame));
            }
            reqs.push(req);
        }
        let mut replies: Vec<Option<Reply>> = vec![None; reqs.len()];
        let mut resent = false;
        loop {
            match self.pump(&reqs, &mut replies) {
                Ok(()) => break,
                Err(LinkError::Timeout(_)) if !resent => {
                    resent = true;
                    if self.framing == Framing::Length {
                        self.resync()?;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        replies
            .into_iter()
            .map(|r| r.ok_or(LinkError::Timeout(self.timeout)))
            .collect()
    }

    /// Send the requests that have no answer yet, within the limits, and collect answers.
    fn pump(&mut self, reqs: &[Request], replies: &mut [Option<Reply>]) -> Result<(), LinkError> {
        let (window, inflight) = self.limits.map_or((usize::MAX, 1), |l| {
            (l.window as usize, usize::from(l.max_inflight))
        });
        let todo: Vec<usize> = (0..reqs.len()).filter(|&i| replies[i].is_none()).collect();
        let mut next = 0; // index into `todo` of the next request to send
        let mut outstanding: VecDeque<(usize, usize)> = VecDeque::new(); // (req index, size)
        let mut bytes_out = 0usize;
        while next < todo.len() || !outstanding.is_empty() {
            // Admit as many as the limits allow, in one write.
            let mut batch = Vec::new();
            while next < todo.len() && outstanding.len() < inflight {
                let i = todo[next];
                let msg = reqs[i].encode();
                if !outstanding.is_empty() && bytes_out + msg.len() > window {
                    break;
                }
                bytes_out += msg.len();
                outstanding.push_back((i, msg.len()));
                batch.extend(match self.framing {
                    Framing::Cobs => serial_frame(&msg),
                    Framing::Length => length_frame(&msg),
                });
                next += 1;
            }
            if !batch.is_empty() {
                self.stream.write_all(&batch)?;
            }
            // Wait for the oldest outstanding answer.
            let Some(&(i, size)) = outstanding.front() else {
                continue;
            };
            let reply = self.wait_result(reqs[i].corr)?;
            replies[i] = Some(reply);
            outstanding.pop_front();
            bytes_out -= size;
        }
        Ok(())
    }

    /// Read until the result for `corr` arrives. Pushes are queued; results for other corrs (a
    /// late answer to an earlier try) are dropped.
    fn wait_result(&mut self, corr: u16) -> Result<Reply, LinkError> {
        let deadline = Instant::now() + self.timeout;
        loop {
            while let Some(m) = self.inbox.pop_front() {
                match Incoming::decode(&m) {
                    Ok(Incoming::Result {
                        corr: c,
                        resolution,
                        payload,
                    }) if c == corr => {
                        return Ok(Reply {
                            resolution,
                            payload,
                        });
                    }
                    Ok(Incoming::Result { .. }) => self.dropped += 1,
                    Ok(Incoming::Event {
                        func,
                        seq,
                        kind,
                        payload,
                    }) => self.pushes.push_back(Push::Event {
                        func,
                        seq,
                        kind,
                        payload,
                    }),
                    Ok(Incoming::Data { func, seq, payload }) => {
                        self.pushes.push_back(Push::Data { func, seq, payload })
                    }
                    Err(_) => self.dropped += 1,
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(LinkError::Timeout(self.timeout));
            }
            self.fill((deadline - now).min(Duration::from_millis(20)))?;
        }
    }

    /// Read once and deframe into the inbox.
    fn fill(&mut self, wait: Duration) -> Result<(), LinkError> {
        let mut buf = [0u8; 4096];
        let n = self.stream.read_timeout(&mut buf, wait)?;
        if n == 0 {
            return Ok(());
        }
        match self.framing {
            Framing::Cobs => {
                let before = self.cobs.dropped;
                self.inbox.extend(self.cobs.push(&buf[..n]));
                self.dropped += self.cobs.dropped - before;
            }
            Framing::Length => match self.length.push(&buf[..n]) {
                Ok(msgs) => self.inbox.extend(msgs),
                Err(FramingLost::TooLong(_)) => {
                    self.dropped += 1;
                    self.resync()?;
                }
            },
        }
        Ok(())
    }

    /// en: Length framing lost its boundaries (core §5.1): discard input until it has been quiet
    /// for `resync_quiet_ms`, then confirm with a fresh corr until its answer comes back. Pushes
    /// that keep the input busy are not stopped here (the session layer sends the blind
    /// unsubscribe / end, which it alone can address).
    /// ja: 長さ見出しの境界を失った: 50 ms 静かになるまで捨て、新しい corr で confirm を送り、その答えを
    /// 待つ(最大 3 回)。
    pub fn resync(&mut self) -> Result<(), LinkError> {
        self.resyncs += 1;
        let quiet = Duration::from_millis(u64::from(timing::RESYNC_QUIET_MS));
        for _ in 0..3 {
            let give_up = Instant::now() + Duration::from_secs(1);
            let mut last = Instant::now();
            let mut buf = [0u8; 4096];
            while Instant::now() < give_up && last.elapsed() < quiet {
                if self
                    .stream
                    .read_timeout(&mut buf, Duration::from_millis(10))?
                    > 0
                {
                    last = Instant::now();
                }
            }
            self.length.reset();
            self.inbox.clear();
            self.corr = next_corr(self.corr);
            let mut payload = constants::CONFIRM_REQUEST_MAGIC.as_bytes().to_vec();
            payload.extend_from_slice(&[0, 0xFF]);
            let req = Request {
                corr: self.corr,
                func: core::FN,
                op: core::op::CONFIRM,
                session: None,
                payload,
            };
            self.stream.write_all(&length_frame(&req.encode()))?;
            if self.wait_result(req.corr).is_ok() {
                return Ok(());
            }
        }
        Err(LinkError::Timeout(self.timeout))
    }
}

/// A serial port as an OEP transport (COBS framing).
struct SerialStream(Box<dyn serialport::SerialPort>);

impl ByteStream for SerialStream {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        Write::write_all(&mut self.0, data)?;
        self.0.flush()
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.0
            .set_timeout(timeout.max(Duration::from_millis(1)))
            .map_err(io::Error::other)?;
        match Read::read(&mut self.0, buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
}

/// en: Open a probe's serial port (USB CDC, USB-Serial/JTAG, UART bridge) as a COBS link: 115200
/// (fixed for UART bridges, a number only on USB), exclusive (the serialport crate sets TIOCEXCL on
/// unix; Windows is exclusive anyway), DTR and RTS asserted like the reference client, so a classic
/// ESP32 bridge does not reboot. Modem lines a port does not have (a pty) are not an error.
/// ja: probe の serial port を COBS の link として開く。115200、排他、DTR / RTS は立てる(classic
/// ESP32 の bridge を再起動させない)。modem 線の無い port(pty)でも失敗にしない。
pub fn open_serial(path: &str) -> Result<Link, LinkError> {
    let mut port = serialport::new(path, 115_200)
        .timeout(Duration::from_millis(20))
        .open()
        .map_err(|e| LinkError::Io(io::Error::other(format!("open {path}: {e}"))))?;
    let _ = port.write_data_terminal_ready(true);
    let _ = port.write_request_to_send(true);
    Ok(Link::new(Box::new(SerialStream(port)), Framing::Cobs))
}

/// Connect to an OEP endpoint over TCP (`length(u16) message`): a probe's TCP transport, or the
/// broker of a ch32rv that holds the probe.
pub fn open_tcp(addr: &str) -> Result<Link, LinkError> {
    let s = TcpStream::connect(addr)?;
    s.set_nodelay(true)?;
    Ok(Link::new(Box::new(s), Framing::Length))
}
