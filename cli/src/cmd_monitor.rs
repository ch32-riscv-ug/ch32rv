//! en: `monitor` (docs/cli.ja.md §4.5). Two backends:
//!   - CDC serial (`uart`, `sdi`): open the probe's CDC port. `sdi` first tells the LinkE to
//!     forward the target's DM data registers to that same port (LinkE only; mixes with uart).
//!     `uart` also forwards stdin to the port; `sdi` is receive-only.
//!   - DMI (`dmdata`, `rtt`): the host reads the target's debug registers / RAM directly while
//!     the core runs, and pushes stdin to the target the same way. The sources themselves live in
//!     [`crate::source`] and are shared with `run` and `arduino monitor`.
//!
//! Output goes to stdout as raw bytes; under `--json` it goes to stderr as `output` NDJSON events
//! so stdout keeps the single result envelope (docs/cli.ja.md §3.5). The loop runs until Ctrl-C
//! or `--duration`; losing the device ends it with a failure exit, not 0.
//!
//! ja: `monitor`。CDC serial(uart/sdi)と DMI(dmdata/rtt)の 2 backend。設計は cli.ja.md §4.5。
//! uart/dmdata/rtt は stdin を target へ流す(sdi は受信のみ)。出力は stdout へ生 byte、`--json`
//! 時は stderr の `output` event(stdout は envelope 専用)。device 喪失は失敗 exit で終わる。

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ch32rv_contract::ErrorKind;
use ch32rv_contract::policy::MonitorSource;

use crate::args::{Cli, MonitorArgs, MonitorCmd, SwitchState};
use crate::cmd_probe::{Entry, fail, mode_str, select_entry};
use crate::parse;
use crate::session::Session;
use crate::source::{self, DmiSource, OpenError, Sink};

pub fn monitor(cli: &Cli, args: &MonitorArgs) -> ExitCode {
    match &args.cmd {
        Some(MonitorCmd::List) => return list(cli),
        Some(MonitorCmd::Sdi { state }) => return sdi_toggle(cli, *state),
        None => {}
    }
    // An OEP probe, or a WCH-Link whose broker runs: the console (or fixture UART) through it.
    let addr = match crate::oep::addr(cli, "monitor") {
        Ok(a) => a,
        Err(c) => return c,
    };
    match (&addr, args.source) {
        // A WCH-Link's broker: rtt and uart (the default) open the Link / its CDC as before
        // (borrowing it).
        (
            Some(crate::oep::OepAddr::Wch(_)),
            None | Some(MonitorSource::Uart | MonitorSource::Rtt),
        )
        | (None, _) => {}
        // en: An OEP probe has no "uart" of its own: say so, rather than falling through to the
        // WCH-Link lookup and reporting the selector as matching nothing. (rtt goes to run_oep:
        // ch32rv runs it over the probe's riscv-dm.)
        // ja: OEP の probe に「uart」は無い。WCH-Link を探しに行って selector が何にも当たらない、と
        // 言う代わりにそう言う(rtt は run_oep へ。ch32rv が probe の riscv-dm の上で行う)。
        (Some(_), Some(MonitorSource::Uart)) => {
            return fail(
                cli,
                "monitor",
                ErrorKind::CapabilityUnsupported,
                "an OEP probe has no uart source here",
                Some(
                    "leave --source out for the target's console (the slot's mechanism, else \
                     dmseq), or name one: dmseq / dmdata / sdi, rtt, or fixture-uart (the fixture's \
                     UART)",
                ),
            );
        }
        (Some(a), _) => return run_oep(cli, args, a),
    }
    let source = args.source.unwrap_or(MonitorSource::Uart);
    match source {
        MonitorSource::Uart => run_uart(cli, args),
        MonitorSource::Sdi => run_sdi(cli, args),
        MonitorSource::Dmdata | MonitorSource::Dmseq | MonitorSource::Rtt => run_dmi(cli, source),
        MonitorSource::FixtureUart => fail(
            cli,
            "monitor",
            ErrorKind::CapabilityUnsupported,
            "fixture-uart is an OEP probe's source, served by `arduino monitor`",
            None,
        ),
    }
}

/// How long to stream before returning; None = until Ctrl-C. From the global `--duration`
/// (distinct from `--timeout`, which is the per-transfer transport timeout).
fn run_duration(cli: &Cli) -> Option<Duration> {
    cli.duration.map(Duration::from_secs)
}

/// Normal end of a stream (`--duration` elapsed): the JSON envelope, or plain exit 0.
fn finish_ok(
    cli: &Cli,
    cmd: &str,
    source: &str,
    warnings: Vec<ch32rv_contract::Warning>,
) -> ExitCode {
    if cli.json {
        let mut env = ch32rv_contract::ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({ "source": source }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        ExitCode::SUCCESS
    }
}

/// en: The warning every attaching monitor prints. A WCH-Link AttachChip may leave the target's clock
/// reprogrammed and not restored - seen on the wire for CH32L103 / V20x / X035 and inferred for V30x,
/// while a V003 attach touches no RCC at all (2026-09-25, `docs/protocol/wch-link.ja.md` §7a) - so
/// the firmware being watched may keep a different clock until it resets. It is worded as a
/// possibility and shown on every target: the per-family picture is still being filled in, and a
/// definite claim would be wrong wherever it turns out otherwise. ch32rv cannot undo it (the original
/// values are overwritten inside AttachChip); `how` says how to watch at the firmware's own clock.
/// ja: attach する monitor が必ず出す警告。WCH-Link の AttachChip は target のクロックを組み直したまま
/// 戻さないことがある(L103 / V20x / X035 は線で確認、V30x は推定、V003 は RCC に触れない。protocol §7a)
/// ので、観測中の firmware は reset まで別のクロックで走っている可能性がある。family ごとの全体像はまだ
/// 埋まっておらず、断定すると外れた family で誤りになるので、「可能性」として全 target に出す。元の値は
/// AttachChip の中で上書きされるので ch32rv は戻せない。`how` は firmware 自身のクロックで見る方法。
fn attach_reclocks_warning(how: &str) -> ch32rv_contract::Warning {
    ch32rv_contract::Warning {
        code: "attach-reclocks-target".to_owned(),
        msg: format!(
            "attaching through a WCH-Link may have reprogrammed the target's clock, and it is not \
             restored: the running firmware may now run at a different clock (UART baud rates and \
             timers off) until it resets. To watch it at its own clock, restart it after attaching: \
             {how}"
        ),
    }
}

// ---- CDC serial backend ----

/// Resolve the CDC serial port for a probe (explicit --port wins).
fn resolve_port(
    cli: &Cli,
    cmd: &str,
    entry: &Entry,
    explicit: &Option<String>,
) -> Result<String, ExitCode> {
    match explicit {
        Some(p) => Ok(p.clone()),
        None => entry.dev.serial_ports().into_iter().next().ok_or_else(|| {
            fail(
                cli,
                cmd,
                ErrorKind::DeviceNotFound,
                "no CDC serial port found for this probe",
                Some("pass --port /dev/ttyACMx, or check the probe's serial interface"),
            )
        }),
    }
}

/// en: Stream the probe's CDC port until the deadline / Ctrl-C.
/// `raw=true` opens the tty as a plain file (like `cat`) WITHOUT touching modem control, which
/// is required for SDI: the serialport crate asserts DTR on open and the WCH-LinkE then stops
/// forwarding SDI after one line (measured). That path is receive-only. `raw=false` uses the
/// serialport crate so `--baud` takes effect for the physical UART bridge (where DTR is
/// harmless) and forwards stdin to the port.
/// ja: probe の CDC を流す。`raw=true` は tty を生ファイルで開き modem 線を触らない(SDI 必須。
/// serialport は DTR を assert して forward を止める実測)、受信のみ。`raw=false` は serialport で
/// baud を効かせ(物理 UART bridge 用、DTR 無害)、stdin を port へ流す。
fn stream_port(
    cli: &Cli,
    cmd: &str,
    port_path: &str,
    baud: u32,
    label: &'static str,
    raw: bool,
    warnings: Vec<ch32rv_contract::Warning>,
) -> ExitCode {
    if !cli.json {
        eprintln!("monitor: {label} on {port_path} @ {baud} baud (Ctrl-C to stop)");
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
    }
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let mut sink = Sink::new(cli, label);
    let mut buf = [0u8; 512];

    #[cfg(unix)]
    if raw {
        use std::io::Read;
        // en: Plain blocking open, exactly like `cat`. Opening with O_NONBLOCK, or via the
        // serialport crate (which asserts DTR), makes the WCH-LinkE stop forwarding SDI after
        // one line (measured); this blocking file read keeps it streaming.
        // ja: cat と同じ素のブロッキング open。O_NONBLOCK や serialport(DTR assert)だと LinkE の
        // SDI forward が 1 行で止まる(実測)。ブロッキング読みなら流れ続ける。
        let mut file = match open_raw(port_path) {
            Ok(f) => f,
            Err(e) => {
                return fail(
                    cli,
                    cmd,
                    ErrorKind::DeviceOpenFailed,
                    format!("open {port_path}: {e}"),
                    None,
                );
            }
        };
        // en: The read blocks until the probe forwards something, so it runs on a thread of its
        // own and this loop waits on it with a timeout. `--duration` has to end the stream even
        // while the target prints nothing: checked only after a read returned, a silent target
        // kept `monitor --source sdi --duration 4` open for 34.8 s (CH32L103, 2026-09-25). The
        // fd stays a plain blocking one (see above for why it must); on the deadline the reader
        // is simply left blocked and goes away with the process.
        // ja: read は probe が何か流すまで戻らないので専用スレッドで回し、このループは timeout 付きで
        // 待つ。`--duration` は target が無音でも効かなければならない(read が戻った後にしか確認
        // しない実装では、無音の L103 で `--duration 4` が 34.8 秒続いた)。fd は素のブロッキングの
        // まま(理由は上)。締め切りでは reader をブロックさせたまま置いていき、プロセスと共に消える。
        let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<Vec<u8>>>();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 512];
            loop {
                let r = file.read(&mut chunk).map(|n| chunk[..n].to_vec());
                let last = !matches!(&r, Ok(v) if !v.is_empty());
                if tx.send(r).is_err() || last {
                    break;
                }
            }
        });
        loop {
            let wait = match deadline {
                Some(dl) => match dl.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => left,
                    _ => {
                        sink.finish();
                        return finish_ok(cli, cmd, label, warnings);
                    }
                },
                None => Duration::from_secs(3600),
            };
            match rx.recv_timeout(wait) {
                Ok(Ok(data)) if !data.is_empty() => sink.write(&data),
                Ok(Ok(_)) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    sink.finish();
                    return fail(
                        cli,
                        cmd,
                        ErrorKind::DeviceNotFound,
                        format!("{port_path} closed (probe disconnected?)"),
                        None,
                    );
                }
                Ok(Err(e)) => {
                    sink.finish();
                    return fail(
                        cli,
                        cmd,
                        ErrorKind::TransferFailed,
                        format!("read {port_path}: {e}"),
                        None,
                    );
                }
                // Nothing yet: go round and re-check the deadline.
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    // en: The raw-open path above is unix-only; on other platforms `raw` has no effect here, so
    // consume it explicitly (otherwise it is an unused variable on e.g. Windows).
    // ja: 上の raw open は unix 限定。他 OS では raw は無効なので明示的に消費(でないと Windows 等で未使用)。
    #[cfg(not(unix))]
    let _ = raw;

    let mut sp = match serialport::new(port_path, baud)
        .timeout(Duration::from_millis(200))
        .open()
    {
        Ok(s) => s,
        Err(e) => {
            return fail(
                cli,
                cmd,
                ErrorKind::DeviceOpenFailed,
                format!("open {port_path}: {e}"),
                None,
            );
        }
    };
    let input = source::spawn_reader(std::io::stdin());
    let mut pending = Vec::new();
    loop {
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            sink.finish();
            return finish_ok(cli, cmd, label, warnings);
        }
        source::drain_input(&input, &mut pending);
        if !pending.is_empty() {
            if let Err(e) = sp.write_all(&pending) {
                sink.finish();
                return fail(
                    cli,
                    cmd,
                    ErrorKind::TransferFailed,
                    format!("write {port_path}: {e}"),
                    None,
                );
            }
            pending.clear();
        }
        match std::io::Read::read(&mut sp, &mut buf) {
            Ok(0) => {}
            Ok(n) => sink.write(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                sink.finish();
                return fail(
                    cli,
                    cmd,
                    ErrorKind::TransferFailed,
                    format!("read {port_path}: {e}"),
                    None,
                );
            }
        }
    }
}

/// `uart`: the physical UART bridge - just open the probe's CDC port, no attach needed.
fn run_uart(cli: &Cli, args: &MonitorArgs) -> ExitCode {
    const CMD: &str = "monitor";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    // No probe lock: the UART bridge is the CDC, not the debug interface, and the tty is opened
    // exclusively; a flash or gdb on the same probe may run meanwhile (docs/cli.ja.md §3.7).
    let port = match resolve_port(cli, CMD, &entry, &args.port) {
        Ok(p) => p,
        Err(c) => return c,
    };
    stream_port(cli, CMD, &port, args.baud, "uart", false, Vec::new())
}

/// en: `sdi`: attach the chip (so the LinkE knows the family / DM data address), resume the
/// core (SerialSDI only prints while running), enable forwarding, then read the CDC while the
/// session is held open. LinkE only.
/// ja: `sdi`: chip を attach(LinkE に family=DM data 番地を知らせる)→ resume → forward 有効化
/// → session を保持したまま CDC を読む。LinkE 専用。
fn run_sdi(cli: &Cli, args: &MonitorArgs) -> ExitCode {
    const CMD: &str = "monitor";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "sdi needs a RISC-V-mode LinkE",
            None,
        );
    }
    // Hold the per-probe lock while streaming so a concurrent flash/attach waits (docs §3.7).
    let _lock = match crate::cmd_probe::lock_probe(cli, CMD, &entry) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    match enable_sdi(&entry, speed, cli.chip.as_deref()) {
        Ok(w) => warnings.extend(w),
        Err(e) => return fail(cli, CMD, e.kind, e.msg, e.hint),
    }
    let port = match resolve_port(cli, CMD, &entry, &args.port) {
        Ok(p) => p,
        Err(c) => return c,
    };
    stream_port(cli, CMD, &port, args.baud, "sdi", true, warnings)
}

/// en: Open a tty as a plain blocking file (the SDI path, see [`stream_port`]) and make it
/// exclusive (TIOCEXCL) like every other serial open, so no second process reads half the bytes.
/// ja: tty を素のブロッキング file で開き(SDI の経路)、他の serial open と同じく排他(TIOCEXCL)に
/// する(2 つ目のプロセスがバイトを半分持っていかないように)。
#[cfg(unix)]
pub(crate) fn open_raw(path: &str) -> std::io::Result<std::fs::File> {
    let file = std::fs::File::open(path)?;
    rustix::termios::ioctl_tiocexcl(&file)?;
    // en: Raw line discipline: the tty keeps whatever the last user set, and in the cooked
    // default ICRNL turns the target's `\r\n` into `\n\n`. Only the termios flags change (no
    // modem lines, same speed).
    // ja: 行規律を raw に。tty は前の利用者の設定のままで、既定の cooked では ICRNL が `\r\n` を
    // `\n\n` にする。termios のフラグだけ替える(modem 線と速さは触らない)。
    let mut t = rustix::termios::tcgetattr(&file)?;
    t.make_raw();
    rustix::termios::tcsetattr(&file, rustix::termios::OptionalActions::Now, &t)?;
    Ok(file)
}

#[cfg(not(unix))]
pub(crate) fn open_raw(path: &str) -> std::io::Result<std::fs::File> {
    // Windows opens a COM port exclusively by itself.
    std::fs::File::open(path)
}

/// Why [`enable_sdi`] failed, in the terms `fail` reports.
pub(crate) struct SdiError {
    pub(crate) kind: ErrorKind,
    pub(crate) msg: String,
    pub(crate) hint: Option<&'static str>,
}

impl SdiError {
    fn new(kind: ErrorKind, msg: String) -> Self {
        Self {
            kind,
            msg,
            hint: None,
        }
    }
}

/// en: Turn on the LinkE's SDI print forwarding to its CDC (shared by `monitor --source sdi` and
/// `arduino monitor`). Returns the warnings to report.
/// ja: LinkE の SDI print の CDC への転送を有効にする(`monitor --source sdi` と `arduino monitor` が共用)。
pub(crate) fn enable_sdi(
    entry: &Entry,
    speed: ch32rv_wchlink::Speed,
    chip: Option<&str>,
) -> Result<Vec<ch32rv_contract::Warning>, SdiError> {
    // en: Minimal wlink-equivalent on a raw link (no ChipInfo read, no halt, no detach):
    // SetSpeed(placeholder) -> AttachChip (learn the family, does not halt) -> enable
    // forwarding. Then KEEP the link open while reading the CDC: dropping the nusb interface
    // mid-process resets the probe and stops forwarding, so the vendor interface is held for
    // the whole session (the CDC is a separate interface on the same device).
    // ja: raw link で最小の wlink 相当。enable 後は link を保持したまま CDC を読む(nusb interface を
    // 途中で drop すると probe がリセットされ forward が止まるため)。
    let warnings = {
        let mut link = match ch32rv_wchlink::WchLink::open(&entry.dev) {
            Ok(l) => l,
            Err(e) => return Err(SdiError::new(ErrorKind::DeviceOpenFailed, e.to_string())),
        };
        match link.probe_info() {
            Ok(info) if matches!(info.variant, ch32rv_wchlink::Variant::LinkE) => {}
            Ok(_) => {
                return Err(SdiError {
                    kind: ErrorKind::CapabilityUnsupported,
                    msg: "SDI print forwarding is only available on a WCH-LinkE".to_owned(),
                    hint: Some(
                        "use --source dmdata (host-side DMI) which works on any probe including the CH549 Link",
                    ),
                });
            }
            Err(e) => return Err(SdiError::new(ErrorKind::DeviceOpenFailed, e.to_string())),
        }
        // en: Exactly wlink's `sdi-print enable` sequence (verified by usbmon): SetSpeed(0x01)
        // -> AttachChip (learn the family; does not halt) -> SetSpeed(real family, so the LinkE
        // forwards from the right DM data address) -> enable (`ee 00`). No detach, no halt.
        // ja: wlink の `sdi-print enable` と同一手順(usbmon 確認): SetSpeed(0x01)→ AttachChip →
        // SetSpeed(実 family)→ enable(`ee 00`)。detach/halt しない。
        let _ = link.set_speed_default(speed);
        let attach = match link.attach_chip() {
            Ok(a) => a,
            Err(e) => return Err(SdiError::new(ErrorKind::AttachFailed, e.to_string())),
        };
        if let Some(requested) = chip
            && let Err(e) =
                crate::session::check_chip(&ch32rv_target::Db::builtin(), requested, &attach)
        {
            let _ = link.detach_chip();
            return Err(SdiError::new(ErrorKind::TargetAmbiguous, e.to_string()));
        }
        let _ = link.set_speed(attach.family_byte, speed);
        let warnings = vec![attach_reclocks_warning(
            "press the board's reset, or use `ch32rv run <elf> --no-flash --source dmdata|dmseq|rtt`",
        )];
        if let Err(e) = link.set_sdi_print_enabled(true) {
            return Err(SdiError::new(
                ErrorKind::TransferFailed,
                format!("enable SDI failed: {e}"),
            ));
        }
        // link drops here, releasing the vendor interface (wlink exits at this point too).
        warnings
    };
    // Give the LinkE a moment to start forwarding before the CDC is opened.
    std::thread::sleep(Duration::from_millis(200));
    Ok(warnings)
}

// ---- DMI backend (dmdata / rtt) ----

/// en: `dmdata` / `rtt`: attach, open the source, resume the core, then exchange output and stdin
/// with the target until Ctrl-C / `--duration`. Works on any probe (no CDC involved).
/// ja: `dmdata` / `rtt`: attach → source を開く → resume → 出力と stdin を交換。任意 probe で動く。
fn run_dmi(cli: &Cli, source: MonitorSource) -> ExitCode {
    const CMD: &str = "monitor";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "{} monitor needs a RISC-V-mode probe (this is {})",
                source.as_str(),
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let mut session = match Session::attach(
        &entry,
        speed,
        Duration::from_millis(1000),
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        cli.db.as_deref(),
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(e) => return crate::cmd_probe::session_error(cli, CMD, e),
    };
    warnings.push(attach_reclocks_warning(&format!(
        "`ch32rv run <elf> --no-flash --source {}` (resets it, then streams)",
        source.as_str()
    )));
    let mut src = match DmiSource::open(&mut session, source, &mut warnings) {
        Ok(s) => s,
        Err(e) => return open_error(cli, CMD, e),
    };
    if !cli.json {
        eprintln!(
            "monitor: {} via {} (Ctrl-C to stop; stdin goes to the target)",
            src.describe(),
            entry.dev.serial().unwrap_or("?")
        );
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
    }
    // Attach (and the rtt scan) leave the core halted; the sources only move while it runs.
    let _ = session.dm().resume();
    let input = source::spawn_reader(std::io::stdin());
    let mut sink = Sink::new(cli, src.name());
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let result = source::stream(&mut session, &mut src, &mut sink, &input, deadline);
    sink.finish();
    match result {
        Ok(()) => finish_ok(cli, CMD, src.name(), warnings),
        Err(e) => fail(
            cli,
            CMD,
            source::dmi_error_kind(&e),
            format!("{} stream failed: {e}", src.name()),
            None,
        ),
    }
}

/// en: The console (dmdata / dmseq / sdi) or the fixture UART of an OEP probe - or of a WCH-Link
/// whose broker runs - through the probe's broker, to stdout (NDJSON `output` events under
/// `--json`), stdin going back to the target, until `--duration` / Ctrl-C.
/// ja: OEP の probe(かブローカーの動いている WCH-Link)の console / fixture UART をブローカー経由で
/// stdout に流し、stdin を target に渡す。
fn run_oep(cli: &Cli, args: &MonitorArgs, a: &crate::oep::OepAddr) -> ExitCode {
    use crate::oep::StreamWanted;
    use ch32rv_oep::stream::Mechanism;
    const CMD: &str = "monitor";
    let wanted = match args.source {
        // No --source: the target's console, by the slot's mechanism (else dmseq).
        None => StreamWanted::ConsoleDefault,
        Some(MonitorSource::Dmdata) => StreamWanted::Console(Mechanism::Dmdata),
        Some(MonitorSource::Dmseq) => StreamWanted::Console(Mechanism::Dmseq),
        Some(MonitorSource::Sdi) => StreamWanted::Console(Mechanism::Sdi),
        Some(MonitorSource::FixtureUart) => StreamWanted::FixtureUart(args.baud),
        Some(MonitorSource::Rtt) => StreamWanted::Rtt,
        Some(MonitorSource::Uart) => {
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                "uart is not served through a broker",
                None,
            );
        }
    };
    let mut c = match crate::oep::ConsoleSession::open(a, wanted, cli.chip.as_deref()) {
        Ok(c) => c,
        Err(m) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, m, None),
    };
    if !cli.json {
        eprintln!(
            "monitor: {} through the probe's broker (Ctrl-C to stop; stdin goes to the target)",
            c.source_name()
        );
    }
    let input = source::spawn_reader(std::io::stdin());
    let mut pending = Vec::new();
    let source_name = c.source_name();
    let mut sink = Sink::new(cli, source_name);
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            sink.finish();
            return finish_ok(cli, CMD, source_name, Vec::new());
        }
        source::drain_input(&input, &mut pending);
        if !pending.is_empty() {
            match c.write(&pending) {
                Ok(n) => {
                    pending.drain(..n.min(pending.len()));
                }
                Err(m) => {
                    sink.finish();
                    return fail(cli, CMD, ErrorKind::TransferFailed, m, None);
                }
            }
        }
        match c.poll() {
            // Wait a little for output, but wake at once for input (as `arduino monitor` does).
            Ok(b) if b.is_empty() => {
                // RTT halts the hart per poll, so it goes at RTT's pace (as on a WCH-Link).
                let idle = Duration::from_millis(if c.is_rtt() { 50 } else { 5 });
                if let Ok(chunk) = input.recv_timeout(idle) {
                    pending.extend_from_slice(&chunk);
                }
            }
            Ok(b) => sink.write(&b),
            Err(m) => {
                sink.finish();
                return fail(cli, CMD, ErrorKind::TransferFailed, m, None);
            }
        }
    }
}

/// Map a source open failure to the CLI's exit vocabulary.
pub(crate) fn open_error(cli: &Cli, cmd: &str, e: OpenError) -> ExitCode {
    match e {
        OpenError::NotDmi => fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            "this source is a CDC serial one (uart/sdi), not a DMI source",
            Some("use --source dmdata, --source dmseq or --source rtt"),
        ),
        OpenError::NoControlBlock { scan_len } => fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            format!(
                "no SEGGER RTT control block in the first {scan_len} bytes of RAM (from 0x2000_0000)"
            ),
            Some("flash a SerialRTT/RTT sketch first; the block only appears after begin()"),
        ),
        OpenError::Dmi(e) => fail(
            cli,
            cmd,
            source::dmi_error_kind(&e),
            format!("open source: {e}"),
            None,
        ),
    }
}

// ---- monitor list / sdi on|off ----

fn list(cli: &Cli) -> ExitCode {
    let entries = crate::cmd_probe::wch_devices().unwrap_or_default();
    if cli.json {
        let ports: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "probe": e.dev.serial(),
                    "mode": mode_str(e.mode),
                    "cdc_ports": e.dev.serial_ports(),
                })
            })
            .collect();
        let mut env = ch32rv_contract::ResultEnvelope::success("monitor.list");
        env.result = Some(serde_json::json!({ "probes": ports }));
        crate::print_envelope(&env)
    } else {
        println!(
            "{:<16} {:<7} CDC PORTS (uart / sdi share these)",
            "PROBE", "MODE"
        );
        for e in &entries {
            println!(
                "{:<16} {:<7} {}",
                e.dev.serial().unwrap_or("-"),
                mode_str(e.mode),
                render_ports(&e.dev.serial_ports())
            );
        }
        println!("\ndmdata / rtt do not use a CDC port (host reads them over DMI).");
        ExitCode::SUCCESS
    }
}

fn render_ports(ports: &[String]) -> String {
    if ports.is_empty() {
        "-".to_owned()
    } else {
        ports.join(", ")
    }
}

fn sdi_toggle(cli: &Cli, state: SwitchState) -> ExitCode {
    const CMD: &str = "monitor.sdi";
    let entry: Entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    let mut link = match ch32rv_wchlink::WchLink::open(&entry.dev) {
        Ok(l) => l,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    let on = matches!(state, SwitchState::On);
    // The probe must know the chip family (attach first) before it can forward SDI.
    let _ = link.probe_info();
    let (speed, _) = parse::speed(&cli.speed).unwrap_or((ch32rv_wchlink::Speed::High, Vec::new()));
    let _ = link.set_speed_default(speed);
    if let Ok(attach) = link.attach_chip() {
        let _ = link.set_speed(attach.family_byte, speed);
    }
    if let Err(e) = link.set_sdi_print_enabled(on) {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("set SDI failed: {e}"),
            None,
        );
    }
    if cli.json {
        let mut env = ch32rv_contract::ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "sdi": on }));
        crate::print_envelope(&env)
    } else {
        println!(
            "SDI print forwarding {}",
            if on { "enabled" } else { "disabled" }
        );
        ExitCode::SUCCESS
    }
}
