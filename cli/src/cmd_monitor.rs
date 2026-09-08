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
    match args.source {
        MonitorSource::Uart => run_uart(cli, args),
        MonitorSource::Sdi => run_sdi(cli, args),
        MonitorSource::Dmdata | MonitorSource::Rtt => run_dmi(cli, args.source),
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
    }
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let mut sink = Sink::new(cli, label);
    let mut buf = [0u8; 512];

    #[cfg(unix)]
    if raw {
        use std::io::Read;
        // en: Plain blocking open, exactly like `cat`. Opening with O_NONBLOCK, or via the
        // serialport crate (which asserts DTR), makes the WCH-LinkE stop forwarding SDI after
        // one line (measured); this blocking file read keeps it streaming. The deadline is
        // checked after each returned chunk (real use ends with Ctrl-C).
        // ja: cat と同じ素のブロッキング open。O_NONBLOCK や serialport(DTR assert)だと LinkE の
        // SDI forward が 1 行で止まる(実測)。ブロッキング読みなら流れ続ける。
        let mut file = match std::fs::File::open(port_path) {
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
        loop {
            if let Some(dl) = deadline
                && Instant::now() >= dl
            {
                sink.finish();
                return finish_ok(cli, cmd, label, warnings);
            }
            match file.read(&mut buf) {
                Ok(0) => {
                    sink.finish();
                    return fail(
                        cli,
                        cmd,
                        ErrorKind::DeviceNotFound,
                        format!("{port_path} closed (probe disconnected?)"),
                        None,
                    );
                }
                Ok(n) => sink.write(&buf[..n]),
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
    // Hold the per-probe lock while streaming so a concurrent flash/attach waits (docs §3.7).
    let _lock = match crate::cmd_probe::lock_probe(cli, CMD, &entry) {
        Ok(l) => l,
        Err(c) => return c,
    };
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
    let (speed, warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    // en: Minimal wlink-equivalent on a raw link (no ChipInfo read, no halt, no detach):
    // SetSpeed(placeholder) -> AttachChip (learn the family, does not halt) -> enable
    // forwarding. Then KEEP the link open while reading the CDC: dropping the nusb interface
    // mid-process resets the probe and stops forwarding, so the vendor interface is held for
    // the whole session (the CDC is a separate interface on the same device).
    // ja: raw link で最小の wlink 相当。enable 後は link を保持したまま CDC を読む(nusb interface を
    // 途中で drop すると probe がリセットされ forward が止まるため)。
    {
        let mut link = match ch32rv_wchlink::WchLink::open(&entry.dev) {
            Ok(l) => l,
            Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
        };
        match link.probe_info() {
            Ok(info) if matches!(info.variant, ch32rv_wchlink::Variant::LinkE) => {}
            Ok(_) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    "SDI print forwarding is only available on a WCH-LinkE",
                    Some(
                        "use --source dmdata (host-side DMI) which works on any probe including the CH549 Link",
                    ),
                );
            }
            Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
        }
        // en: Exactly wlink's `sdi-print enable` sequence (verified by usbmon): SetSpeed(0x01)
        // -> AttachChip (learn the family; does not halt) -> SetSpeed(real family, so the LinkE
        // forwards from the right DM data address) -> enable (`ee 00`). No detach, no halt.
        // ja: wlink の `sdi-print enable` と同一手順(usbmon 確認): SetSpeed(0x01)→ AttachChip →
        // SetSpeed(実 family)→ enable(`ee 00`)。detach/halt しない。
        let _ = link.set_speed_default(speed);
        let attach = match link.attach_chip() {
            Ok(a) => a,
            Err(e) => return fail(cli, CMD, ErrorKind::AttachFailed, e.to_string(), None),
        };
        let _ = link.set_speed(attach.family_byte, speed);
        if let Err(e) = link.set_sdi_print_enabled(true) {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("enable SDI failed: {e}"),
                None,
            );
        }
        // link drops here, releasing the vendor interface (wlink exits at this point too).
    }
    std::thread::sleep(Duration::from_millis(200));
    let port = match resolve_port(cli, CMD, &entry, &args.port) {
        Ok(p) => p,
        Err(c) => return c,
    };
    // en: KNOWN LIMITATION (2026-09-01): the enable command succeeds and the core runs, but
    // in-process SDI forwarding to the CDC does not activate the way it does under the wlink
    // binary (same command bytes). This needs a usbmon capture to diff the sequences.
    // `dmdata` is the working, probe-agnostic alternative; `wlink sdi-print enable` also works.
    // ja: 既知の制約(2026-09-01): enable は成功し core も走るが、in-process では CDC への SDI
    // forward が起動しない(wlink バイナリと同一バイトなのに)。usbmon で差分要調査。当面は
    // dmdata(任意 probe で動作)を使う。
    if !cli.json {
        eprintln!(
            "note: sdi CDC forwarding is not yet reliable from ch32rv; if nothing appears, use \
             `--source dmdata` (SerialDMDATA) or `wlink sdi-print enable`."
        );
    }
    stream_port(cli, CMD, &port, args.baud, "sdi", true, warnings)
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
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(e) => return crate::cmd_probe::session_error(cli, CMD, e),
    };
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

/// Map a source open failure to the CLI's exit vocabulary.
pub(crate) fn open_error(cli: &Cli, cmd: &str, e: OpenError) -> ExitCode {
    match e {
        OpenError::NotDmi => fail(
            cli,
            cmd,
            ErrorKind::CapabilityUnsupported,
            "this source is a CDC serial one (uart/sdi), not a DMI source",
            Some("use --source dmdata or --source rtt"),
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
