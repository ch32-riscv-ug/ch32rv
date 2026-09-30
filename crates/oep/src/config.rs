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
    /// The line speed ceiling the slot's attach takes (Hz; `None` = no ceiling, or a probe from
    /// before slots carried one).
    pub max_speed: Option<u32>,
    /// How the slot's line idles (`wire_rvswd::enums::idle_clock`: 0 high, 1 low).
    pub idle_clock: u8,
    /// The console mechanism (`oep.target.console`).
    pub mechanism: u8,
    pub name: String,
    /// The lock: (scheme, mask, value), when the slot has one.
    pub lock: Option<(u8, Vec<u8>, Vec<u8>)>,
}

impl Slot {
    /// en: `slot(u8) wire_fn(u16) swdio(u16) swclk(u16) attach(u8) retry_s(u16) max_speed(u32)
    /// idle_clock(u8) mechanism(u8) name_len(u8) name lock_scheme(u8) [lock_mask(n) lock_value(n)]`
    /// (oep-spec 5bfe052). A probe from before that (oep-probe-arduino 0.0.5) has no max_speed /
    /// idle_clock (name at 12, not 17); the revision did not change, so the shape is told from
    /// the item itself: the one whose name fits, reads as text and leaves an even lock tail.
    /// ja: 5bfe052 からの形。それより前の probe(0.0.5)には max_speed / idle_clock が無い(name が 12、
    /// 今は 17)。revision は変わっていないので、名前が収まり、文字として読め、lock の残りが偶数の方で読む。
    fn parse(v: &[u8]) -> Option<Slot> {
        Self::parse_at(v, true).or_else(|| Self::parse_at(v, false))
    }

    fn parse_at(v: &[u8], line: bool) -> Option<Slot> {
        let fixed = if line { 17 } else { 12 };
        if v.len() < fixed + 1 {
            return None;
        }
        let le16 = |i: usize| u16::from_le_bytes([v[i], v[i + 1]]);
        let name_len = usize::from(v[fixed - 1]);
        let name_end = fixed + name_len;
        let name = std::str::from_utf8(v.get(fixed..name_end)?).ok()?;
        if name.is_empty() || name.chars().any(char::is_control) {
            return None;
        }
        let scheme = *v.get(name_end)?;
        let rest = &v[name_end + 1..];
        let lock = if scheme == 0 {
            if !rest.is_empty() {
                return None;
            }
            None
        } else {
            if rest.is_empty() || !rest.len().is_multiple_of(2) {
                return None;
            }
            let n = rest.len() / 2;
            Some((scheme, rest[..n].to_vec(), rest[n..].to_vec()))
        };
        let (max_speed, idle_clock, mechanism) = if line {
            let hz = u32::from_le_bytes([v[10], v[11], v[12], v[13]]);
            ((hz != 0).then_some(hz), v[14], v[15])
        } else {
            (None, 0, v[10])
        };
        Some(Slot {
            slot: v[0],
            wire_fn: le16(1),
            swdio: le16(3),
            swclk: le16(5),
            at_boot: v[7] == cfg::enums::slot_attach::AT_BOOT,
            retry_s: le16(8),
            max_speed,
            idle_clock,
            mechanism,
            name: name.to_owned(),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn item(line: bool, name: &str, lock: &[u8]) -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend_from_slice(&3u16.to_le_bytes()); // wire_fn
        v.extend_from_slice(&7u16.to_le_bytes()); // swdio
        v.extend_from_slice(&8u16.to_le_bytes()); // swclk
        v.push(cfg::enums::slot_attach::AT_BOOT);
        v.extend_from_slice(&5u16.to_le_bytes()); // retry_s
        if line {
            v.extend_from_slice(&400_000u32.to_le_bytes());
            v.push(1); // idle_clock low
        }
        v.push(2); // mechanism dmseq
        v.push(name.len() as u8);
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(lock);
        v
    }

    #[test]
    fn reads_the_slot_with_line_settings() {
        let s = Slot::parse(&item(true, "x035", &[0])).unwrap();
        assert_eq!(s.name, "x035");
        assert_eq!((s.swdio, s.swclk, s.retry_s), (7, 8, 5));
        assert_eq!(s.max_speed, Some(400_000));
        assert_eq!((s.idle_clock, s.mechanism), (1, 2));
        assert!(s.at_boot && s.lock.is_none());
    }

    #[test]
    fn still_reads_the_older_shape() {
        let s = Slot::parse(&item(false, "v003", &[1, 0xff, 0x09])).unwrap();
        assert_eq!(s.name, "v003");
        assert_eq!((s.max_speed, s.idle_clock, s.mechanism), (None, 0, 2));
        assert_eq!(s.lock, Some((1, vec![0xff], vec![0x09])));
    }

    #[test]
    fn a_locked_slot_with_line_settings() {
        let s = Slot::parse(&item(true, "x", &[1, 0xff, 0xff, 0x35, 0x06])).unwrap();
        assert_eq!(s.name, "x");
        assert_eq!(s.lock, Some((1, vec![0xff, 0xff], vec![0x35, 0x06])));
    }
}
