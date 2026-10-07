//! en: An OEP probe with a session (core §6-§9, docs/oep-host.ja.md §4.1): discovery (list /
//! describe, the name -> fn cache kept while boot_id holds), open / end / keepalive / lock_state,
//! and typed errors for the rejections a host acts on. Which lock-stealing rule applies is the
//! caller's decision (it knows the transports); this layer only reports `Locked`.
//! ja: session を持つ OEP の probe。discovery(list / describe、boot_id が同じ間の name -> fn の写し)、
//! open / end / keepalive / lock_state、host が扱う拒否の型付き error。lock をどう奪うかは呼び出し側。

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use crate::codec::{Resolution, Tlv, parse_tlvs, put_tlv};
use crate::link::{Call, Limits, Link, LinkError, Reply};
use crate::registry::{core, reject_reasons};

#[derive(Debug, thiserror::Error)]
pub enum OepError {
    #[error(transparent)]
    Link(#[from] LinkError),
    #[error(
        "the probe holds its lock for another session{} ({remaining_ms} ms of lease left)",
        owner.as_deref().map(|o| format!(" ({o})")).unwrap_or_default()
    )]
    Locked {
        remaining_ms: u32,
        /// What the holder called itself in its open (core §6.4), if it did.
        owner: Option<String>,
    },
    #[error("the probe rejected the request: reason 0x{reason:02x}")]
    Rejected { reason: u8, payload: Vec<u8> },
    #[error("the request failed on the probe (outcome 0x{outcome:02x})")]
    Failed { outcome: u8, payload: Vec<u8> },
    #[error("the probe's answer is malformed: {0}")]
    Malformed(String),
    #[error("the probe has no interface `{0}`")]
    NoInterface(String),
}

/// One `list` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub func: u16,
    pub instance: u16,
    pub revision: u8,
    pub name: String,
}

/// What `open` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    pub lease_ms: u32,
    pub boot_id: u32,
}

/// What `lock_state` answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockState {
    pub locked: bool,
    pub remaining_ms: u32,
    /// What the holder called itself (core §6.4), if it did.
    pub owner: Option<String>,
}

/// The owner TLV in a TLV tail, if any (a broken tail just means no owner: it is display only).
fn owner_in(tail: &[u8], tag: u8) -> Option<String> {
    parse_tlvs(tail)
        .ok()?
        .into_iter()
        .find(|t| t.tag == tag)
        .map(|t| String::from_utf8_lossy(&t.value).into_owned())
}

/// A fresh random session id (never 0).
pub fn random_session_id() -> u32 {
    // RandomState is seeded from the OS per instance; no extra dependency needed.
    loop {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(std::process::id().into());
        let v = h.finish() as u32;
        if v != 0 {
            return v;
        }
    }
}

pub struct Probe {
    link: Link,
    limits: Limits,
    session: Option<u32>,
    boot_id: Option<u32>,
    /// The probe's `max_op_ms` (fn 0 describe), learned once per boot.
    max_op_ms: Option<u32>,
    fns: HashMap<String, Interface>,
    /// Each fn's declared ops (describe tag `ops`), learned once per boot; `None` = no ops tag.
    ops: HashMap<u16, Option<Vec<u8>>>,
}

impl Probe {
    /// Confirm the probe (no session yet).
    pub fn connect(mut link: Link) -> Result<Self, OepError> {
        let limits = link.confirm()?;
        Ok(Probe {
            link,
            limits,
            session: None,
            boot_id: None,
            max_op_ms: None,
            fns: HashMap::new(),
            ops: HashMap::new(),
        })
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn link(&mut self) -> &mut Link {
        &mut self.link
    }

    pub fn session_id(&self) -> Option<u32> {
        self.session
    }

    pub fn boot_id(&self) -> Option<u32> {
        self.boot_id
    }

    /// A lock-free core request (session_id 0).
    fn core_call(&mut self, op: u8, payload: Vec<u8>) -> Result<Vec<u8>, OepError> {
        let r = self.link.call(Call {
            func: core::FN,
            op,
            session: None,
            payload,
        })?;
        check(r)
    }

    /// A request under the session (its session_id in the header).
    pub fn call(&mut self, func: u16, op: u8, payload: Vec<u8>) -> Result<Reply, OepError> {
        Ok(self.link.call(Call {
            func,
            op,
            session: self.session,
            payload,
        })?)
    }

    /// Several requests under the session, pipelined.
    pub fn exchange(&mut self, calls: Vec<(u16, u8, Vec<u8>)>) -> Result<Vec<Reply>, OepError> {
        let s = self.session;
        Ok(self.link.exchange(
            calls
                .into_iter()
                .map(|(func, op, payload)| Call {
                    func,
                    op,
                    session: s,
                    payload,
                })
                .collect(),
        )?)
    }

    /// en: `open`: take the lock with `session_id` (in the header, core §6.4). `force` steals it
    /// (the caller decides when). `owner` names this host for others who find the probe locked
    /// (1..32 bytes; display only). There is no resume: a session that ended stays ended, and
    /// an open with this host's last id while the lock is free is a new session like any other.
    /// ja: `open`。`owner` はロックに阻まれた他の host に見せる名前(1〜32 byte、表示専用)。再開は無い。
    pub fn open(
        &mut self,
        session_id: u32,
        lease_ms: u32,
        force: bool,
        owner: Option<&str>,
    ) -> Result<Opened, OepError> {
        let mut p = Vec::with_capacity(5);
        p.extend_from_slice(&lease_ms.to_le_bytes());
        p.push(u8::from(force));
        if let Some(o) = owner.filter(|o| !o.is_empty()) {
            let b = &o.as_bytes()[..o.len().min(32)];
            put_tlv(&mut p, core::tlvs::open::OWNER, false, b);
        }
        let r = self.link.call(Call {
            func: core::FN,
            op: core::op::OPEN,
            session: Some(session_id),
            payload: p,
        })?;
        let p = check(r)?;
        if p.len() < 8 {
            return Err(OepError::Malformed(
                "open answer shorter than 8 bytes".into(),
            ));
        }
        let opened = Opened {
            lease_ms: le32(&p, 0),
            boot_id: le32(&p, 4),
        };
        // A new boot means new fn numbers (and no resources): forget the cache (oep-core §6.5).
        if self.boot_id != Some(opened.boot_id) {
            self.forget_interfaces();
        }
        self.boot_id = Some(opened.boot_id);
        // The link's view of the boot is this open's from now on (a restart is told by a later
        // confirm that differs from it).
        self.link.last_boot_id = Some(opened.boot_id);
        self.session = Some(session_id);
        // The probe dropped its resend table: numbering may restart.
        self.link.reset_corr();
        Ok(opened)
    }

    /// `end`: release the lock and everything the session made (core §6.4, §9).
    pub fn end(&mut self) -> Result<(), OepError> {
        let r = self.call(core::FN, core::op::END, Vec::new())?;
        check(r)?;
        self.session = None;
        Ok(())
    }

    pub fn keepalive(&mut self) -> Result<(), OepError> {
        let r = self.call(core::FN, core::op::KEEPALIVE, Vec::new())?;
        check(r).map(|_| ())
    }

    /// `lock_state`: whether the lock is held, the lease left, and the holder's owner name.
    pub fn lock_state(&mut self) -> Result<LockState, OepError> {
        let p = self.core_call(core::op::LOCK_STATE, Vec::new())?;
        if p.len() < 5 {
            return Err(OepError::Malformed(
                "lock_state answer shorter than 5 bytes".into(),
            ));
        }
        Ok(LockState {
            locked: p[0] != 0,
            remaining_ms: le32(&p, 1),
            owner: owner_in(&p[5..], core::tlvs::lock_state_answer::OWNER),
        })
    }

    /// `list` the interfaces under `prefix` (matched on label boundaries: `oep.wire` finds
    /// `oep.wire.rvswd`; `""` lists everything), following the pages.
    pub fn list(&mut self, prefix: &str) -> Result<Vec<Interface>, OepError> {
        let mut out = Vec::new();
        loop {
            let mut p = vec![0u8];
            p.extend_from_slice(&(out.len() as u16).to_le_bytes());
            p.push(prefix.len() as u8);
            p.extend_from_slice(prefix.as_bytes());
            let a = self.core_call(core::op::LIST, p)?;
            if a.len() < 3 {
                return Err(OepError::Malformed(
                    "list answer shorter than 3 bytes".into(),
                ));
            }
            let total = usize::from(u16::from_le_bytes([a[0], a[1]]));
            let count = usize::from(a[2]);
            let mut at = 3;
            // count x entry, each fn(u16) instance(u16) revision(u8) flags(u8) name_len(u8) name
            // (core §2.3, §7.2: no element length).
            for _ in 0..count {
                let e = a
                    .get(at..at + 7)
                    .ok_or_else(|| OepError::Malformed("list entry cut short".into()))?;
                let name = a
                    .get(at + 7..at + 7 + usize::from(e[6]))
                    .ok_or_else(|| OepError::Malformed("list entry name cut short".into()))?;
                at += 7 + name.len();
                let func = u16::from_le_bytes([e[0], e[1]]);
                self.note_arg_times(func, &String::from_utf8_lossy(name));
                out.push(Interface {
                    func,
                    instance: u16::from_le_bytes([e[2], e[3]]),
                    revision: e[4],
                    name: String::from_utf8_lossy(name).into_owned(),
                });
            }
            if out.len() >= total || count == 0 {
                return Ok(out);
            }
        }
    }

    /// en: Tell the link the argument time of `func`'s ops whose wait core §4.4 lengthens (by its
    /// name from list): riscv-dm run (timeout_ms) and reset (reset_settle_ms), a wire's attach
    /// (attach_budget_ms, + hold_ms + reset_settle_ms with the reset TLV), probe.config save
    /// (max_op_ms). ja: 引数の時間で待ちの延びる op を link に教える。
    fn note_arg_times(&mut self, func: u16, name: &str) {
        use crate::link::ArgTime;
        use crate::registry::{limits, probe_config, target_riscv_dm as dm, wire_rvswd};
        let t = &mut self.link.arg_time;
        match name {
            n if n == dm::NAME => {
                // connection(u16) pc(u32) timeout_ms(u32) ... (oep-if-debug §4.4)
                t.insert((func, dm::op::RUN), ArgTime::U32At(6));
                t.insert(
                    (func, dm::op::RESET),
                    ArgTime::Fixed(limits::RESET_SETTLE_MS),
                );
            }
            n if n.starts_with("oep.wire.") => {
                t.insert((func, wire_rvswd::op::ATTACH), ArgTime::Attach);
            }
            n if n == probe_config::NAME => {
                t.insert((func, probe_config::op::SAVE), ArgTime::MaxOp);
            }
            _ => {}
        }
    }

    /// The interface named `name` (first instance), from the cache or `list`.
    pub fn interface(&mut self, name: &str) -> Result<Interface, OepError> {
        if let Some(i) = self.fns.get(name) {
            return Ok(i.clone());
        }
        // The standard ones in one list; a name outside `oep` (a vendor's) by its own name.
        let prefix = if name.starts_with("oep.") {
            "oep"
        } else {
            name
        };
        for i in self.list(prefix)? {
            self.fns.entry(i.name.clone()).or_insert(i);
        }
        self.fns
            .get(name)
            .cloned()
            .ok_or_else(|| OepError::NoInterface(name.to_owned()))
    }

    /// en: The longest one request may take on this probe (fn 0 describe `max_op_ms`, core §7.5):
    /// a run's timeout and a dmi request's waits stay at or under it. 2000 ms when the probe does
    /// not say (it must; the value only keeps ch32rv conservative).
    /// ja: 1 要求の最長時間(fn 0 の describe の max_op_ms)。run の timeout と dmi の待ちはこれ以下。
    pub fn max_op_ms(&mut self) -> Result<u32, OepError> {
        if let Some(ms) = self.max_op_ms {
            return Ok(ms);
        }
        let ms = self
            .describe(core::FN)?
            .iter()
            .find(|t| t.tag == core::tlvs::describe::MAX_OP_MS && t.value.len() == 4)
            .map_or(2000, |t| le32(&t.value, 0));
        // 1..=max_op_ms_max (oep-core §7.5, C-47): a probe declaring anything else is not used.
        if !(1..=crate::registry::limits::MAX_OP_MS_MAX).contains(&ms) {
            return Err(OepError::Malformed(format!(
                "the probe declares max_op_ms {ms}, outside 1..={}",
                crate::registry::limits::MAX_OP_MS_MAX
            )));
        }
        self.max_op_ms = Some(ms);
        self.link.max_op_ms = Some(ms);
        Ok(ms)
    }

    /// en: `oep.probe.restart`'s `restart_max_ms` (oep-if-restart §1): the longest from restart's
    /// answer until the probe answers confirm again. `None`: the probe offers no restart.
    /// ja: oep.probe.restart の restart_max_ms。restart が無ければ None。
    pub fn restart_max_ms(&mut self) -> Result<Option<u32>, OepError> {
        use crate::registry::probe_restart as r;
        let Ok(i) = self.interface(r::NAME) else {
            return Ok(None);
        };
        Ok(self
            .describe(i.func)?
            .iter()
            .find(|t| t.tag == r::tlvs::describe::RESTART_MAX_MS && t.value.len() == 4)
            .map(|t| le32(&t.value, 0)))
    }

    /// en: Send `oep.probe.restart` restart (oep-if-restart §2, under the session) and check its
    /// answer; the probe restarts right after it. The session is gone with it:
    /// [`Self::await_restart`] waits for the probe.
    /// ja: restart を送り、答えを確かめる。probe はその直後に再起動し、session は無くなる。
    pub fn request_restart(&mut self) -> Result<(), OepError> {
        use crate::registry::probe_restart as r;
        let func = self.interface(r::NAME)?.func;
        let r = self.call(func, r::op::RESTART, Vec::new())?;
        check(r)?;
        self.session = None;
        self.forget_interfaces();
        Ok(())
    }

    /// en: Wait for the restarted probe (`wait`: its restart_max_ms, read before the restart) and
    /// confirm it again; true when it came back with another boot_id (false: the same one - the
    /// restart did not happen). An error when it did not answer within `wait`.
    /// ja: 再起動した probe を待って confirm し直す。boot_id が変われば true。戻らなければ error。
    pub fn await_restart(&mut self, wait: Duration) -> Result<bool, OepError> {
        let before = self.boot_id.or(Some(self.limits.boot_id));
        if !self.link.await_restart(wait) {
            return Err(OepError::Link(LinkError::Timeout(wait)));
        }
        self.limits = self.link.confirm()?;
        self.boot_id = Some(self.limits.boot_id);
        self.link.reset_corr();
        Ok(before != Some(self.limits.boot_id))
    }

    /// en: Whether the probe restarted since this session was opened: the last confirm answer
    /// carried another boot_id than the open did (oep-core §6.5). ja: open の後に再起動したか。
    pub fn rebooted(&self) -> bool {
        matches!((self.boot_id, self.link.last_boot_id), (Some(a), Some(b)) if a != b)
    }

    /// Forget the name → fn mapping and the declarations learned from the probe (after a restart,
    /// or a session the probe no longer knows).
    pub fn forget_interfaces(&mut self) {
        self.fns.clear();
        self.max_op_ms = None;
        self.ops.clear();
    }

    /// en: The ops `func` declares in its describe's `ops` tag (core §1.2, §7.4: base(u8) bitmap,
    /// every op the fn offers, the optional ones included); `None` when the describe carries none
    /// (a probe that does not conform yet: the caller sends and lets the probe answer).
    /// A broken ops tag (core §7.4) is an error: the fn is not used.
    /// ja: `func` の describe の ops(base + bitmap)が立てる op。ops が無ければ None。
    pub fn ops(&mut self, func: u16) -> Result<Option<Vec<u8>>, OepError> {
        if let Some(o) = self.ops.get(&func) {
            return Ok(o.clone());
        }
        let mut found: Option<Vec<u8>> = None;
        for t in self.describe(func)? {
            if t.tag != crate::registry::describe_common::OPS {
                continue;
            }
            let set = decode_ops(&t.value).ok_or_else(|| {
                OepError::Malformed(format!(
                    "fn {func} declares broken ops (core §7.4: {:02x?})",
                    t.value
                ))
            })?;
            found.get_or_insert_with(Vec::new).extend(set);
        }
        if let Some(set) = found.as_mut() {
            set.sort_unstable();
            set.dedup();
        }
        self.ops.insert(func, found.clone());
        Ok(found)
    }

    /// Whether `func` offers `op` by its ops tag (true when it declares no ops: the probe decides).
    pub fn offers(&mut self, func: u16, op: u8) -> Result<bool, OepError> {
        Ok(self.ops(func)?.is_none_or(|o| o.contains(&op)))
    }

    /// `describe` of `func`, following `more`.
    pub fn describe(&mut self, func: u16) -> Result<Vec<Tlv>, OepError> {
        let mut out = Vec::new();
        loop {
            let mut p = func.to_le_bytes().to_vec();
            p.extend_from_slice(&(out.len() as u16).to_le_bytes());
            let a = self.core_call(core::op::DESCRIBE, p)?;
            let (&more, rest) = a
                .split_first()
                .ok_or_else(|| OepError::Malformed("empty describe answer".into()))?;
            let got = parse_tlvs(rest).map_err(|e| OepError::Malformed(e.to_string()))?;
            let n = got.len();
            out.extend(got);
            if more == 0 || n == 0 {
                return Ok(out);
            }
        }
    }
}

/// en: The ops set a describe `ops` value declares (core §7.4: base(u8) and a bitmap of 1 byte
/// or more, base + 8 x bitmap bytes <= 256 - the bitmap never passes op 0xFF), or `None` when the
/// value breaks that. One set may have several values (a trailing zero byte, base below the
/// lowest op). A host does not use an fn whose ops are broken.
/// ja: describe の ops の値が立てる op の集合。形が崩れていれば None。
pub fn decode_ops(v: &[u8]) -> Option<Vec<u8>> {
    let valid = v.len() >= 2 && usize::from(v[0]) + 8 * (v.len() - 1) <= 256;
    if !valid {
        return None;
    }
    let base = usize::from(v[0]);
    let mut out = Vec::new();
    for (i, byte) in v[1..].iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                out.push(u8::try_from(base + i * 8 + bit).ok()?);
            }
        }
    }
    Some(out)
}

/// The payload of a completed-success answer, or the typed error.
pub fn check(r: Reply) -> Result<Vec<u8>, OepError> {
    match r.resolution {
        Resolution::Completed(o) if o == crate::registry::outcomes::SUCCESS => Ok(r.payload),
        Resolution::Completed(outcome) => Err(OepError::Failed {
            outcome,
            payload: r.payload,
        }),
        Resolution::Rejected(reason) if reason == reject_reasons::LOCKED => {
            let (remaining_ms, owner) = if r.payload.len() >= 4 {
                (
                    le32(&r.payload, 0),
                    owner_in(&r.payload[4..], core::tlvs::locked_payload::OWNER),
                )
            } else {
                (0, None)
            };
            Err(OepError::Locked {
                remaining_ms,
                owner,
            })
        }
        Resolution::Rejected(reason) => Err(OepError::Rejected {
            reason,
            payload: r.payload,
        }),
        Resolution::Unknown(a, b) => Err(OepError::Malformed(format!(
            "resolution 0x{a:02x} (detail 0x{b:02x}) is not one a v1 host knows"
        ))),
    }
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
