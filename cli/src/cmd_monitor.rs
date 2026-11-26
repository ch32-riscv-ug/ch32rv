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
use std::sync::atomic::{AtomicBool, Ordering};
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
    if args.source == Some(MonitorSource::Auto) {
        return run_auto(cli, args, addr);
    }
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
                    "leave --source out for the target's console (dmseq), or name one: dmseq / dmdata / sdi, rtt, or fixture-uart (the fixture's \
                     UART)",
                ),
            );
        }
        (Some(a), _) => return run_oep(cli, args, a),
    }
    let source = args.source.unwrap_or(MonitorSource::Uart);
    match source {
        MonitorSource::Uart => run_uart(cli, args),
        MonitorSource::Dmdata | MonitorSource::Dmseq | MonitorSource::Sdi | MonitorSource::Rtt => {
            run_dmi(cli, source)
        }
        MonitorSource::FixtureUart => fail(
            cli,
            "monitor",
            ErrorKind::CapabilityUnsupported,
            "fixture-uart is an OEP probe's source, served by `arduino monitor`",
            None,
        ),
        MonitorSource::Auto => run_auto(cli, args, None),
    }
}

/// en: `--source auto` (`crate::auto_console`): a WCH-Link's CDC (`--port`, else the Link's own)
/// and debug paths after a reset, or an OEP probe's fixture UART and consoles, merged to stdout
/// (NDJSON `output` events under `--json`); stdin goes to the path the output last came from.
/// ja: `--source auto`。WCH-Link は CDC(`--port`、無ければ Link のもの)と reset 後のデバッグの経路、
/// OEP は fixture の UART と console を 1 本で stdout へ。stdin は出力が最後に来た経路へ。
fn run_auto(cli: &Cli, args: &MonitorArgs, addr: Option<crate::oep::OepAddr>) -> ExitCode {
    use crate::auto_console::Auto;
    const CMD: &str = "monitor";
    let (speed, warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let opened = match addr {
        Some(a) if !matches!(a, crate::oep::OepAddr::Wch(_)) => Auto::open_oep(
            &a,
            cli.chip.as_deref(),
            Some(crate::oep::speed_hz(speed)),
            args.baud,
        ),
        _ => {
            let entry = match select_entry(cli, CMD) {
                Ok(e) => e,
                Err(c) => return c,
            };
            let path = args
                .port
                .clone()
                .or_else(|| entry.dev.serial_ports().into_iter().next());
            let port = path.and_then(|p| {
                serialport::new(&p, args.baud)
                    .timeout(Duration::from_millis(1))
                    .open()
                    .ok()
            });
            Auto::open_wch(
                &entry,
                port,
                speed,
                Duration::from_secs(cli.lock_timeout),
                cli.chip.as_deref(),
            )
        }
    };
    let mut auto = match opened {
        Ok(a) => a,
        Err(m) => return fail(cli, CMD, ErrorKind::TargetNoResponse, m, None),
    };
    let mut warnings = warnings;
    warnings.append(&mut auto.warnings);
    if !cli.json {
        eprintln!(
            "monitor: auto (Ctrl-C to stop; stdin goes to the path the output last came from)"
        );
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
    }
    let input = source::spawn_reader(std::io::stdin());
    let mut sink = Sink::new(cli, "auto");
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let mut pending = Vec::new();
    let result = loop {
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            break Ok(());
        }
        source::drain_input(&input, &mut pending);
        auto.input(&pending);
        pending.clear();
        match auto.poll() {
            Ok(out) if out.is_empty() => std::thread::sleep(auto.idle()),
            Ok(out) => sink.write(&out),
            Err(e) => break Err(e),
        }
    };
    sink.finish();
    match result {
        Ok(()) => finish_ok(cli, CMD, "auto", warnings),
        Err(m) => fail(cli, CMD, ErrorKind::TransferFailed, m, None),
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
/// while a V003 attach touches no RCC at all (2026-09-25, wch-protocols `protocols/pc-to-link.ja.md#quirks`) - so
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

/// en: Stream the probe's CDC port until the deadline / Ctrl-C, through the serialport crate so
/// that `--baud` takes effect, with stdin going to the port.
/// ja: probe の CDC を流す(serialport で baud を効かせ、stdin を port へ)。
fn stream_port(
    cli: &Cli,
    cmd: &str,
    port_path: &str,
    baud: u32,
    label: &'static str,
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
        if deadline.is_some_and(|dl| Instant::now() >= dl) || INTERRUPTED.load(Ordering::SeqCst) {
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
    stream_port(cli, CMD, &port, args.baud, "uart", Vec::new())
}

/// en: Set by the Ctrl-C / SIGTERM handler: the stream ends as at `--duration`.
/// ja: Ctrl-C / SIGTERM の handler が立てる。stream は `--duration` と同じく終わる。
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// en: Empty a WCH-LinkE's CDC buffer: a change of the line rate makes the LinkE drop what it
/// holds for the host (setting the same rate again does not), so switch to another rate and
/// back. Measured on a LinkE 2.22: the old SDI output stopped replacing the UART's bytes.
/// ja: WCH-LinkE の CDC の buffer を空にする。line rate を変えると LinkE は host 向けに持っている
/// ものを捨てる(同じ rate の設定では捨てない)ので、別の rate にして戻す。LinkE 2.22 で実測。
#[cfg(unix)]
pub(crate) fn clear_linke_cdc(cdc: &std::fs::File) -> std::io::Result<()> {
    use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};
    let was = tcgetattr(cdc)?;
    let mut other = was.clone();
    other.set_speed(if was.output_speed() == 9600 {
        19200
    } else {
        9600
    })?;
    tcsetattr(cdc, OptionalActions::Now, &other)?;
    std::thread::sleep(Duration::from_millis(50));
    tcsetattr(cdc, OptionalActions::Now, &was)?;
    Ok(())
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

/// en: One DMI read (dmstatus) between the attach and the SDI enable. Without it, a LinkE 2.22
/// on a running CH32V307VCT6 took the enable (answer `00`) but about every other time did not
/// forward: it printed "Please check the SDI, and Enable this function through the upper
/// computer software" on its CDC instead. With the read, 16 of 16 enables forwarded (a 50 ms
/// sleep in its place did not help). wlink's sequence has no such read; on V003 / V203 / L103 it
/// is not needed, and a read changes nothing there.
/// ja: attach と SDI の有効化の間に DMI の読み取り(dmstatus)を 1 回挟む。挟まないと LinkE 2.22 +
/// 走っている CH32V307VCT6 で、有効化は `00` で通るのに 2 回に 1 回ほど転送せず、CDC に
/// "Please check the SDI, ..." を出した。挟むと 16 回中 16 回転送した(50 ms 待つだけでは直らない)。
fn settle_before_sdi_enable(link: &mut ch32rv_wchlink::WchLink) {
    // dmstatus (Debug Spec 0x11).
    let _ = ch32rv_dmi::DtmAccess::dmi_read(link, 0x11);
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
    // en: CH32V103: the attach overwrote s1, so restart the program before watching it (the
    // session's warning says so). ja: V103 は attach が s1 を壊すので、見る前に起動し直す。
    session.keep_running();
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
        // No --source: the target's console, by dmseq.
        None => StreamWanted::ConsoleDefault,
        Some(MonitorSource::Dmdata) => StreamWanted::Console(Mechanism::Dmdata),
        Some(MonitorSource::Dmseq) => StreamWanted::Console(Mechanism::Dmseq),
        Some(MonitorSource::Sdi) => StreamWanted::Console(Mechanism::Sdi),
        Some(MonitorSource::FixtureUart) => StreamWanted::FixtureUart(args.baud),
        Some(MonitorSource::Rtt) => StreamWanted::Rtt,
        Some(MonitorSource::Auto) => unreachable!("auto runs in run_auto"),
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
    let max_speed_hz = match crate::parse::speed(&cli.speed) {
        Ok((s, _)) => Some(crate::oep::speed_hz(s)),
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let mut c = match crate::oep::ConsoleSession::open(a, wanted, cli.chip.as_deref(), max_speed_hz)
    {
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
        OpenError::NoControlBlock { window } => fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            format!(
                "no SEGGER RTT control block in {} bytes of RAM from 0x{:08x}",
                window.len, window.base
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
        // CH32V103 / V205 / H417 with only core 0 running: the attach overwrote a GPR; restart the
        // program so it does not fault. Otherwise resume the H417 harts that ran.
        let ran = crate::session::harts_that_ran(&mut link, attach.family_byte);
        if crate::session::attach_corrupted_register_in(attach.family_byte, attach.chip_id, ran)
            .is_some()
        {
            let _ = crate::cmd_flash::soft_reset_and_run_link(&mut link);
            let _ = link.select_hart(0);
        } else {
            crate::session::resume_running_harts(&mut link, attach.family_byte);
        }
    }
    if on {
        settle_before_sdi_enable(&mut link);
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
    // en: Off also empties the CDC buffer. Left on with nobody reading the CDC, the LinkE keeps
    // pulling the target's SDI output into the buffer its UART bridge sends from, and later sends
    // that old output in place of the UART's bytes (a WCH-LinkE 2.22 + CH32V203, 2026-11-12: only
    // a probe power cycle cleared it before this); a line-rate change ([`clear_linke_cdc`]) empties
    // it.
    // ja: off は CDC の buffer も空にする。CDC を誰も読まないまま転送を有効にしておくと、LinkE は
    // target の SDI 出力を UART bridge の buffer に溜め続け、あとで UART の byte の代わりに送る
    // (LinkE 2.22 + V203)。line rate の変更で消える。
    let cleared = if on {
        None
    } else {
        Some(match entry.dev.serial_ports().into_iter().next() {
            Some(port) => clear_cdc_port(&port).map_err(|e| format!("{port}: {e}")),
            None => Err("no CDC serial port found for this probe".to_owned()),
        })
    };
    if let Some(Err(why)) = &cleared {
        crate::warn(
            "cdc-not-cleared",
            format!(
                "SDI forwarding is off, but the LinkE's CDC buffer was not emptied ({why}); old SDI output may still come out of the UART port: close what holds the port and run this again"
            ),
        );
    }
    let cdc_cleared = cleared.as_ref().map(Result::is_ok);
    if cli.json {
        let mut env = ch32rv_contract::ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "sdi": on, "cdc_cleared": cdc_cleared }));
        crate::print_envelope(&env)
    } else {
        println!(
            "SDI print forwarding {}{}",
            if on { "enabled" } else { "disabled" },
            if cdc_cleared == Some(true) {
                "; CDC buffer emptied"
            } else {
                ""
            }
        );
        ExitCode::SUCCESS
    }
}

/// en: Open the probe's CDC and empty the LinkE's buffer behind it ([`clear_linke_cdc`]). Fails
/// when something else holds the port open (it is exclusive).
/// ja: probe の CDC を開き、その裏の LinkE の buffer を空にする。他が開いていれば失敗する(排他)。
fn clear_cdc_port(path: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        let cdc = open_raw(path).map_err(|e| e.to_string())?;
        clear_linke_cdc(&cdc).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        // en: The open sets one rate and the second call another, which is the change the LinkE
        // needs; 115200 is the bridge's usual rate.
        // ja: open で 1 つの rate、次の呼び出しで別の rate にする(LinkE に要る変更)。115200 は
        // bridge の通常の rate。
        let mut port = serialport::new(path, 9600)
            .open()
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(50));
        port.set_baud_rate(115_200).map_err(|e| e.to_string())
    }
}
