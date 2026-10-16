//! en: `oep.probe.config` read side (oep-if-probe-config) and the host's label convention (host
//! guide §5.3). Settings are a named list: the k-th `setting` the config fn declares in describe
//! is item number k for this boot, so items are looked up by name. A probe keeps no wiring: a
//! target's port is the `S` of the saved labels `S.swdio` / `S.swclk` / `S.swio` / `S.nrst`, and
//! its lines are found by those labels. Everything here is lock-free.
//! ja: `oep.probe.config` の読む側とラベルの約束。設定は名前の一覧で、番号はその起動の describe の
//! 位置。probe は配線を持たず、ポートは保存したラベル `S.swdio` などの `S`、線はラベルで探す。

use crate::registry::{core as oep_core, probe_config as cfg};
use crate::session::{OepError, Probe, check};

/// One item: its setting's name, its key (a channel for `label` / `idle` / `disable`) and data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub key: u16,
    pub data: Vec<u8>,
}

/// The settings the config fn declares, in describe order: index k = item number k + 1.
fn settings(p: &mut Probe, func: u16) -> Result<Vec<String>, OepError> {
    // setting: form(u8) keys(u16) max_len(u8) flags(u8) name (oep-if-probe-config §4).
    Ok(p.describe(func)?
        .into_iter()
        .filter(|t| t.tag == cfg::tlvs::describe::SETTING && t.value.len() >= 5)
        .map(|t| String::from_utf8_lossy(&t.value[5..]).into_owned())
        .collect())
}

/// en: Every item of the probe's configuration (`get`, paged, lock-free). The answer is
/// `more(u8) generation(u32) count(u8)`, then count x `number(u8) len(u16) key(u16) data` (len
/// counts key and data), then a TLV tail; a generation that changes between pages means the
/// settings moved, and the read starts over. Empty when the probe has no `oep.probe.config`.
/// ja: 設定のすべての item(get、ページ送り、lock 無し)。途中で generation が変われば読み直す。
pub fn items(p: &mut Probe) -> Result<Vec<Item>, OepError> {
    let Ok(func) = p.interface(cfg::NAME).map(|i| i.func) else {
        return Ok(Vec::new());
    };
    let names = settings(p, func)?;
    'again: for _ in 0..8 {
        let mut out = Vec::new();
        let mut generation = None;
        loop {
            let a = check(p.link().call(crate::link::Call {
                func,
                op: cfg::op::GET,
                session: None,
                payload: (out.len() as u16).to_le_bytes().to_vec(),
            })?)?;
            if a.len() < 6 {
                return Err(OepError::Malformed("config get answer too short".into()));
            }
            let more = a[0];
            let generation_now = u32::from_le_bytes([a[1], a[2], a[3], a[4]]);
            let count = usize::from(a[5]);
            if *generation.get_or_insert(generation_now) != generation_now {
                continue 'again;
            }
            let mut at = 6;
            for _ in 0..count {
                let e = a
                    .get(at..at + 5)
                    .ok_or_else(|| OepError::Malformed("config item cut short".into()))?;
                let number = e[0];
                let len = usize::from(u16::from_le_bytes([e[1], e[2]]));
                if number == 0 || len < 2 {
                    return Err(OepError::Malformed(format!(
                        "config item {number} len {len}"
                    )));
                }
                let data = a
                    .get(at + 5..at + 3 + len)
                    .ok_or_else(|| OepError::Malformed("config item data cut short".into()))?
                    .to_vec();
                let name = names
                    .get(usize::from(number) - 1)
                    .cloned()
                    .unwrap_or_else(|| number.to_string());
                out.push(Item {
                    name,
                    key: u16::from_le_bytes([e[3], e[4]]),
                    data,
                });
                at += 3 + len;
            }
            if more == 0 || count == 0 {
                return Ok(out);
            }
        }
    }
    Err(OepError::Malformed(
        "the configuration kept changing while it was read".into(),
    ))
}

/// The saved labels as (channel, text): the `label` items (key = channel, data = 1-32 bytes).
pub fn labels(p: &mut Probe) -> Result<Vec<(u16, String)>, OepError> {
    Ok(items(p)?
        .into_iter()
        .filter(|it| it.name == cfg::names::setting::LABEL)
        .map(|it| (it.key, String::from_utf8_lossy(&it.data).into_owned()))
        .collect())
}

/// The firmware's fixed channel names (fn 0 describe `label`: channel(u16) text, repeats).
pub fn firmware_labels(p: &mut Probe) -> Result<Vec<(u16, String)>, OepError> {
    Ok(p.describe(oep_core::FN)?
        .into_iter()
        .filter(|t| t.tag == oep_core::tlvs::describe::LABEL && t.value.len() >= 2)
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
