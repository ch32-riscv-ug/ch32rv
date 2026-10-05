//! en: An OEP probe with a session (core §6-§9, docs/oep-host.ja.md §4.1): discovery (list /
//! describe, the name -> fn cache kept while boot_id holds), open / end / keepalive / lock_state,
//! and typed errors for the rejections a host acts on. Which lock-stealing rule applies is the
//! caller's decision (it knows the transports); this layer only reports `Locked`.
//! ja: session を持つ OEP の probe。discovery(list / describe、boot_id が同じ間の name -> fn の写し)、
//! open / end / keepalive / lock_state、host が扱う拒否の型付き error。lock をどう奪うかは呼び出し側。

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};

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
    /// en: The session's lease lapsed and the probe released what it held (core §6.2): open
    /// again; nothing the session made is there any more.
    /// ja: lease が切れ、probe は session の資源を外した。open からやり直す。
    #[error("the probe's session lease lapsed and its resources were released: open again")]
    Expired,
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
    /// The same session id took the lock again and its resources are still there (`resumed` 1).
    pub resumed: bool,
    /// The same session id, but after a lapse: its resources were released (`resumed` 2).
    pub swept: bool,
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

    /// A lock-free core request (role 0x01).
    fn core_call(&mut self, op: u8, payload: Vec<u8>) -> Result<Vec<u8>, OepError> {
        let r = self.link.call(Call {
            func: core::FN,
            op,
            session: None,
            payload,
        })?;
        check(r)
    }

    /// A request under the session (role 0x81).
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

    /// en: `open`: take the lock with `session_id`. `force` steals it (the caller decides when).
    /// `owner` names this host for others who find the probe locked (1..32 bytes; display only).
    /// ja: `open`。`owner` はロックに阻まれた他の host に見せる名前(1〜32 byte、表示専用)。
    pub fn open(
        &mut self,
        session_id: u32,
        lease_ms: u32,
        force: bool,
        owner: Option<&str>,
    ) -> Result<Opened, OepError> {
        let mut p = Vec::with_capacity(9);
        p.extend_from_slice(&session_id.to_le_bytes());
        p.extend_from_slice(&lease_ms.to_le_bytes());
        p.push(u8::from(force));
        if let Some(o) = owner.filter(|o| !o.is_empty()) {
            let b = &o.as_bytes()[..o.len().min(32)];
            put_tlv(&mut p, core::tlvs::open::OWNER, false, b);
        }
        let r = self.link.call(Call {
            func: core::FN,
            op: core::op::OPEN,
            session: None,
            payload: p,
        })?;
        let p = check(r)?;
        if p.len() < 9 {
            return Err(OepError::Malformed(
                "open answer shorter than 9 bytes".into(),
            ));
        }
        let opened = Opened {
            lease_ms: le32(&p, 0),
            boot_id: le32(&p, 4),
            resumed: p[8] == core::enums::resumed::RESUMED,
            swept: p[8] == core::enums::resumed::SWEPT,
        };
        // A new boot means new fn numbers (and no resources): forget the cache. So does an open
        // with this host's last session_id answered resumed = 0: the probe no longer knows that
        // session (a reboot whose boot_id repeated, or another host in between; oep-core §6.5).
        let forgotten = self.session == Some(session_id) && p[8] == core::enums::resumed::NEW;
        if self.boot_id != Some(opened.boot_id) || opened.boot_id == 0 || forgotten {
            self.fns.clear();
            self.max_op_ms = None;
        }
        self.boot_id = Some(opened.boot_id);
        self.session = Some(session_id);
        // The probe dropped its resend table: numbering may restart.
        self.link.reset_corr();
        Ok(opened)
    }

    /// `end`: release the lock; the session's resources stay for the next open.
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
            // count x (len(u8), entry); an entry longer than ch32rv knows is read up to what it
            // knows (core §2.3: readers skip the unknown tail, writers only append).
            for _ in 0..count {
                let Some(&len) = a.get(at) else {
                    return Err(OepError::Malformed("list entry cut short".into()));
                };
                let e = a
                    .get(at + 1..at + 1 + usize::from(len))
                    .ok_or_else(|| OepError::Malformed("list entry cut short".into()))?;
                at += 1 + usize::from(len);
                // fn(u16) instance(u16) revision(u8) flags(u8) name_len(u8) name
                if e.len() < 7 {
                    return Err(OepError::Malformed(
                        "list entry shorter than 7 bytes".into(),
                    ));
                }
                let name = e
                    .get(7..7 + usize::from(e[6]))
                    .ok_or_else(|| OepError::Malformed("list entry name cut short".into()))?;
                out.push(Interface {
                    func: u16::from_le_bytes([e[0], e[1]]),
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
        self.max_op_ms = Some(ms);
        Ok(ms)
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
        Resolution::Rejected(reason) if reason == reject_reasons::EXPIRED => Err(OepError::Expired),
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
