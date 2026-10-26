//! en: Position streams (oep-if-common §1): `oep.target.console` (a stream per connection and
//! mechanism) and `oep.fixture.uart` (the fixture's UART). Reading never consumes, so a reader
//! keeps its own position; bytes evicted before they were read show up as a gap.
//! ja: 位置付きのストリーム。`oep.target.console`(接続と mechanism ごと)と `oep.fixture.uart`。
//! 読んでも消えないので、読み手が自分の位置を持つ。読む前に追い出された分は gap になる。

use crate::registry::{fixture_uart, target_console as console};
use crate::session::{OepError, Probe, check};

/// Where a read starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum From {
    Position(u64),
    Oldest,
    Now,
    /// The last mark of this kind (0 = any).
    LastMark(u8),
}

/// en: A console mechanism (oep-if-console §1). Mechanisms are a named list (core §7.7): on the
/// wire a mechanism is its place (from 1) in the list the console fn declares in describe
/// (`mechanism`, one text TLV each), so the code is looked up per probe.
/// ja: console の mechanism。名前の一覧で、線上では describe が並べた中の位置(1 から)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    Sdi,
    Dmdata,
    Dmseq,
}

impl Mechanism {
    /// The mechanism of a standard name, if ch32rv knows it.
    pub fn from_name(name: &str) -> Option<Self> {
        use console::names::mechanism as m;
        match name {
            n if n == m::SDI => Some(Mechanism::Sdi),
            n if n == m::DMDATA => Some(Mechanism::Dmdata),
            n if n == m::DMSEQ => Some(Mechanism::Dmseq),
            _ => None,
        }
    }

    /// Its standard name (also how `--source` spells it).
    pub fn name(self) -> &'static str {
        use console::names::mechanism as m;
        match self {
            Mechanism::Sdi => m::SDI,
            Mechanism::Dmdata => m::DMDATA,
            Mechanism::Dmseq => m::DMSEQ,
        }
    }

    /// Its wire value on this probe: the place (from 1) of its name in the console fn's
    /// describe `mechanism` list; an error when the probe does not offer it.
    fn place(self, p: &mut Probe, func: u16) -> Result<u8, OepError> {
        let names: Vec<Vec<u8>> = p
            .describe(func)?
            .into_iter()
            .filter(|t| t.tag == console::tlvs::describe::MECHANISM)
            .map(|t| t.value)
            .collect();
        names
            .iter()
            .position(|n| n.as_slice() == self.name().as_bytes())
            .and_then(|i| u8::try_from(i + 1).ok())
            .ok_or_else(|| {
                OepError::Malformed(format!(
                    "the probe's console does not offer {}",
                    self.name()
                ))
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Console { func: u16, stream: u32 },
    Uart { func: u16 },
}

/// What one read brought.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chunk {
    pub data: Vec<u8>,
    /// Bytes lost before this chunk (evicted before they were read).
    pub gap: u64,
    /// More is buffered than one read carried.
    pub more: bool,
}

/// A stream with the reader's position.
#[derive(Debug, Clone)]
pub struct PosStream {
    kind: Kind,
    pub pos: u64,
}

impl PosStream {
    /// en: `oep.target.console` open on `connection`: a new stream (one per connection; a second
    /// open on it is refused, oep-if-console §2). conn(u32) mech(u8) -> stream(u32). A probe may
    /// offer several consoles; one that answers `unavailable` is passed over for the next
    /// (ESP32-P4 0.0.38: its console/0 refused every open while console/1 opened all three).
    /// ja: console を開く(接続ごとに 1 つ。2 つ目は断られる)。console が複数あれば、unavailable を
    /// 返すものは飛ばして次を試す。
    pub fn open_console(p: &mut Probe, connection: u32, mech: Mechanism) -> Result<Self, OepError> {
        let mut funcs: Vec<(u16, u16)> = p
            .list(console::NAME)?
            .into_iter()
            .filter(|i| i.name == console::NAME)
            .map(|i| (i.instance, i.func))
            .collect();
        funcs.sort_unstable();
        if funcs.is_empty() {
            funcs.push((0, p.interface(console::NAME)?.func));
        }
        let mut last = None;
        for (_, func) in funcs {
            match Self::open_console_on(p, func, connection, mech) {
                Err(OepError::Rejected { reason, payload })
                    if reason == crate::registry::reject_reasons::UNAVAILABLE =>
                {
                    last = Some(OepError::Rejected { reason, payload });
                }
                other => return other,
            }
        }
        Err(last.unwrap_or_else(|| OepError::NoInterface(console::NAME.into())))
    }

    fn open_console_on(
        p: &mut Probe,
        func: u16,
        connection: u32,
        mech: Mechanism,
    ) -> Result<Self, OepError> {
        let mut pl = connection.to_le_bytes().to_vec();
        pl.push(mech.place(p, func)?);
        let a = check(p.call(func, console::op::OPEN, pl)?)?;
        if a.len() < 4 {
            return Err(OepError::Malformed("console open answer too short".into()));
        }
        Ok(PosStream {
            kind: Kind::Console {
                func,
                stream: u32::from_le_bytes([a[0], a[1], a[2], a[3]]),
            },
            pos: 0,
        })
    }

    /// en: Whether this console stream is still open, by `streams` (lock-free, required,
    /// oep-if-console §1: only live streams are listed): `Some(false)` when it is no longer
    /// listed, `None` for a stream that is not a console's.
    /// ja: console のストリームがまだ開いているか(streams は生きているものだけを並べる)。
    pub fn is_open(&self, p: &mut Probe) -> Result<Option<bool>, OepError> {
        let Kind::Console { func, stream } = self.kind else {
            return Ok(None);
        };
        // first(u16) (oep-if-console §1, oep-spec 0098b56).
        let mut first: u16 = 0;
        for _ in 0..16 {
            let a = check(p.link().call(crate::link::Call {
                func,
                op: console::op::STREAMS,
                session: None,
                payload: first.to_le_bytes().to_vec(),
            })?)?;
            let (Some(&more), Some(&count)) = (a.first(), a.get(1)) else {
                return Err(OepError::Malformed("streams answer too short".into()));
            };
            // count x (stream(u32) connection(u32) mechanism(u8)): 9 bytes, live streams only.
            for k in 0..usize::from(count) {
                let Some(e) = a.get(2 + 9 * k..2 + 9 * (k + 1)) else {
                    break;
                };
                if u32::from_le_bytes([e[0], e[1], e[2], e[3]]) == stream {
                    return Ok(Some(true));
                }
            }
            first = first.saturating_add(u16::from(count));
            if more == 0 || count == 0 {
                break;
            }
        }
        Ok(Some(false))
    }

    /// en: Open the console again on `connection` (after a lost line was attached again). A new
    /// open is always a new stream number with a fresh buffer and no marks (numbers are never
    /// reused within a boot, core §9), so the reader starts at its oldest byte.
    /// ja: console を開き直す。番号は必ず新しく、バッファも mark も引き継がないので、最も古い所から読む。
    pub fn reopen_console(
        &mut self,
        p: &mut Probe,
        connection: u32,
        mech: Mechanism,
    ) -> Result<(), OepError> {
        let fresh = Self::open_console(p, connection, mech)?;
        self.kind = fresh.kind;
        self.pos = self.read(p, From::Oldest, 0)?.0;
        Ok(())
    }

    /// en: The fixture's UART, set to `baud`; returns the real baud. A probe may offer several
    /// `oep.fixture.uart`, and only one with pins planned takes a configure (the others answer
    /// unavailable), so each is tried in order and the first that takes it is used.
    /// ja: fixture の UART を `baud` にして開く。`oep.fixture.uart` が複数あり、configure を受けるのは
    /// ピンの plan のあるものだけ(ほかは unavailable)なので、順に試して最初に受けたものを使う。
    pub fn open_uart(
        p: &mut Probe,
        baud: u32,
        pins: Option<(u16, Option<u16>)>,
    ) -> Result<(Self, u32), OepError> {
        let uarts: Vec<u16> = p
            .list(fixture_uart::NAME)?
            .into_iter()
            .filter(|i| i.name == fixture_uart::NAME)
            .map(|i| i.func)
            .collect();
        if uarts.is_empty() {
            return Err(OepError::NoInterface(fixture_uart::NAME.to_owned()));
        }
        let plan = p
            .interface(crate::registry::probe_plan::NAME)
            .ok()
            .map(|i| i.func);
        let mut last = None;
        for func in uarts {
            // en: The UART takes data once the session plans its pins: an RX (role 1) and, where
            // the target's RX is wired, a TX (role 2) on another channel (oep-if-fixture §2: a
            // plan without TX receives only, a write is then unavailable; oep-if-plan §2.1). The
            // channels are the host's to know (the port's `rx` / `tx` labels).
            // ja: UART はピンを plan してはじめて動く。RX と(配線があれば)TX。TX の無い plan は受信だけ。
            if let (Some((rx, tx)), Some(plan)) = (pins, plan) {
                use crate::registry::{fixture_uart::enums::role, probe_plan};
                let mut pl = Vec::new();
                for (r, ch) in std::iter::once((role::RX, rx)).chain(tx.map(|t| (role::TX, t))) {
                    let mut v = func.to_le_bytes().to_vec();
                    v.push(r);
                    v.extend_from_slice(&ch.to_le_bytes());
                    crate::codec::put_tlv(
                        &mut pl,
                        probe_plan::tlvs::plan_apply::ROLE_ASSIGNMENT,
                        false,
                        &v,
                    );
                }
                if let Err(e) = check(p.call(plan, probe_plan::op::PLAN_APPLY, pl)?) {
                    last = Some(e);
                    continue;
                }
            }
            let s = PosStream {
                kind: Kind::Uart { func },
                pos: 0,
            };
            match s.configure_baud(p, baud) {
                Ok(real) => return Ok((s, real)),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| OepError::NoInterface(fixture_uart::NAME.to_owned())))
    }

    /// `oep.fixture.uart` configure (the UART's speed is set only here, never by a line coding).
    pub fn configure_baud(&self, p: &mut Probe, baud: u32) -> Result<u32, OepError> {
        let Kind::Uart { func } = self.kind else {
            return Ok(baud);
        };
        let a = check(p.call(
            func,
            fixture_uart::op::CONFIGURE,
            baud.to_le_bytes().to_vec(),
        )?)?;
        Ok(a.get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(baud))
    }

    fn head(&self) -> (u16, Vec<u8>) {
        match self.kind {
            Kind::Console { func, stream } => (func, stream.to_le_bytes().to_vec()),
            Kind::Uart { func } => (func, Vec::new()),
        }
    }

    fn read_op(&self) -> u8 {
        match self.kind {
            Kind::Console { .. } => console::op::READ,
            Kind::Uart { .. } => fixture_uart::op::READ,
        }
    }

    /// One read from `from`, at most `max` bytes. Lock-free (sent without the session).
    pub fn read(&self, p: &mut Probe, from: From, max: u16) -> Result<(u64, Chunk), OepError> {
        use crate::registry::common::enum_::read_from as f;
        let (func, mut pl) = self.head();
        let (code, arg) = match from {
            From::Position(x) => (f::POSITION, x),
            From::Oldest => (f::OLDEST, 0),
            From::Now => (f::NOW, 0),
            From::LastMark(k) => (f::LAST_MARK, u64::from(k)),
        };
        pl.push(code);
        pl.extend_from_slice(&arg.to_le_bytes());
        pl.extend_from_slice(&max.to_le_bytes());
        let a = check(p.call(func, self.read_op(), pl)?)?;
        // start(u64), flags(u8), len(u16), data, [TLV] (oep-if-common §1.2)
        let len = a
            .get(9..11)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])));
        let (Some(len), Some(_)) = (len, a.get(..11)) else {
            return Err(OepError::Malformed("stream read answer too short".into()));
        };
        let Some(data) = a.get(11..11 + len) else {
            return Err(OepError::Malformed(
                "stream read answer shorter than its len".into(),
            ));
        };
        let start = u64::from_le_bytes([a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]]);
        Ok((
            start,
            Chunk {
                data: data.to_vec(),
                gap: 0,
                more: a[8] & crate::registry::common::enum_::read_flags::MORE != 0,
            },
        ))
    }

    /// en: Place the reader at the last reset mark (so a monitor opened right after a flash starts
    /// with the target's first line), or at "now" when there is none. Only a console has reset
    /// marks (kind 0x40, oep-if-console), and only when the probe offers marks (an optional op):
    /// without them, and on a fixture UART, the reader starts now. `read(from 3)` answers "now"
    /// when no mark of the kind exists (oep-if-common §1.2).
    /// ja: 最後の reset の mark に置く。mark は console だけで、marks を持つ probe だけ。無ければ「今」。
    pub fn start_at_last_reset(&mut self, p: &mut Probe) -> Result<(), OepError> {
        let from = match self.kind {
            Kind::Console { func, .. } if p.offers(func, console::op::MARKS)? => {
                From::LastMark(console::enums::mark_kind::RESET)
            }
            _ => From::Now,
        };
        self.pos = self.read(p, from, 0)?.0;
        Ok(())
    }

    /// Read what has come since the position (at most `max`), advancing it.
    pub fn poll(&mut self, p: &mut Probe, max: u16) -> Result<Chunk, OepError> {
        // A read changes nothing: one whose answer was lost on the way (a broker answers
        // result_lost then) is simply asked again on the next poll. A closed stream answers
        // no_resource (oep-if-console §1): the caller learns it from the error.
        let (start, mut c) = match self.read(p, From::Position(self.pos), max) {
            Err(OepError::Rejected { reason, .. })
                if reason == crate::registry::reject_reasons::RESULT_LOST =>
            {
                return Ok(Chunk {
                    data: Vec::new(),
                    gap: 0,
                    more: false,
                });
            }
            r => r?,
        };
        c.gap = start.saturating_sub(self.pos);
        self.pos = start + c.data.len() as u64;
        Ok(c)
    }

    /// en: Send `data` to the target; returns how many bytes were taken (the rest is sent again
    /// later). `write` is optional (oep-if-console §1, oep-if-fixture §2): a probe that does not
    /// offer it takes no input - an error the caller reports. At most what fits max_frame
    /// (header 10, stream 4, count 2) goes in one request.
    /// ja: 入力を送る。write は任意の op で、無ければ入力を受けない(エラー)。max_frame に入る分だけ。
    pub fn write(&self, p: &mut Probe, data: &[u8]) -> Result<usize, OepError> {
        let (func, mut pl) = self.head();
        let op = match self.kind {
            Kind::Console { .. } => console::op::WRITE,
            Kind::Uart { .. } => fixture_uart::op::WRITE,
        };
        if !p.offers(func, op)? {
            return Err(OepError::Malformed(
                "the probe takes no input on this stream (no write op)".into(),
            ));
        }
        let room = usize::from(p.limits().max_frame).saturating_sub(10 + pl.len() + 2);
        let data = &data[..data.len().min(room)];
        pl.extend_from_slice(&(data.len() as u16).to_le_bytes());
        pl.extend_from_slice(data);
        let r = p.call(func, op, pl)?;
        // Not all taken is completed partial: still an answer with `accepted`.
        let a = match r.resolution {
            crate::codec::Resolution::Completed(_) => r.payload,
            _ => check(r)?,
        };
        Ok(a.get(..2)
            .map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
            .unwrap_or(0))
    }
}
