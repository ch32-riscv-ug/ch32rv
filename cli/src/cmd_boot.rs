//! `boot hid flash`: rv003usb/UIAPduino HID scratchpad bootloader.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ch32rv_boot::{HidBoot, HidBootError};
use ch32rv_contract::{ErrorKind, ResultEnvelope};

use crate::args::Cli;
use crate::cmd_probe::fail;

pub fn hid_flash(
    cli: &Cli,
    file: &std::path::Path,
    usb_id: Option<&str>,
    wait_secs: u64,
) -> ExitCode {
    const CMD: &str = "boot.hid.flash";
    let id = match usb_id.map(parse_usb_id).transpose() {
        Ok(id) => id,
        Err(message) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                message,
                Some("use VID:PID, e.g. 1209:b803"),
            );
        }
    };
    let image = match std::fs::read(file) {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                "image is empty".to_owned(),
                None,
            );
        }
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("cannot read {}: {e}", file.display()),
                None,
            );
        }
    };
    if cli.dry_run {
        println!(
            "would flash {} byte(s) through HID {}",
            image.len(),
            usb_id.unwrap_or("1209:b803 or 1209:b003")
        );
        return ExitCode::SUCCESS;
    }

    // en: `--probe usb:<topology>`, or the IDE's `port:hid://<topology>`, picks one bootloader
    // by position; without it the first one found is used.
    // ja: `--probe usb:<topology>` か IDE の `port:hid://<topology>` で位置の bootloader を選ぶ。
    // 無ければ最初の 1 台。
    let topology = match crate::cmd_probe::parse_selector(cli, CMD) {
        Ok(None) => None,
        Ok(Some(ch32rv_usb::Selector::Topology(t))) => Some(t),
        Ok(Some(ch32rv_usb::Selector::Port(p))) if p.starts_with("hid://") => {
            p.strip_prefix("hid://").map(str::to_owned)
        }
        Ok(Some(_)) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                "a HID bootloader is selected by position only",
                Some("use --probe usb:<bus>-<ports> or port:hid://<bus>-<ports>"),
            );
        }
        Err(c) => return c,
    };
    let wait = Duration::from_secs(wait_secs);
    let boot = match open_waiting(id, topology.as_deref(), wait) {
        Ok(boot) => boot,
        Err(e) => return hid_error(cli, CMD, e),
    };
    let report = match boot.flash(&image, true) {
        Ok(report) => report,
        Err(e) => return hid_error(cli, CMD, e),
    };

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "flash": {
                "bytes": report.bytes,
                "programmer": "hid",
                "usb": format!("{:04x}:{:04x}", report.vid, report.pid),
                "sectors_written": report.sectors_written,
                "sector_size": 64,
                "skipped": false,
                "scope": "sectors",
                "verified": true,
                "running": true,
            }
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "flashed {} byte(s) through {:04x}:{:04x}: {} sector(s) changed, verified, application started",
            report.bytes, report.vid, report.pid, report.sectors_written
        );
        ExitCode::SUCCESS
    }
}

/// en: Open the bootloader, waiting up to `wait` for it to appear. The bootloader is entered by
/// hand (UIAPduino: RESET held while plugging in, then released at once), so the first miss
/// prints how, once, on stderr - also under `--progress none`, since the IDE shows only that.
/// Right after it appears the OS may not have granted access to the node yet, so open failures
/// are retried too once we have been waiting. Replay never waits.
/// ja: bootloader を最大 `wait` 待って開く。入るのは手動なので、最初に見つからなかったときに
/// 入り方を stderr へ 1 回出す(IDE はそれしか見せないので `--progress none` でも出す)。現れた
/// 直後は OS がノードの権限をまだ付けていないことがあるので、待っていた場合は open の失敗も再試行する。
fn open_waiting(
    usb_id: Option<(u16, u16)>,
    topology: Option<&str>,
    wait: Duration,
) -> Result<HidBoot, HidBootError> {
    let deadline = Instant::now() + wait;
    let mut waiting = false;
    loop {
        let err = match HidBoot::open_at(usb_id, topology) {
            Ok(boot) => {
                if waiting {
                    eprintln!("bootloader found");
                }
                return Ok(boot);
            }
            Err(e) => e,
        };
        let retry = match err {
            HidBootError::NotFound(..) | HidBootError::NotAt(_) => true,
            HidBootError::Init(_) | HidBootError::Transfer(_) => waiting,
            _ => false,
        };
        if !retry || ch32rv_usb::replay::active() || Instant::now() >= deadline {
            return Err(err);
        }
        if !waiting {
            waiting = true;
            eprintln!(
                "waiting up to {} s for the HID bootloader ({}) ...",
                wait.as_secs(),
                usb_id.map_or("1209:b803 or 1209:b003".to_owned(), |(v, p)| format!(
                    "{v:04x}:{p:04x}"
                ))
            );
            eprintln!(
                "  hold RESET, plug in USB, and release RESET right away \
                 (holding it longer makes the host give up enumerating)"
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn parse_usb_id(value: &str) -> Result<(u16, u16), String> {
    let Some((vid, pid)) = value.split_once(':') else {
        return Err(format!("invalid USB ID `{value}`"));
    };
    if pid.contains(':') {
        return Err(format!("invalid USB ID `{value}`"));
    }
    let vid = u16::from_str_radix(vid.trim_start_matches("0x"), 16)
        .map_err(|_| format!("invalid VID in `{value}`"))?;
    let pid = u16::from_str_radix(pid.trim_start_matches("0x"), 16)
        .map_err(|_| format!("invalid PID in `{value}`"))?;
    Ok((vid, pid))
}

fn hid_error(cli: &Cli, cmd: &str, error: HidBootError) -> ExitCode {
    let kind = match error {
        HidBootError::NotFound(..) | HidBootError::NotAt(_) => ErrorKind::DeviceNotFound,
        HidBootError::Ambiguous(_) => ErrorKind::DeviceAmbiguous,
        HidBootError::Init(_) | HidBootError::Transfer(_) => ErrorKind::DeviceOpenFailed,
        HidBootError::ReadProtected(_) => ErrorKind::TargetProtected,
        HidBootError::Verify(_) => ErrorKind::VerifyMismatch,
        HidBootError::ImageTooLarge { .. } | HidBootError::Payload => ErrorKind::Usage,
        HidBootError::Timeout | HidBootError::FlashLocked(_) | HidBootError::FlashStatus(_) => {
            ErrorKind::TransferFailed
        }
    };
    let hint = matches!(error, HidBootError::NotFound(..) | HidBootError::NotAt(_)).then_some(
        "hold RESET, plug in USB, and release RESET right away; --wait <SECS> waits longer",
    );
    fail(cli, cmd, kind, error.to_string(), hint)
}

// ---- USB DFU 1.1 download ----

/// en: The DFU device `--probe` names (any of `isp --app-device`'s forms), else the only one
/// present with a DFU run-time or DFU-mode interface.
/// ja: `--probe` が名指す DFU の device(`isp --app-device` と同じ書き方)。無ければ唯一の DFU device。
fn dfu_device(cli: &Cli, cmd: &str) -> Result<ch32rv_usb::UsbDeviceInfo, ExitCode> {
    if let Some(sel) = cli.probe.as_deref() {
        return crate::cmd_isp::app_device(cli, cmd, sel).map(|a| a.dev);
    }
    let all = ch32rv_usb::enumerate()
        .map_err(|e| fail(cli, cmd, ErrorKind::DeviceOpenFailed, e.to_string(), None))?;
    let mut dfu: Vec<_> = all
        .into_iter()
        .filter(|d| d.dfu_runtime_interface().is_some() || d.dfu_mode_interface().is_some())
        .collect();
    match dfu.len() {
        1 => Ok(dfu.remove(0)),
        0 => Err(fail(
            cli,
            cmd,
            ErrorKind::DeviceNotFound,
            "no USB device with a DFU interface is connected".to_owned(),
            Some("is the application running with its DFU run-time interface?"),
        )),
        n => Err(fail(
            cli,
            cmd,
            ErrorKind::TargetAmbiguous,
            format!(
                "{n} USB devices have a DFU interface: {}",
                dfu.iter()
                    .map(|d| format!("usb:{} ({})", d.topology(), d.usb_id()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some("name one with --probe usb:<bus>-<ports> or VID:PID:SERIAL"),
        )),
    }
}

/// en: The device in DFU mode: `dev` itself, or - after DFU_DETACH to its run-time interface -
/// the device that comes back with a DFU-mode interface, matched by serial (else by VID:PID),
/// opened once the OS lets us (udev grants the new node a moment later).
/// ja: DFU mode の device。run-time なら DFU_DETACH して、DFU mode で出直した device を serial
/// (無ければ VID:PID)で探し、OS が開かせてくれるまで待って開く。
fn enter_dfu_mode(
    cli: &Cli,
    cmd: &str,
    dev: ch32rv_usb::UsbDeviceInfo,
) -> Result<ch32rv_usb::dfu::DfuMode, ExitCode> {
    let err = |kind, m: String| fail(cli, cmd, kind, m, None);
    let dev = if dev.dfu_mode_interface().is_some() {
        dev
    } else {
        let Some(intf) = dev.dfu_runtime_interface() else {
            return Err(err(
                ErrorKind::CapabilityUnsupported,
                format!("usb:{} has no DFU interface", dev.topology()),
            ));
        };
        // The device may leave before the status stage: an error here is not a failure.
        let _ = dev.dfu_detach(intf, 1000);
        if !cli.json {
            eprintln!(
                "sent DFU_DETACH to usb:{} (interface {intf})",
                dev.topology()
            );
        }
        let (serial, id) = (dev.serial().map(str::to_owned), (dev.vid(), dev.pid()));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let found = ch32rv_usb::enumerate().ok().and_then(|all| {
                all.into_iter().find(|d| {
                    d.dfu_mode_interface().is_some()
                        && match &serial {
                            Some(s) => d.serial() == Some(s.as_str()),
                            None => (d.vid(), d.pid()) == id,
                        }
                })
            });
            if let Some(d) = found {
                break d;
            }
            if Instant::now() > deadline {
                return Err(err(
                    ErrorKind::DeviceNotFound,
                    "no DFU-mode device came back within 10 s of DFU_DETACH".to_owned(),
                ));
            }
        }
    };
    // en: A device that just came back is often not ours yet: udev grants the node a moment
    // later (access denied), and a system service may open it to look at it first (fwupd
    // probes DFU devices - the interface is busy). Both pass; wait them out.
    // ja: 出直したばかりの device はすぐには開けない。udev の権限付けが少し遅れ(access denied)、
    // システムのサービス(fwupd は DFU の device を調べる)が先に開くこともある(busy)。待つ。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match ch32rv_usb::dfu::DfuMode::open(&dev) {
            Ok(m) => return Ok(m),
            Err(ch32rv_usb::UsbError::AccessDenied(_) | ch32rv_usb::UsbError::Busy(_))
                if Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(err(ErrorKind::DeviceOpenFailed, e.to_string())),
        }
    }
}

/// `boot dfu flash`: write `file` from the start of code flash through DFU 1.1.
pub fn dfu_flash(
    cli: &Cli,
    file: &std::path::Path,
    format: ch32rv_contract::policy::ImageFormat,
    at: Option<&str>,
) -> ExitCode {
    const CMD: &str = "boot.dfu.flash";
    const BASE: u32 = ch32rv_flash::CODE_FLASH_START;
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("cannot read {}: {e}", file.display()),
                None,
            );
        }
    };
    let at = match at.map(crate::parse::u32_addr).transpose() {
        Ok(a) => a,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let image = match crate::cmd_flash::parse_image(&bytes, format, file, at, BASE) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
    };
    // DFU blocks run from the first flash byte: the image laid flat from there, gaps 0xff.
    let end = image
        .segments
        .iter()
        .map(|s| s.addr + s.data.len() as u32)
        .max()
        .unwrap_or(BASE);
    if image.segments.iter().any(|s| s.addr < BASE) || end <= BASE {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "the image does not lie in code flash".to_owned(),
            None,
        );
    }
    let mut flat = vec![0xffu8; (end - BASE) as usize];
    for s in &image.segments {
        let o = (s.addr - BASE) as usize;
        flat[o..o + s.data.len()].copy_from_slice(&s.data);
    }
    if cli.dry_run {
        println!("would write {} byte(s) through DFU", flat.len());
        return ExitCode::SUCCESS;
    }
    let dev = match dfu_device(cli, CMD) {
        Ok(d) => d,
        Err(c) => return c,
    };
    let dfu = match enter_dfu_mode(cli, CMD, dev) {
        Ok(m) => m,
        Err(c) => return c,
    };
    let fail_t = |m: String| fail(cli, CMD, ErrorKind::TransferFailed, m, None);
    if let Err(e) = dfu.reset_state() {
        return fail_t(format!("DFU: {e}"));
    }
    let started = Instant::now();
    let size = usize::from(dfu.transfer_size);
    let mut block: u16 = 0;
    for chunk in flat.chunks(size) {
        if let Err(e) = dfu.download(block, chunk) {
            return fail_t(format!("DFU download at block {block}: {e}"));
        }
        block = block.wrapping_add(1);
    }
    if let Err(e) = dfu.finish(block) {
        return fail_t(format!("DFU: {e}"));
    }
    let secs = started.elapsed().as_secs_f64();
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "flash": {
                "bytes": flat.len(),
                "blocks": block,
                "transfer_size": dfu.transfer_size,
                "seconds": secs,
            }
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "wrote {} bytes through DFU in {secs:.2} s ({block} block(s) of {size}); the device manifests and restarts",
            flat.len()
        );
        ExitCode::SUCCESS
    }
}

/// `boot dfu info`: the DFU device, its interfaces and (in DFU mode) its state.
pub fn dfu_info(cli: &Cli) -> ExitCode {
    const CMD: &str = "boot.dfu.info";
    let dev = match dfu_device(cli, CMD) {
        Ok(d) => d,
        Err(c) => return c,
    };
    let runtime = dev.dfu_runtime_interface();
    let mode = dev.dfu_mode_interface();
    let detail = mode
        .and_then(|_| ch32rv_usb::dfu::DfuMode::open(&dev).ok())
        .map(|m| (m.transfer_size, m.manifestation_tolerant, m.status().ok()));
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "usb": dev.usb_id(),
            "topology": dev.topology(),
            "serial": dev.serial(),
            "runtime_interface": runtime,
            "dfu_interface": mode,
            "transfer_size": detail.map(|d| d.0),
            "manifestation_tolerant": detail.map(|d| d.1),
            "state": detail.and_then(|d| d.2).map(|s| s.state),
        }));
        return crate::print_envelope(&env);
    }
    println!("usb:      {} at usb:{}", dev.usb_id(), dev.topology());
    println!("serial:   {}", dev.serial().unwrap_or("-"));
    match (runtime, mode) {
        (_, Some(i)) => println!("dfu:      DFU mode (interface {i})"),
        (Some(i), None) => {
            println!("dfu:      run-time (interface {i}); `boot dfu flash` detaches it first")
        }
        (None, None) => println!("dfu:      none"),
    }
    if let Some((size, tolerant, status)) = detail {
        println!("transfer: {size} bytes per block, manifestation tolerant: {tolerant}");
        if let Some(s) = status {
            println!("state:    {} (status {})", s.state, s.status);
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_id() {
        assert_eq!(parse_usb_id("1209:b803"), Ok((0x1209, 0xb803)));
        assert_eq!(parse_usb_id("0x1209:0xb003"), Ok((0x1209, 0xb003)));
        assert!(parse_usb_id("1209").is_err());
        assert!(parse_usb_id("1209:b803:x").is_err());
    }
}
