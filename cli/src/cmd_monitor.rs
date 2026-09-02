//! en: `monitor` (docs/cli.ja.md §4.5). Two backends, per the corrected design:
//!   - CDC serial (`uart`, `sdi`): open the probe's CDC port. `sdi` first tells the LinkE to
//!     forward the target's DM data registers to that same port (LinkE only; mixes with uart).
//!   - DMI (`dmdata`, `rtt`): the host reads the target's debug registers directly while the
//!     core runs. `dmdata` polls the ch32fun/minichlink data0/data1 mailbox (SerialDMDATA).
//!
//! `rtt` finds a SEGGER-format control block in RAM (by scanning for its magic), then drains the
//! up (target->host) ring buffer. Reading arbitrary RAM and writing back the read offset both go
//! over the Debug Module, which needs the hart halted, so this backend briefly halts the core once
//! per poll (unlike probe-rs's background access, so the target's timers drift a little while a
//! monitor is attached). The loop runs until Ctrl-C or, in tests, a bounded run via `--timeout`.
//!
//! ja: `monitor`。CDC serial(uart/sdi)と DMI(dmdata/rtt)の 2 backend。設計は cli.ja.md §4.5。
//! `rtt` は RAM 内の SEGGER 形式 control block を magic 走査で見つけ up(target→host)リングを
//! 汲む。RAM 読みと read offset 書戻しは DM 経由=hart halt 要なので poll ごとに一瞬 halt する
//! (probe-rs の background access と違い target のタイマが僅かにドリフトする)。

use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ch32rv_contract::ErrorKind;
use ch32rv_contract::policy::MonitorSource;

use crate::args::{Cli, MonitorArgs, MonitorCmd, SwitchState};
use crate::cmd_probe::{Entry, fail, mode_str, select_entry};
use crate::parse;
use crate::session::Session;

pub fn monitor(cli: &Cli, args: &MonitorArgs) -> ExitCode {
    match &args.cmd {
        Some(MonitorCmd::List) => return list(cli),
        Some(MonitorCmd::Sdi { state }) => return sdi_toggle(cli, *state),
        None => {}
    }
    match args.source {
        MonitorSource::Uart => run_uart(cli, args),
        MonitorSource::Sdi => run_sdi(cli, args),
        MonitorSource::Dmdata => run_dmdata(cli, args),
        MonitorSource::Rtt => run_rtt(cli, args),
    }
}

/// How long to run before returning (test knob); None = until Ctrl-C.
fn run_duration(cli: &Cli) -> Option<Duration> {
    // Reuse the global --timeout as a bounded run length for scripting/tests.
    cli.timeout.map(Duration::from_secs)
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

/// en: Stream the probe's CDC port to stdout until the deadline / Ctrl-C.
/// `raw=true` opens the tty as a plain file (like `cat`) WITHOUT touching modem control, which
/// is required for SDI: the serialport crate asserts DTR on open and the WCH-LinkE then stops
/// forwarding SDI after one line (measured). `raw=false` uses the serialport crate so `--baud`
/// takes effect for the physical UART bridge (where DTR is harmless).
/// ja: probe の CDC を stdout へ流す。`raw=true` は tty を生ファイルで開き modem 線を触らない
/// (SDI 必須。serialport は DTR を assert して forward を止める実測)。`raw=false` は
/// serialport で baud を効かせる(物理 UART bridge 用、DTR 無害)。
fn stream_port(
    cli: &Cli,
    cmd: &str,
    port_path: &str,
    baud: u32,
    label: &str,
    raw: bool,
) -> ExitCode {
    if !cli.json {
        eprintln!("monitor: {label} on {port_path} @ {baud} baud (Ctrl-C to stop)");
    }
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let mut out = std::io::stdout().lock();
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
                break;
            }
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = out.write_all(&buf[..n]);
                    let _ = out.flush();
                }
                Err(e) => {
                    eprintln!("\nmonitor: serial error: {e}");
                    break;
                }
            }
        }
        return ExitCode::SUCCESS;
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
    loop {
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            break;
        }
        match std::io::Read::read(&mut sp, &mut buf) {
            Ok(0) => {}
            Ok(n) => {
                let _ = out.write_all(&buf[..n]);
                let _ = out.flush();
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("\nmonitor: serial error: {e}");
                break;
            }
        }
    }
    ExitCode::SUCCESS
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
    stream_port(cli, CMD, &port, args.baud, "uart", false)
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
    // en: Attach so the LinkE learns the family / DM data address; resume (attach halts the
    // core, but SerialSDI must run to print); enable forwarding; then KEEP the chip attached
    // (detaching stops forwarding - wlink's --no-detach). The session is dropped without
    // detaching, leaving the LinkE forwarding to its CDC, which we then read.
    // ja: attach で family=DM data 番地を LinkE に知らせ、resume で走らせ(attach は halt する)、
    // forward を有効化して attach を保つ(detach で forward 停止 = wlink の --no-detach)。
    let _ = &mut warnings;
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
                ErrorKind::TransportTimeout,
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
    stream_port(cli, CMD, &port, args.baud, "sdi", true)
}

// ---- DMI backend (dmdata) ----

fn run_dmdata(cli: &Cli, _args: &MonitorArgs) -> ExitCode {
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
                "dmdata monitor needs a RISC-V-mode probe (this is {})",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    // Attach WITHOUT halting - the target must keep running for the mailbox to move.
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

    if !cli.json {
        eprintln!(
            "monitor: dmdata (DMI poll, core runs) via {} (Ctrl-C to stop)",
            entry.dev.serial().unwrap_or("?")
        );
    }
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let mut out = std::io::stdout().lock();
    let mut dm = session.dm();
    // en: Attach leaves the core halted; the SerialDMDATA mailbox only moves while it runs, so
    // resume it (minichlink's `-T` resumes/reboots at terminal start too).
    // ja: attach は core を halt したままにするので resume する(minichlink -T も同様)。
    let _ = dm.resume();
    loop {
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            break;
        }
        match dm.dmdata_poll(&[]) {
            Ok(Some(bytes)) if !bytes.is_empty() => {
                let _ = out.write_all(&bytes);
                let _ = out.flush();
            }
            Ok(_) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => {
                eprintln!("\nmonitor: dmi error: {e}");
                break;
            }
        }
    }
    ExitCode::SUCCESS
}

// ---- DMI backend (rtt / SEGGER-format RAM ring buffer) ----

/// All CH32 parts map SRAM at this base.
const RTT_RAM_BASE: u32 = 0x2000_0000;
/// The control-block id string the target publishes once its RTT channel is up.
const RTT_MAGIC: &[u8] = b"SEGGER RTT";
/// Sanity cap on a ring-buffer size read out of RAM (reject a half-initialized / garbage block).
const RTT_MAX_BUF: u32 = 0x1_0000;
/// Scan length when the target's SRAM size is unknown (the `_SEGGER_RTT` block lives in early .bss).
const RTT_DEFAULT_SCAN: u32 = 8 * 1024;

/// The WCH-Link's bulk read rejects/times-out on a very large single region, so read the scan
/// window in transfers this size (8 KiB is proven to work well within the transport timeout).
const RTT_READ_CHUNK: u32 = 8192;

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Read `len` bytes of target memory into one buffer, chunked so each transfer stays small. Stops
/// early (returning what it has) if a chunk fails, so a short read still lets the scan try.
fn read_region(session: &mut Session, base: u32, len: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len as usize);
    let mut off = 0u32;
    while off < len {
        let want = RTT_READ_CHUNK.min(len - off);
        match session.link().read_memory(base + off, want) {
            Ok(mut chunk) => {
                buf.append(&mut chunk);
                off += want;
            }
            Err(_) => break,
        }
    }
    buf
}

/// Find the byte offset, within a RAM snapshot, of a SEGGER RTT control block whose up[0]
/// descriptor validates (buffer pointer in RAM, sane size, offsets in range). Validating rejects a
/// stray copy of the magic that lives in a ring buffer's own contents, not in a real block.
fn find_control_block(snap: &[u8]) -> Option<usize> {
    let mut from = 0usize;
    while let Some(rel) = snap[from..]
        .windows(RTT_MAGIC.len())
        .position(|w| w == RTT_MAGIC)
    {
        let pos = from + rel;
        // up[0] descriptor sits right after id[16] + max_up(4) + max_down(4).
        let d = pos + 24;
        if d + 24 <= snap.len() {
            let buffer = le32(snap, d + 4);
            let size = le32(snap, d + 8);
            let wr = le32(snap, d + 12);
            let rd = le32(snap, d + 16);
            if size > 0 && size <= RTT_MAX_BUF && wr < size && rd < size && buffer >= RTT_RAM_BASE {
                return Some(pos);
            }
        }
        from = pos + 1;
    }
    None
}

fn run_rtt(cli: &Cli, _args: &MonitorArgs) -> ExitCode {
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
                "rtt monitor needs a RISC-V-mode probe (this is {})",
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

    // How much RAM to scan for the control block: the target's SRAM (from the DB) or a default.
    let scan_len = {
        let db = ch32rv_target::Db::builtin();
        match db.resolve_by_chip_id(session.attach.chip_id) {
            ch32rv_target::Resolution::Sku(s) if s.sram_bytes > 0 => {
                (s.sram_bytes as u32).min(64 * 1024)
            }
            _ => RTT_DEFAULT_SCAN,
        }
    };

    // Find the control block. The target has been running its sketch since power-on, so begin()
    // has already published the block; retry a few times in case we attached very early.
    let cb_base = 'find: {
        for _ in 0..10 {
            let _ = session.dm().halt();
            let snap = read_region(&mut session, RTT_RAM_BASE, scan_len);
            if let Some(cb) = find_control_block(&snap) {
                break 'find Some(RTT_RAM_BASE + cb as u32);
            }
            let _ = session.dm().resume();
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    };
    let cb_base = match cb_base {
        Some(b) => b,
        None => {
            let _ = session.dm().resume();
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                format!(
                    "no SEGGER RTT control block in the first {scan_len} bytes of RAM (from 0x{RTT_RAM_BASE:08x})"
                ),
                Some("flash a SerialRTT/RTT sketch first; the block only appears after begin()"),
            );
        }
    };

    if !cli.json {
        eprintln!(
            "monitor: rtt (RAM ring @ 0x{cb_base:08x}, core briefly halts per poll) via {} (Ctrl-C to stop)",
            entry.dev.serial().unwrap_or("?")
        );
    }

    // up[0] descriptor: control block is id[16] + max_up(4) + max_down(4), then up[0].
    let up = cb_base + 24;
    let deadline = run_duration(cli).map(|d| Instant::now() + d);
    let mut out = std::io::stdout().lock();
    loop {
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            break;
        }
        let _ = session.dm().halt();
        // up[0]: name(+0), buffer(+4), size(+8), write_off(+12), read_off(+16), flags(+20).
        let desc = match session.link().read_memory(up, 24) {
            Ok(d) if d.len() >= 24 => d,
            _ => {
                let _ = session.dm().resume();
                break;
            }
        };
        let buffer = le32(&desc, 4);
        let size = le32(&desc, 8);
        let wr = le32(&desc, 12);
        let rd = le32(&desc, 16);
        if size == 0 || size > RTT_MAX_BUF || wr >= size || rd >= size || buffer < RTT_RAM_BASE {
            // Not ready or garbage; let it run and retry.
            let _ = session.dm().resume();
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if wr != rd {
            let bytes = if wr > rd {
                session.link().read_memory(buffer + rd, wr - rd).ok()
            } else {
                // Wrapped: [rd, size) then [0, wr).
                let mut v = Vec::new();
                if let Ok(a) = session.link().read_memory(buffer + rd, size - rd) {
                    v.extend_from_slice(&a);
                }
                if let Ok(b) = session.link().read_memory(buffer, wr) {
                    v.extend_from_slice(&b);
                }
                Some(v)
            };
            if let Some(bytes) = bytes {
                let _ = out.write_all(&bytes);
                let _ = out.flush();
            }
            // Tell the target we drained: up[0].read_off (at up + 16) = write_off.
            let _ = session.dm().write_mem32(up + 16, wr);
        }
        let _ = session.dm().resume();
        std::thread::sleep(Duration::from_millis(50));
    }
    ExitCode::SUCCESS
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
            ErrorKind::TransportTimeout,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a RAM snapshot with a valid control block whose magic starts at `at`.
    fn snapshot_with_cb(at: usize, buffer: u32, size: u32, wr: u32, rd: u32) -> Vec<u8> {
        let mut ram = vec![0u8; at + 24 + 24 + 8];
        ram[at..at + RTT_MAGIC.len()].copy_from_slice(RTT_MAGIC);
        let d = at + 24; // up[0] descriptor
        ram[d + 4..d + 8].copy_from_slice(&buffer.to_le_bytes());
        ram[d + 8..d + 12].copy_from_slice(&size.to_le_bytes());
        ram[d + 12..d + 16].copy_from_slice(&wr.to_le_bytes());
        ram[d + 16..d + 20].copy_from_slice(&rd.to_le_bytes());
        ram
    }

    #[test]
    fn finds_valid_control_block() {
        let ram = snapshot_with_cb(64, RTT_RAM_BASE + 0x100, 256, 10, 0);
        assert_eq!(find_control_block(&ram), Some(64));
    }

    #[test]
    fn skips_magic_with_bogus_descriptor() {
        // A stray "SEGGER RTT" in buffer contents: the descriptor after it is garbage (size huge),
        // so it must not be mistaken for a real block.
        let ram = snapshot_with_cb(64, 0, 0xFFFF_FFFF, 0, 0);
        assert_eq!(find_control_block(&ram), None);
    }

    #[test]
    fn no_magic_returns_none() {
        assert_eq!(find_control_block(&[0u8; 64]), None);
    }

    #[test]
    fn le32_reads_little_endian() {
        assert_eq!(le32(&[0x78, 0x56, 0x34, 0x12], 0), 0x1234_5678);
    }
}
