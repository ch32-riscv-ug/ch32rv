//! en: Finding OEP probes that listen on TCP: a DNS-SD (RFC 6763) browse for `_oep._tcp` over mDNS
//! (RFC 6762), as oep-spec transports §3 has them announce it - TXT `unit_id=<unit_id>`, the port
//! from SRV, the instance and host names the probe's. A minimal one-shot query with no dependency:
//! PTR `_oep._tcp.local` (then SRV / TXT / A for what the answers left out), sent to
//! 224.0.0.251:5353 from an ephemeral port with the unicast-response bit, so responders answer
//! this socket directly (RFC 6762 §5.4, §6.7). IPv4 only. mDNS stays on the local link: behind a
//! NAT (WSL 2's default network, a VM) nothing is found, and the user names the probe as
//! `tcp:<host>:<port>`. The same as oep-client-python's `discovery` module.
//! ja: TCP で待ち受ける OEP の probe を見つける(DNS-SD の `_oep._tcp` を mDNS で引く)。依存の無い
//! 最小の 1 回の問い合わせ。IPv4 だけ。mDNS は同じリンクの中だけ(NAT の裏では見つからない)。

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const SERVICE: &str = "_oep._tcp.local";
const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_SRV: u16 = 33;
/// A question's class with the unicast-response bit (RFC 6762 §5.4).
const QU_IN: u16 = 0x8001;

/// One probe announcing `_oep._tcp`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Found {
    pub instance: String,
    /// TXT `unit_id` (None when the probe gives none).
    pub unit_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub addrs: Vec<Ipv4Addr>,
}

impl Found {
    /// `<address>:<port>` to open, when SRV gave a port and A (or the host name) a place.
    pub(crate) fn endpoint(&self) -> Option<String> {
        if self.port == 0 {
            return None;
        }
        match self.addrs.first() {
            Some(a) => Some(format!("{a}:{}", self.port)),
            None if !self.host.is_empty() => Some(format!("{}:{}", self.host, self.port)),
            None => None,
        }
    }
}

/// Where the query goes: mDNS's group, or `CH32RV_MDNS_TARGET` (a test's responder; outside the
/// contract).
fn target() -> SocketAddr {
    std::env::var("CH32RV_MDNS_TARGET")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([224, 0, 0, 251], 5353)))
}

/// en: Browse `_oep._tcp` for `wait`: every instance that answered, with what its records say.
/// Nothing found (no network, mDNS blocked) is an empty list, never an error.
/// ja: `wait` の間 `_oep._tcp` を引く。見つからなければ空(誤りにしない)。
pub(crate) fn browse(wait: Duration) -> Vec<Found> {
    browse_at(target(), wait)
}

fn browse_at(to: SocketAddr, wait: Duration) -> Vec<Found> {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else {
        return Vec::new();
    };
    let _ = sock.set_multicast_ttl_v4(255);
    let mut st = State::default();
    let deadline = Instant::now() + wait;
    let mut asked: Vec<(String, u16)> = vec![(SERVICE.to_owned(), T_PTR)];
    let _ = sock.send_to(&query(&asked), to);
    let mut buf = [0u8; 9000];
    let mut next_ask = Instant::now() + wait / 3;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        let _ = sock.set_read_timeout(Some(
            left.min(Duration::from_millis(50))
                .max(Duration::from_millis(1)),
        ));
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            st.feed(&buf[..n]);
        }
        // Ask for what the answers left out (SRV / TXT of an instance, A of a host), once.
        if Instant::now() >= next_ask {
            next_ask = deadline;
            let more: Vec<(String, u16)> = st
                .missing()
                .into_iter()
                .filter(|q| !asked.contains(q))
                .collect();
            if !more.is_empty() {
                let _ = sock.send_to(&query(&more), to);
                asked.extend(more);
            }
        }
    }
    st.found()
}

/// The probe whose TXT unit_id is `unit_id` (case aside, oep-core §3.3), browsing up to `wait`.
pub(crate) fn find_unit(unit_id: &str, wait: Duration) -> Option<Found> {
    browse(wait).into_iter().find(|f| {
        f.unit_id
            .as_deref()
            .is_some_and(|u| u.eq_ignore_ascii_case(unit_id))
    })
}

// ---- the DNS message form (RFC 1035 §4), only what a browse needs ----

fn encode_name(name: &str, out: &mut Vec<u8>) {
    for label in name.trim_end_matches('.').split('.') {
        let b = label.as_bytes();
        out.push(b.len().min(63) as u8);
        out.extend_from_slice(&b[..b.len().min(63)]);
    }
    out.push(0);
}

/// A query for `questions` (name, type), each with the unicast-response bit.
fn query(questions: &[(String, u16)]) -> Vec<u8> {
    let mut q = vec![0, 0, 0, 0];
    q.extend_from_slice(&(questions.len() as u16).to_be_bytes());
    q.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    for (name, t) in questions {
        encode_name(name, &mut q);
        q.extend_from_slice(&t.to_be_bytes());
        q.extend_from_slice(&QU_IN.to_be_bytes());
    }
    q
}

/// A name at `at` (compression followed, RFC 1035 §4.1.4); the name and where the record goes on.
fn read_name(p: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut end = None;
    for _ in 0..64 {
        let len = *p.get(at)?;
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(at + 1)));
        }
        if len & 0xC0 == 0xC0 {
            let ptr = usize::from(u16::from_be_bytes([len & 0x3F, *p.get(at + 1)?]));
            end.get_or_insert(at + 2);
            at = ptr;
            continue;
        }
        let l = usize::from(len);
        labels.push(String::from_utf8_lossy(p.get(at + 1..at + 1 + l)?).into_owned());
        at += 1 + l;
    }
    None
}

/// A record's data, as far as a browse reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rdata {
    Ptr(String),
    Srv { port: u16, host: String },
    Txt(Vec<String>),
    A(Ipv4Addr),
}

/// Every answer, authority and additional record of a message: (name, data).
fn records(p: &[u8]) -> Vec<(String, Rdata)> {
    let mut out = Vec::new();
    let Some(h) = p.get(..12) else {
        return out;
    };
    let count = |i: usize| usize::from(u16::from_be_bytes([h[i], h[i + 1]]));
    let (qd, rr) = (count(4), count(6) + count(8) + count(10));
    let mut at = 12;
    for _ in 0..qd {
        let Some((_, end)) = read_name(p, at) else {
            return out;
        };
        at = end + 4;
    }
    for _ in 0..rr {
        let Some((name, end)) = read_name(p, at) else {
            return out;
        };
        let Some(f) = p.get(end..end + 10) else {
            return out;
        };
        let t = u16::from_be_bytes([f[0], f[1]]);
        let len = usize::from(u16::from_be_bytes([f[8], f[9]]));
        let rd_at = end + 10;
        let Some(rd) = p.get(rd_at..rd_at + len) else {
            return out;
        };
        let data = match t {
            T_PTR => read_name(p, rd_at).map(|(n, _)| Rdata::Ptr(n)),
            T_SRV if len >= 7 => read_name(p, rd_at + 6).map(|(host, _)| Rdata::Srv {
                port: u16::from_be_bytes([rd[4], rd[5]]),
                host,
            }),
            T_TXT => {
                let mut strings = Vec::new();
                let mut i = 0;
                while i < rd.len() {
                    let l = usize::from(rd[i]);
                    if let Some(s) = rd.get(i + 1..i + 1 + l) {
                        strings.push(String::from_utf8_lossy(s).into_owned());
                    }
                    i += 1 + l;
                }
                Some(Rdata::Txt(strings))
            }
            T_A if len == 4 => Some(Rdata::A(Ipv4Addr::new(rd[0], rd[1], rd[2], rd[3]))),
            _ => None,
        };
        if let Some(d) = data {
            out.push((name, d));
        }
        at = rd_at + len;
    }
    out
}

/// What the answers said so far.
#[derive(Default)]
struct State {
    instances: BTreeMap<String, Found>,
    hosts: BTreeMap<String, Vec<Ipv4Addr>>,
}

impl State {
    fn feed(&mut self, packet: &[u8]) {
        let key = |n: &str| n.to_ascii_lowercase();
        for (name, d) in records(packet) {
            match d {
                Rdata::Ptr(inst) if key(&name) == SERVICE => {
                    self.instances.entry(key(&inst)).or_insert_with(|| Found {
                        instance: inst.clone(),
                        ..Found::default()
                    });
                }
                Rdata::Srv { port, host } => {
                    let f = self.instances.entry(key(&name)).or_insert_with(|| Found {
                        instance: name.clone(),
                        ..Found::default()
                    });
                    f.port = port;
                    f.host = host;
                }
                Rdata::Txt(strings) => {
                    let f = self.instances.entry(key(&name)).or_insert_with(|| Found {
                        instance: name.clone(),
                        ..Found::default()
                    });
                    for s in strings {
                        if let Some((k, v)) = s.split_once('=')
                            && k.eq_ignore_ascii_case("unit_id")
                        {
                            f.unit_id = Some(v.to_owned());
                        }
                    }
                }
                Rdata::A(a) => {
                    let v = self.hosts.entry(key(&name)).or_default();
                    if !v.contains(&a) {
                        v.push(a);
                    }
                }
                Rdata::Ptr(_) => {}
            }
        }
    }

    /// Questions for what is still unknown: SRV / TXT of an instance, A of a host.
    fn missing(&self) -> Vec<(String, u16)> {
        let mut q = Vec::new();
        for f in self.instances.values() {
            if f.port == 0 {
                q.push((f.instance.clone(), T_SRV));
            }
            if f.unit_id.is_none() {
                q.push((f.instance.clone(), T_TXT));
            }
            if !f.host.is_empty() && !self.hosts.contains_key(&f.host.to_ascii_lowercase()) {
                q.push((f.host.clone(), T_A));
            }
        }
        q
    }

    /// The instances of `_oep._tcp`, with their hosts' addresses.
    fn found(self) -> Vec<Found> {
        self.instances
            .into_values()
            .filter(|f| f.instance.to_ascii_lowercase().ends_with(SERVICE))
            .map(|mut f| {
                f.addrs = self
                    .hosts
                    .get(&f.host.to_ascii_lowercase())
                    .cloned()
                    .unwrap_or_default();
                f
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// An answer as a reference probe gives it: PTR, then SRV / TXT / A as additional records.
    pub(crate) fn announce(unit: &str, port: u16, ip: [u8; 4]) -> Vec<u8> {
        let inst = format!("OEP {unit}.{SERVICE}");
        let host = format!("oep-{unit}.local");
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        let rr = |p: &mut Vec<u8>, name: &str, t: u16, data: &[u8]| {
            encode_name(name, p);
            p.extend_from_slice(&t.to_be_bytes());
            p.extend_from_slice(&[0x80, 1, 0, 0, 0x11, 0x94]);
            p.extend_from_slice(&(data.len() as u16).to_be_bytes());
            p.extend_from_slice(data);
        };
        let mut d = Vec::new();
        encode_name(&inst, &mut d);
        rr(&mut p, SERVICE, T_PTR, &d);
        let mut d = vec![0, 0, 0, 0];
        d.extend_from_slice(&port.to_be_bytes());
        encode_name(&host, &mut d);
        rr(&mut p, &inst, T_SRV, &d);
        let txt = format!("unit_id={unit}");
        let mut d = vec![txt.len() as u8];
        d.extend_from_slice(txt.as_bytes());
        rr(&mut p, &inst, T_TXT, &d);
        rr(&mut p, &host, T_A, &ip);
        p
    }

    #[test]
    fn an_announcement_reads_back() {
        let mut st = State::default();
        st.feed(&announce("fafe00000003", 7450, [192, 168, 1, 23]));
        let f = st.found();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].unit_id.as_deref(), Some("fafe00000003"));
        assert_eq!(f[0].endpoint().as_deref(), Some("192.168.1.23:7450"));
    }

    #[test]
    fn compressed_names_and_a_missing_srv_are_handled() {
        // PTR only: SRV and TXT are asked for next.
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0];
        encode_name(SERVICE, &mut p);
        p.extend_from_slice(&T_PTR.to_be_bytes());
        p.extend_from_slice(&[0, 1, 0, 0, 0x11, 0x94, 0, 6]);
        // "OEP x" + a pointer to the service name at offset 12.
        p.extend_from_slice(&[5, b'O', b'E', b'P', b' ', b'x', 0xC0, 12]);
        let mut st = State::default();
        st.feed(&p);
        let q = st.missing();
        assert!(
            q.iter()
                .any(|(n, t)| n == "OEP x._oep._tcp.local" && *t == T_SRV)
        );
        assert!(q.iter().any(|(_, t)| *t == T_TXT));
    }

    #[test]
    fn a_browse_finds_a_responder() {
        // A responder on loopback answers the query from its source port (legacy unicast).
        let r = UdpSocket::bind("127.0.0.1:0").unwrap();
        let at = r.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (_, from) = r.recv_from(&mut buf).unwrap();
            r.send_to(&announce("abc123", 7450, [10, 0, 0, 5]), from)
                .unwrap();
        });
        let f = browse_at(at, Duration::from_millis(300));
        t.join().unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].unit_id.as_deref(), Some("abc123"));
        assert_eq!(f[0].endpoint().as_deref(), Some("10.0.0.5:7450"));
    }
}
