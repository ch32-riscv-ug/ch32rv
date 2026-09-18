//! `boot hid flash`: rv003usb/UIAPduino HID scratchpad bootloader.

use std::process::ExitCode;

use ch32rv_boot::{HidBoot, HidBootError};
use ch32rv_contract::{ErrorKind, ResultEnvelope};

use crate::args::Cli;
use crate::cmd_probe::fail;

pub fn hid_flash(cli: &Cli, file: &std::path::Path, usb_id: Option<&str>) -> ExitCode {
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

    let boot = match HidBoot::open(id) {
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
            "usb": format!("{:04x}:{:04x}", report.vid, report.pid),
            "bytes": report.bytes,
            "sectors_written": report.sectors_written,
            "sector_size": 64,
            "verified": true,
            "run": true,
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
        HidBootError::NotFound(..) => ErrorKind::DeviceNotFound,
        HidBootError::Init(_) | HidBootError::Transfer(_) => ErrorKind::DeviceOpenFailed,
        HidBootError::ReadProtected(_) => ErrorKind::TargetProtected,
        HidBootError::Verify(_) => ErrorKind::VerifyMismatch,
        HidBootError::ImageTooLarge { .. } | HidBootError::Payload => ErrorKind::Usage,
        HidBootError::Timeout | HidBootError::FlashLocked(_) | HidBootError::FlashStatus(_) => {
            ErrorKind::TransferFailed
        }
    };
    fail(cli, cmd, kind, error.to_string(), None)
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
