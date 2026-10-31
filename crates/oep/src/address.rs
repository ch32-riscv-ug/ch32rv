//! en: The OEP address (oep-spec host guide §5.3):
//! `oep://<probe>[/<port>[/<target>]][?via=<route>]`. The place (probe, port, target) names what
//! is used; `via` only picks how it is reached. The query is taken off before the place is split
//! (a `tty:` value holds `/`), `via` is the only query name (any other is refused), and `?`, `#`,
//! `&`, `%` and space in its value are written `%XX`.
//! ja: OEP のアドレス(host ガイド §5.3)。場所(probe、口、target)と、そこへ届く経路 `via`。
//! query を先に外してから場所を `/` で割る。query は `via` だけで、知らない名前は断る。

use std::fmt;

/// How the place is reached (`?via=`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// `usb`: any of the probe's USB transports (the host picks: vendor bulk, HID, CDC).
    Usb,
    /// `bulk`: the USB vendor bulk pair only.
    Bulk,
    /// `hid`: the USB vendor HID only.
    Hid,
    /// `cdc`: the USB CDC serial port only.
    Cdc,
    /// `net`: TCP at the endpoint DNS-SD announces for the unit id.
    Net,
    /// `tcp:<host>:<port>`: TCP at that endpoint (`host:port`, an IPv6 host in `[...]`).
    Tcp(String),
    /// `tty:<path>`: that OS serial port (`/dev/ttyACM0`, `/dev/serial/by-id/...`, `COM3`).
    Tty(String),
}

impl Via {
    /// en: The route as written after `via=` (decoded). ja: `via=` の値(復号済み)から。
    pub fn parse(v: &str) -> Result<Via, String> {
        Ok(match v {
            "usb" => Via::Usb,
            "bulk" => Via::Bulk,
            "hid" => Via::Hid,
            "cdc" => Via::Cdc,
            "net" => Via::Net,
            _ => {
                if let Some(a) = v.strip_prefix("tcp:") {
                    Via::Tcp(tcp_endpoint(a)?)
                } else if let Some(p) = v.strip_prefix("tty:") {
                    if p.is_empty() {
                        return Err("via=tty: needs the serial port's path".to_owned());
                    }
                    Via::Tty(p.to_owned())
                } else {
                    return Err(format!(
                        "via={v} is not a route (usb, bulk, hid, cdc, net, tcp:<host>:<port>, tty:<path>)"
                    ));
                }
            }
        })
    }
}

/// `<host>:<port>` with a non-empty host (`[...]` around an IPv6 one) and a decimal port.
fn tcp_endpoint(a: &str) -> Result<String, String> {
    let bad = || format!("via=tcp:{a} is not tcp:<host>:<port> (an IPv6 host in [...])");
    let (host, port) = a.rsplit_once(':').ok_or_else(bad)?;
    let host_ok = if let Some(h) = host.strip_prefix('[') {
        h.strip_suffix(']').is_some_and(|h| !h.is_empty())
    } else {
        !host.is_empty() && !host.contains(':')
    };
    if !host_ok || port.parse::<u16>().is_err() {
        return Err(bad());
    }
    Ok(a.to_owned())
}

impl fmt::Display for Via {
    /// en: As written in an address (`%XX` where the spec says). ja: アドレスに書く形。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Via::Usb => f.write_str("usb"),
            Via::Bulk => f.write_str("bulk"),
            Via::Hid => f.write_str("hid"),
            Via::Cdc => f.write_str("cdc"),
            Via::Net => f.write_str("net"),
            Via::Tcp(a) => write!(f, "tcp:{}", encode(a)),
            Via::Tty(p) => write!(f, "tty:{}", encode(p)),
        }
    }
}

/// A parsed `oep://` address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// The probe's unit id, as written.
    pub probe: String,
    pub port: Option<String>,
    /// The target's individual name, lowercased (`<family>-<unique id hex>`).
    pub target: Option<String>,
    pub via: Option<Via>,
}

impl Address {
    /// en: Parse `oep://<probe>[/<port>[/<target>]][?via=<route>]`. Segments are letters, digits,
    /// `-` and `.`, written in full (`any` is refused). ja: パースする。段は英数字、`-`、`.`。
    pub fn parse(s: &str) -> Result<Address, String> {
        let rest = s
            .strip_prefix("oep://")
            .ok_or_else(|| format!("`{s}` is not an oep:// address"))?;
        // The query first: a `tty:` value holds `/` (host guide §5.3).
        let (place, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        let via = match query {
            None => None,
            Some(q) => Some(parse_query(s, q)?),
        };
        let segs: Vec<&str> = place.split('/').collect();
        let ok = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        };
        if segs.len() > 3 || !segs.iter().all(|s| ok(s)) {
            return Err(format!(
                "`{s}` is not oep://<probe>[/<port>[/<target>]][?via=<route>] (segments of letters, digits, - and . only)"
            ));
        }
        if segs.iter().any(|s| s.eq_ignore_ascii_case("any")) {
            return Err(format!(
                "`{s}`: an address is written in full (`any` is gone, host guide §5.3); `ch32rv target list` lists the ports"
            ));
        }
        Ok(Address {
            probe: segs[0].to_owned(),
            port: segs.get(1).map(|s| (*s).to_owned()),
            target: segs.get(2).map(|s| s.to_ascii_lowercase()),
            via,
        })
    }

    /// en: The place alone (`oep://<probe>[/<port>[/<target>]]`, no `via`): what two addresses
    /// are compared by. ja: 場所だけ(`via` を除く)。比べるときはこれで比べる。
    pub fn place(&self) -> String {
        let mut s = format!("oep://{}", self.probe);
        for seg in [&self.port, &self.target].into_iter().flatten() {
            s.push('/');
            s.push_str(seg);
        }
        s
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.place())?;
        match &self.via {
            Some(v) => write!(f, "?via={v}"),
            None => Ok(()),
        }
    }
}

/// The query `q` of address `s`: exactly one `via=<route>`, nothing else.
fn parse_query(s: &str, q: &str) -> Result<Via, String> {
    let mut via = None;
    for part in q.split('&') {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        if name != "via" {
            return Err(format!(
                "`{s}`: unknown query `{name}` (an address takes only ?via=<route>)"
            ));
        }
        if via.is_some() {
            return Err(format!("`{s}`: via is given twice"));
        }
        via = Some(Via::parse(
            &decode(value).map_err(|e| format!("`{s}`: {e}"))?,
        )?);
    }
    via.ok_or_else(|| format!("`{s}`: an empty query"))
}

/// en: Undo `%XX`; a raw `#` or space (which must be written `%XX`) is refused.
/// ja: `%XX` を戻す。そのまま書いた `#` や空白は断る。
fn decode(v: &str) -> Result<String, String> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = v
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                    .ok_or_else(|| format!("`{v}`: % is not followed by two hex digits"))?;
                out.push(hex);
                i += 3;
            }
            b'#' | b' ' => {
                return Err(format!("`{v}`: write # and space as %23 and %20"));
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| format!("`{v}`: not UTF-8 once decoded"))
}

/// `?`, `#`, `&`, `%` and space as `%XX` (host guide §5.3); `/` and `:` stay.
fn encode(v: &str) -> String {
    let mut s = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '?' | '#' | '&' | '%' | ' ' => s.push_str(&format!("%{:02X}", c as u32)),
            c => s.push(c),
        }
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn place_and_via() {
        let a = Address::parse("oep://30eda0e34a0e/weact/CH32V003-0123").unwrap();
        assert_eq!(a.probe, "30eda0e34a0e");
        assert_eq!(a.port.as_deref(), Some("weact"));
        assert_eq!(a.target.as_deref(), Some("ch32v003-0123"));
        assert_eq!(a.via, None);
        for (v, want) in [
            ("usb", Via::Usb),
            ("bulk", Via::Bulk),
            ("hid", Via::Hid),
            ("cdc", Via::Cdc),
            ("net", Via::Net),
            ("tcp:192.168.1.5:4242", Via::Tcp("192.168.1.5:4242".into())),
            ("tcp:[fe80::1]:4242", Via::Tcp("[fe80::1]:4242".into())),
            ("tcp:probe.local:4242", Via::Tcp("probe.local:4242".into())),
            (
                "tty:/dev/serial/by-id/usb-x-if02",
                Via::Tty("/dev/serial/by-id/usb-x-if02".into()),
            ),
            ("tty:COM3", Via::Tty("COM3".into())),
            ("tty:/dev/my%20port%23", Via::Tty("/dev/my port#".into())),
        ] {
            let s = format!("oep://u1/p?via={v}");
            let a = Address::parse(&s).unwrap();
            assert_eq!(a.via, Some(want), "{s}");
            assert_eq!(a.place(), "oep://u1/p");
            assert_eq!(a.to_string(), s);
        }
        // The query is taken off first: the tty path's `/` is not a segment.
        let a = Address::parse("oep://u1?via=tty:/dev/ttyACM0").unwrap();
        assert_eq!(a.port, None);
    }

    #[test]
    fn refused() {
        for bad in [
            "oep://",
            "oep://u1/p/t/x",
            "oep://u1/any",
            "oep://u1?via=",
            "oep://u1?via=wifi",
            "oep://u1?via=usb&via=hid",
            "oep://u1?route=usb",
            "oep://u1?via=usb&x=1",
            "oep://u1?via=tcp:host",
            "oep://u1?via=tcp::4242",
            "oep://u1?via=tcp:fe80::1:4242",
            "oep://u1?via=tcp:h:99999",
            "oep://u1?via=tty:",
            "oep://u1?via=tty:/dev/a b",
            "oep://u1?via=tty:%zz",
            "oep://u1?",
            "oep-net://u1",
        ] {
            assert!(Address::parse(bad).is_err(), "{bad}");
        }
    }
}
