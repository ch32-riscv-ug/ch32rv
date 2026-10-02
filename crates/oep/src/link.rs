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
    /// The host side's line speed (a serial port; others have none).
    fn set_baud(&mut self, _baud: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this stream has no line speed",
        ))
    }
    /// Drop what the OS has received and not read yet.
    fn clear_input(&mut self) {}
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
    /// A frame broke while an answer was awaited (COBS / CRC): it was that answer, resend at once.
    #[error("a frame broke on the way")]
    Broken,
}

/// The most answer bytes a serial link keeps in flight at once (in-flight requests x max_frame).
pub const ANSWER_BURST_MAX: usize = 6144;

/// Reply wait before confirm and for ordinary requests.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// en: Reply wait on a TCP link (a broker, or a probe reached over TCP). TCP loses nothing, so an
/// answer that is late is still coming: behind the broker it may wait out a lost answer on the
/// probe's serial link (3 s) and the resend, so this must be longer than that (a client that gave
/// up first made the broker's late answer meet a resent corr: `result_lost`, V003 jig over a
/// CP2102, 2026-10-01).
/// ja: TCP の link の応答待ち。TCP は失わないので、遅い答えはまだ来る途中。ブローカーの裏で probe の
/// serial の失われた答え(3 秒)と再送を待つことがあるので、それより長くする。
pub const TCP_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// Requests sent again after their answer did not come (diagnostics).
    pub resends: u64,
    /// COBS frames that broke on the line (bad encoding or CRC), a part of `dropped`.
    pub broken: u64,
    /// Frames that decoded (answers and pushes): the "good" of oep-spec's host guide §7.4.
    pub good: u64,
    /// Requests whose answer did not come within the wait (the guide's "lost").
    pub lost: u64,
    /// Whether a lost answer is waited for and the request sent again once: not on a lossless
    /// stream (TCP), where a missing answer is only late.
    resend: bool,
    /// en: Resend at once when a frame breaks while an answer is awaited, instead of waiting out
    /// the timeout. Right only while this host's session holds the port (then every frame on it is
    /// an answer or a push, never the probe's raw output, oep-core §3.5).
    /// ja: 答えを待つ間にフレームが壊れたら、時間切れを待たずにすぐ送り直す。session が口を持っている
    /// 間だけ正しい(その口のフレームは答えか通知で、raw の出力ではない)。
    pub resend_on_broken: bool,
    /// The serial port's speed at the probe's boot (every revert's target) and now.
    base_baud: Option<u32>,
    baud: Option<u32>,
    /// At most this many requests outstanding (0: as the probe allows): a raised speed that broke
    /// while both ways carried at once is used one request at a time.
    pub inflight_cap: usize,
    /// Times the probe was found back at the boot speed by itself (diagnostics; the owner of the
    /// link then stays there for the session, oep-core §3.5).
    pub speed_fallbacks: u64,
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
            resends: 0,
            broken: 0,
            good: 0,
            lost: 0,
            resend: true,
            resend_on_broken: false,
            base_baud: None,
            baud: None,
            inflight_cap: 0,
            speed_fallbacks: 0,
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
        // en: A serial port just opened may find the probe still at a speed the previous host
        // raised it to (that host gone): it goes back after at most port_speed_idle_max_ms of
        // silence, so confirm is tried that long and a bit (oep-core §3.5). Not for a quick look
        // (discovery's short timeout).
        // ja: 開いたばかりの serial では、前の host が上げた速さが残っていることがある。probe は
        // 最長 3 秒の黙りで戻るので、その間 confirm を繰り返す。discovery の短い待ちでは行わない。
        if self.framing == Framing::Cobs
            && self.base_baud.is_some()
            && self.timeout >= Duration::from_secs(1)
        {
            let deadline = Instant::now()
                + Duration::from_millis(u64::from(timing::PORT_SPEED_IDLE_MAX_MS) + 1000);
            while !self.confirm_raw(Duration::from_millis(400)) && Instant::now() < deadline {}
        }
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
        // Looks at the boot speed after a raised one stopped answering (once: then it stays there).
        let mut fallbacks = 0;
        // en: A broken frame starts the resend once; after that the answers are waited for to
        // the timeout, more broken frames or not (a noisy line breaks several in a row: a second
        // one must not fail the request, V003 jig at 115200, 2026-10-01).
        // ja: 壊れたフレームで送り直すのは 1 回。その後は壊れたフレームが続いても時間切れまで待つ。
        let on_broken = self.resend_on_broken;
        let r = self.exchange_inner(&reqs, &mut replies, &mut resent, &mut fallbacks);
        self.resend_on_broken = on_broken;
        r?;
        replies
            .into_iter()
            .map(|r| r.ok_or(LinkError::Timeout(self.timeout)))
            .collect()
    }

    fn exchange_inner(
        &mut self,
        reqs: &[Request],
        replies: &mut [Option<Reply>],
        resent: &mut bool,
        fallbacks: &mut u32,
    ) -> Result<(), LinkError> {
        loop {
            let r = self.pump(reqs, replies);
            if matches!(r, Err(LinkError::Timeout(_))) {
                self.lost += 1;
            }
            match r {
                Ok(()) => return Ok(()),
                // en: Above the boot speed and no answer (oep-core §3.5, host duty 5): back to the
                // boot speed, confirm until port_speed_idle_max_ms + 1000 ms. It converges either
                // way - a probe still up there takes these confirms as broken and goes back after
                // three - and passing, the rest is sent again there for the session. Not passing
                // is a link failure (no going back up to wait again).
                // ja: 上げた速さで答えが無い(義務 5): 起動時の速さに戻り、上限まで confirm を繰り返す。
                // probe がまだ上に居ても、この confirm が壊れ 3 つになって戻るので収束する。通れば残りを
                // そこで送り直す。通らなければリンクの失敗(上げた速さに戻って待ち直さない)。
                Err(LinkError::Timeout(_)) if *fallbacks < 1 && self.baud != self.base_baud => {
                    *fallbacks += 1;
                    let wait =
                        Duration::from_millis(u64::from(timing::PORT_SPEED_IDLE_MAX_MS) + 1000);
                    if !self.back_to_base(wait) {
                        return Err(LinkError::Timeout(wait));
                    }
                    self.speed_fallbacks += 1;
                }
                Err(LinkError::Timeout(_) | LinkError::Broken) if !*resent && self.resend => {
                    *resent = true;
                    self.resends += 1;
                    self.resend_on_broken = false;
                    if self.framing == Framing::Length {
                        self.resync()?;
                    }
                }
                Err(LinkError::Broken) => return Err(LinkError::Timeout(self.timeout)),
                Err(e) => return Err(e),
            }
        }
    }

    /// Send the requests that have no answer yet, within the limits, and collect answers.
    fn pump(&mut self, reqs: &[Request], replies: &mut [Option<Reply>]) -> Result<(), LinkError> {
        let (window, mut inflight) = self.limits.map_or((usize::MAX, 1), |l| {
            (l.window as usize, usize::from(l.max_inflight))
        });
        if self.inflight_cap > 0 {
            inflight = inflight.min(self.inflight_cap);
        }
        // en: On a serial port, keep the answers that may come back at once within
        // ANSWER_BURST_MAX (oep-core §3.4): the probe's limits are what it can take, not what the
        // host's tty path can - Linux's cdc_acm dropped 20-30 % of 8 x 1008-byte answers in
        // flight on an ESP32-P4's HS CDC, none at 7 (dev-oep, 2026-10-02).
        // ja: serial の口では、同時に返りうる答えの量を ANSWER_BURST_MAX 以内にする(probe の上限は
        // probe の受けの上限で、host の tty の経路の上限ではない)。
        if self.framing == Framing::Cobs {
            let frame = self.limits.map_or(1, |l| usize::from(l.max_frame).max(1));
            inflight = inflight.min((ANSWER_BURST_MAX / frame).max(1));
        }
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
        let broken_at_start = self.cobs.dropped;
        loop {
            if self.resend_on_broken
                && self.framing == Framing::Cobs
                && self.cobs.dropped != broken_at_start
                && self.inbox.is_empty()
            {
                return Err(LinkError::Broken);
            }
            while let Some(m) = self.inbox.pop_front() {
                self.good += 1;
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
                self.broken += self.cobs.dropped - before;
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

    /// The serial port's speed at the probe's boot, when this link is a serial port.
    pub fn base_baud(&self) -> Option<u32> {
        self.base_baud
    }

    /// The serial port's speed now.
    pub fn baud(&self) -> Option<u32> {
        self.baud
    }

    /// en: The host side of the serial port to `rate`: after the switch, 20 ms for both ends to
    /// settle (the bytes in flight meanwhile break, oep-core §3.5), then what was read dropped.
    /// ja: host 側の serial の速さを変える。両端が落ち着くまで 20 ms 待ち、読んだものを捨てる。
    pub fn set_baud(&mut self, rate: u32) -> Result<(), LinkError> {
        self.stream.set_baud(rate)?;
        self.baud = Some(rate);
        std::thread::sleep(Duration::from_millis(20));
        self.stream.clear_input();
        self.cobs = SerialDeframer::new(self.limits.map_or(0xFFFF, |l| usize::from(l.max_frame)));
        self.inbox.clear();
        Ok(())
    }

    /// A confirm straight on the link: true when its answer came within `timeout`.
    pub fn confirm_raw(&mut self, timeout: Duration) -> bool {
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
        let frame = match self.framing {
            Framing::Cobs => serial_frame(&req.encode()),
            Framing::Length => length_frame(&req.encode()),
        };
        if self.stream.write_all(&frame).is_err() {
            return false;
        }
        let saved = (self.timeout, self.resend_on_broken);
        self.timeout = timeout;
        self.resend_on_broken = false;
        let ok = self.wait_result(req.corr).is_ok();
        (self.timeout, self.resend_on_broken) = saved;
        ok
    }

    /// en: Back at the boot speed, confirmed there: confirms every 0.25 s up to `wait` (a probe
    /// still trying waits out its verify_ms). ja: 起動時の速さに戻り、confirm で確かめる。
    pub fn back_to_base(&mut self, wait: Duration) -> bool {
        self.inflight_cap = 0;
        let Some(base) = self.base_baud else {
            return false;
        };
        if self.set_baud(base).is_err() {
            return false;
        }
        let deadline = Instant::now() + wait;
        loop {
            if self.confirm_raw(Duration::from_millis(250)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// en: Requests as they are, pipelined `inflight` at a time, with no resend: the answers that
    /// came, in order, up to the first that did not (a measuring tool: port_speed's verify).
    /// ja: 要求をそのまま `inflight` 本ずつ送り、送り直さない。来た答えを順に、最初に来なかった所まで返す。
    pub fn exchange_once(
        &mut self,
        calls: Vec<Call>,
        inflight: usize,
        timeout: Duration,
    ) -> Vec<Reply> {
        let saved = (
            self.timeout,
            self.resend,
            self.inflight_cap,
            self.resend_on_broken,
        );
        self.timeout = timeout;
        self.resend = false;
        self.inflight_cap = inflight.max(1);
        self.resend_on_broken = true;
        let mut reqs = Vec::with_capacity(calls.len());
        for c in calls {
            self.corr = next_corr(self.corr);
            reqs.push(Request {
                corr: self.corr,
                func: c.func,
                op: c.op,
                session: c.session,
                payload: c.payload,
            });
        }
        let mut replies: Vec<Option<Reply>> = vec![None; reqs.len()];
        let _ = self.pump(&reqs, &mut replies);
        (
            self.timeout,
            self.resend,
            self.inflight_cap,
            self.resend_on_broken,
        ) = saved;
        replies.into_iter().map_while(|r| r).collect()
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

    fn set_baud(&mut self, baud: u32) -> io::Result<()> {
        self.0.set_baud_rate(baud).map_err(io::Error::other)
    }

    fn clear_input(&mut self) {
        let _ = self.0.clear(serialport::ClearBuffer::Input);
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
    let mut link = Link::new(Box::new(SerialStream(port)), Framing::Cobs);
    link.base_baud = Some(115_200);
    link.baud = Some(115_200);
    Ok(link)
}

/// Connect to an OEP endpoint over TCP (`length(u16) message`): a probe's TCP transport, or the
/// broker of a ch32rv that holds the probe.
pub fn open_tcp(addr: &str) -> Result<Link, LinkError> {
    let s = TcpStream::connect(addr)?;
    s.set_nodelay(true)?;
    let mut link = Link::new(Box::new(s), Framing::Length);
    link.timeout = TCP_TIMEOUT;
    link.resend = false;
    Ok(link)
}
