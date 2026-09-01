//! en: Shared attach session for probe-routed commands that talk to the target
//! (`target info`, `dbg *`, `read`). Opens the probe, attaches, hands the caller a
//! [`ch32rv_dmi::DebugModule`], and always detaches on drop. Read-only by itself: it issues
//! SetSpeed + AttachChip, never a write to flash.
//! ja: target と会話する probe 経路コマンド共通の attach セッション。probe を開いて attach し、
//! DebugModule を渡し、drop 時に必ず detach する。それ自体は読み取り専用。

use std::time::Duration;

use ch32rv_contract::Warning;
use ch32rv_dmi::DebugModule;
use ch32rv_wchlink::{
    AttachInfo, ChipInfo, ChipInfoStatus, ProbeInfo, Speed, WchLink, WchLinkError, family_name,
};

use crate::cmd_probe::Entry;

/// A held attach session. Detaches the target core when dropped (unless suppressed).
pub struct Session {
    link: WchLink,
    pub attach: AttachInfo,
    pub probe_info: ProbeInfo,
    /// ChipInfo readback (flash size / UUID), when the target answered it.
    pub chip: Option<ChipInfo>,
}

/// What went wrong, so the CLI can pick the right exit code.
pub enum SessionError {
    Open(WchLinkError),
    ProbeInfo(WchLinkError),
    Attach(String),
}

impl Session {
    /// en: Open + clear state + attach. `warnings` accumulates non-fatal notes (corrupted
    /// readback recovery, etc.). Retries the open per docs/cli.ja.md §3.7.
    /// ja: open + 状態クリア + attach。`warnings` に非致命の注記を積む。open は再試行する。
    pub fn attach(
        entry: &Entry,
        speed: Speed,
        timeout: Duration,
        warnings: &mut Vec<Warning>,
    ) -> Result<Self, SessionError> {
        let mut link = open_with_retry(entry).map_err(SessionError::Open)?;
        link.set_timeout(timeout);
        // Clear any leftover state a previous session left holding the target.
        let _ = link.detach_chip();

        let probe_info = link.probe_info().map_err(SessionError::ProbeInfo)?;
        let attach = attach_once(&mut link, speed).map_err(SessionError::Attach)?;

        // en: Read ChipInfo, recovering once from the known LinkE corrupted-readback state.
        // ja: ChipInfo を読み、LinkE の壊れ読み値からは 1 度だけ復旧する。
        let chip = match link.chip_info() {
            Ok(ChipInfoStatus::Ok(ci)) => Some(ci),
            Ok(ChipInfoStatus::NoAnswer) => {
                warnings.push(Warning {
                    code: "uuid-unavailable".to_owned(),
                    msg: "the target did not answer the UUID query (protected, or unsupported by this family)".to_owned(),
                });
                None
            }
            Ok(ChipInfoStatus::CorruptedReadback) => {
                warnings.push(Warning {
                    code: "probe-readback-corrupted".to_owned(),
                    msg: "the probe held a corrupted target readback (known LinkE state); recovered via re-detect".to_owned(),
                });
                let _ = link.redetect_chip();
                let _ = link.detach_chip();
                let _ = attach_once(&mut link, speed).map_err(SessionError::Attach)?;
                match link.chip_info() {
                    Ok(ChipInfoStatus::Ok(ci)) => Some(ci),
                    _ => {
                        warnings.push(Warning {
                            code: "probe-readback-corrupted".to_owned(),
                            msg: "recovery did not produce a clean readback; replug the probe if values look wrong".to_owned(),
                        });
                        None
                    }
                }
            }
            Err(_) => None,
        };
        Ok(Self {
            link,
            attach,
            probe_info,
            chip,
        })
    }

    /// Family name from the attach signature (None -> "unknown(0xNN)").
    pub fn family(&self) -> String {
        family_name(self.attach.family_byte)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("unknown(0x{:02x})", self.attach.family_byte))
    }

    /// Borrow a Debug Module driver over the probe's DMI transport.
    pub fn dm(&mut self) -> DebugModule<'_, WchLink> {
        DebugModule::new(&mut self.link)
    }

    /// en: Borrow the raw probe (for flash/erase/reset that live on `WchLink`).
    /// ja: raw probe を借りる(flash/erase/reset は `WchLink` 側)。
    pub fn link(&mut self) -> &mut WchLink {
        &mut self.link
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Always release the core, on every path.
        let _ = self.link.detach_chip();
    }
}

fn attach_once(link: &mut WchLink, speed: Speed) -> Result<AttachInfo, String> {
    link.set_speed_default(speed)
        .map_err(|e| format!("SetSpeed failed: {e}"))?;
    link.attach_chip().map_err(|e| match e {
        WchLinkError::Protocol { reason: 0x55, .. } | WchLinkError::UnexpectedResponse(_) => {
            "no target detected on the debug pins".to_owned()
        }
        other => format!("attach failed: {other}"),
    })
}

fn open_with_retry(entry: &Entry) -> Result<WchLink, WchLinkError> {
    let mut last = WchLink::open(&entry.dev);
    for _ in 0..2 {
        match &last {
            Err(WchLinkError::Usb(ch32rv_usb::UsbError::AccessDenied(_))) | Ok(_) => break,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(1));
                last = WchLink::open(&entry.dev);
            }
        }
    }
    last
}
