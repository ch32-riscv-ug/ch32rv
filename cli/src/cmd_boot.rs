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
