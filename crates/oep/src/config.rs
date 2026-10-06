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
    /// How often an at-boot slot that found nothing tries again (ms; 0 = never).
    pub retry_ms: u32,
    /// The line speed ceiling the slot's attach takes (Hz; `None` = no ceiling).
    pub max_speed: Option<u32>,
    /// How the slot's line idles (`wire_rvswd::enums::idle_clock`: 0 high, 1 low).
    pub idle_clock: u8,
    /// The console mechanism (`oep.target.console`), or 0xFF: a slot without a console.
    pub mechanism: u8,
    pub name: String,
    /// The lock: (scheme, mask, value), when the slot has one.
    pub lock: Option<(u8, Vec<u8>, Vec<u8>)>,
}

impl Slot {
    /// en: `slot(u8) wire_fn(u16) swdio(u16) swclk(u16) attach(u8) boot_reset(u8) retry_ms(u32)
    /// max_speed_hz(u32) idle_clock(u8) mechanism(u8) name_len(u8) name lock_len(u8)
    /// [lock_scheme(u8) lock_mask(n) lock_value(n)]`, lock_len = 0 or 1 + 2n; the item ends with
    /// the lock (oep-if-probe-config §1.1, core §2.3). Only the current shape: until v1 is
    /// frozen the tools follow each change together, with no compatibility for older probes (the
    /// user's policy, 2026-09-30).
    /// ja: 今の形だけを読む(v1 の凍結までは各ツールが変更にまとめて追従し、古い probe との互換は持たない)。
    fn parse(v: &[u8]) -> Option<Slot> {
        const FIXED: usize = 20;
        if v.len() < FIXED + 1 {
            return None;
        }
        let le16 = |i: usize| u16::from_le_bytes([v[i], v[i + 1]]);
        let le32 = |i: usize| u32::from_le_bytes([v[i], v[i + 1], v[i + 2], v[i + 3]]);
        let name_end = FIXED + usize::from(v[FIXED - 1]);
        let name = String::from_utf8_lossy(v.get(FIXED..name_end)?).into_owned();
        let lock_len = usize::from(*v.get(name_end)?);
        let lock = if lock_len == 0 {
            None
        } else {
            let l = v.get(name_end + 1..name_end + 1 + lock_len)?;
            let n = (lock_len - 1) / 2;
            (n >= 1).then(|| (l[0], l[1..1 + n].to_vec(), l[1 + n..1 + 2 * n].to_vec()))
        };
        let hz = le32(13);
        Some(Slot {
            slot: v[0],
            wire_fn: le16(1),
            swdio: le16(3),
            swclk: le16(5),
            at_boot: v[7] == cfg::enums::slot_attach::AT_BOOT,
            retry_ms: le32(9),
            max_speed: (hz != 0).then_some(hz),
            idle_clock: v[17],
            mechanism: v[18],
            name,
            lock,
        })
    }
}

/// A slot's state (oep-if-probe-config §3.2, the `state` op's `slot_state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotState {
    pub slot: u8,
    /// 0 connected, 1 absent, 2 lock mismatch, 3 no target id.
    pub state: u8,
    /// The slot's connection, when it has one.
    pub connection: Option<u16>,
    /// When the last automatic attach was tried (the probe's clock, ns; None: never tried).
    pub last_try_at_ns: Option<u64>,
    /// The target id seen: (scheme, value).
    pub target_id: Option<(u8, Vec<u8>)>,
}

impl SlotState {
    /// `slot(u8) state(u8) connection(u16) last_try_at_ns(u64) reset_at_ns(u64) tid_scheme(u8)
    /// tid_len(u8) tid` (oep-if-probe-config §3.3): the state and the bytes it took.
    fn parse(v: &[u8]) -> Option<(SlotState, usize)> {
        if v.len() < 22 {
            return None;
        }
        let conn = u16::from_le_bytes([v[2], v[3]]);
        let mut last = [0u8; 8];
        last.copy_from_slice(&v[4..12]);
        let last = u64::from_le_bytes(last);
        let scheme = v[20];
        let len = usize::from(v[21]);
        let tid = v.get(22..22 + len)?.to_vec();
        let st = SlotState {
            slot: v[0],
            state: v[1],
            connection: (conn != 0).then_some(conn),
            last_try_at_ns: (last != u64::MAX).then_some(last),
            target_id: (scheme != 0).then_some((scheme, tid)),
        };
        Some((st, 22 + len))
    }

    /// The WCH chip id seen (target_id scheme 1), if any.
    pub fn wch_chip_id(&self) -> Option<u32> {
        match &self.target_id {
            Some((s, v))
                if *s == crate::registry::common::enum_::target_id_scheme::WCH_DMI_7F
                    && v.len() == 4 =>
            {
                Some(u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            }
            _ => None,
        }
    }
}

/// Every item of the probe's configuration (`get`, following `more`), lock-free. Empty when the
/// probe has no `oep.probe.config`.
fn items(p: &mut Probe) -> Result<Vec<crate::codec::Tlv>, OepError> {
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
        let got = parse_tlvs(&a[5..]).map_err(|e| OepError::Malformed(e.to_string()))?;
        first = first.saturating_add(got.len() as u16);
        let n = got.len();
        out.extend(got);
        if a[0] == 0 || n == 0 {
            return Ok(out);
        }
    }
}

/// The registered slots (`get`, following `more`). Empty when the probe has no `oep.probe.config`.
pub fn slots(p: &mut Probe) -> Result<Vec<Slot>, OepError> {
    Ok(items(p)?
        .iter()
        .filter(|t| t.tag & 0x7F == cfg::tlvs::item::SLOT)
        .filter_map(|t| Slot::parse(&t.value))
        .collect())
}

/// The channel names set in the configuration (label items: `channel(u16) text`).
pub fn labels(p: &mut Probe) -> Result<Vec<(u16, String)>, OepError> {
    Ok(items(p)?
        .iter()
        .filter(|t| t.tag & 0x7F == cfg::tlvs::item::LABEL && t.value.len() >= 2)
        .map(|t| {
            (
                u16::from_le_bytes([t.value[0], t.value[1]]),
                String::from_utf8_lossy(&t.value[2..]).into_owned(),
            )
        })
        .collect())
}

/// en: The channel of the line `name` (`nrst`, `power_hi`, `power_lo`) for the slot `slot`, by
/// oep-if-probe-config §1.3 (the probe uses the same rule for its own reset retry): names compared
/// case aside, `<slot>.<name>` first, then the bare `<name>` - the bare one only on a probe with at
/// most one slot. Two channels matching at the same step is no line. `Ok(None)`: no such line.
/// `Err`: only a bare name on a probe with several slots (said so, rather than no line).
/// ja: スロットの線の channel(§1.3、probe の自分のやり直しと同じ規則)。大小は区別せず、`<slot>.<name>`
/// を先に、無ければ素の名前(スロットが 1 つ以下だけ)。同じ段で 2 つ一致したら線は無い。
pub fn find_line(
    labels: &[(u16, String)],
    slot: &str,
    slot_count: usize,
    name: &str,
) -> Result<Option<u16>, String> {
    let one = |want: &str| -> Option<Option<u16>> {
        let hits: Vec<u16> = labels
            .iter()
            .filter(|(_, t)| t.eq_ignore_ascii_case(want))
            .map(|(c, _)| *c)
            .collect();
        match hits.as_slice() {
            [] => None,
            [c] => Some(Some(*c)),
            _ => Some(None), // two at the same step: no line
        }
    };
    let scoped = format!("{slot}.{name}");
    if let Some(found) = one(&scoped) {
        return Ok(found);
    }
    match one(name) {
        Some(found) if slot_count <= 1 => Ok(found),
        Some(_) => Err(format!(
            "the probe has {slot_count} slots and only a bare `{name}` label: name it `{scoped}` for this slot"
        )),
        None => Ok(None),
    }
}

/// Every slot's state (the `state` op, following `more`), lock-free.
pub fn slot_states(p: &mut Probe) -> Result<Vec<SlotState>, OepError> {
    let Ok(func) = p.interface(cfg::NAME).map(|i| i.func) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut first_slot: u8 = 0;
    for _ in 0..64 {
        let r = p.link().call(crate::link::Call {
            func,
            op: cfg::op::STATE,
            session: None,
            payload: vec![first_slot, 0],
        })?;
        let a = check(r)?;
        // more, storage_state, storage_hash(u32), unreadable_reason, n_slots, n_slots x slot_state,
        // ... (no element length, core §2.3)
        let (Some(&more), Some(&n)) = (a.first(), a.get(7)) else {
            return Err(OepError::Malformed("config state answer too short".into()));
        };
        let mut at = 8;
        for _ in 0..n {
            let Some((st, used)) = a.get(at..).and_then(SlotState::parse) else {
                return Err(OepError::Malformed("config state entry cut short".into()));
            };
            at += used;
            out.push(st);
        }
        first_slot = first_slot.saturating_add(n);
        if more == 0 || n == 0 {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn item(name: &str, lock: &[u8]) -> Vec<u8> {
        let mut v = vec![1u8];
        v.extend_from_slice(&3u16.to_le_bytes()); // wire_fn
        v.extend_from_slice(&7u16.to_le_bytes()); // swdio
        v.extend_from_slice(&8u16.to_le_bytes()); // swclk
        v.push(cfg::enums::slot_attach::AT_BOOT);
        v.push(0); // boot_reset
        v.extend_from_slice(&5000u32.to_le_bytes()); // retry_ms
        v.extend_from_slice(&400_000u32.to_le_bytes());
        v.push(1); // idle_clock low
        v.push(2); // mechanism dmseq
        v.push(name.len() as u8);
        v.extend_from_slice(name.as_bytes());
        v.push(lock.len() as u8); // lock_len
        v.extend_from_slice(lock);
        v
    }

    #[test]
    fn a_line_is_found_by_slot_then_bare_name() {
        let labels = vec![(23u16, "nrst".to_owned()), (5, "x035.power_hi".to_owned())];
        assert_eq!(find_line(&labels, "v003", 1, "nrst"), Ok(Some(23)));
        assert!(find_line(&labels, "v003", 2, "nrst").is_err());
        assert_eq!(find_line(&labels, "x035", 2, "power_hi"), Ok(Some(5)));
        assert_eq!(find_line(&labels, "x035", 2, "power_lo"), Ok(None));
        // Case aside, and two at the same step is no line (oep-if-probe-config §1.3).
        assert_eq!(find_line(&labels, "X035", 2, "POWER_HI"), Ok(Some(5)));
        let two = vec![(1u16, "v003.nrst".to_owned()), (2, "V003.NRST".to_owned())];
        assert_eq!(find_line(&two, "v003", 1, "nrst"), Ok(None));
    }

    #[test]
    fn reads_the_slot_with_line_settings() {
        let s = Slot::parse(&item("x035", &[])).unwrap();
        assert_eq!(s.name, "x035");
        assert_eq!((s.swdio, s.swclk, s.retry_ms), (7, 8, 5000));
        assert_eq!(s.max_speed, Some(400_000));
        assert_eq!((s.idle_clock, s.mechanism), (1, 2));
        assert!(s.at_boot && s.lock.is_none());
    }

    #[test]
    fn a_locked_slot_with_line_settings() {
        let s = Slot::parse(&item("x", &[1, 0xff, 0xff, 0x35, 0x06])).unwrap();
        assert_eq!(s.name, "x");
        assert_eq!(s.lock, Some((1, vec![0xff, 0xff], vec![0x35, 0x06])));
    }

    #[test]
    fn a_slot_state_with_its_reset_time_and_tid() {
        let mut v = vec![0u8, 0, 2, 0];
        v.extend_from_slice(&7u64.to_le_bytes()); // last_try_at_ns
        v.extend_from_slice(&u64::MAX.to_le_bytes()); // reset_at_ns: none
        v.extend_from_slice(&[1, 4, 0x00, 0x05, 0x31, 0x20]);
        v.push(0xEE); // the next element
        let (st, used) = SlotState::parse(&v).unwrap();
        assert_eq!(used, 26);
        assert_eq!(st.connection, Some(2));
        assert_eq!(st.wch_chip_id(), Some(0x2031_0500));
    }
}
