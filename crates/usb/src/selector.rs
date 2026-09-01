//! en: Probe-selector grammar (docs/cli.ja.md §3.4).
//! ja: probe selector の文法(docs/cli.ja.md §3.4)。
//!
//! ```text
//! VID:PID[:SERIAL]      canonical (probe-rs compatible), e.g. 1a86:8010:434A124C5596
//! serial:<sn>           select by serial only
//! name:<alias>          alias from the config file
//! usb:<bus>-<ports>     USB topology (physical port on a fixed hub; for HIL lanes)
//! index:<n>             enumeration order (discouraged; rejected under --non-interactive)
//! ```

use std::str::FromStr;

/// en: A parsed selector. Resolution against real devices happens in the enumeration layer.
/// ja: パース済み selector。解決(実 device への対応付け)は列挙側で行う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// `VID:PID[:SERIAL]`
    UsbId {
        vid: u16,
        pid: u16,
        serial: SerialFilter,
    },
    /// `serial:<sn>`
    Serial(String),
    /// `name:<alias>` (resolved through the config file)
    Name(String),
    /// `usb:<bus>-<ports>` (e.g. `3-1.4.2`)
    Topology(String),
    /// `index:<n>` (discouraged; rejected under `--non-interactive`)
    Index(usize),
}

/// Meaning of the third field of `VID:PID[:SERIAL]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialFilter {
    /// Field omitted (`VID:PID`): ignore the serial.
    Any,
    /// Empty field (`VID:PID:`): only devices without a serial.
    NoSerial,
    /// Exact match. Colons inside the serial are allowed (ESP-JTAG MAC form, etc.).
    Exact(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorParseError {
    #[error("empty selector")]
    Empty,
    #[error("invalid hex in `{field}`: `{value}`")]
    InvalidHex { field: &'static str, value: String },
    #[error("invalid index: `{0}`")]
    InvalidIndex(String),
    #[error("empty value after `{0}:`")]
    EmptyValue(&'static str),
    #[error(
        "unrecognized selector `{0}` (expected VID:PID[:SERIAL], serial:<sn>, name:<alias>, usb:<bus>-<ports>, or index:<n>)"
    )]
    Unrecognized(String),
}

impl FromStr for Selector {
    type Err = SelectorParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(SelectorParseError::Empty);
        }
        if let Some(v) = s.strip_prefix("serial:") {
            return keyword_value(v, "serial").map(Selector::Serial);
        }
        if let Some(v) = s.strip_prefix("name:") {
            return keyword_value(v, "name").map(Selector::Name);
        }
        if let Some(v) = s.strip_prefix("usb:") {
            return keyword_value(v, "usb").map(Selector::Topology);
        }
        if let Some(v) = s.strip_prefix("index:") {
            return v
                .parse::<usize>()
                .map(Selector::Index)
                .map_err(|_| SelectorParseError::InvalidIndex(v.to_owned()));
        }

        // en: VID:PID[:SERIAL]. Split into at most 3 parts because a serial may contain `:`.
        // ja: VID:PID[:SERIAL]。SERIAL は `:` を含みうるので 3 分割まで。
        let mut parts = s.splitn(3, ':');
        let vid = parts.next().unwrap_or_default();
        let Some(pid) = parts.next() else {
            return Err(SelectorParseError::Unrecognized(s.to_owned()));
        };
        let vid = u16::from_str_radix(vid, 16).map_err(|_| SelectorParseError::InvalidHex {
            field: "VID",
            value: vid.to_owned(),
        })?;
        let pid = u16::from_str_radix(pid, 16).map_err(|_| SelectorParseError::InvalidHex {
            field: "PID",
            value: pid.to_owned(),
        })?;
        let serial = match parts.next() {
            Option::None => SerialFilter::Any,
            Some("") => SerialFilter::NoSerial,
            Some(sn) => SerialFilter::Exact(sn.to_owned()),
        };
        Ok(Selector::UsbId { vid, pid, serial })
    }
}

fn keyword_value(v: &str, kw: &'static str) -> Result<String, SelectorParseError> {
    if v.is_empty() {
        Err(SelectorParseError::EmptyValue(kw))
    } else {
        Ok(v.to_owned())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn canonical_vid_pid_serial() {
        let s: Selector = "1a86:8010:434A124C5596".parse().unwrap();
        assert_eq!(
            s,
            Selector::UsbId {
                vid: 0x1a86,
                pid: 0x8010,
                serial: SerialFilter::Exact("434A124C5596".into()),
            }
        );
    }

    #[test]
    fn vid_pid_without_serial_matches_any() {
        let s: Selector = "1a86:8010".parse().unwrap();
        assert_eq!(
            s,
            Selector::UsbId {
                vid: 0x1a86,
                pid: 0x8010,
                serial: SerialFilter::Any,
            }
        );
    }

    #[test]
    fn trailing_colon_means_no_serial() {
        let s: Selector = "4348:55e0:".parse().unwrap();
        assert_eq!(
            s,
            Selector::UsbId {
                vid: 0x4348,
                pid: 0x55e0,
                serial: SerialFilter::NoSerial,
            }
        );
    }

    #[test]
    fn serial_may_contain_colons() {
        // en: probe-rs compatible: ESP-JTAG MAC-address style serial.
        // ja: probe-rs 互換: ESP-JTAG の MAC アドレス形式。
        let s: Selector = "303a:1001:DC:DA:0C:D3:FE:D8".parse().unwrap();
        assert_eq!(
            s,
            Selector::UsbId {
                vid: 0x303a,
                pid: 0x1001,
                serial: SerialFilter::Exact("DC:DA:0C:D3:FE:D8".into()),
            }
        );
    }

    #[test]
    fn keyword_forms() {
        assert_eq!(
            "serial:ABC".parse::<Selector>().unwrap(),
            Selector::Serial("ABC".into())
        );
        assert_eq!(
            "name:bench-01".parse::<Selector>().unwrap(),
            Selector::Name("bench-01".into())
        );
        assert_eq!(
            "usb:3-1.4.2".parse::<Selector>().unwrap(),
            Selector::Topology("3-1.4.2".into())
        );
        assert_eq!("index:2".parse::<Selector>().unwrap(), Selector::Index(2));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            "zz86:8010".parse::<Selector>(),
            Err(SelectorParseError::InvalidHex { field: "VID", .. })
        ));
        assert!(matches!(
            "".parse::<Selector>(),
            Err(SelectorParseError::Empty)
        ));
        assert!(matches!(
            "wchlink".parse::<Selector>(),
            Err(SelectorParseError::Unrecognized(_))
        ));
        assert!(matches!(
            "index:x".parse::<Selector>(),
            Err(SelectorParseError::InvalidIndex(_))
        ));
        assert!(matches!(
            "serial:".parse::<Selector>(),
            Err(SelectorParseError::EmptyValue("serial"))
        ));
    }
}
