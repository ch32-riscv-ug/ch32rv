//! en: `isp` (docs/cli.ja.md §4.7): the chip's factory ISP bootloader over USB. `list` / `info` /
//! `flash` / `verify` / `erase` / `reset`; the rest of the namespace stays reserved.
//! ja: `isp`。チップの factory ISP bootloader を USB で使う。

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ch32rv_contract::policy::ImageFormat;
use ch32rv_contract::{ErrorKind, ResultEnvelope};
use ch32rv_isp::{Identity, IspConfig, IspDevice, IspError};
use ch32rv_usb::UsbDeviceInfo;

use ch32rv_dmi::{DmiError, ResetMode, TargetAccess};

use crate::args::{
    Cli, IspArgs, IspCmd, IspEntry, IspErase, IspReset, IspTransport, IspVerify, IspVia,
};
use crate::cmd_probe::fail;

const CODE_FLASH_START: u32 = 0x0800_0000;

pub fn isp(cli: &Cli, args: &IspArgs) -> ExitCode {
    let cmd = crate::canonical_name(&cli.command);
    if args.transport == IspTransport::Uart {
        return crate::unimplemented_cmd(cli, "isp --transport uart");
    }
    match &args.cmd {
        IspCmd::List => list(cli),
        IspCmd::Info => info(cli, args),
        IspCmd::Flash {
            file,
            erase,
            verify,
            reset,
        } => flash(cli, args, file, *erase, *verify, *reset),
        IspCmd::Verify { file } => verify_only(cli, args, file),
        IspCmd::Erase => erase_only(cli, args),
        IspCmd::Reset => reset(cli, args),
        IspCmd::Enter { via: IspVia::Probe } => enter(cli, args),
        _ => crate::unimplemented_cmd(cli, cmd),
    }
}

/// A connected bootloader with what it said about itself.
struct Opened {
    dev: IspDevice,
    usb: String,
    topology: String,
    id: Identity,
    cfg: IspConfig,
}

/// en: Pick the one ISP device (`--device usb:<bus>-<ports>`, or the only one present), waiting
/// up to `--wait` for it to appear, then Identify and ReadConfig it.
/// ja: ISP device を 1 台選び(`--device usb:<位置>` か唯一の 1 台)、`--wait` まで待ち、Identify と
/// ReadConfig をする。
fn open(cli: &Cli, cmd: &str, args: &IspArgs) -> Result<Opened, ExitCode> {
    open_with(cli, cmd, args, args.entry)
}

fn open_with(cli: &Cli, cmd: &str, args: &IspArgs, entry: IspEntry) -> Result<Opened, ExitCode> {
    let want = match args.device.as_deref() {
        None => None,
        Some(d) => match d.strip_prefix("usb:") {
            Some(t) => Some(t.to_owned()),
            None => {
                return Err(fail(
                    cli,
                    cmd,
                    ErrorKind::Usage,
                    format!("invalid --device `{d}`"),
                    Some("ISP devices carry no serial: use --device usb:<bus>-<ports>"),
                ));
            }
        },
    };
    let present = |want: &Option<String>| {
        ch32rv_isp::list().is_ok_and(|v| {
            v.iter()
                .any(|d| want.as_deref().is_none_or(|t| d.topology() == t))
        })
    };
    let mut wait = Duration::from_secs(args.wait);
    let mut entered = false;
    if entry == IspEntry::Probe && !ch32rv_usb::replay::active() && !present(&want) {
        enter_via_probe(cli, cmd)?;
        entered = true;
        wait = wait.max(ENTER_WAIT);
    }
    let deadline = Instant::now() + wait;
    let mut told = false;
    let dev = loop {
        let found = match ch32rv_isp::list() {
            Ok(v) => v,
            Err(e) => {
                return Err(fail(
                    cli,
                    cmd,
                    ErrorKind::DeviceOpenFailed,
                    e.to_string(),
                    None,
                ));
            }
        };
        let mut found: Vec<UsbDeviceInfo> = found
            .into_iter()
            .filter(|d| want.as_deref().is_none_or(|t| d.topology() == t))
            .collect();
        if found.len() > 1 {
            let at: Vec<serde_json::Value> = found
                .iter()
                .map(|d| serde_json::json!(format!("usb:{}", d.topology())))
                .collect();
            return Err(crate::cmd_probe::fail_with_candidates(
                cli,
                cmd,
                ErrorKind::DeviceAmbiguous,
                format!("{} ISP devices are connected", found.len()),
                Some("pick one with --device usb:<bus>-<ports>"),
                at,
            ));
        }
        if let Some(d) = found.pop() {
            break d;
        }
        if ch32rv_usb::replay::active() || Instant::now() >= deadline {
            return Err(fail(
                cli,
                cmd,
                ErrorKind::DeviceNotFound,
                match &want {
                    Some(t) => format!("no ISP device at usb:{t}"),
                    None => "no ISP device (1a86:55e0 / 4348:55e0) was found".to_owned(),
                },
                Some(ENTRY_HINT),
            ));
        }
        // en: After an entry over the debug wire the device is simply on its way; otherwise say
        // how to get it there, once (also under `--progress none`: an IDE shows only stderr).
        // ja: debug 線で入れた後は来るのを待つだけ。それ以外は入り方を 1 回出す。
        if !told && !entered {
            told = true;
            eprintln!(
                "waiting up to {} s for an ISP device (1a86:55e0) ...",
                wait.as_secs()
            );
            eprintln!("  {ENTRY_HINT}");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if told {
        eprintln!("ISP device found at usb:{}", dev.topology());
    }
    let usb = dev.usb_id();
    let topology = dev.topology();
    let mut isp = IspDevice::open(&dev).map_err(|e| isp_fail(cli, cmd, e))?;
    if let Some(s) = cli.timeout {
        isp.set_timeout(Duration::from_secs(s));
    }
    let id = isp.identify().map_err(|e| isp_fail(cli, cmd, e))?;
    let cfg = isp.read_config().map_err(|e| isp_fail(cli, cmd, e))?;
    Ok(Opened {
        dev: isp,
        usb,
        topology,
        id,
        cfg,
    })
}

const ENTRY_HINT: &str = "put the chip in its bootloader: CH32X03x - plug in USB with the BOOT/DOWNLOAD key (PC17 = D+ high) held, or `--entry probe` over a debug probe; BOOT0 parts - BOOT0 high at reset";

fn isp_fail(cli: &Cli, cmd: &str, e: IspError) -> ExitCode {
    let (kind, hint) = match &e {
        IspError::Usb(ch32rv_usb::UsbError::Open(_)) => (
            ErrorKind::DeviceOpenFailed,
            Some("check permissions for the device (udev rule for 1a86:55e0 / 4348:55e0)"),
        ),
        IspError::Usb(_) | IspError::ShortWrite { .. } | IspError::UnexpectedReply { .. } => {
            (ErrorKind::TransferFailed, None)
        }
        IspError::NotFactoryIsp(_) => (
            ErrorKind::CapabilityUnsupported,
            Some("a WCH-Link in IAP mode uses the same USB ID: see `ch32rv probe firmware`"),
        ),
        IspError::KeyMismatch { .. } | IspError::Status { .. } | IspError::NoKey => {
            (ErrorKind::TransferFailed, None)
        }
        IspError::VerifyMismatch(_) => (ErrorKind::VerifyMismatch, None),
        IspError::ImageTooLarge { .. } => (ErrorKind::Usage, None),
    };
    fail(cli, cmd, kind, e.to_string(), hint)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn series(id: Identity) -> String {
    ch32rv_isp::series_name(id.device_type).map_or_else(
        || format!("unknown (0x{:02x})", id.device_type),
        str::to_owned,
    )
}

fn describe(o: &Opened) -> serde_json::Value {
    serde_json::json!({
        "usb": o.usb,
        "topology": o.topology,
        "series": ch32rv_isp::series_name(o.id.device_type),
        "chip_id": format!("0x{:02x}", o.id.chip_id),
        "device_type": format!("0x{:02x}", o.id.device_type),
        "btver": o.cfg.btver_text(),
        "uid": hex(&o.cfg.uid),
        "read_protected": o.cfg.read_protected(),
        "config": hex(&o.cfg.config),
        "flash_bytes": ch32rv_isp::flash_size(o.id),
    })
}

fn list(cli: &Cli) -> ExitCode {
    const CMD: &str = "isp.list";
    let devs = match ch32rv_isp::list() {
        Ok(v) => v,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    let mut rows = Vec::new();
    for d in &devs {
        // Ask each to Identify: a factory bootloader answers, a WCH-Link in IAP mode does not.
        let answer = IspDevice::open(d).and_then(|mut i| {
            i.set_timeout(Duration::from_secs(1));
            i.identify()
        });
        rows.push(match answer {
            Ok(id) => serde_json::json!({
                "usb": d.usb_id(), "topology": d.topology(), "kind": "factory-isp",
                "series": ch32rv_isp::series_name(id.device_type),
                "chip_id": format!("0x{:02x}", id.chip_id),
                "device_type": format!("0x{:02x}", id.device_type),
            }),
            Err(e) => serde_json::json!({
                "usb": d.usb_id(), "topology": d.topology(), "kind": "unknown",
                "error": e.to_string(),
            }),
        });
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "devices": rows }));
        return crate::print_envelope(&env);
    }
    if rows.is_empty() {
        println!("no ISP device");
    }
    for r in &rows {
        let s = |k: &str| r[k].as_str().unwrap_or("-").to_owned();
        if s("kind") == "factory-isp" {
            println!(
                "usb:{:<14} {}  {} chip 0x{} type {}",
                s("topology"),
                s("usb"),
                s("series"),
                s("chip_id").trim_start_matches("0x"),
                s("device_type")
            );
        } else {
            println!(
                "usb:{:<14} {}  not a factory bootloader ({})",
                s("topology"),
                s("usb"),
                s("error")
            );
        }
    }
    ExitCode::SUCCESS
}

fn info(cli: &Cli, args: &IspArgs) -> ExitCode {
    const CMD: &str = "isp.info";
    let o = match open(cli, CMD, args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(describe(&o));
        return crate::print_envelope(&env);
    }
    println!("usb:      {} at usb:{}", o.usb, o.topology);
    println!(
        "chip:     {} (chip 0x{:02x}, type 0x{:02x})",
        series(o.id),
        o.id.chip_id,
        o.id.device_type
    );
    if let Some(n) = ch32rv_isp::flash_size(o.id) {
        println!("flash:    {} KiB", n / 1024);
    }
    println!("btver:    {}", o.cfg.btver_text());
    println!("uid:      {}", hex(&o.cfg.uid));
    println!(
        "protect:  {}",
        if o.cfg.read_protected() {
            "read-protected"
        } else {
            "off"
        }
    );
    println!("config:   {}", hex(&o.cfg.config));
    ExitCode::SUCCESS
}

/// en: The image as one block from the start of code flash (`0xff` in gaps), for the bootloader,
/// which addresses flash by offset. ja: code flash 先頭からの 1 塊にした image。
fn load_image(cli: &Cli, cmd: &str, file: &std::path::Path) -> Result<Vec<u8>, ExitCode> {
    let bytes = std::fs::read(file).map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::Usage,
            format!("read {}: {e}", file.display()),
            None,
        )
    })?;
    let image =
        crate::cmd_flash::parse_image(&bytes, ImageFormat::Auto, file, None, CODE_FLASH_START)
            .map_err(|e| fail(cli, cmd, ErrorKind::Usage, e.to_string(), None))?;
    let mut flat = Vec::new();
    for s in &image.segments {
        // Low alias (0x0000_0000) and the physical address both count from the start of flash.
        let base = if s.addr >= CODE_FLASH_START {
            s.addr - CODE_FLASH_START
        } else {
            s.addr
        };
        if base >= 0x0010_0000 {
            return Err(fail(
                cli,
                cmd,
                ErrorKind::Usage,
                format!(
                    "segment at 0x{:08x} is outside code flash; the ISP route writes code flash only",
                    s.addr
                ),
                None,
            ));
        }
        let end = base as usize + s.data.len();
        if flat.len() < end {
            flat.resize(end, 0xff);
        }
        flat[base as usize..end].copy_from_slice(&s.data);
    }
    if flat.is_empty() {
        return Err(fail(cli, cmd, ErrorKind::Usage, "image is empty", None));
    }
    Ok(flat)
}

fn check_size(cli: &Cli, cmd: &str, o: &Opened, image: &[u8]) -> Result<(), ExitCode> {
    match ch32rv_isp::flash_size(o.id) {
        Some(limit) if image.len() > limit => Err(isp_fail(
            cli,
            cmd,
            IspError::ImageTooLarge {
                actual: image.len(),
                limit,
            },
        )),
        _ => Ok(()),
    }
}

fn progress(cli: &Cli, phase: &'static str, total: usize) -> impl FnMut(usize) {
    let show =
        !cli.json && cli.progress.is_none() && std::io::IsTerminal::is_terminal(&std::io::stderr());
    let mut last = 0usize;
    move |done| {
        if show && (done - last >= 4096 || done == total) {
            last = done;
            eprint!("\r{phase}: {done}/{total} bytes");
            if done == total {
                eprintln!();
            }
        }
    }
}

fn flash(
    cli: &Cli,
    args: &IspArgs,
    file: &std::path::Path,
    erase: IspErase,
    verify: IspVerify,
    reset: IspReset,
) -> ExitCode {
    const CMD: &str = "isp.flash";
    let image = match load_image(cli, CMD, file) {
        Ok(i) => i,
        Err(c) => return c,
    };
    let mut o = match open(cli, CMD, args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if let Err(c) = check_size(cli, CMD, &o, &image) {
        return c;
    }
    let cfg = o.cfg.clone();
    if let Err(e) = o.dev.set_key(&cfg) {
        return isp_fail(cli, CMD, e);
    }
    if erase == IspErase::Auto
        && let Err(e) = o.dev.erase(ch32rv_isp::sectors_for(image.len()))
    {
        return isp_fail(cli, CMD, e);
    }
    if let Err(e) = o.dev.program(&image, progress(cli, "program", image.len())) {
        return isp_fail(cli, CMD, e);
    }
    let verified = verify == IspVerify::On;
    if verified && let Err(e) = o.dev.verify(&image, progress(cli, "verify", image.len())) {
        return isp_fail(cli, CMD, e);
    }
    let running = reset == IspReset::Run;
    let summary = describe(&o);
    if let Err(e) = o.dev.end(running) {
        return isp_fail(cli, CMD, e);
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "flash": {
                "bytes": image.len(),
                "programmer": "isp",
                "usb": summary["usb"],
                "chip_id": summary["chip_id"],
                "erased_sectors": (erase == IspErase::Auto).then(|| ch32rv_isp::sectors_for(image.len()).max(8)),
                "verified": verified.then_some(true),
                "running": running.then_some(true),
                "skipped": false,
                "scope": "image",
            },
            "device": summary,
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "flashed {} byte(s) through the ISP bootloader ({} at usb:{}){}{}",
            image.len(),
            series(o.id),
            o.topology,
            if verified { ", verified" } else { "" },
            if running {
                ", application started"
            } else {
                ", left in the bootloader"
            }
        );
        ExitCode::SUCCESS
    }
}

fn verify_only(cli: &Cli, args: &IspArgs, file: &std::path::Path) -> ExitCode {
    const CMD: &str = "isp.verify";
    let image = match load_image(cli, CMD, file) {
        Ok(i) => i,
        Err(c) => return c,
    };
    let mut o = match open(cli, CMD, args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if let Err(c) = check_size(cli, CMD, &o, &image) {
        return c;
    }
    let cfg = o.cfg.clone();
    if let Err(e) = o.dev.set_key(&cfg) {
        return isp_fail(cli, CMD, e);
    }
    if let Err(e) = o.dev.verify(&image, progress(cli, "verify", image.len())) {
        return isp_fail(cli, CMD, e);
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "bytes": image.len(), "verified": true }));
        crate::print_envelope(&env)
    } else {
        println!("verified {} byte(s): match", image.len());
        ExitCode::SUCCESS
    }
}

fn erase_only(cli: &Cli, args: &IspArgs) -> ExitCode {
    const CMD: &str = "isp.erase";
    let mut o = match open(cli, CMD, args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let Some(size) = ch32rv_isp::flash_size(o.id) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "code flash size of {} is not known; nothing was erased",
                series(o.id)
            ),
            None,
        );
    };
    if let Err(m) = crate::cmd_probe::confirm_destructive(cli, "erase all of code flash?") {
        return fail(cli, CMD, ErrorKind::Usage, m, None);
    }
    let sectors = ch32rv_isp::sectors_for(size);
    if let Err(e) = o.dev.erase(sectors) {
        return isp_fail(cli, CMD, e);
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "erased_sectors": sectors }));
        crate::print_envelope(&env)
    } else {
        println!("erased {sectors} sector(s) (all of code flash)");
        ExitCode::SUCCESS
    }
}

fn reset(cli: &Cli, args: &IspArgs) -> ExitCode {
    const CMD: &str = "isp.reset";
    let o = match open(cli, CMD, args) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if let Err(e) = o.dev.end(true) {
        return isp_fail(cli, CMD, e);
    }
    if cli.json {
        crate::print_envelope(&ResultEnvelope::success(CMD))
    } else {
        println!("left the bootloader; application started");
        ExitCode::SUCCESS
    }
}

/// How long a chip put in its bootloader over the debug wire takes to enumerate, at most.
const ENTER_WAIT: Duration = Duration::from_secs(5);

const FLASH_KEYR: u32 = 0x4002_2004;
const FLASH_STATR: u32 = 0x4002_200c;
const FLASH_BOOT_MODEKEYR: u32 = 0x4002_2028;
const FLASH_KEY1: u32 = 0x4567_0123;
const FLASH_KEY2: u32 = 0xcdef_89ab;
/// `FLASH_STATR.BOOT_MODE`: boot from the BOOT area after the next system reset.
const STATR_BOOT_MODE: u32 = 1 << 14;

/// en: Families whose boot area is chosen in software (`FLASH_STATR.BOOT_MODE`) and holds the
/// factory ISP bootloader; ch32rv has entered it this way on CH32X035 (2026-10-13).
/// ja: BOOT 領域をソフトウェアで選べ、そこに factory ISP がある系列。
fn boot_mode_family(family: &str) -> bool {
    matches!(
        family.to_ascii_uppercase().as_str(),
        "CH32X035" | "CH32X033"
    )
}

/// en: Over the debug wire: reset the hart and catch it at the reset vector (whatever is in flash
/// must not run first - it may touch the flash keys and lock them until the next reset), unlock
/// the boot-mode key, set `BOOT_MODE`, and reset into the bootloader.
/// ja: debug 線で、hart を reset vector で捕まえ(flash の中身を先に走らせない。鍵を触って次の reset
/// までロックしうる)、boot mode の鍵を外し、`BOOT_MODE` を立てて bootloader へ reset する。
fn set_boot_mode<T: TargetAccess + ?Sized>(t: &mut T) -> Result<(), DmiError> {
    t.reset(ResetMode::HaltAtReset)?;
    t.write_words(FLASH_KEYR, &[FLASH_KEY1])?;
    t.write_words(FLASH_KEYR, &[FLASH_KEY2])?;
    t.write_words(FLASH_BOOT_MODEKEYR, &[FLASH_KEY1])?;
    t.write_words(FLASH_BOOT_MODEKEYR, &[FLASH_KEY2])?;
    let statr = t.read_words(FLASH_STATR, 1)?[0];
    t.write_words(FLASH_STATR, &[statr | STATR_BOOT_MODE])?;
    let statr = t.read_words(FLASH_STATR, 1)?[0];
    if statr & STATR_BOOT_MODE == 0 {
        return Err(DmiError::OperationFailed(format!(
            "BOOT_MODE did not stick (FLASH_STATR = 0x{statr:08x})"
        )));
    }
    t.reset(ResetMode::Run).map(|_| ())
}

/// en: `--entry probe` / `isp enter --via probe`: through `--probe` (an OEP probe; a WCH-Link is
/// not wired up yet), set the boot mode and reset. The caller then waits for the device.
/// ja: `--probe`(OEP の probe)で boot mode を立てて reset する。device を待つのは呼び出し側。
fn enter_via_probe(cli: &Cli, cmd: &str) -> Result<(), ExitCode> {
    let a = match crate::oep::addr(cli, cmd) {
        Ok(Some(a)) => a,
        Ok(None) => {
            return Err(fail(
                cli,
                cmd,
                ErrorKind::CapabilityUnsupported,
                "entering the bootloader over the debug wire needs an OEP probe (--probe); a WCH-Link is not supported yet",
                Some(ENTRY_HINT),
            ));
        }
        Err(c) => return Err(c),
    };
    let mut outcome = Ok(());
    let code = crate::oep::with_attached(cli, cmd, &a, true, |x| {
        let family = x.family.clone().unwrap_or_default();
        if !boot_mode_family(&family) {
            return fail(
                cli,
                cmd,
                ErrorKind::CapabilityUnsupported,
                format!(
                    "{} has no software-selected boot mode ch32rv knows; nothing was changed",
                    if family.is_empty() {
                        "this chip"
                    } else {
                        &family
                    }
                ),
                Some(ENTRY_HINT),
            );
        }
        if let Err(e) = set_boot_mode(&mut x.t) {
            outcome = Err(());
            return fail(
                cli,
                cmd,
                ErrorKind::TransferFailed,
                format!("entering the bootloader: {e}"),
                None,
            );
        }
        ExitCode::SUCCESS
    });
    if code != ExitCode::SUCCESS || outcome.is_err() {
        return Err(code);
    }
    if !cli.json {
        eprintln!("reset into the bootloader over the debug wire");
    }
    Ok(())
}

fn enter(cli: &Cli, args: &IspArgs) -> ExitCode {
    const CMD: &str = "isp.enter";
    let o = match open_with(cli, CMD, args, IspEntry::Probe) {
        Ok(o) => o,
        Err(c) => return c,
    };
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(describe(&o));
        crate::print_envelope(&env)
    } else {
        println!(
            "in the bootloader: {} (chip 0x{:02x}) at usb:{}, btver {}",
            series(o.id),
            o.id.chip_id,
            o.topology,
            o.cfg.btver_text()
        );
        ExitCode::SUCCESS
    }
}
