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

/// A console mechanism (oep-if-console §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    Sdi,
    Dmdata,
    Dmseq,
}

impl Mechanism {
    fn code(self) -> u8 {
        use console::enums::mechanism as m;
        match self {
            Mechanism::Sdi => m::SDI,
            Mechanism::Dmdata => m::DMDATA,
            Mechanism::Dmseq => m::DMSEQ,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Console { func: u16, stream: u16 },
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
    /// `oep.target.console` open on `connection` (the existing stream when it is already open).
    pub fn open_console(p: &mut Probe, connection: u16, mech: Mechanism) -> Result<Self, OepError> {
        let func = p.interface(console::NAME)?.func;
        let mut pl = connection.to_le_bytes().to_vec();
        pl.push(mech.code());
        let a = check(p.call(func, console::op::OPEN, pl)?)?;
        if a.len() < 3 {
            return Err(OepError::Malformed("console open answer too short".into()));
        }
        Ok(PosStream {
            kind: Kind::Console {
                func,
                stream: u16::from_le_bytes([a[0], a[1]]),
            },
            pos: 0,
        })
    }

    /// The fixture's UART (the first `oep.fixture.uart`), set to `baud`; returns the real baud.
    pub fn open_uart(p: &mut Probe, baud: u32) -> Result<(Self, u32), OepError> {
        let func = p.interface(fixture_uart::NAME)?.func;
        let s = PosStream {
            kind: Kind::Uart { func },
            pos: 0,
        };
        let real = s.configure_baud(p, baud)?;
        Ok((s, real))
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
        use console::enums::read_from as f;
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
        if a.len() < 9 {
            return Err(OepError::Malformed("stream read answer too short".into()));
        }
        let start = u64::from_le_bytes([a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]]);
        Ok((
            start,
            Chunk {
                data: a[9..].to_vec(),
                gap: 0,
                more: a[8] & 1 != 0,
            },
        ))
    }

    /// en: Place the reader at the last reset mark (so a monitor opened right after a flash starts
    /// with the target's first line), or at "now" when there is none.
    /// ja: 最後の reset の mark に読み手を置く(書き込み直後に開いた monitor が最初の行から読める)。
    /// mark が無ければ「今」。
    pub fn start_at_last_reset(&mut self, p: &mut Probe) -> Result<(), OepError> {
        let reset = console::enums::mark_kind::RESET;
        self.pos = match self.read(p, From::LastMark(reset), 0) {
            Ok((start, _)) => start,
            Err(_) => self.read(p, From::Now, 0)?.0,
        };
        Ok(())
    }

    /// Read what has come since the position (at most `max`), advancing it.
    pub fn poll(&mut self, p: &mut Probe, max: u16) -> Result<Chunk, OepError> {
        let (start, mut c) = self.read(p, From::Position(self.pos), max)?;
        c.gap = start.saturating_sub(self.pos);
        self.pos = start + c.data.len() as u64;
        Ok(c)
    }

    /// Send `data` to the target; returns how many bytes were taken (the rest is sent again later).
    pub fn write(&self, p: &mut Probe, data: &[u8]) -> Result<usize, OepError> {
        let (func, mut pl) = self.head();
        let op = match self.kind {
            Kind::Console { .. } => console::op::WRITE,
            Kind::Uart { .. } => fixture_uart::op::WRITE,
        };
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
