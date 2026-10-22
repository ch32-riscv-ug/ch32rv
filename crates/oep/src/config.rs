//! en: Reading the probe's settings (core §13) and the host's label convention (host guide
//! §5.3). The core gathers every owner's items: fn 0 `get` (op 0x20) pages through the values,
//! each `fn number key origin len data`, and an item's number is its place among the `setting`s
//! its owner fn declares in describe, so items are looked up by name. A probe keeps no wiring: a
//! target's port is the `S` of the labels `S.swdio` / `S.swclk` / `S.swio` / `S.nrst` (core item
//! `channel.{}.label`, whatever its origin), and its lines are found by those labels. A probe
//! on an older firmware still answers through `oep.probe.config` (its own get, the saved labels,
//! and the firmware's fixed names in fn 0's describe); that is read the same way. Lock-free.
//! ja: probe の設定(core §13)の読む側とラベルの約束。fn 0 の get で値を読み、番号は持ち主の fn の
//! describe の setting の位置。ポートはラベル `S.swdio` などの `S`(core の `channel.{}.label`、
//! 出どころは問わない)。古い firmware の `oep.probe.config` も同じように読む。

use crate::registry::{core as oep_core, describe_common};
use crate::session::{OepError, Probe, check};

/// One value: its owner fn, the item's name, its key (a channel for `channel.{}.*`), origin
/// (1 fixed, 2 default, 3 user; 3 for the older `oep.probe.config`) and data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub func: u16,
    pub name: String,
    pub key: u16,
    pub origin: u8,
    pub data: Vec<u8>,
}

/// en: The older firmware's `oep.probe.config` (before oep-spec 9fc6ad9): its get, its describe
/// `setting`, its label item, and fn 0's describe `label` (the firmware's fixed names).
/// ja: 古い firmware の `oep.probe.config` の番号(oep-spec 9fc6ad9 より前)。
mod legacy {
    pub const NAME: &str = "oep.probe.config";
    pub const GET: u8 = 0x10;
    pub const SETTING: u8 = 0x41;
    pub const LABEL: &str = "label";
    pub const FIRMWARE_LABEL: u8 = 0x46;
}

/// The names of the items `func` declares, in describe order (index k = item number k + 1).
/// `setting` is `form key_kind keys(u16) max_len flags name` (core §13.2); the older one had no
/// key_kind.
fn setting_names(p: &mut Probe, func: u16, tag: u8, head: usize) -> Result<Vec<String>, OepError> {
    Ok(p.describe(func)?
        .into_iter()
        .filter(|t| t.tag == tag && t.value.len() > head)
        .map(|t| String::from_utf8_lossy(&t.value[head..]).into_owned())
        .collect())
}

/// en: Every value of the probe's settings (`get`, paged, lock-free); a generation that changes
/// between pages means the settings moved, and the read starts over. Empty when the probe has no
/// settings at all.
/// ja: 設定のすべての値(get、ページ送り、lock 無し)。途中で generation が変われば読み直す。
pub fn items(p: &mut Probe) -> Result<Vec<Item>, OepError> {
    match read_values(p, oep_core::FN, oep_core::op::GET, true) {
        Err(OepError::Rejected { .. }) => {}
        r => return r,
    }
    let Ok(func) = p.interface(legacy::NAME).map(|i| i.func) else {
        return Ok(Vec::new());
    };
    read_values(p, func, legacy::GET, false)
}

/// One full read: the core's get (`core`) or the older interface's (`fn` is then `func`, no
/// origin in the value).
fn read_values(p: &mut Probe, func: u16, op: u8, core: bool) -> Result<Vec<Item>, OepError> {
    let mut names: std::collections::HashMap<u16, Vec<String>> = std::collections::HashMap::new();
    'again: for _ in 0..8 {
        let mut out = Vec::new();
        let mut generation = None;
        loop {
            let a = check(p.link().call(crate::link::Call {
                func,
                op,
                session: None,
                payload: (out.len() as u16).to_le_bytes().to_vec(),
            })?)?;
            if a.len() < 6 {
                return Err(OepError::Malformed("settings get answer too short".into()));
            }
            let more = a[0];
            let generation_now = u32::from_le_bytes([a[1], a[2], a[3], a[4]]);
            let count = usize::from(a[5]);
            if *generation.get_or_insert(generation_now) != generation_now {
                continue 'again;
            }
            let mut at = 6;
            for _ in 0..count {
                let cut = || OepError::Malformed("settings value cut short".into());
                // core: fn(u16) number key(u16) origin len(u16) data; older: number len(u16)
                // key(u16) data, len counting key and data.
                let (owner, number, key, origin, data, next) = if core {
                    let e = a.get(at..at + 8).ok_or_else(cut)?;
                    let len = usize::from(u16::from_le_bytes([e[6], e[7]]));
                    let data = a.get(at + 8..at + 8 + len).ok_or_else(cut)?.to_vec();
                    let key = u16::from_le_bytes([e[3], e[4]]);
                    (
                        u16::from_le_bytes([e[0], e[1]]),
                        e[2],
                        key,
                        e[5],
                        data,
                        at + 8 + len,
                    )
                } else {
                    let e = a.get(at..at + 5).ok_or_else(cut)?;
                    let len = usize::from(u16::from_le_bytes([e[1], e[2]]));
                    if len < 2 {
                        return Err(OepError::Malformed(format!("config item len {len}")));
                    }
                    let data = a.get(at + 5..at + 3 + len).ok_or_else(cut)?.to_vec();
                    (
                        func,
                        e[0],
                        u16::from_le_bytes([e[3], e[4]]),
                        3,
                        data,
                        at + 3 + len,
                    )
                };
                if number == 0 {
                    return Err(OepError::Malformed("settings value number 0".into()));
                }
                if let std::collections::hash_map::Entry::Vacant(e) = names.entry(owner) {
                    let declared = if core {
                        setting_names(p, owner, describe_common::SETTING, 6)
                    } else {
                        setting_names(p, owner, legacy::SETTING, 5)
                    };
                    e.insert(declared.unwrap_or_default());
                }
                let name = names[&owner]
                    .get(usize::from(number) - 1)
                    .cloned()
                    .unwrap_or_else(|| number.to_string());
                out.push(Item {
                    func: owner,
                    name,
                    key,
                    origin,
                    data,
                });
                at = next;
            }
            if more == 0 || count == 0 {
                return Ok(out);
            }
        }
    }
    Err(OepError::Malformed(
        "the settings kept changing while they were read".into(),
    ))
}

/// The labels as (channel, text): the core's `channel.{}.label` values, or the older
/// interface's saved `label` items.
pub fn labels(p: &mut Probe) -> Result<Vec<(u16, String)>, OepError> {
    Ok(items(p)?
        .into_iter()
        .filter(|it| {
            (it.func == oep_core::FN && it.name == oep_core::settings::CHANNEL_LABEL)
                || (it.func != oep_core::FN && it.name == legacy::LABEL)
        })
        .map(|it| (it.key, String::from_utf8_lossy(&it.data).into_owned()))
        .collect())
}

/// en: The older firmware's fixed channel names (fn 0 describe `label`: channel(u16) text,
/// repeats), searched after the saved labels. Empty on a probe with core settings: its fixed
/// labels are `channel.{}.label` values already in [`labels`].
/// ja: 古い firmware の固定の名前。core の設定を持つ probe では空(固定のラベルも `labels` に入る)。
pub fn firmware_labels(p: &mut Probe) -> Result<Vec<(u16, String)>, OepError> {
    if p.interface(legacy::NAME).is_err() {
        return Ok(Vec::new());
    }
    Ok(p.describe(oep_core::FN)?
        .into_iter()
        .filter(|t| t.tag == legacy::FIRMWARE_LABEL && t.value.len() >= 2)
        .map(|t| {
            (
                u16::from_le_bytes([t.value[0], t.value[1]]),
                String::from_utf8_lossy(&t.value[2..]).into_owned(),
            )
        })
        .collect())
}

/// en: The ports the saved labels name (host guide §5.3): the distinct `S` of the `S.N` labels,
/// ASCII-lowercased, in order. None named: the probe has one unnamed port.
/// ja: ラベルが名指すポート(`S.N` の `S`、小文字にして重複なく)。
pub fn ports_named(labels: &[(u16, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, text) in labels {
        if let Some((port, rest)) = text.split_once('.')
            && !port.is_empty()
            && !rest.is_empty()
        {
            let port = port.to_ascii_lowercase();
            if !out.contains(&port) {
                out.push(port);
            }
        }
    }
    out
}

/// en: The channel of the line `name` (`nrst`, `power_hi`, `power_lo`, `swdio`, `swclk`, `swio`)
/// by host guide §5.3, case aside, stopping at the first step that finds exactly one: (a) with a
/// port, the saved label `<port>.<name>`; (b) only while at most one port is named, the saved
/// label `<name>`; (c) the same, the firmware label `<name>`. Two at one step: no line.
/// ja: 線の channel(ホストガイド §5.3)。`<port>.<name>`、次に(ポートが 1 つ以下のときだけ)素の名前を
/// 保存したラベル、ファームのラベルの順。同じ段で 2 つ見つかれば線は無い。
pub fn find_line(
    labels: &[(u16, String)],
    firmware: &[(u16, String)],
    port: Option<&str>,
    name: &str,
) -> Option<u16> {
    let single = ports_named(labels).len() <= 1;
    let scoped = port.map(|s| format!("{s}.{name}"));
    let mut steps: Vec<(&[(u16, String)], &str)> = Vec::new();
    if let Some(s) = scoped.as_deref() {
        steps.push((labels, s));
    }
    if single {
        steps.push((labels, name));
        steps.push((firmware, name));
    }
    for (source, want) in steps {
        let mut hits: Vec<u16> = source
            .iter()
            .filter(|(_, t)| t.eq_ignore_ascii_case(want))
            .map(|(c, _)| *c)
            .collect();
        hits.sort_unstable();
        hits.dedup();
        match hits.as_slice() {
            [] => {}
            [c] => return Some(*c),
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(c: u16, t: &str) -> (u16, String) {
        (c, t.to_owned())
    }

    #[test]
    fn ports_are_the_prefixes_of_dotted_labels() {
        let labels = [
            l(3, "V003.swio"),
            l(4, "v003.nrst"),
            l(5, "L103.swdio"),
            l(6, "led"),
        ];
        assert_eq!(ports_named(&labels), vec!["v003", "l103"]);
    }

    #[test]
    fn find_line_scoped_then_bare_on_one_port() {
        let labels = [l(3, "dut.swio"), l(4, "nrst")];
        assert_eq!(find_line(&labels, &[], Some("dut"), "swio"), Some(3));
        // One port named: the bare label counts.
        assert_eq!(find_line(&labels, &[], Some("dut"), "nrst"), Some(4));
        // Firmware label when no saved one.
        assert_eq!(
            find_line(&labels, &[l(9, "power_hi")], None, "power_hi"),
            Some(9)
        );
    }

    #[test]
    fn find_line_bare_is_ignored_with_several_ports() {
        let labels = [l(3, "a.swio"), l(5, "b.swio"), l(4, "nrst")];
        assert_eq!(find_line(&labels, &[], Some("a"), "nrst"), None);
        assert_eq!(find_line(&labels, &[], Some("b"), "SWIO"), Some(5));
    }

    #[test]
    fn find_line_two_at_one_step_is_no_line() {
        let labels = [l(3, "nrst"), l(4, "NRST")];
        assert_eq!(find_line(&labels, &[], None, "nrst"), None);
    }
}
