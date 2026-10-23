//! en: Reading the probe's settings (core §13) and the host's port convention (host guide §5.3).
//! The core gathers every owner's items: fn 0 `get` (op 0x20) pages through the values, each
//! `fn number key len data`, and an item's number is its place among the `setting`s its owner fn
//! declares in describe, so items are looked up by name. A probe keeps no wiring: a target's port
//! is the `S` of the labels `S.N` (core item `channel.{}.label`; `S` a letter then letters, digits
//! and `-`, 1-16 of them; `N` letters, digits, `-` and `_`), kept as written and compared ignoring
//! ASCII case, and its lines are found by those labels. A label of another shape names no port.
//! Lock-free.
//! ja: probe の設定(core §13)の読む側と、口の約束(host guide §5.3)。fn 0 の get で値を読み、
//! 番号は持ち主の fn の describe の setting の位置。口はラベル `S.N` の `S`(書いたとおりに持ち、
//! ASCII の大小文字を区別せずに比べる)。形の合わないラベルは口を名付けない。

use crate::registry::{core as oep_core, describe_common};
use crate::session::{OepError, Probe, check};

/// One value: its owner fn, the item's name, its key (a channel for `channel.{}.*`) and data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub func: u16,
    pub name: String,
    pub key: u16,
    pub data: Vec<u8>,
}

/// The names of the items `func` declares, in describe order (index k = item number k + 1).
/// `setting` is `form key_kind keys(u16) max_len flags name` (core §13.2).
fn setting_names(p: &mut Probe, func: u16) -> Result<Vec<String>, OepError> {
    Ok(p.describe(func)?
        .into_iter()
        .filter(|t| t.tag == describe_common::SETTING && t.value.len() > 6)
        .map(|t| String::from_utf8_lossy(&t.value[6..]).into_owned())
        .collect())
}

/// en: Every value of the probe's settings (`get`, paged, lock-free); a generation that changes
/// between pages means the settings moved, and the read starts over. Empty when the probe has no
/// settings at all (no `get` in fn 0's ops: it is refused).
/// ja: 設定のすべての値(get、ページ送り、lock 無し)。途中で generation が変われば読み直す。
pub fn items(p: &mut Probe) -> Result<Vec<Item>, OepError> {
    let mut names: std::collections::HashMap<u16, Vec<String>> = std::collections::HashMap::new();
    'again: for _ in 0..8 {
        let mut out = Vec::new();
        let mut generation = None;
        loop {
            let r = p.link().call(crate::link::Call {
                func: oep_core::FN,
                op: oep_core::op::GET,
                session: None,
                payload: (out.len() as u16).to_le_bytes().to_vec(),
            })?;
            let a = match check(r) {
                Ok(a) => a,
                Err(OepError::Rejected { .. }) => return Ok(Vec::new()),
                Err(e) => return Err(e),
            };
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
                // fn(u16) number(u8) key(u16) len(u16) data (core §13.5).
                let cut = || OepError::Malformed("settings value past the answer".into());
                let e = a.get(at..at + 7).ok_or_else(cut)?;
                let owner = u16::from_le_bytes([e[0], e[1]]);
                let number = e[2];
                let key = u16::from_le_bytes([e[3], e[4]]);
                let len = usize::from(u16::from_le_bytes([e[5], e[6]]));
                let data = a.get(at + 7..at + 7 + len).ok_or_else(cut)?.to_vec();
                if number == 0 {
                    return Err(OepError::Malformed("settings value number 0".into()));
                }
                if let std::collections::hash_map::Entry::Vacant(v) = names.entry(owner) {
                    v.insert(setting_names(p, owner).unwrap_or_default());
                }
                let name = names[&owner]
                    .get(usize::from(number) - 1)
                    .cloned()
                    .unwrap_or_else(|| number.to_string());
                out.push(Item {
                    func: owner,
                    name,
                    key,
                    data,
                });
                at += 7 + len;
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

/// The labels as (channel, text): the core's `channel.{}.label` values.
pub fn labels(p: &mut Probe) -> Result<Vec<(u16, String)>, OepError> {
    Ok(items(p)?
        .into_iter()
        .filter(|it| it.func == oep_core::FN && it.name == oep_core::settings::CHANNEL_LABEL)
        .map(|it| (it.key, String::from_utf8_lossy(&it.data).into_owned()))
        .collect())
}

/// en: A label's `(S, N)` when it names a port's line (host guide §5.3): one `.`, `S` a letter
/// then letters, digits and `-` (1-16), `N` letters, digits, `-` and `_` (1 or more). None for
/// any other label (a pin's plain name such as `NRST`, or `My Port.swdio`).
/// ja: 口の線を名付けるラベルなら `(S, N)`。それ以外(`NRST`、`My Port.swdio` など)は None。
pub fn port_line(label: &str) -> Option<(&str, &str)> {
    let (s, n) = label.split_once('.')?;
    let s_ok = (1..=16).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let n_ok = !n.is_empty()
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    (s_ok && n_ok).then_some((s, n))
}

/// en: The ports the labels name (host guide §5.3): the distinct `S` of the `S.N` labels, as
/// written (the first spelling), distinct ignoring ASCII case, in order. Empty: the probe has no
/// port an address can name.
/// ja: ラベルが名指す口(`S.N` の `S`。書いたとおり、大小文字を区別せずに重ねない)。
pub fn ports_named(labels: &[(u16, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (_, text) in labels {
        if let Some((port, _)) = port_line(text)
            && !out.iter().any(|p| p.eq_ignore_ascii_case(port))
        {
            out.push(port.to_owned());
        }
    }
    out
}

/// en: The channel of the line `name` (`nrst`, `power_hi`, `power_lo`, `swdio`, `swclk`, `swio`,
/// `rx`, `tx`) of `port`: its label `<port>.<name>`, ASCII case aside (host guide §5.3). Labels
/// are unique in a probe ignoring case (core §13.8); two found anyway mean no line.
/// ja: 口 `port` の線 `name` の channel(ラベル `<port>.<name>`、大小文字を区別しない)。
pub fn find_line(labels: &[(u16, String)], port: &str, name: &str) -> Option<u16> {
    let mut hits: Vec<u16> = labels
        .iter()
        .filter(|(_, t)| {
            port_line(t)
                .is_some_and(|(s, n)| s.eq_ignore_ascii_case(port) && n.eq_ignore_ascii_case(name))
        })
        .map(|(c, _)| *c)
        .collect();
    hits.sort_unstable();
    hits.dedup();
    match hits[..] {
        [c] => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(c: u16, t: &str) -> (u16, String) {
        (c, t.to_owned())
    }

    #[test]
    fn ports_are_the_prefixes_of_port_labels_as_written() {
        let labels = [
            l(3, "V003.swio"),
            l(4, "v003.nrst"),
            l(5, "L103.swdio"),
            l(6, "led"),
            l(7, "NRST"),
            l(8, "My Port.swdio"),
            l(9, "9lives.swdio"),
            l(10, "a-very-long-port-name.swdio"),
        ];
        assert_eq!(ports_named(&labels), vec!["V003", "L103"]);
    }

    #[test]
    fn a_line_is_its_port_label_ignoring_case() {
        let labels = [
            l(3, "dut.swio"),
            l(4, "DUT.NRST"),
            l(5, "nrst"),
            l(6, "power_hi"),
        ];
        assert_eq!(find_line(&labels, "dut", "swio"), Some(3));
        assert_eq!(find_line(&labels, "Dut", "nrst"), Some(4));
        // A bare line name names no port's line.
        assert_eq!(find_line(&labels, "dut", "power_hi"), None);
    }

    #[test]
    fn port_line_shapes() {
        assert_eq!(port_line("v003.power_hi"), Some(("v003", "power_hi")));
        assert_eq!(port_line("a.b.c"), None);
        assert_eq!(port_line("x_y.swdio"), None);
        assert_eq!(port_line(".swdio"), None);
        assert_eq!(port_line("v003."), None);
    }
}
