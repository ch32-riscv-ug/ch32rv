//! en: Probe-selector grammar (docs/cli.ja.md §3.4).
//! ja: probe selector の文法(docs/cli.ja.md §3.4)。
//!
//! ```text
//! VID:PID[:SERIAL]      canonical (probe-rs compatible), e.g. 1a86:8010:434A124C5596
//! serial:<sn>           select by serial only
//! name:<alias>          alias from the config file
//! topology:<bus>-<ports>  USB position (physical port on a fixed hub; for HIL lanes)
//! <address>             an IDE port address as it is: a probe's serial port (/dev/ttyACM0,
//!                       COM3), wchlink://<serial>, oep://<probe>[/<port>[/<target>]][?via=<route>]
//! ```
//!
//! en: The forms that are gone (`port:`, `tcp:`, `usb:`, `oep-net://`, `hid://`) are refused with
//! the form to write instead. A write target's address (`ch32rv-hid://`, `ch32rv-app://`,
//! `ch32rv-isp://`) is not a probe: it goes to `--device`.
//! ja: 無くなった形は、代わりの書き方を添えて断る。書き込み先の住所は probe ではない(`--device`)。

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
    /// `topology:<bus>-<ports>` (e.g. `3-1.4.2`)
    Topology(String),
    /// An IDE port address as it is - the probe that owns this serial port (`/dev/ttyACM0`,
    /// `COM3`), or discovery's `oep://...` (`wchlink://<serial>` is [`Selector::Serial`]). An `oep://` address is never a USB device
    /// here; the OEP layer resolves it.
    Port(String),
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
    #[error("empty value after `{0}:`")]
    EmptyValue(&'static str),
    #[error(
        "`{0}` is not a probe selector: use VID:PID[:SERIAL] (hex, e.g. 1a86:8010), serial:<sn>, name:<alias>, topology:<bus>-<ports>, or an IDE port address as it is (/dev/ttyACM0, COM3, wchlink://<sn>, oep://<probe>[/<port>[/<target>]][?via=usb|bulk|hid|cdc|net|tcp:<host>:<port>|tty:<path>])"
    )]
    Unrecognized(String),
    /// A write target's address (a bootloader or an application), not a debug probe.
    #[error(
        "`{0}` names a write target, not a debug probe: pass it as --device (isp, boot dfu, boot hid)"
    )]
    NotAProbe(String),
    /// A form that is gone, with what to write instead.
    #[error("`{0}`: this form is gone; write {1}")]
    Removed(String, String),
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
        if let Some(v) = s.strip_prefix("topology:") {
            return keyword_value(v, "topology").map(Selector::Topology);
        }
        if let Some(instead) = removed(s) {
            return Err(SelectorParseError::Removed(s.to_owned(), instead));
        }
        // en: An IDE port address is a selector as it is.
        // ja: IDE の port の住所はそのまま selector。
        if let Some(r) = address(s) {
            return r;
        }

        // en: VID:PID[:SERIAL], when the first two fields are 1-4 hex digits each; a serial may
        // contain `:`, so at most 3 parts. Anything else matches no form.
        // ja: VID:PID[:SERIAL](先頭 2 つが 1〜4 桁の 16 進のとき)。どの形でもなければ判別できない。
        let mut parts = s.splitn(3, ':');
        let hex4 = |f: &str| (1..=4).contains(&f.len()) && f.chars().all(|c| c.is_ascii_hexdigit());
        let (Some(vid), Some(pid)) = (parts.next(), parts.next()) else {
            return Err(SelectorParseError::Unrecognized(s.to_owned()));
        };
        if !hex4(vid) || !hex4(pid) {
            return Err(SelectorParseError::Unrecognized(s.to_owned()));
        }
        let (Ok(vid), Ok(pid)) = (u16::from_str_radix(vid, 16), u16::from_str_radix(pid, 16))
        else {
            return Err(SelectorParseError::Unrecognized(s.to_owned()));
        };
        let serial = match parts.next() {
            Option::None => SerialFilter::Any,
            Some("") => SerialFilter::NoSerial,
            Some(sn) => SerialFilter::Exact(sn.to_owned()),
        };
        Ok(Selector::UsbId { vid, pid, serial })
    }
}

/// en: What to write instead of a form that is gone (`port:`, `tcp:`, `usb:`, `oep-net://`,
/// `hid://`), or None. ja: 無くなった形なら、代わりの書き方。
pub fn removed(s: &str) -> Option<String> {
    if let Some(v) = s.strip_prefix("hid://") {
        return Some(format!("ch32rv-hid://{v}"));
    }
    if let Some(v) = s.strip_prefix("oep-net://") {
        let (place, _) = v.split_once('?').unwrap_or((v, ""));
        return Some(format!("oep://{place}?via=net"));
    }
    if let Some(v) = s.strip_prefix("port:") {
        return Some(if v.is_empty() {
            "the IDE port's address as it is (no port is selected)".to_owned()
        } else {
            format!("the address as it is: {v}")
        });
    }
    if let Some(v) = s.strip_prefix("tcp:") {
        let v = v.strip_prefix("//").unwrap_or(v);
        return Some(if v.contains(':') {
            format!("oep://<unit_id>?via=tcp:{v}")
        } else {
            format!("oep://{v}?via=net")
        });
    }
    if let Some(v) = s.strip_prefix("usb:") {
        return Some(format!(
            "topology:{v} for a USB position, or oep://<unit_id>?via=usb for an OEP probe on USB"
        ));
    }
    None
}

/// en: An IDE port address of a probe, told apart from the keyword and VID:PID forms by its
/// shape: a URL of a probe scheme discovery lists (`wchlink://<serial>`,
/// `wchlink://usb-<bus>-<ports>`, `oep://...`; a write target's scheme is refused, another scheme
/// is no form), or a serial port's path (`/dev/ttyACM0`, `COM3`, `\\.\COM12`). None for anything
/// else.
/// ja: IDE の port の住所(URL か serial port の path)。形で見分ける。
fn address(a: &str) -> Option<Result<Selector, SelectorParseError>> {
    if let Some(v) = a.strip_prefix("wchlink://") {
        return Some(match v.strip_prefix("usb-") {
            Some(t) => keyword_value(t, "topology").map(Selector::Topology),
            None => keyword_value(v, "wchlink").map(Selector::Serial),
        });
    }
    let com = a.len() > 3
        && a[..3].eq_ignore_ascii_case("com")
        && a[3..].chars().all(|c| c.is_ascii_digit());
    if DEVICE_SCHEMES.iter().any(|scheme| a.starts_with(scheme)) {
        return Some(Err(SelectorParseError::NotAProbe(a.to_owned())));
    }
    let url = a.starts_with("oep://");
    (url || a.starts_with('/') || a.starts_with("\\\\.\\") || com)
        .then(|| Ok(Selector::Port(a.to_owned())))
}

/// The URL forms of the addresses discovery lists (besides `wchlink://`, a serial).
pub const ADDRESS_SCHEMES: [&str; 4] =
    ["oep://", "ch32rv-hid://", "ch32rv-app://", "ch32rv-isp://"];

/// en: The URL forms that name a write target - a bootloader or a running application - which
/// `--device` takes and `--probe` refuses. ja: 書き込み先の URL(`--device` で渡す)。
pub const DEVICE_SCHEMES: [&str; 3] = ["ch32rv-hid://", "ch32rv-app://", "ch32rv-isp://"];

fn keyword_value(v: &str, kw: &'static str) -> Result<String, SelectorParseError> {
    if v.is_empty() {
        Err(SelectorParseError::EmptyValue(kw))
    } else {
        Ok(v.to_owned())
    }
}

impl Selector {
    /// en: Whether this selector matches the device at `index` in the enumeration order.
    /// `Name` never matches here: aliases must be resolved to another selector first.
    /// ja: 列挙順 `index` の device にこの selector が一致するか。`Name` はここでは一致しない
    /// (別名は先に別の selector へ解決しておく)。
    pub fn matches(&self, dev: &crate::UsbDeviceInfo, _index: usize) -> bool {
        match self {
            Selector::UsbId { vid, pid, serial } => {
                dev.vid() == *vid
                    && dev.pid() == *pid
                    && match serial {
                        SerialFilter::Any => true,
                        SerialFilter::NoSerial => dev.serial().is_none(),
                        SerialFilter::Exact(sn) => {
                            dev.serial().is_some_and(|d| d.eq_ignore_ascii_case(sn))
                        }
                    }
            }
            // Case aside: some OSes and tools show a serial in capitals (oep-core §3.3 compares unit_id
            // and serial the same way).
            Selector::Serial(sn) => dev.serial().is_some_and(|d| d.eq_ignore_ascii_case(sn)),
            Selector::Topology(t) => dev.topology() == *t,
            // en: The IDE's port addresses, so a recipe can pass `{upload.port.address}` whatever
            // the user picked: `wchlink://<serial>` (discovery's own address) names the device
            // directly; anything else is a serial port path.
            // ja: IDE の port address。discovery 自身の address(`wchlink://<serial>`)は device を
            // 直接指し、それ以外は serial port の path。
            Selector::Port(p) => {
                if let Some(id) = p.strip_prefix("wchlink://") {
                    return dev.port_id().as_deref() == Some(id);
                }
                let want = normalize_port(p);
                dev.serial_ports().iter().any(|q| normalize_port(q) == want)
            }
            Selector::Name(_) => false,
        }
    }
}

/// en: A serial-port path in a comparable form: symlinks resolved on unix
/// (`/dev/serial/by-id/...` names the same port as `/dev/ttyACM0`), `\\.\` stripped and case
/// folded on Windows (`com3` is `COM3`).
/// ja: 比較用の serial port の path。unix は symlink を解く(by-id の名前も同じ port)、Windows は
/// `\\.\` を外して大文字小文字を畳む。
pub fn normalize_port(path: &str) -> String {
    #[cfg(windows)]
    {
        path.trim_start_matches(r"\\.\").to_ascii_uppercase()
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned())
    }
}

/// en: Failure modes of [`resolve`], mapped to exit codes 10 / 14 by the caller.
/// ja: [`resolve`] の失敗。呼び出し側で exit code 10 / 14 に写像する。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("no matching device")]
    NotFound,
    #[error("selector matches {} devices (fail-closed)", .0.len())]
    Ambiguous(Vec<usize>),
    #[error("alias `{0}` was not resolved against the config file")]
    UnresolvedName(String),
}

/// en: Resolve a selector against an enumerated device list, fail-closed: without a selector
/// the list must contain exactly one device; with one, exactly one must match.
/// Returns the index into `devs`.
/// ja: selector を列挙結果へ fail-closed に解決する。selector 無しなら 1 台のみ、有りなら
/// 一致がちょうど 1 台であること。`devs` 内の index を返す。
pub fn resolve<'a, I>(selector: Option<&Selector>, devs: I) -> Result<usize, ResolveError>
where
    I: IntoIterator<Item = &'a crate::UsbDeviceInfo>,
{
    if let Some(Selector::Name(name)) = selector {
        return Err(ResolveError::UnresolvedName(name.clone()));
    }
    let matches: Vec<usize> = devs
        .into_iter()
        .enumerate()
        .filter(|(i, d)| selector.is_none_or(|s| s.matches(d, *i)))
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [] => Err(ResolveError::NotFound),
        [one] => Ok(*one),
        _ => Err(ResolveError::Ambiguous(matches)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn discovery_addresses_are_selectors() {
        assert_eq!(
            "wchlink://49808F06CE30".parse::<Selector>().unwrap(),
            Selector::Serial("49808F06CE30".to_owned())
        );
        assert_eq!(
            "wchlink://usb-3-2.2.1".parse::<Selector>().unwrap(),
            Selector::Topology("3-2.2.1".to_owned())
        );
        assert_eq!(
            "oep://30eda0e34a0e/weact".parse::<Selector>().unwrap(),
            Selector::Port("oep://30eda0e34a0e/weact".to_owned())
        );
        for a in [
            "/dev/ttyACM0",
            "COM3",
            "oep://50029191fe34/weact?via=net",
            "oep://50029191fe34?via=tty:/dev/ttyUSB0",
            "com12",
        ] {
            assert_eq!(a.parse::<Selector>().unwrap(), Selector::Port(a.to_owned()));
        }
        // A write target's address is not a probe.
        for a in [
            "ch32rv-hid://3-2.4",
            "ch32rv-app://3-2.4.3",
            "ch32rv-isp://3-2.4.3",
        ] {
            assert_eq!(
                a.parse::<Selector>(),
                Err(SelectorParseError::NotAProbe(a.to_owned()))
            );
        }
        // Not an address: still VID:PID.
        assert!(matches!(
            "1a86:8010".parse::<Selector>().unwrap(),
            Selector::UsbId { .. }
        ));
    }

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
            "topology:3-1.4.2".parse::<Selector>().unwrap(),
            Selector::Topology("3-1.4.2".into())
        );
    }

    #[test]
    fn removed_forms_say_what_to_write() {
        for (gone, instead) in [
            ("port:/dev/ttyACM0", "/dev/ttyACM0"),
            ("port:", "no port is selected"),
            (
                "tcp:192.168.1.5:4242",
                "oep://<unit_id>?via=tcp:192.168.1.5:4242",
            ),
            (
                "tcp://192.168.1.5:4242",
                "oep://<unit_id>?via=tcp:192.168.1.5:4242",
            ),
            ("tcp:50029191fe34", "oep://50029191fe34?via=net"),
            ("usb:3-1.4.2", "topology:3-1.4.2"),
            ("hid://3-2.4", "ch32rv-hid://3-2.4"),
            (
                "oep-net://50029191fe34/weact",
                "oep://50029191fe34/weact?via=net",
            ),
        ] {
            match gone.parse::<Selector>() {
                Err(SelectorParseError::Removed(g, i)) => {
                    assert_eq!(g, gone);
                    assert!(i.contains(instead), "{gone}: {i}");
                }
                other => panic!("{gone}: {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_garbage() {
        // Not hex, or too long a field: no form matches.
        for bad in [
            "zz86:8010",
            "1a86:80100",
            "wchlink:49808F06CE30",
            "oep:x",
            "wchlnk://x",
        ] {
            assert!(
                matches!(
                    bad.parse::<Selector>(),
                    Err(SelectorParseError::Unrecognized(_))
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            "".parse::<Selector>(),
            Err(SelectorParseError::Empty)
        ));
        assert!(matches!(
            "wchlink".parse::<Selector>(),
            Err(SelectorParseError::Unrecognized(_))
        ));
        assert!(matches!(
            "index:2".parse::<Selector>(),
            Err(SelectorParseError::Unrecognized(_))
        ));
        assert!(matches!(
            "serial:".parse::<Selector>(),
            Err(SelectorParseError::EmptyValue("serial"))
        ));
    }
}
