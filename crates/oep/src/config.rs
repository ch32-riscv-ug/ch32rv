//! en: `oep.probe.config` read side (oep-if-probe-config): the registered slots (the places a
//! target is wired to; `oep://<probe>/<slot name>` in the IDE) and their state, both lock-free,
//! so discovery and slot selection never take the probe's lock.
//! ja: `oep.probe.config` の読む側。登録されたスロット(target がつながる場所。IDE の
//! `oep://<probe>/<slot の name>`)とその状態。どちらも lock 無しで読める。

use crate::codec::parse_tlvs;
use crate::registry::probe_config as cfg;
use crate::session::{OepError, Probe, check};

/// One registered slot (oep-if-probe-config §1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub slot: u8,
    /// The wire interface's fn (`oep.wire.rvswd` / `oep.wire.swio`).
    pub wire_fn: u16,
    pub swdio: u16,
    /// 0xFFFF on one wire.
    pub swclk: u16,
    pub at_boot: bool,
    pub retry_s: u16,
    /// The console mechanism (`oep.target.console`).
    pub mechanism: u8,
    pub name: String,
    /// The lock: (scheme, mask, value), when the slot has one.
    pub lock: Option<(u8, Vec<u8>, Vec<u8>)>,
}

impl Slot {
    /// `slot(u8) wire_fn(u16) swdio(u16) swclk(u16) attach(u8) retry_s(u16) mechanism(u8)
    /// name_len(u8) name lock_scheme(u8) [lock_mask(n) lock_value(n)]`
    fn parse(v: &[u8]) -> Option<Slot> {
        if v.len() < 13 {
            return None;
        }
        let le16 = |i: usize| u16::from_le_bytes([v[i], v[i + 1]]);
        let name_end = 12 + usize::from(v[11]);
        let name = String::from_utf8_lossy(v.get(12..name_end)?).into_owned();
        let scheme = *v.get(name_end)?;
        let lock = if scheme == 0 {
            None
        } else {
            let rest = &v[name_end + 1..];
            let n = rest.len() / 2;
            (n >= 1).then(|| (scheme, rest[..n].to_vec(), rest[n..2 * n].to_vec()))
        };
        Some(Slot {
            slot: v[0],
            wire_fn: le16(1),
            swdio: le16(3),
            swclk: le16(5),
            at_boot: v[7] == cfg::enums::slot_attach::AT_BOOT,
            retry_s: le16(8),
            mechanism: v[10],
            name,
            lock,
        })
    }
}

/// A slot's state (oep-if-probe-config §3.2, describe `slot_state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotState {
    pub slot: u8,
    /// 0 connected, 1 absent, 2 lock mismatch, 3 no target id.
    pub state: u8,
    /// The slot's connection, when it has one.
    pub connection: Option<u16>,
    /// ms since the last automatic attach try (None: never tried).
    pub last_try_ms: Option<u32>,
    /// The target id seen: (scheme, value).
    pub target_id: Option<(u8, Vec<u8>)>,
}

impl SlotState {
    fn parse(v: &[u8]) -> Option<SlotState> {
        if v.len() < 10 {
            return None;
        }
        let conn = u16::from_le_bytes([v[2], v[3]]);
        let last = u32::from_le_bytes([v[4], v[5], v[6], v[7]]);
        let scheme = v[8];
        let len = usize::from(v[9]);
        let tid = v.get(10..10 + len)?.to_vec();
        Some(SlotState {
            slot: v[0],
            state: v[1],
            connection: (conn != 0).then_some(conn),
            last_try_ms: (last != u32::MAX).then_some(last),
            target_id: (scheme != 0).then_some((scheme, tid)),
        })
    }

    /// The WCH chip id seen (target_id scheme 1), if any.
    pub fn wch_chip_id(&self) -> Option<u32> {
        match &self.target_id {
            Some((s, v))
                if *s == crate::registry::wire_rvswd::enums::target_id_scheme::WCH_DMI_7F
                    && v.len() == 4 =>
            {
                Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            }
            _ => None,
        }
    }
}

/// The registered slots (`get`, following `more`). Empty when the probe has no `oep.probe.config`.
pub fn slots(p: &mut Probe) -> Result<Vec<Slot>, OepError> {
    let Ok(func) = p.interface(cfg::NAME).map(|i| i.func) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut first: u16 = 0;
    loop {
        let r = p.link().call(crate::link::Call {
            func,
            op: cfg::op::GET,
            session: None,
            payload: first.to_le_bytes().to_vec(),
        })?;
        let a = check(r)?;
        if a.len() < 5 {
            return Err(OepError::Malformed("config get answer too short".into()));
        }
        let items = parse_tlvs(&a[5..]).map_err(|e| OepError::Malformed(e.to_string()))?;
        first = first.saturating_add(items.len() as u16);
        for t in &items {
            if t.tag & 0x7F == cfg::tlvs::item::SLOT
                && let Some(s) = Slot::parse(&t.value)
            {
                out.push(s);
            }
        }
        if a[0] == 0 || items.is_empty() {
            return Ok(out);
        }
    }
}

/// Every slot's state (describe `slot_state`), lock-free.
pub fn slot_states(p: &mut Probe) -> Result<Vec<SlotState>, OepError> {
    let Ok(func) = p.interface(cfg::NAME).map(|i| i.func) else {
        return Ok(Vec::new());
    };
    Ok(p.describe(func)?
        .iter()
        .filter(|t| t.tag == cfg::tlvs::describe::SLOT_STATE)
        .filter_map(|t| SlotState::parse(&t.value))
        .collect())
}
