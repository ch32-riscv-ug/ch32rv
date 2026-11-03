//! en: The one lock the broker answers with as a probe, for the clients that do not share it
//! (oep-core §5.1, §6): the session table (§6.2), the lease (§6.1), and the one resend state - the
//! last session's last corr and a bounded record of its requests and answers. The ch32rv tools
//! that open with `SHARE_EXTENSION` share the lock instead (host guide §7) and are kept by the
//! broker itself; while they do, an open from anyone else is locked, and the other way round.
//! ja: 相乗りしない client に、ブローカーが probe として答える 1 つのロック(§6.2 の表、lease、
//! 再送の状態: 最後の session の最後の corr と、要求と答えの記録)。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The requests and answers kept for resends (core §5.1 lets a probe keep any number, even 0).
const HISTORY: usize = 64;
/// `corr_half_range` (core §5.1): a corr this far or farther from the last is not new.
const HALF: u16 = 0x8000;

/// The session that holds the lock.
pub(crate) struct Holder {
    pub sid: u32,
    /// The client that opened it: its resources go when the session ends (core §9).
    pub client: u64,
    lease: Duration,
    deadline: Instant,
    owner: Option<Vec<u8>>,
}

struct Record {
    corr: u16,
    request: Vec<u8>,
    answer: Option<Vec<u8>>,
}

/// What a resend check says of a request under the last session (core §5.1, §4.3 step 2).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Replay {
    /// Not a resend: go on (and [`CoreLock::note`] it once it passes the session check).
    New,
    /// The same request again: its recorded answer.
    Stored(Vec<u8>),
    /// result_lost: an old corr, or one whose answer is not kept.
    Lost,
    /// The same corr with another request.
    Malformed,
}

/// What the session table says (core §6.2).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Access {
    /// The request is the holder's: serve it.
    Owner,
    NoSession,
    /// Another session holds the lock: `remaining_ms` and the owner, for the payload.
    Locked(u32, Option<Vec<u8>>),
}

/// An open's outcome.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Opened {
    /// The lock is this session's, with this lease; `stolen` names the client whose session a
    /// force ended (its resources are to be released).
    Held {
        lease_ms: u32,
        stolen: Option<u64>,
    },
    Refused(Access),
}

#[derive(Default)]
pub(crate) struct CoreLock {
    pub holder: Option<Holder>,
    /// S (core §6.2): the session that holds, or last held, the lock; the resend state is its.
    last: Option<u32>,
    /// S ended (end, lapse or force): its id opens no new session.
    ended: bool,
    last_corr: Option<u16>,
    records: VecDeque<Record>,
}

impl CoreLock {
    /// End a lapsed lease (core §6.1): the client whose resources are to be released.
    pub fn expire(&mut self, now: Instant) -> Option<u64> {
        if self.holder.as_ref().is_some_and(|h| h.deadline <= now) {
            return self.end();
        }
        None
    }

    /// End the holder's session (end, or a lapse): the client whose resources are to go.
    pub fn end(&mut self) -> Option<u64> {
        let h = self.holder.take()?;
        self.ended = true;
        Some(h.client)
    }

    /// `remaining_ms` and the owner while the lock is held (lock_state, locked's payload).
    pub fn state(&self, now: Instant) -> Option<(u32, Option<Vec<u8>>)> {
        self.holder.as_ref().map(|h| {
            let ms = h.deadline.saturating_duration_since(now).as_millis();
            (u32::try_from(ms).unwrap_or(u32::MAX), h.owner.clone())
        })
    }

    fn locked(&self, now: Instant) -> Access {
        let (ms, owner) = self.state(now).unwrap_or((0, None));
        Access::Locked(ms, owner)
    }

    /// Step 2 (core §5.1): a request with session `sid` and `corr`, its bytes `request`.
    pub fn replay(&self, sid: u32, corr: u16, request: &[u8]) -> Replay {
        if self.last != Some(sid) {
            return Replay::New;
        }
        if let Some(r) = self.records.iter().find(|r| r.corr == corr) {
            if r.request != request {
                return Replay::Malformed;
            }
            return match &r.answer {
                Some(a) => Replay::Stored(a.clone()),
                None => Replay::Lost,
            };
        }
        match self.last_corr {
            Some(l) if !(1..HALF).contains(&corr.wrapping_sub(l)) => Replay::Lost,
            _ => Replay::New,
        }
    }

    /// A new request of the last session got past step 2: it is the last corr now, and kept
    /// until its answer is [`answered`](Self::answered).
    pub fn note(&mut self, corr: u16, request: Vec<u8>) {
        self.last_corr = Some(corr);
        // Half a turn back or more is never matched again (core §5.1).
        self.records
            .retain(|r| corr.wrapping_sub(r.corr) < HALF && r.corr != corr);
        self.records.push_back(Record {
            corr,
            request,
            answer: None,
        });
        while self.records.len() > HISTORY {
            self.records.pop_front();
        }
    }

    /// The answer to a request [`note`](Self::note)d under `sid`: kept for its resends, and the
    /// holder's lease counts again from now (core §6.1: from the answer, a refusal too).
    pub fn answered(&mut self, sid: u32, corr: u16, answer: &[u8], now: Instant) {
        if self.last == Some(sid)
            && let Some(r) = self.records.iter_mut().find(|r| r.corr == corr)
        {
            r.answer.get_or_insert_with(|| answer.to_vec());
        }
        if let Some(h) = self.holder.as_mut().filter(|h| h.sid == sid) {
            h.deadline = now + h.lease;
        }
    }

    /// Step 3 for anything but open (core §6.2).
    pub fn access(&self, sid: u32, now: Instant) -> Access {
        match &self.holder {
            None => Access::NoSession,
            Some(h) if h.sid == sid => Access::Owner,
            Some(_) => self.locked(now),
        }
    }

    /// An open (core §6.2, §6.4) by `client`, past step 2. `shared` is whether the ch32rv tools
    /// share the lock now (then any open here is locked, host guide §7). A new session's open
    /// starts the resend state with itself (`corr`, `request`).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        sid: u32,
        client: u64,
        lease_ms: u32,
        force: bool,
        owner: Option<Vec<u8>>,
        shared: Option<u32>,
        corr: u16,
        request: Vec<u8>,
        now: Instant,
    ) -> Opened {
        let lease_ms = if lease_ms == 0 {
            DEFAULT_LEASE_MS
        } else {
            lease_ms
        }
        .clamp(1000, 60_000);
        let lease = Duration::from_millis(u64::from(lease_ms));
        if let Some(ms) = shared {
            return Opened::Refused(Access::Locked(ms, None));
        }
        let mut stolen = None;
        match &mut self.holder {
            // The holder's own open: a new lease, the rest kept (core §6.2).
            Some(h) if h.sid == sid => {
                h.lease = lease;
                h.deadline = now + lease;
                self.note(corr, request);
                return Opened::Held {
                    lease_ms,
                    stolen: None,
                };
            }
            Some(_) if !force => return Opened::Refused(self.locked(now)),
            Some(_) => stolen = self.end(),
            None if self.ended && self.last == Some(sid) => {
                return Opened::Refused(Access::NoSession);
            }
            None => {}
        }
        // A new session: the resend state starts again with this open (core §5.1).
        if self.last != Some(sid) {
            self.last = Some(sid);
            self.records.clear();
        }
        self.ended = false;
        self.note(corr, request);
        self.holder = Some(Holder {
            sid,
            client,
            lease,
            deadline: now + lease,
            owner,
        });
        Opened::Held { lease_ms, stolen }
    }
}

/// The lease an open of 0 gets (core §6.4: the probe's default).
pub(crate) const DEFAULT_LEASE_MS: u32 = 10_000;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn opened(l: &mut CoreLock, sid: u32, corr: u16, force: bool, now: Instant) -> Opened {
        l.open(sid, 1, 1000, force, None, None, corr, vec![corr as u8], now)
    }

    #[test]
    fn one_lock_with_lease_end_and_force() {
        let t = Instant::now();
        let mut l = CoreLock::default();
        assert_eq!(l.access(5, t), Access::NoSession);
        assert!(matches!(
            opened(&mut l, 5, 1, false, t),
            Opened::Held { lease_ms: 1000, .. }
        ));
        assert_eq!(l.access(5, t), Access::Owner);
        // Another id: locked, unless forced.
        assert!(matches!(
            opened(&mut l, 6, 1, false, t),
            Opened::Refused(Access::Locked(1000, None))
        ));
        assert_eq!(
            opened(&mut l, 6, 1, true, t),
            Opened::Held {
                lease_ms: 1000,
                stolen: Some(1)
            }
        );
        // The lease lapses; the ended id opens nothing.
        assert_eq!(l.expire(t + Duration::from_millis(999)), None);
        assert_eq!(l.expire(t + Duration::from_secs(1)), Some(1));
        assert_eq!(l.access(6, t), Access::NoSession);
        assert_eq!(
            opened(&mut l, 6, 2, false, t),
            Opened::Refused(Access::NoSession)
        );
        // While the ch32rv tools share it, an open is locked.
        assert!(matches!(
            l.open(7, 2, 0, true, None, Some(10_000), 1, vec![], t),
            Opened::Refused(Access::Locked(10_000, None))
        ));
    }

    #[test]
    fn resends_answer_from_the_record() {
        let t = Instant::now();
        let mut l = CoreLock::default();
        opened(&mut l, 5, 10, false, t);
        l.answered(5, 10, b"open", t);
        assert_eq!(l.replay(5, 10, &[10]), Replay::Stored(b"open".to_vec()));
        assert_eq!(l.replay(5, 10, &[11]), Replay::Malformed);
        assert_eq!(l.replay(5, 11, &[11]), Replay::New);
        l.note(11, vec![11]);
        // Not answered yet, an old corr, and the half turn: result_lost.
        assert_eq!(l.replay(5, 11, &[11]), Replay::Lost);
        assert_eq!(l.replay(5, 9, &[9]), Replay::Lost);
        assert_eq!(l.replay(5, 11 + HALF, &[0]), Replay::Lost);
        // The state survives end; only a new id's open replaces it.
        l.end();
        assert_eq!(l.replay(5, 10, &[10]), Replay::Stored(b"open".to_vec()));
        opened(&mut l, 6, 1, false, t);
        assert_eq!(l.replay(5, 10, &[10]), Replay::New);
    }
}
