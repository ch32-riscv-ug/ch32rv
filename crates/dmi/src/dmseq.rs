//! en: `dmseq` - the sequenced debug-module console (OEP `target.console` framing 2, spec:
//! `oep-spec/docs/target-console-dmseq.ja.md`, agreed with the ArduinoCore-CH32 side 2026-09-24).
//! It carries the same DATA0/DATA1 mailbox as [`DebugModule::dmdata_poll`]'s framing, but adds a
//! 1-bit sequence number in each direction and a CRC-8 over every word, which is what lets the
//! host tell a lost answer from a new frame: without them a dropped answer makes the target's word
//! be read twice (duplicated bytes) and a "read it back" repair cannot tell that duplicate from
//! the target posting an identical next frame (a dropped byte).
//!
//! This module is the **host** side: [`DmSeq`] holds one session's state and
//! [`DebugModule::dmseq_poll`] performs one exchange. The framing decisions live in
//! [`DmSeq::react`], which is pure - it takes the eight bytes of a frame and returns the answer
//! word - so every rule below is unit-tested without a target.
//!
//! ja: `dmseq` = 通番付き debug module console(OEP `target.console` framing 2。仕様は
//! `oep-spec/docs/target-console-dmseq.ja.md`、2026-09-24 に ArduinoCore-CH32 側と合意)。搬送は
//! `dmdata` と同じ DATA0/DATA1 mailbox だが、**両方向の 1 bit 通番**と**全 word の CRC-8** が付く。
//! これにより「答えが落ちた」と「新しいフレーム」を区別できる(通番が無いと、落ちた答えのせいで
//! target の word を二度読んで重複し、読み戻しで直そうとすると同内容の次フレームと取り違えて
//! 欠落する)。ここは **host 側**で、[`DmSeq`] が 1 session の状態、[`DebugModule::dmseq_poll`] が
//! 1 回の交換。判断は純粋関数 [`DmSeq::react`] に寄せてあるので、実機なしで全規則を試験できる。

use crate::dm::{DMDATA0, DMDATA1, DebugModule};
use crate::{DmiError, DtmAccess};

/// en: Largest target -> host payload (byte 0 is the status, the CRC follows the payload, and the
/// two data registers hold eight bytes). ja: target → host の payload 上限。
pub const MAX_TARGET_PAYLOAD: usize = 6;
/// en: Largest host -> target payload: the host only ever writes DATA0, and the CRC follows.
/// ja: host → target の payload 上限(host は DATA0 しか書かないため)。
pub const MAX_HOST_PAYLOAD: usize = 2;

/// Status-byte bits (both directions; the host's answer has bit 7 clear).
const BIT_T: u8 = 0x80;
const BIT_TO: u8 = 0x40;
const BIT_S: u8 = 0x20;
const BIT_A: u8 = 0x10;
const BIT_SYN: u8 = 0x08;
const MASK_N: u8 = 0x07;

/// en: CRC-8, poly 0x07, init 0xFF, no reflection, no final xor - over the status byte and the
/// payload, stored right after them. `init 0xFF` is what makes an all-zero word invalid, so a data
/// register that holds nothing (a V4 part with no debugger attached reads 0) and a mailbox the
/// host has cleared are never mistaken for a frame or an answer.
/// ja: CRC-8(poly 0x07 / init 0xFF / 反転なし / 最終 XOR なし)。status byte と payload にかけ、
/// 直後に置く。`init 0xFF` により全 0 の word が常に無効になる(値を保持しないレジスタや、host が
/// 消した mailbox をフレームと取り違えない)。
#[must_use]
pub fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0xffu8;
    for &b in bytes {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Result of one [`DebugModule::dmseq_poll`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DmSeqPoll {
    /// Target -> host payload accepted this poll (empty for a duplicate, an empty frame, or when
    /// the mailbox held no frame).
    pub received: Vec<u8>,
    /// How many leading bytes of `host_input` were taken over for delivery (0..=2).
    pub sent: usize,
    /// A valid frame was read (new or duplicate). False for "nothing there" and for a bad word.
    pub frame: bool,
    /// en: The target had stopped waiting for an answer, so output written in the meantime was
    /// dropped. Reported once per episode (see [`DmSeq::react`]). ja: target が答えを待つのを
    /// やめていた = その間の出力は捨てられている。1 episode に 1 回だけ立つ。
    pub timed_out: bool,
}

/// en: One host-side dmseq session. A session starts unsynced; **a host that resets the target
/// starts a new one** (drop this and make a fresh [`DmSeq`]), because a 1-bit SYN cannot always
/// tell a restarted target from a reposted frame.
/// ja: host 側の dmseq session 1 つ。未同期で始まる。**target を reset した host は新しい session
/// を始める**(これを捨てて作り直す)。SYN は 1 bit なので、再起動と再送を常には区別できない。
#[derive(Debug, Clone, Default)]
pub struct DmSeq {
    /// A frame has been accepted this session.
    synced: bool,
    /// S of the last accepted frame.
    last_s: bool,
    /// The last accepted frame had SYN set.
    last_syn: bool,
    /// Sequence bit of the host payload not yet known delivered.
    h: bool,
    /// Host payload handed over but not yet acknowledged (re-sent until it is).
    pending: Vec<u8>,
    /// Consecutive invalid reads (the rule-1 deadlock breaker fires at three).
    bad_run: u8,
    /// A TO frame has been reported; cleared by the first frame without TO.
    to_reported: bool,
    /// Frames discarded as duplicates (diagnostics).
    pub duplicates: u64,
    /// Words rejected as invalid - bad CRC or N > 6 (diagnostics).
    pub invalid: u64,
}

impl DmSeq {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a frame has ever been accepted in this session (used to report "no console here").
    #[must_use]
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// Build an answer word: K = `k`, H = this session's `h`, M = `payload.len()`, then the CRC.
    fn answer(&self, k: bool, payload: &[u8]) -> u32 {
        let mut b = [0u8; 4];
        b[0] = (if k { BIT_S } else { 0 }) | (if self.h { BIT_A } else { 0 }) | payload.len() as u8;
        b[1..1 + payload.len()].copy_from_slice(payload);
        b[1 + payload.len()] = crc8(&b[..1 + payload.len()]);
        u32::from_le_bytes(b)
    }

    /// en: The host rules of the spec, applied to the eight bytes of a mailbox read. Returns the
    /// word to write back (None = do not answer, read again next poll) and what the caller should
    /// report. Pure: all of the framing is decided here.
    /// ja: 仕様の host 規則を、読んだ 8 byte に適用する。返り値は書き戻す word(None = 答えず次の
    /// poll で読み直す)と報告内容。判断はすべてここ。
    fn react(&mut self, b: &[u8; 8], host_input: &[u8]) -> (Option<u32>, DmSeqPoll) {
        let n = (b[0] & MASK_N) as usize;
        // Rule 1. Check N before locating the CRC: byte 1+N does not exist for N = 7, and N = 7 is
        // exactly what the 0xffffffff a WCH-Link attach leaves in DATA0 decodes to.
        if n > MAX_TARGET_PAYLOAD || crc8(&b[..1 + n]) != b[1 + n] {
            self.invalid += 1;
            self.bad_run = self.bad_run.saturating_add(1);
            if self.bad_run >= 3 && self.synced {
                // The word may be our own answer corrupted into bit 7 set, in which case both
                // sides are waiting. K = last_s is safe whatever the target has outstanding: a
                // frame with that S was already accepted, and a new frame has the other S, fails
                // the target's K check and is posted again.
                self.bad_run = 0;
                return (Some(self.answer(self.last_s, &[])), DmSeqPoll::default());
            }
            return (None, DmSeqPoll::default());
        }
        self.bad_run = 0;
        let s = b[0] & BIT_S != 0;
        let a = b[0] & BIT_A != 0;
        let syn = b[0] & BIT_SYN != 0;
        let timed_out = b[0] & BIT_TO != 0;

        // Rule 2: a reposted SYN frame is a duplicate like any other - resyncing on every SYN
        // frame is what delivered its payload twice before this rule existed.
        let duplicate = self.synced && s == self.last_s && (!syn || self.last_syn);
        // Rule 3, continuing into rule 4 (not a separate branch).
        if !duplicate && (syn || !self.synced) {
            self.last_s = !s;
            self.h = !a;
            self.pending.clear();
            self.synced = true;
        }
        // Rule 4.
        let mut received = Vec::new();
        if s != self.last_s {
            received.extend_from_slice(&b[1..1 + n]);
            self.last_s = s;
            self.last_syn = syn;
        } else {
            self.duplicates += 1;
        }
        // Rule 5: the target acknowledges a host payload by echoing its bit in A.
        if !self.pending.is_empty() && a == self.h {
            self.h = !self.h;
            self.pending.clear();
        }
        let mut sent = 0;
        if self.pending.is_empty() && !host_input.is_empty() {
            sent = host_input.len().min(MAX_HOST_PAYLOAD);
            self.pending.extend_from_slice(&host_input[..sent]);
        }
        // Report a timeout once per episode: a latched target keeps its TO frame posted, so the
        // same one can be read repeatedly, and the user needs the warning once, not per poll.
        let report_to = timed_out && !self.to_reported;
        self.to_reported = timed_out;
        let word = self.answer(s, &self.pending.clone());
        (
            Some(word),
            DmSeqPoll {
                received,
                sent,
                frame: true,
                timed_out: report_to,
            },
        )
    }
}

impl<T: DtmAccess> DebugModule<'_, T> {
    /// en: One dmseq exchange, in the order the spec fixes: read DATA0; if it holds a frame whose
    /// N >= 3, read DATA1; check; only then write the answer (once answered, the target may post
    /// its next frame and overwrite DATA1). The core keeps running - this only touches the DM data
    /// registers. Returns what the target sent and how much of `host_input` was taken over.
    ///
    /// A word with bit 7 clear is the host's own answer, still uncollected, or nothing yet: the
    /// mailbox is not ours to touch then, and the poll returns empty.
    ///
    /// ja: dmseq の 1 交換。順序は仕様どおり: DATA0 を読む → フレームで N >= 3 なら DATA1 を読む →
    /// 検査 → その後で答えを書く(答えた時点で target は次のフレームを出して DATA1 を上書きしてよい)。
    /// core は running のまま。bit 7 が 0 の word は host 自身の答え(未回収)か、まだ何も無い状態で、
    /// こちらが触ってよい word ではないので空で返す。
    pub fn dmseq_poll(
        &mut self,
        session: &mut DmSeq,
        host_input: &[u8],
    ) -> Result<DmSeqPoll, DmiError> {
        let w0 = self.read(DMDATA0)?;
        if w0 & u32::from(BIT_T) == 0 {
            return Ok(DmSeqPoll::default());
        }
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&w0.to_le_bytes());
        let n = (b[0] & MASK_N) as usize;
        if (3..=MAX_TARGET_PAYLOAD).contains(&n) {
            let w1 = self.read(DMDATA1)?;
            b[4..].copy_from_slice(&w1.to_le_bytes());
        }
        let (answer, poll) = session.react(&b, host_input);
        if let Some(word) = answer {
            self.write(DMDATA0, word)?;
        }
        Ok(poll)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Build a target frame word pair the way the target does.
    fn frame(s: bool, a: bool, syn: bool, to: bool, payload: &[u8]) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0] = BIT_T
            | (if to { BIT_TO } else { 0 })
            | (if s { BIT_S } else { 0 })
            | (if a { BIT_A } else { 0 })
            | (if syn { BIT_SYN } else { 0 })
            | payload.len() as u8;
        b[1..1 + payload.len()].copy_from_slice(payload);
        b[1 + payload.len()] = crc8(&b[..1 + payload.len()]);
        b
    }

    /// Decode an answer word the way the target does.
    fn answer_parts(word: u32) -> (bool, bool, Vec<u8>) {
        let b = word.to_le_bytes();
        let m = (b[0] & MASK_N) as usize;
        assert!(m <= MAX_HOST_PAYLOAD, "M must fit the host's two bytes");
        assert_eq!(crc8(&b[..1 + m]), b[1 + m], "answer CRC");
        assert_eq!(b[0] & BIT_T, 0, "an answer never sets bit 7");
        (b[0] & BIT_S != 0, b[0] & BIT_A != 0, b[1..1 + m].to_vec())
    }

    /// The CRC is the one both sides compute (values taken from the agreed poly/init).
    #[test]
    fn crc8_matches_the_spec_parameters() {
        assert_eq!(crc8(&[]), 0xff);
        assert_eq!(crc8(&[0x00]), 0xf3);
        // An all-zero word must never validate: that is what keeps a register holding nothing,
        // and a mailbox the host cleared, from reading as a frame.
        let zero = [0u8; 8];
        let n = (zero[0] & MASK_N) as usize;
        assert_ne!(crc8(&zero[..1 + n]), zero[1 + n]);
    }

    /// The first frame of a session syncs and is delivered, and the answer acknowledges its S.
    #[test]
    fn first_frame_syncs_and_is_accepted() {
        let mut st = DmSeq::new();
        let (answer, poll) = st.react(&frame(false, true, true, false, b"hi"), b"");
        assert_eq!(poll.received, b"hi");
        assert!(poll.frame);
        let (k, _h, payload) = answer_parts(answer.unwrap());
        assert!(!k, "K is the frame's S");
        assert!(payload.is_empty());
        assert!(st.synced());
    }

    /// A frame posted again (its answer was lost) must not be delivered twice - the whole point.
    #[test]
    fn a_reposted_frame_is_a_duplicate() {
        let mut st = DmSeq::new();
        let f = frame(false, true, false, false, b"ab");
        st.react(&f, b"");
        let (answer, poll) = st.react(&f, b"");
        assert!(poll.received.is_empty(), "the same S is a duplicate");
        assert_eq!(st.duplicates, 1);
        let (k, _, _) = answer_parts(answer.unwrap());
        assert!(!k, "a duplicate is still answered, with its own S");
    }

    /// The defect found in review: a SYN frame reposted after a lost answer used to resync and so
    /// be delivered a second time. It is a duplicate like any other.
    #[test]
    fn a_reposted_syn_frame_is_a_duplicate() {
        let mut st = DmSeq::new();
        let f = frame(false, true, true, false, b"READY");
        assert_eq!(st.react(&f, b"").1.received, b"READY");
        let (_, poll) = st.react(&f, b"");
        assert!(
            poll.received.is_empty(),
            "a reposted SYN frame must not be delivered twice"
        );
    }

    /// A target that restarts re-enters SYN; that frame is new, not a duplicate, and resyncing
    /// throws away host payload queued for the session that ended.
    #[test]
    fn a_restart_resyncs_and_drops_pending_host_input() {
        let mut st = DmSeq::new();
        st.react(&frame(false, true, false, false, b"x"), b"ab");
        assert_eq!(st.pending, b"ab", "handed over, not yet acknowledged");
        // The target restarts: SYN again, sequence starts over, A back to its initial value.
        let (answer, poll) = st.react(&frame(false, true, true, false, b"READY"), b"");
        assert_eq!(poll.received, b"READY", "a restart is not a duplicate");
        assert!(
            st.pending.is_empty(),
            "queued input belonged to the old session"
        );
        let (_, _, payload) = answer_parts(answer.unwrap());
        assert!(payload.is_empty());
    }

    /// Host payload rides on the answers, two bytes at a time, and is re-sent until the target
    /// echoes its sequence bit in A.
    #[test]
    fn host_payload_is_resent_until_acknowledged() {
        let mut st = DmSeq::new();
        let (answer, poll) = st.react(&frame(false, true, false, false, b""), b"abc");
        assert_eq!(poll.sent, 2, "at most two bytes per answer");
        let (_, h, payload) = answer_parts(answer.unwrap());
        assert_eq!(payload, b"ab");
        // The target has not taken it yet (A unchanged): the same bytes go out again, unchanged.
        let (answer, poll) = st.react(&frame(true, true, false, false, b""), b"c");
        assert_eq!(
            poll.sent, 0,
            "nothing new is taken while one is outstanding"
        );
        let (_, h2, payload2) = answer_parts(answer.unwrap());
        assert_eq!((h2, payload2), (h, b"ab".to_vec()));
        // Now the target echoes H in A: the next answer carries the next byte with the flipped H.
        let (answer, poll) = st.react(&frame(false, h, false, false, b""), b"c");
        assert_eq!(poll.sent, 1);
        let (_, h3, payload3) = answer_parts(answer.unwrap());
        assert_eq!(payload3, b"c");
        assert_ne!(h3, h, "the sequence bit flips once it is delivered");
    }

    /// N = 7 is not a payload length: it is what an attach's 0xffffffff decodes to, and reading a
    /// CRC at byte 1+N would run off the frame.
    #[test]
    fn attach_garbage_is_rejected_not_parsed() {
        let mut st = DmSeq::new();
        let (answer, poll) = st.react(&[0xff; 8], b"");
        assert!(answer.is_none(), "an invalid word is not answered");
        assert!(!poll.frame);
        assert_eq!(st.invalid, 1);
    }

    /// Three invalid reads in a row, once synced, break a deadlock where the host's own answer was
    /// corrupted into a word with bit 7 set and both sides are waiting.
    #[test]
    fn three_invalid_reads_answer_the_last_accepted_s() {
        let mut st = DmSeq::new();
        st.react(&frame(true, true, false, false, b"x"), b"");
        let bad = [0x80u8, 0, 0, 0, 0, 0, 0, 0]; // N = 0 with a wrong CRC
        assert!(st.react(&bad, b"").0.is_none());
        assert!(st.react(&bad, b"").0.is_none());
        let (answer, _) = st.react(&bad, b"");
        let (k, _, payload) = answer_parts(answer.expect("the breaker answers"));
        assert!(k, "K = the last accepted S");
        assert!(payload.is_empty(), "the breaker carries no payload");
        // Not synced: there is nothing safe to answer, so it stays quiet.
        let mut fresh = DmSeq::new();
        for _ in 0..5 {
            assert!(fresh.react(&bad, b"").0.is_none());
        }
    }

    /// A TO frame is an ordinary frame; the warning is raised once per episode, not per poll.
    #[test]
    fn timeout_is_reported_once_per_episode() {
        let mut st = DmSeq::new();
        let f = frame(false, true, false, true, b"lost");
        let (_, poll) = st.react(&f, b"");
        assert_eq!(
            poll.received, b"lost",
            "a TO frame still carries its payload"
        );
        assert!(poll.timed_out);
        assert!(
            !st.react(&f, b"").1.timed_out,
            "the repost does not warn again"
        );
        // A frame without TO ends the episode, so the next one warns again.
        st.react(&frame(true, true, false, false, b"ok"), b"");
        assert!(
            st.react(&frame(false, true, false, true, b"x"), b"")
                .1
                .timed_out
        );
    }

    /// A six-byte payload uses both registers and its CRC lands in the last byte of DATA1.
    #[test]
    fn a_full_frame_spans_both_registers() {
        let f = frame(false, true, false, false, b"abcdef");
        assert_eq!(f[0] & MASK_N, 6);
        assert_eq!(f[7], crc8(&f[..7]), "the CRC is the last byte of DATA1");
        let mut st = DmSeq::new();
        assert_eq!(st.react(&f, b"").1.received, b"abcdef");
    }
}
