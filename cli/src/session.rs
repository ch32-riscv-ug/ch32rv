//! en: Shared attach session for probe-routed commands that talk to the target
//! (`target info`, `dbg *`, `read`). Opens the probe, attaches, hands the caller a
//! [`ch32rv_dmi::DebugModule`], and always detaches on drop. Read-only by itself: it issues
//! SetSpeed + AttachChip, never a write to flash.
//! ja: target と会話する probe 経路コマンド共通の attach セッション。probe を開いて attach し、
//! DebugModule を渡し、drop 時に必ず detach する。それ自体は読み取り専用。

use std::time::Duration;

use ch32rv_contract::Warning;
use ch32rv_dmi::DebugModule;
use ch32rv_usb::{DeviceLock, LockError};
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
    /// en: The target DB this invocation resolved against - the built-in tables, or those plus a
    /// `--db` overlay. Held here so every consumer (SKU lookup, family string, RTT scan length)
    /// agrees; building it per call site is how `--db` silently applied to some of them and not
    /// others. ja: この実行が使う target DB(内蔵、または `--db` overlay 込み)。各所で作り直すと
    /// `--db` が一部にしか効かないので session が 1 つ持つ。
    db: ch32rv_target::Db,
    /// Per-probe advisory lock, held for the session's lifetime (released on drop).
    _lock: DeviceLock,
}

/// What went wrong, so the CLI can pick the right exit code.
pub enum SessionError {
    Open(WchLinkError),
    ProbeInfo(WchLinkError),
    /// No target answered on the debug pins (attach got 0x55 / no response) - exit 20.
    NoTarget,
    /// Attach failed for another reason (SetSpeed, unexpected error) - exit 22.
    Attach(String),
    /// `--chip` conflicts with the detected target (exit 23).
    ChipMismatch(String),
    /// `--chip` names something the DB has never heard of, so it cannot be checked (exit 20).
    ChipNotInDb(String),
    /// The `--db` overlay could not be read or parsed (exit 2).
    DbOverlay(String),
    /// The per-probe advisory lock could not be taken in time (exit 13).
    Busy(LockError),
}

/// One line for places that can only show text (the Arduino monitor's data stream).
impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Open(e) | SessionError::ProbeInfo(e) => write!(f, "{e}"),
            SessionError::NoTarget => f.write_str("no target detected on the debug pins"),
            SessionError::Attach(m)
            | SessionError::ChipMismatch(m)
            | SessionError::ChipNotInDb(m)
            | SessionError::DbOverlay(m) => f.write_str(m),
            SessionError::Busy(e) => write!(f, "{e} (another ch32rv is using this probe)"),
        }
    }
}

impl Session {
    /// en: Open + clear state + attach. `warnings` accumulates non-fatal notes (corrupted
    /// readback recovery, etc.). Retries the open per docs/cli.ja.md §3.7. When `chip` (`--chip`)
    /// is given, it is validated against the detected chip and a conflict fails closed (exit 23).
    /// ja: open + 状態クリア + attach。`--chip` 指定時は検出と突き合わせ、矛盾なら fail-closed(exit 23)。
    pub fn attach(
        entry: &Entry,
        speed: Speed,
        timeout: Duration,
        lock_timeout: Duration,
        chip: Option<&str>,
        db_overlay: Option<&std::path::Path>,
        warnings: &mut Vec<Warning>,
    ) -> Result<Self, SessionError> {
        // en: Take the per-probe advisory lock before opening so concurrent ch32rv processes on
        // the same probe serialize instead of colliding (docs/cli.ja.md §3.7). Key by serial, or
        // by bus topology when the probe reports no serial. Held for the whole session.
        // ja: open 前に probe 単位の advisory lock を取り、同一 probe への並行アクセスを直列化する。
        let lock_key = entry
            .dev
            .serial()
            .map(str::to_owned)
            .unwrap_or_else(|| entry.dev.topology());
        let lock = DeviceLock::acquire(&lock_key, lock_timeout).map_err(SessionError::Busy)?;

        let db = match db_overlay {
            Some(path) => ch32rv_target::Db::with_overlay(path).map_err(SessionError::DbOverlay)?,
            None => ch32rv_target::Db::builtin(),
        };

        let mut link = open_with_retry(entry).map_err(SessionError::Open)?;
        link.set_timeout(timeout);
        // Clear any leftover state a previous session left holding the target.
        let _ = link.detach_chip();

        let probe_info = link.probe_info().map_err(SessionError::ProbeInfo)?;
        let attach = attach_once(&mut link, speed)?;

        // Validate an explicit --chip against the detected target (fail-closed on a family conflict).
        if let Some(requested) = chip
            && let Err(e) = check_chip(&db, requested, &attach)
        {
            let _ = link.detach_chip();
            return Err(e);
        }

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
                let _ = attach_once(&mut link, speed)?;
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
        if let Some(ci) = &chip
            && reported_flash_bytes(ci).is_none()
        {
            let (_, source) = flash_capacity(chip.as_ref(), &db, attach.chip_id);
            warnings.push(Warning {
                code: "flash-capacity-unavailable".to_owned(),
                msg: format!(
                    "ChipInfo flash capacity 0x{:04x} is zero or an erased pattern; capacity source: {source}",
                    ci.flash_bytes / 1024
                ),
            });
        }
        Ok(Self {
            link,
            attach,
            probe_info,
            chip,
            db,
            _lock: lock,
        })
    }

    /// Family name from the attach signature (None -> "unknown(0xNN)").
    pub fn family(&self) -> String {
        family_name(self.attach.family_byte)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("unknown(0x{:02x})", self.attach.family_byte))
    }

    /// Borrow a Debug Module driver over the probe's DMI transport.
    /// The target DB for this invocation (built-in, plus `--db` when given).
    pub fn db(&self) -> &ch32rv_target::Db {
        &self.db
    }

    /// Capacity from the probe when populated, otherwise from an unambiguous DB SKU.
    pub fn flash_capacity(&self) -> (Option<u32>, &'static str) {
        flash_capacity(self.chip.as_ref(), &self.db, self.attach.chip_id)
    }

    pub fn dm(&mut self) -> DebugModule<'_, WchLink> {
        DebugModule::new(&mut self.link)
    }

    /// en: Borrow the raw probe (for flash/erase/reset that live on `WchLink`).
    /// ja: raw probe を借りる(flash/erase/reset は `WchLink` 側)。
    pub fn link(&mut self) -> &mut WchLink {
        &mut self.link
    }
}

fn reported_flash_bytes(chip: &ChipInfo) -> Option<u32> {
    // X315's unpopulated signature reads 0xe339e339, also returned by ChipInfo.
    match chip.flash_bytes / 1024 {
        0 | 0xffff | 0xe339 => None,
        _ => Some(chip.flash_bytes),
    }
}

fn flash_capacity(
    chip: Option<&ChipInfo>,
    db: &ch32rv_target::Db,
    chip_id: u32,
) -> (Option<u32>, &'static str) {
    if let Some(bytes) = chip.and_then(reported_flash_bytes) {
        return (Some(bytes), "probe");
    }
    match db.resolve_by_chip_id(chip_id) {
        ch32rv_target::Resolution::Sku(s) if s.flash_bytes > 0 => (Some(s.flash_bytes), "db"),
        _ => (None, "unavailable"),
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Always release the core, on every path.
        let _ = self.link.detach_chip();
    }
}

/// en: Check an explicit `--chip` against what AttachChip found: fail-closed on a name the DB
/// does not know and on a family conflict. The caller detaches on `Err`.
/// ja: 明示の `--chip` を AttachChip の結果と突き合わせる。DB に無い名前と family の矛盾は
/// fail-closed。`Err` のとき detach は呼び出し側。
pub(crate) fn check_chip(
    db: &ch32rv_target::Db,
    requested: &str,
    attach: &AttachInfo,
) -> Result<(), SessionError> {
    let req_fams = db.families_for_chip_name(requested);
    let detected_fam = match db.resolve_by_chip_id(attach.chip_id) {
        ch32rv_target::Resolution::Sku(s) => s.family.clone(),
        ch32rv_target::Resolution::Family(f, _) => f,
        ch32rv_target::Resolution::Unknown => {
            family_name(attach.family_byte).unwrap_or("").to_owned()
        }
    };
    // en: A name the DB does not know cannot be checked against anything, so accepting it
    // would silently program whatever happens to be on the pins - exactly what `--chip`
    // exists to prevent, and what its "fail-closed on ambiguity" contract promises. The
    // gap series (V205/V407/V467/X305/X315/M030/M103) land here, which is the point: a
    // downstream IDE that offers them must hear "not in the DB", not flash a different
    // part. Auto-detection still works with `--chip` omitted.
    // ja: DB が知らない名前は何とも突き合わせられないので、受け入れると「刺さっている別の
    // チップに黙って書く」ことになる。`--chip` の存在意義と help の fail-closed 宣言に反する。
    // 未発売の gap series がここに落ちるのが狙いどおりで、`--chip` を省けば自動検出で動く。
    if req_fams.is_empty() {
        return Err(SessionError::ChipNotInDb(format!(
            "--chip {requested} is not in the target DB (detected {} from chip_id 0x{:08x})",
            if detected_fam.is_empty() {
                "an unknown part"
            } else {
                &detected_fam
            },
            attach.chip_id
        )));
    }
    // Reject a clear conflict: none of the requested name's families match the detected one.
    if !req_fams.is_empty()
        && !detected_fam.is_empty()
        && !req_fams
            .iter()
            .any(|f| f.eq_ignore_ascii_case(&detected_fam))
    {
        return Err(SessionError::ChipMismatch(format!(
            "--chip {requested} (family {}) conflicts with the detected {detected_fam} (chip_id 0x{:08x})",
            req_fams.join("/"),
            attach.chip_id
        )));
    }
    // A SKU is checked as that SKU (its device id), not only its family.
    if let Some(other) = db.sku_conflict(requested, attach.chip_id) {
        return Err(SessionError::ChipMismatch(format!(
            "--chip {requested} conflicts with the detected {other} (chip_id 0x{:08x})",
            attach.chip_id
        )));
    }
    Ok(())
}

fn attach_once(link: &mut WchLink, speed: Speed) -> Result<AttachInfo, SessionError> {
    link.set_speed_default(speed)
        .map_err(|e| SessionError::Attach(format!("SetSpeed failed: {e}")))?;
    link.attach_chip().map_err(|e| match e {
        // No response on the debug pins - a distinct, common case worth its own exit code (20).
        WchLinkError::Protocol { reason: 0x55, .. } | WchLinkError::UnexpectedResponse(_) => {
            SessionError::NoTarget
        }
        other => SessionError::Attach(format!("attach failed: {other}")),
    })
}

pub(crate) fn open_with_retry(entry: &Entry) -> Result<WchLink, WchLinkError> {
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

#[cfg(test)]
mod capacity_tests {
    use super::*;

    #[test]
    fn erased_capacity_falls_back_only_for_a_resolved_sku() {
        let db = ch32rv_target::Db::builtin();
        let mut chip = ChipInfo {
            flash_bytes: 0,
            uuid: [1; 8],
            protection_raw: [0; 4],
            chip_id_echo: 0x31500000,
        };
        for kib in [0, 0xffff, 0xe339] {
            chip.flash_bytes = kib * 1024;
            assert_eq!(
                flash_capacity(Some(&chip), &db, 0x31500000),
                (Some(196608), "db")
            );
            assert_eq!(
                flash_capacity(Some(&chip), &db, 0xdeadbeef),
                (None, "unavailable")
            );
        }
        // Valid probe capacities retain precedence, including measured values above the DB's.
        chip.flash_bytes = 288 * 1024;
        assert_eq!(
            flash_capacity(Some(&chip), &db, 0x30700528),
            (Some(288 * 1024), "probe")
        );
        assert_eq!(flash_capacity(None, &db, 0xdeadbeef), (None, "unavailable"));
    }
}
