//! en: The one option-byte vocabulary of `target option get|set` (debug probe) and
//! `isp config get|set` (factory bootloader), docs/cli.ja.md §4.3. The USER-byte options go by
//! the WCH EVT `FLASH_UserOptionByteConfig` names (`iwdg=sw|hw` is OB_IWDG_SW / OB_IWDG_HW,
//! `stop-reset=on|off` is OB_STOP_RST / OB_STOP_NoRST, ...), so the polarity is in the value word;
//! a USER bit EVT names no option for is written by its reference-manual field name as
//! `user.<FIELD>=<bit value>`, raw. Both `get`s print the same shape: every key with the value word
//! `set` takes and the raw field value.
//! ja: `target option` と `isp config` の共通の語彙。USER byte は EVT の
//! `FLASH_UserOptionByteConfig` の名前で書き(極性は値の語が持つ)、EVT に名前の無い bit は RM の
//! field 名で `user.<FIELD>=<bit の値>`(生の値)と書く。2 つの `get` は同じ形を出す。

use ch32rv_isp::OptionFields;
use ch32rv_target::{OptionSemantic, OptionUserField};
use serde_json::{Map, Value, json};

/// RDPR value that leaves read protection off.
pub(crate) const RDPR_OFF: u8 = 0xA5;

/// en: What a path knows of its chip's USER byte. ja: その経路が知っている USER byte の配置。
pub(crate) struct Layout {
    /// The family name used in messages.
    pub label: String,
    /// The EVT options; `None` when the family is not known.
    pub semantics: Option<&'static [OptionSemantic]>,
    /// The reference-manual fields from the device DB (`user.<FIELD>`).
    pub user_fields: Vec<OptionUserField>,
}

impl Layout {
    /// The layout of a DB family (semantic options and DB fields).
    pub(crate) fn of(db_family: &str) -> Self {
        Self {
            label: db_family.to_owned(),
            semantics: ch32rv_target::option_semantics(db_family),
            user_fields: ch32rv_target::option_user_fields(db_family),
        }
    }

    fn semantic(&self, key: &str) -> Option<&'static OptionSemantic> {
        self.semantics?.iter().find(|s| s.key == key)
    }

    /// The DB fields that no semantic option covers: the only ones `user.<FIELD>` reaches.
    fn raw_fields(&self) -> impl Iterator<Item = &OptionUserField> {
        self.user_fields.iter().filter(|f| {
            self.semantics
                .unwrap_or_default()
                .iter()
                .all(|s| s.mask() & f.mask() == 0)
        })
    }

    /// en: Every key `set` takes on this chip, for a hint. ja: `set` が受ける key の一覧。
    pub(crate) fn keys(&self) -> String {
        let mut k = vec![
            "read-protect=on|off".to_owned(),
            "write-protect=<mask>".to_owned(),
            "data0=<byte>".to_owned(),
            "data1=<byte>".to_owned(),
        ];
        for s in self.semantics.unwrap_or_default() {
            let words: Vec<&str> = s.values.iter().map(|v| v.word).collect();
            k.push(format!("{}={}", s.key, words.join("|")));
        }
        for f in self.raw_fields() {
            k.push(format!("user.{}=0..{}", f.field, (1u16 << f.width) - 1));
        }
        k.join(" ")
    }

    /// en: The warning when the USER byte is not known. ja: USER byte を知らないときの警告。
    pub(crate) fn interim_warning(&self) -> Option<String> {
        self.semantics.is_none().then(|| {
            format!(
                "the USER-byte options of {} are not known; only read-protect, write-protect, data0 and data1 are decoded",
                self.label
            )
        })
    }
}

/// en: The logical option fields of the 16 option bytes the debug path reads (each byte followed
/// by its complement). ja: debug 線が読む 16 byte(補数つき)の論理値。
pub(crate) fn from_ob16(ob: &[u8; 16]) -> OptionFields {
    OptionFields {
        rdpr: ob[0],
        user: ob[2],
        data0: ob[4],
        data1: ob[6],
        wrpr: u32::from_le_bytes([ob[8], ob[10], ob[12], ob[14]]),
    }
}

/// en: The 16 option bytes with `f` in place and every complement recomputed.
/// ja: `f` を置き、補数を全部計算し直した 16 byte。
pub(crate) fn to_ob16(f: OptionFields) -> [u8; 16] {
    let w = f.wrpr.to_le_bytes();
    let logical = [f.rdpr, f.user, f.data0, f.data1, w[0], w[1], w[2], w[3]];
    let mut ob = [0u8; 16];
    for (i, b) in logical.into_iter().enumerate() {
        ob[2 * i] = b;
        ob[2 * i + 1] = !b;
    }
    ob
}

/// en: What `apply` changed. ja: `apply` が変えたもの。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// `Some(protected)` when read protection changes (the destructive change).
    pub read_protect: Option<bool>,
    /// The pairs as applied, for the confirmation prompt.
    pub changes: Vec<String>,
}

fn number(key: &str, v: &str) -> Result<u32, String> {
    let r = match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => v.parse(),
    };
    r.map_err(|_| format!("{key}: `{v}` is not a number"))
}

/// en: Apply `key=value` pairs to `f`. `rdpr_on` is the RDPR byte this path writes for
/// `read-protect=on` (any value but 0xA5 protects). ja: `key=value` を当てる。
pub(crate) fn apply(
    layout: &Layout,
    f: &mut OptionFields,
    kv: &[String],
    rdpr_on: u8,
) -> Result<Applied, String> {
    let mut out = Applied::default();
    for item in kv {
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| format!("`{item}`: expected key=value"))?;
        match k {
            "data0" | "data1" => {
                let b = u8::try_from(number(k, v)?)
                    .map_err(|_| format!("{k}: `{v}` is out of 0..=255"))?;
                if k == "data0" {
                    f.data0 = b;
                } else {
                    f.data1 = b;
                }
                out.changes.push(format!("{k}=0x{b:02x}"));
            }
            "write-protect" => {
                // en: EVT FLASH_EnableWriteProtection: a set bit protects its sectors; WRPR holds
                // the inverse. ja: EVT と同じく立った bit が保護。WRPR はその反転。
                let mask = number(k, v)?;
                f.wrpr = !mask;
                out.changes.push(format!("{k}=0x{mask:08x}"));
            }
            "read-protect" => {
                let want = match v {
                    "on" => true,
                    "off" => false,
                    _ => return Err(format!("read-protect: expected on|off, got `{v}`")),
                };
                if want != (f.rdpr != RDPR_OFF) {
                    f.rdpr = if want { rdpr_on } else { RDPR_OFF };
                    out.read_protect = Some(want);
                }
                out.changes.push(format!("{k}={v}"));
            }
            _ => {
                if let Some(field) = k.strip_prefix("user.") {
                    let bits = user_field(layout, field, v)?;
                    f.user = bits.0.set(f.user, bits.1);
                    out.changes
                        .push(format!("user.{}={}", bits.0.field, bits.1));
                } else {
                    let s = semantic(layout, k)?;
                    let val = s.values.iter().find(|x| x.word == v).ok_or_else(|| {
                        let words: Vec<&str> = s.values.iter().map(|x| x.word).collect();
                        format!("{k}: expected {}, got `{v}`", words.join("|"))
                    })?;
                    f.user = s.set(f.user, val.bits);
                    out.changes.push(format!("{k}={v}"));
                }
            }
        }
    }
    Ok(out)
}

fn semantic(layout: &Layout, key: &str) -> Result<&'static OptionSemantic, String> {
    if let Some(s) = layout.semantic(key) {
        return Ok(s);
    }
    const SEMANTIC_KEYS: &[&str] = &["iwdg", "stop-reset", "standby-reset", "reset-pin"];
    if SEMANTIC_KEYS.contains(&key) {
        return Err(match layout.semantics {
            None => format!(
                "{key}: the USER-byte options of {} are not known",
                layout.label
            ),
            Some(_) => format!(
                "{key}: {} has no such option (WCH EVT FLASH_UserOptionByteConfig)",
                layout.label
            ),
        });
    }
    // A bare reference-manual name: say how it is written now.
    if let Some(f) = layout
        .user_fields
        .iter()
        .find(|f| f.field.eq_ignore_ascii_case(key))
    {
        return Err(match covering(layout, f) {
            Some(s) => format!("unknown key `{key}`: write it as {}", spelled(s)),
            None => format!(
                "unknown key `{key}`: write it as user.{}=<bit value>",
                f.field
            ),
        });
    }
    Err(format!("unknown key `{key}`"))
}

fn user_field<'a>(
    layout: &'a Layout,
    name: &str,
    v: &str,
) -> Result<(&'a OptionUserField, u8), String> {
    let key = format!("user.{name}");
    let Some(f) = layout
        .user_fields
        .iter()
        .find(|f| f.field.eq_ignore_ascii_case(name))
    else {
        return Err(format!(
            "{key}: {} has no USER field {name} in the device DB",
            layout.label
        ));
    };
    if let Some(s) = covering(layout, f) {
        return Err(format!(
            "{key}: this bit has a WCH EVT name; write it as {}",
            spelled(s)
        ));
    }
    let top = (1u16 << f.width) - 1;
    let bits = number(&key, v)?;
    match u8::try_from(bits) {
        Ok(b) if u16::from(b) <= top => Ok((f, b)),
        _ => Err(format!("{key}: expected 0..={top}, got `{v}`")),
    }
}

fn covering(layout: &Layout, f: &OptionUserField) -> Option<&'static OptionSemantic> {
    layout.semantics?.iter().find(|s| s.mask() & f.mask() != 0)
}

fn spelled(s: &OptionSemantic) -> String {
    let words: Vec<String> = s
        .values
        .iter()
        .map(|v| format!("{} ({})", v.word, v.evt))
        .collect();
    format!("{}={}", s.key, words.join(" | "))
}

fn entry(value: String, raw: u32) -> Value {
    json!({ "value": value, "raw": raw })
}

/// en: Every key with its value word and raw value: `{"<key>": {"value": "<word>", "raw": <n>}}`.
/// ja: key ごとに値の語と生の値。
pub(crate) fn decode(layout: &Layout, f: OptionFields) -> Map<String, Value> {
    let mut m = Map::new();
    let rp = if f.rdpr == RDPR_OFF { "off" } else { "on" };
    m.insert("read-protect".into(), entry(rp.into(), f.rdpr.into()));
    m.insert(
        "write-protect".into(),
        entry(format!("0x{:08x}", !f.wrpr), f.wrpr),
    );
    m.insert(
        "data0".into(),
        entry(format!("0x{:02x}", f.data0), f.data0.into()),
    );
    m.insert(
        "data1".into(),
        entry(format!("0x{:02x}", f.data1), f.data1.into()),
    );
    for s in layout.semantics.unwrap_or_default() {
        let bits = s.get(f.user);
        let word = s
            .word(bits)
            .map_or_else(|| bits.to_string(), |v| v.word.to_owned());
        m.insert(s.key.into(), entry(word, bits.into()));
    }
    for fl in layout.raw_fields() {
        let bits = fl.get(f.user);
        m.insert(
            format!("user.{}", fl.field),
            entry(bits.to_string(), bits.into()),
        );
    }
    m
}

/// en: The `get` result both paths print. `raw` is the bytes the path read (the debug path: 16
/// bytes with complements; the bootloader: its 12 config bytes).
/// ja: 両経路の `get` の result。`raw` はその経路が読んだ byte。
pub(crate) fn result(
    family: Option<&str>,
    db_family: Option<&str>,
    raw: &str,
    layout: &Layout,
    f: OptionFields,
) -> Value {
    json!({
        "family": family,
        "db_family": db_family,
        "raw": raw,
        "read_protected": f.rdpr != RDPR_OFF,
        "write_protected": f.wrpr != 0xFFFF_FFFF,
        "options": decode(layout, f),
    })
}

/// en: The text form of [`result`]: one `key=value` per line, as `set` takes it.
/// ja: [`result`] の文字の形。`set` に渡せる `key=value` を 1 行ずつ。
pub(crate) fn print(v: &Value) {
    if let Some(f) = v["family"].as_str() {
        println!("family: {f}");
    }
    println!("raw:    {}", v["raw"].as_str().unwrap_or("-"));
    let Some(opts) = v["options"].as_object() else {
        return;
    };
    // Protection first, then the USER options, then data.
    let order = |k: &str| match k {
        "read-protect" => 0,
        "write-protect" => 1,
        "data0" => 3,
        "data1" => 4,
        _ => 2,
    };
    let mut keys: Vec<&String> = opts.keys().collect();
    keys.sort_by_key(|k| (order(k), k.starts_with("user."), (*k).clone()));
    for k in keys {
        let e = &opts[k.as_str()];
        let pair = format!("{k}={}", e["value"].as_str().unwrap_or("-"));
        let raw = e["raw"].as_u64().unwrap_or(0);
        let note = match k.as_str() {
            "write-protect" => format!("  (a set bit protects its sectors; WRPR 0x{raw:08x})"),
            "read-protect" => format!("  (rdpr 0x{raw:02x})"),
            _ if k.starts_with("user.") || k.starts_with("data") => String::new(),
            _ => format!("  (raw {raw})"),
        };
        println!("{}", format!("{pair:<24}{note}").trim_end());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    fn factory() -> OptionFields {
        OptionFields {
            rdpr: RDPR_OFF,
            user: 0xff,
            data0: 0xff,
            data1: 0xff,
            wrpr: 0xFFFF_FFFF,
        }
    }

    #[test]
    fn semantic_words_set_the_evt_bits() {
        let l = Layout::of("CH32X035");
        let mut f = factory();
        let a = apply(
            &l,
            &mut f,
            &s(&[
                "iwdg=hw",
                "stop-reset=on",
                "standby-reset=off",
                "reset-pin=12ms",
            ]),
            0x00,
        )
        .unwrap();
        // OB_IWDG_HW (bit0=0), OB_STOP_RST (bit1=0), OB_STDBY_NoRST (bit2=1), OB_RST_EN_DT12ms (10b).
        assert_eq!(f.user, 0b1111_0100);
        assert_eq!(a.read_protect, None);
        let d = decode(&l, f);
        assert_eq!(d["iwdg"], json!({"value": "hw", "raw": 0}));
        assert_eq!(d["stop-reset"], json!({"value": "on", "raw": 0}));
        assert_eq!(d["standby-reset"], json!({"value": "off", "raw": 1}));
        assert_eq!(d["reset-pin"], json!({"value": "12ms", "raw": 2}));
    }

    #[test]
    fn family_specific_bits_go_by_their_rm_name_raw() {
        let l = Layout::of("CH32L103");
        let mut f = factory();
        apply(&l, &mut f, &s(&["user.CFGCANM=0"]), 0x00).unwrap();
        assert_eq!(f.user, 0xdf);
        assert_eq!(
            decode(&l, f)["user.CFGCANM"],
            json!({"value": "0", "raw": 0})
        );
        // A bit with an EVT name is not reachable by its RM name, bare or under user.
        let e = apply(&l, &mut f, &s(&["user.STOPRST=0"]), 0x00).unwrap_err();
        assert!(e.contains("stop-reset=on (OB_STOP_RST)"), "{e}");
        let e = apply(&l, &mut f, &s(&["STOPRST=0"]), 0x00).unwrap_err();
        assert!(e.contains("stop-reset="), "{e}");
        let e = apply(&l, &mut f, &s(&["CFGCANM=0"]), 0x00).unwrap_err();
        assert!(e.contains("user.CFGCANM=<bit value>"), "{e}");
        assert!(apply(&l, &mut f, &s(&["user.CFGCANM=2"]), 0x00).is_err());
        assert!(!decode(&l, f).contains_key("user.STOPRST"));
    }

    #[test]
    fn options_a_family_lacks_are_refused() {
        let mut f = factory();
        // CH32V003's FLASH_UserOptionByteConfig takes no OB_STOP.
        let e = apply(&Layout::of("CH32V003"), &mut f, &s(&["stop-reset=on"]), 0).unwrap_err();
        assert!(e.contains("no such option"), "{e}");
        // CH32M030 names other reset-pin delays.
        let l = Layout::of("CH32M030");
        assert!(apply(&l, &mut f, &s(&["reset-pin=12ms"]), 0).is_err());
        apply(&l, &mut f, &s(&["reset-pin=512us"]), 0).unwrap();
        assert_eq!((f.user >> 3) & 3, 1);
        let unknown = Layout::of("CH32NOPE");
        assert!(unknown.interim_warning().is_some());
        assert!(apply(&unknown, &mut f, &s(&["iwdg=sw"]), 0).is_err());
        apply(&unknown, &mut f, &s(&["data0=0x5a"]), 0).unwrap();
        assert_eq!(f.data0, 0x5a);
    }

    #[test]
    fn read_protect_changes_only_when_the_state_does() {
        let l = Layout::of("CH32V307");
        let mut f = factory();
        let a = apply(&l, &mut f, &s(&["read-protect=off"]), 0xff).unwrap();
        assert_eq!(a.read_protect, None);
        let a = apply(&l, &mut f, &s(&["read-protect=on"]), 0xff).unwrap();
        assert_eq!((a.read_protect, f.rdpr), (Some(true), 0xff));
        let a = apply(&l, &mut f, &s(&["read-protect=off"]), 0xff).unwrap();
        assert_eq!((a.read_protect, f.rdpr), (Some(false), RDPR_OFF));
    }

    #[test]
    fn write_protect_is_the_evt_mask_and_raw_is_wrpr() {
        let l = Layout::of("CH32V307");
        let mut f = factory();
        // FLASH_WRProt_Pages0to3 | Pages4to7: protect the first two groups.
        apply(&l, &mut f, &s(&["write-protect=0x3"]), 0).unwrap();
        assert_eq!(f.wrpr, 0xFFFF_FFFC);
        let d = decode(&l, f);
        assert_eq!(
            d["write-protect"],
            json!({"value": "0x00000003", "raw": 0xFFFF_FFFCu32})
        );
        assert_eq!(result(None, None, "", &l, f)["write_protected"], true);
        let d = decode(&l, factory());
        assert_eq!(d["write-protect"]["value"], "0x00000000");
    }

    #[test]
    fn ob16_round_trips_with_complements() {
        let mut ob = [0u8; 16];
        for i in 0..8 {
            ob[2 * i] = 0x10 + i as u8;
            ob[2 * i + 1] = !(0x10 + i as u8);
        }
        let f = from_ob16(&ob);
        assert_eq!(f.wrpr, 0x1716_1514);
        assert_eq!(to_ob16(f), ob);
    }

    #[test]
    fn keys_list_the_family_vocabulary() {
        let k = Layout::of("CH32V003").keys();
        assert!(k.contains("reset-pin=off|12ms|1ms|128us"), "{k}");
        assert!(k.contains("user.START_MODE=0..1"), "{k}");
        assert!(!k.contains("STANDYRST"), "{k}");
        assert!(!k.contains("stop-reset"), "{k}");
    }
}
