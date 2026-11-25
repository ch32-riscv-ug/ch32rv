//! en: The `auto` monitor source: whatever the target talks on, read at once and merged into one
//! stream - the UART group (a serial port's / a WCH-Link CDC's `uart`, an OEP probe's
//! `fixture-uart`), one mailbox console (dmseq, sdi or dmdata, whichever the sketch uses) and RTT
//! (when its control block is there). What the user types goes to the path the sketch's output
//! last came from (sdi carries nothing back, so then to the UART; before any output, to the
//! UART), in order and never dropped, except at a target restart.
//!
//! On a WCH-Link the target is attached, its mailbox emptied and reset to run (which also undoes
//! what the attach did to its clock and registers); one reader tells the mailbox framings apart by
//! the word's shape. An OEP probe's target is not reset; its console mechanisms are tried in turn
//! (dmseq, sdi, dmdata) until one carries data.
//!
//! ja: monitor の `auto`。target が使う経路を全部同時に読み、1 本にまとめる: UART 系(serial の口 /
//! WCH-Link の CDC の uart、OEP の fixture-uart)、郵便受けの console 1 つ(dmseq / sdi / dmdata の
//! sketch が使うもの)、RTT(control block があれば)。入力は sketch の出力が最後に来た経路へ送る
//! (sdi は返せないので UART。出力の前も UART)。順を保ち、落とさない(target の再起動では捨てる)。
//! WCH-Link は attach → 郵便受けを空に → reset して走らせる(attach のクロックとレジスタの副作用も
//! 消える)。郵便受けは 1 つの読み手が語の形で見分ける。OEP は reset せず、方式を順に試す。

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use ch32rv_contract::Warning;
use ch32rv_contract::policy::MonitorSource;
use ch32rv_dmi::MailboxShape;
use serialport::SerialPort;

use crate::cmd_probe::Entry;
use crate::session::Session;
use crate::source::{DmiSource, RttChannels, RttTarget};

/// en: RTT on a session whose mailbox a console is using: a memory access through the Debug Module
/// goes through DATA0 / DATA1, so they are saved once the hart is halted and put back before it
/// runs (measured: RTT scans and polls cut SDI and DMDATA input and output off on a CH32V307).
/// ja: 郵便受けを console が使っている session での RTT。DM 越しのメモリの読み書きは DATA0 / DATA1
/// を通るので、止めたら控え、走らせる前に戻す(V307 で RTT が SDI と DMDATA を切った実測)。
struct KeepMailbox<'a> {
    session: &'a mut Session,
    saved: Option<(u32, u32)>,
}

impl RttTarget for KeepMailbox<'_> {
    fn is_halted(&mut self) -> Result<bool, ch32rv_dmi::DmiError> {
        self.session.is_halted()
    }
    fn halt(&mut self) -> Result<(), ch32rv_dmi::DmiError> {
        self.session.halt()?;
        if self.saved.is_none() {
            self.saved = Some(self.session.dm().mailbox_words()?);
        }
        Ok(())
    }
    fn resume(&mut self) -> Result<(), ch32rv_dmi::DmiError> {
        if let Some((d0, d1)) = self.saved.take() {
            self.session.dm().set_mailbox_words(d0, d1)?;
        }
        self.session.resume()
    }
    fn read(&mut self, addr: u32, len: u32) -> Result<Vec<u8>, ch32rv_dmi::DmiError> {
        self.session.read(addr, len)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), ch32rv_dmi::DmiError> {
        RttTarget::write(self.session, addr, data)
    }
}

/// en: When (after the open, or a target restart) a WCH-Link target's RAM is scanned for an RTT
/// control block, once each: a scan halts the hart, so not forever.
/// ja: RTT の control block を探す時刻(開いた後、再起動の後)。探すと hart が止まるので数回だけ。
const RTT_SCANS_MS: [u64; 3] = [500, 2000, 4000];
/// How often a found RTT ring is polled (each poll halts the hart briefly).
const RTT_EVERY: Duration = Duration::from_millis(50);
/// en: How long an OEP console mechanism is given to carry data before the next is tried.
/// ja: OEP の console の方式 1 つに、データが来るのを待つ時間。
const OEP_TRY: Duration = Duration::from_secs(3);
/// The order the mailbox consoles are tried in on an OEP probe.
const OEP_ORDER: [MonitorSource; 3] = [
    MonitorSource::Dmseq,
    MonitorSource::Sdi,
    MonitorSource::Dmdata,
];
/// Unknown words in a row before an unattended mailbox is emptied (what attach / flash left).
const LEFTOVER_READS: u8 = 3;

/// Where a byte came from / goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    Uart,
    Mail,
    Rtt,
}

/// The paths behind one `auto` monitor.
enum Kind {
    /// A serial port with no debug probe behind it (or one not to be touched): the UART only.
    Uart,
    Wch(Box<Wch>),
    Oep(Box<Oep>),
}

struct Wch {
    session: Session,
    /// The mailbox console once its framing is known.
    mail: Option<DmiSource>,
    /// Unknown words read in a row while no framing is known.
    other_run: u8,
    rtt: Option<RttChannels>,
    rtt_polled: Instant,
    /// When the RTT scan schedule started (the open, or the last restart).
    rtt_from: Instant,
    rtt_scans: usize,
    /// dmseq restarts seen.
    restarts: u64,
}

struct Oep {
    addr: crate::oep::OepAddr,
    chip: Option<String>,
    speed: Option<u32>,
    fixture: Option<crate::oep::ConsoleSession>,
    /// The mechanism on trial (an index into [`OEP_ORDER`]) and its stream.
    mail: Option<crate::oep::ConsoleSession>,
    trying: usize,
    since: Instant,
    locked: bool,
    rtt: Option<crate::oep::ConsoleSession>,
    rtt_tried: bool,
    rtt_polled: Instant,
    opened: Instant,
}

pub(crate) struct Auto {
    kind: Kind,
    uart: Option<Box<dyn SerialPort>>,
    /// The path the sketch's output last came from (None: nothing yet).
    last: Option<Path>,
    q_uart: Vec<u8>,
    q_mail: Vec<u8>,
    q_rtt: Vec<u8>,
    /// The status line last said.
    said: String,
    /// Lines for the user, given out with the next poll.
    notes: Vec<u8>,
    /// Each path's output, merged a line at a time (uart, mailbox, rtt).
    lines: [Lines; 3],
    pub(crate) warnings: Vec<Warning>,
}

/// en: How long a path's unfinished line (a prompt, say) waits for the rest before it is given
/// out as it is.
/// ja: 経路の途中の行(プロンプトなど)が続きを待つ時間。過ぎたらそのまま出す。
const LINE_WAIT: Duration = Duration::from_millis(100);

/// en: One path's output, given out a line at a time so that two paths do not cut into each
/// other's lines (measured: a DMSeq and a UART line of one TICK came out interleaved).
/// ja: 1 つの経路の出力を行ごとに出す(2 つの経路の行が混ざらないように)。
#[derive(Default)]
struct Lines {
    buf: Vec<u8>,
    since: Option<Instant>,
}

impl Lines {
    fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        if data.is_empty() {
            return;
        }
        self.buf.extend_from_slice(data);
        if let Some(i) = self.buf.iter().rposition(|&b| b == b'\n') {
            out.extend(self.buf.drain(..=i));
            self.since = None;
        }
        if !self.buf.is_empty() && self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    fn flush_old(&mut self, out: &mut Vec<u8>) {
        if self.since.is_some_and(|t| t.elapsed() >= LINE_WAIT) {
            out.append(&mut self.buf);
            self.since = None;
        }
    }
}

fn note(text: &str) -> Vec<u8> {
    format!("\r\n[ch32rv monitor] {text}\r\n").into_bytes()
}

impl Auto {
    /// en: Only a serial port (no debug probe): `uart` as it is.
    /// ja: serial の口だけ(debug の probe なし)。
    pub(crate) fn open_uart(port: Box<dyn SerialPort>) -> Self {
        let mut a = Auto::new(Kind::Uart, Some(port));
        a.say();
        a
    }

    /// en: A WCH-Link: its CDC (`uart`, if it has one), then attach, empty the mailbox, reset to
    /// run. The CDC is opened first so that the sketch's first lines are not missed.
    /// ja: WCH-Link。CDC(あれば)を先に開き、attach → 郵便受けを空に → reset して走らせる。
    pub(crate) fn open_wch(
        entry: &Entry,
        port: Option<Box<dyn SerialPort>>,
        speed: ch32rv_wchlink::Speed,
        lock_timeout: Duration,
        chip: Option<&str>,
    ) -> Result<Self, String> {
        let mut warnings = Vec::new();
        let mut session = Session::attach(
            entry,
            speed,
            Duration::from_millis(1000),
            lock_timeout,
            chip,
            None,
            &mut warnings,
        )
        .map_err(|e| format!("attach: {e}"))?;
        // A word attach left in DATA0 would keep an SDI_Printf target waiting and make a dmdata
        // target read input nobody typed; 0 is silence to all three framings.
        session
            .dm()
            .dmdata_clear()
            .map_err(|e| format!("mailbox: {e}"))?;
        // The reset undoes what the attach did (the clock, a V103 / V205 / H417 register).
        crate::cmd_flash::soft_reset_and_run(&mut session)?;
        follow_running_hart(&mut session);
        // The attach's own warnings (re-clocking, corrupted registers) no longer apply.
        warnings.retain(|w| {
            !matches!(
                w.code.as_str(),
                "attach-reclocks-target"
                    | "attach-corrupts-register"
                    | "attach-may-corrupt-register"
            )
        });
        let now = Instant::now();
        let mut a = Auto::new(
            Kind::Wch(Box::new(Wch {
                session,
                mail: None,
                other_run: 0,
                rtt: None,
                rtt_polled: now,
                rtt_from: now,
                rtt_scans: 0,
                restarts: 0,
            })),
            port,
        );
        a.warnings = warnings;
        a.say();
        Ok(a)
    }

    /// en: An OEP probe: its fixture UART when the port's labels give one, and the console
    /// mechanisms in turn. The target is not reset (an OEP attach changes nothing on it).
    /// ja: OEP の probe。口のラベルに rx/tx があれば fixture の UART、console の方式は順に試す。
    pub(crate) fn open_oep(
        addr: &crate::oep::OepAddr,
        chip: Option<&str>,
        speed: Option<u32>,
        baud: u32,
    ) -> Result<Self, String> {
        let fixture = crate::oep::ConsoleSession::open(
            addr,
            crate::oep::StreamWanted::FixtureUart(baud),
            chip,
            speed,
        )
        .ok();
        let mut oep = Oep {
            addr: addr.clone(),
            chip: chip.map(str::to_owned),
            speed,
            fixture,
            mail: None,
            trying: 0,
            since: Instant::now(),
            locked: false,
            rtt: None,
            rtt_tried: false,
            rtt_polled: Instant::now(),
            opened: Instant::now(),
        };
        oep.mail = oep.open_mail(0).ok();
        if oep.fixture.is_none() && oep.mail.is_none() {
            // Neither path opens: say why the console does not.
            oep.open_mail(0)?;
        }
        let mut a = Auto::new(Kind::Oep(Box::new(oep)), None);
        a.say();
        Ok(a)
    }

    fn new(kind: Kind, uart: Option<Box<dyn SerialPort>>) -> Self {
        Auto {
            kind,
            uart,
            last: None,
            q_uart: Vec::new(),
            q_mail: Vec::new(),
            q_rtt: Vec::new(),
            said: String::new(),
            notes: Vec::new(),
            lines: Default::default(),
            warnings: Vec::new(),
        }
    }

    /// en: The paths open now, as the status line names them.
    /// ja: 今開いている経路(状態の行の名前)。
    pub(crate) fn status(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut looking: Vec<&str> = Vec::new();
        match &self.kind {
            Kind::Uart => parts.push("uart".to_owned()),
            Kind::Wch(w) => {
                if self.uart.is_some() {
                    parts.push("uart".to_owned());
                }
                match &w.mail {
                    Some(m) => parts.push(m.name().to_owned()),
                    None => looking.push("dmseq / sdi / dmdata"),
                }
                if w.rtt.is_some() {
                    parts.push("rtt".to_owned());
                } else if w.rtt_scans < RTT_SCANS_MS.len() {
                    looking.push("rtt");
                }
            }
            Kind::Oep(o) => {
                if o.fixture.is_some() {
                    parts.push("fixture-uart".to_owned());
                }
                if o.locked {
                    parts.push(OEP_ORDER[o.trying].as_str().to_owned());
                } else {
                    looking.push("dmseq / sdi / dmdata");
                }
                if o.rtt.is_some() {
                    parts.push("rtt".to_owned());
                } else if !o.rtt_tried {
                    looking.push("rtt");
                }
            }
        }
        let mut s = if parts.is_empty() {
            "auto: (nothing yet)".to_owned()
        } else {
            format!("auto: {}", parts.join(" + "))
        };
        if !looking.is_empty() {
            s.push_str(&format!("; looking for {}", looking.join(", ")));
        }
        s
    }

    /// Queue the status line when it changed.
    fn say(&mut self) {
        let s = self.status();
        if s != self.said {
            self.notes.extend(note(&s));
            self.said = s;
        }
    }

    /// en: What the user typed: to the path the output last came from (the UART before any).
    /// ja: 入力。出力が最後に来た経路へ(何も来ていなければ UART)。
    pub(crate) fn input(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let q = match self.last {
            Some(Path::Mail) => &mut self.q_mail,
            Some(Path::Rtt) => &mut self.q_rtt,
            Some(Path::Uart) | None => &mut self.q_uart,
        };
        q.extend_from_slice(data);
    }

    /// A target restart: what was typed and not yet sent is dropped.
    fn restarted(&mut self) {
        self.q_uart.clear();
        self.q_mail.clear();
        self.q_rtt.clear();
        self.last = None;
    }

    /// The UART's line settings, as the IDE changes them.
    pub(crate) fn set_line(&mut self, baud: u32, dtr: bool, rts: bool) -> Result<(), String> {
        if let Some(p) = self.uart.as_mut() {
            let _ = p.set_baud_rate(baud);
            let _ = p.write_data_terminal_ready(dtr);
            let _ = p.write_request_to_send(rts);
        }
        if let Kind::Oep(o) = &mut self.kind
            && let Some(f) = o.fixture.as_mut()
        {
            f.set_baud(baud)?;
        }
        Ok(())
    }

    /// en: One round over every path: send what is queued, read what came. The bytes returned are
    /// the target's output, merged, and the status lines.
    /// ja: 全経路を 1 周: 溜まった入力を送り、来たものを読む。返すのは target の出力と状態の行。
    pub(crate) fn poll(&mut self) -> Result<Vec<u8>, String> {
        let mut out = std::mem::take(&mut self.notes);
        let mut from_uart = Vec::new();
        // The UART group.
        if let Some(p) = self.uart.as_mut() {
            if !self.q_uart.is_empty() {
                p.write_all(&self.q_uart)
                    .map_err(|e| format!("serial port write failed: {e}"))?;
                self.q_uart.clear();
            }
            let mut buf = [0u8; 512];
            match p.read(&mut buf) {
                Ok(n) if n > 0 => {
                    from_uart.extend_from_slice(&buf[..n]);
                    self.last = Some(Path::Uart);
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(format!("serial port read failed: {e}")),
            }
        }
        let got = match &mut self.kind {
            Kind::Uart => Got::default(),
            Kind::Wch(w) => w.poll(&mut self.q_mail, &mut self.q_rtt)?,
            Kind::Oep(o) => o.poll(&mut self.q_uart, &mut self.q_mail, &mut self.q_rtt)?,
        };
        if got.restarted {
            self.restarted();
        }
        for (path, bytes) in [
            (Path::Uart, got.uart),
            (Path::Mail, got.mail),
            (Path::Rtt, got.rtt),
        ] {
            let merged = if path == Path::Uart {
                // The WCH-Link's CDC and an OEP fixture UART are one path.
                from_uart.extend_from_slice(&bytes);
                &from_uart
            } else {
                &bytes
            };
            self.lines[path as usize].push(merged, &mut out);
            if !merged.is_empty() {
                // sdi carries nothing back: its output leaves the UART the way back.
                self.last = Some(if path == Path::Mail && got.mail_is_sdi {
                    Path::Uart
                } else {
                    path
                });
            }
        }
        // en: With no UART to send to (an OEP port without rx / tx labels, a Link without a CDC),
        // what was typed before any output waits for the first path that can take it.
        // ja: UART が無いとき、出力の前に打った分は、受け取れる最初の経路を待つ。
        if !self.has_uart() && !self.q_uart.is_empty() {
            match self.last {
                Some(Path::Mail) => self.q_mail.append(&mut self.q_uart),
                Some(Path::Rtt) => self.q_rtt.append(&mut self.q_uart),
                _ => {}
            }
        }
        for (code, msg) in got.notices {
            self.notes.extend(note(&format!("{code}: {msg}")));
        }
        for l in &mut self.lines {
            l.flush_old(&mut out);
        }
        self.say();
        out.extend(std::mem::take(&mut self.notes));
        Ok(out)
    }

    fn has_uart(&self) -> bool {
        self.uart.is_some() || matches!(&self.kind, Kind::Oep(o) if o.fixture.is_some())
    }

    /// How long to wait after a round in which nothing came.
    pub(crate) fn idle(&self) -> Duration {
        Duration::from_millis(2)
    }
}

/// What one round over the debug paths brought.
#[derive(Default)]
struct Got {
    uart: Vec<u8>,
    mail: Vec<u8>,
    mail_is_sdi: bool,
    rtt: Vec<u8>,
    restarted: bool,
    notices: Vec<(&'static str, String)>,
}

/// en: After the reset of a CH32H417 running a V5F build, core 1 runs the sketch and core 0 waits:
/// the session follows core 1 (memory reads through core 0 return the console's words there).
/// ja: V5F のビルドの H417 は reset の後 core 1 が sketch を走らせる。session を core 1 に向ける。
fn follow_running_hart(session: &mut Session) {
    if session.attach.family_byte != 0xc6 {
        return;
    }
    std::thread::sleep(Duration::from_millis(50));
    let family = session.attach.family_byte;
    let ran = crate::session::harts_that_ran(session.link(), family);
    if ran[1] {
        let _ = session.select_core(1);
    }
}

impl Wch {
    fn poll(&mut self, q_mail: &mut Vec<u8>, q_rtt: &mut Vec<u8>) -> Result<Got, String> {
        let mut got = Got::default();
        match &mut self.mail {
            None => {
                let (d0, d1) = self
                    .session
                    .dm()
                    .mailbox_words()
                    .map_err(|e| format!("mailbox: {e}"))?;
                let source = match ch32rv_dmi::mailbox_shape(d0, d1) {
                    MailboxShape::Empty => None,
                    MailboxShape::Sdi => Some(MonitorSource::Sdi),
                    MailboxShape::Dmseq => Some(MonitorSource::Dmseq),
                    MailboxShape::Dmdata => Some(MonitorSource::Dmdata),
                    MailboxShape::Other => {
                        self.other_run += 1;
                        if self.other_run >= LEFTOVER_READS {
                            self.other_run = 0;
                            // Clears only a word that is no framing's.
                            let _ = self.session.dm().sdi_prepare();
                        }
                        None
                    }
                };
                if let Some(source) = source {
                    self.other_run = 0;
                    // dmdata: no clearing here (the frame just seen is the sketch's output).
                    self.mail = Some(if source == MonitorSource::Dmdata {
                        DmiSource::Dmdata
                    } else {
                        DmiSource::open(&mut self.session, source, &mut Vec::new())
                            .map_err(|e| format!("open {}: {e}", source.as_str()))?
                    });
                }
            }
            Some(m) => {
                let is_sdi = m.name() == "sdi";
                let mut none = Vec::new();
                let q = if is_sdi { &mut none } else { q_mail };
                got.mail = m
                    .poll(&mut self.session, q)
                    .map_err(|e| format!("{} failed: {e}", m.name()))?;
                got.mail_is_sdi = is_sdi;
                // A framing that turns out not to be this sketch's: look again.
                if m.take_notices()
                    .iter()
                    .any(|(code, _)| code.ends_with("-no-console"))
                {
                    self.mail = None;
                } else if m.restarts() > self.restarts {
                    self.restarts = m.restarts();
                    got.restarted = true;
                    self.rtt = None;
                    self.rtt_scans = 0;
                    self.rtt_from = Instant::now();
                }
            }
        }
        // RTT: scan on schedule; poll at RTT's pace once found.
        match &mut self.rtt {
            None => {
                if self.rtt_scans < RTT_SCANS_MS.len()
                    && self.rtt_from.elapsed()
                        >= Duration::from_millis(RTT_SCANS_MS[self.rtt_scans])
                {
                    self.rtt_scans += 1;
                    let window = crate::source::rtt_window_in(
                        self.session.db(),
                        Some(self.session.attach.chip_id),
                    );
                    let mut keep = KeepMailbox {
                        session: &mut self.session,
                        saved: None,
                    };
                    if let Ok(ch) =
                        crate::source::rtt_find_tries(&mut keep, window, &mut Vec::new(), 1)
                    {
                        self.rtt = Some(ch);
                        self.rtt_polled = Instant::now();
                    }
                    // Found or not, the core runs on (with its mailbox put back).
                    let _ = keep.resume();
                }
            }
            Some(ch) => {
                if self.rtt_polled.elapsed() >= RTT_EVERY {
                    self.rtt_polled = Instant::now();
                    let mut keep = KeepMailbox {
                        session: &mut self.session,
                        saved: None,
                    };
                    got.rtt = crate::source::rtt_poll(&mut keep, *ch, q_rtt)
                        .map_err(|e| format!("rtt failed: {e}"))?;
                }
            }
        }
        Ok(got)
    }
}

impl Oep {
    fn open_mail(&self, i: usize) -> Result<crate::oep::ConsoleSession, String> {
        use ch32rv_oep::stream::Mechanism;
        let mech = match OEP_ORDER[i] {
            MonitorSource::Dmseq => Mechanism::Dmseq,
            MonitorSource::Sdi => Mechanism::Sdi,
            _ => Mechanism::Dmdata,
        };
        crate::oep::ConsoleSession::open(
            &self.addr,
            crate::oep::StreamWanted::Console(mech),
            self.chip.as_deref(),
            self.speed,
        )
    }

    fn poll(
        &mut self,
        q_uart: &mut Vec<u8>,
        q_mail: &mut Vec<u8>,
        q_rtt: &mut Vec<u8>,
    ) -> Result<Got, String> {
        let mut got = Got::default();
        if let Some(f) = self.fixture.as_mut() {
            if !q_uart.is_empty() {
                let n = f.write(q_uart)?;
                q_uart.drain(..n.min(q_uart.len()));
            }
            got.uart = f.poll()?;
        }
        let is_sdi = OEP_ORDER[self.trying] == MonitorSource::Sdi;
        if let Some(m) = self.mail.as_mut() {
            if self.locked && !is_sdi && !q_mail.is_empty() {
                let n = m.write(q_mail)?;
                q_mail.drain(..n.min(q_mail.len()));
            }
            got.mail = m.poll()?;
            got.mail_is_sdi = is_sdi;
            if !got.mail.is_empty() {
                self.locked = true;
            }
        }
        if !self.locked && self.since.elapsed() >= OEP_TRY {
            // Not this one: the next mechanism (a wrong one only stays quiet).
            self.mail = None;
            self.trying = (self.trying + 1) % OEP_ORDER.len();
            self.since = Instant::now();
            self.mail = self.open_mail(self.trying).ok();
        }
        if !self.rtt_tried && self.opened.elapsed() >= Duration::from_millis(RTT_SCANS_MS[0]) {
            self.rtt_tried = true;
            self.rtt = crate::oep::ConsoleSession::open(
                &self.addr,
                crate::oep::StreamWanted::Rtt,
                self.chip.as_deref(),
                self.speed,
            )
            .ok();
        }
        if let Some(r) = self.rtt.as_mut()
            && self.rtt_polled.elapsed() >= RTT_EVERY
        {
            // Each RTT poll halts the hart briefly: RTT's pace.
            self.rtt_polled = Instant::now();
            if !q_rtt.is_empty() {
                let n = r.write(q_rtt)?;
                q_rtt.drain(..n.min(q_rtt.len()));
            }
            got.rtt = r.poll()?;
        }
        Ok(got)
    }
}
