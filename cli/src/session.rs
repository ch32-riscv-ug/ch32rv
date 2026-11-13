//! en: Shared attach session for probe-routed commands that talk to the target
//! (`target info`, `dbg *`, `read`). Opens the probe, attaches, hands the caller a
//! [`ch32rv_dmi::DebugModule`], and always detaches on drop. Read-only by itself: it issues
//! SetSpeed + AttachChip, never a write to flash.
//! ja: target と会話する probe 経路コマンド共通の attach セッション。probe を開いて attach し、
//! DebugModule を渡し、drop 時に必ず detach する。それ自体は読み取り専用。

use std::time::Duration;

use ch32rv_contract::Warning;
use ch32rv_dmi::{DebugModule, DtmAccess};
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
    /// A reset that runs the target was done (`--release-dm` resets once more on drop).
    reset_run: bool,
    /// H417: which harts were running before this session stopped them (see [`hart_ran`]).
    ran_before: [bool; 2],
    /// The live GPR the attach overwrote in the running program, if any
    /// ([`attach_corrupted_register_in`]).
    corrupted: Option<&'static str>,
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
    pub fn select_core(&mut self, core: u32) -> Result<(), String> {
        if core == 0 {
            return Ok(());
        }
        if core != 1 || self.attach.family_byte != 0xc6 {
            return Err("--core 1 requires an attached CH32H417 target".into());
        }
        self.link.select_hart(core).map_err(|e| e.to_string())
    }

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
        // en: Only when no size is known at all: a probe that reports none (X315: 0xe339) with the
        // DB filling it in is the normal case, shown by `target info` (flash_capacity_source).
        // ja: 大きさがどこからも分からないときだけ警告する(X315 のように DB で埋まるのは普通)。
        if let Some(ci) = &chip
            && reported_flash_bytes(ci).is_none()
            && let (None, source) = flash_capacity(chip.as_ref(), &db, attach.chip_id)
        {
            warnings.push(Warning {
                code: "flash-capacity-unavailable".to_owned(),
                msg: format!(
                    "ChipInfo flash capacity 0x{:04x} is zero or an erased pattern; capacity source: {source}",
                    ci.flash_bytes / 1024
                ),
            });
        }
        // en: H417: which harts ran decides the restart (core 0 alone: its a0 was overwritten)
        // and what is resumed. `dbg` leaves the harts as they are (it does not look), so it
        // neither restarts an H417 program nor resumes the other core: a restart would wipe the
        // state it is there to inspect.
        // ja: H417 では、走っていた hart で起動し直し(core 0 だけなら a0 が壊れた)と resume を決める。
        // `dbg` は hart を見ない(起動し直しも他の core の resume もしない。調べたい状態を消すため)。
        let ran_before = if RESUME_OTHER_HARTS.load(std::sync::atomic::Ordering::Relaxed) {
            harts_that_ran(&mut link, attach.family_byte)
        } else {
            [false; 2]
        };
        let corrupted =
            attach_corrupted_register_in(attach.family_byte, attach.chip_id, ran_before);
        if let Some(reg) = corrupted {
            warnings.push(attach_corrupts_register_warning(reg));
        } else if h417_both_cores_ran(attach.family_byte, ran_before) {
            warnings.push(h417_core0_a0_warning());
        }
        Ok(Self {
            link,
            attach,
            probe_info,
            chip,
            db,
            _lock: lock,
            reset_run: false,
            ran_before,
            corrupted,
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

    /// en: On a chip whose attach overwrote a GPR of the running program (CH32V103 s1, CH32V205 s0,
    /// CH32H417 core 0's a0 when only core 0 ran), restart the program now, for a session that
    /// keeps the target running while it holds it (monitor, the broker). A one-shot command leaves
    /// it to the drop, after its reads. Not gated on the hart's state: the V103 is halted by the
    /// attach and DetachChip lets it run, even after `dbg halt` (measured 2026-11-12, WCH-Link
    /// CH549 2.12; H417 core 0 likewise, 2026-11-13). True when it restarted.
    /// ja: attach が走っている program の GPR を壊した chip(V103 は s1、V205 は s0、H417 は core 0
    /// だけが走っていたときの core 0 の a0)で、今すぐ program を起動し直す。target を走らせたまま
    /// 持つ session(monitor、ブローカー)用。単発のコマンドは drop に任せる。V103 は attach で止まり、
    /// DetachChip で(`dbg halt` の後でも)走り出すので、hart の状態では分けない(H417 の core 0 も同じ)。
    pub fn restart_if_attach_corrupted(&mut self) -> bool {
        if self.corrupted.is_none() {
            return false;
        }
        crate::cmd_flash::soft_reset_and_run(self).is_ok()
    }

    /// en: For a session that keeps the target running while it holds it (monitor, the Arduino
    /// monitor, the WCH-Link broker): undo now what the attach did to the running program, which a
    /// one-shot command leaves to its drop. Restarts the program where the attach overwrote a GPR
    /// ([`Self::restart_if_attach_corrupted`]), and on a CH32H417 resumes the hart the command did
    /// not select when it was running before: the LinkE's AttachChip stops it, and with
    /// ArduinoCore-CH32RV's default build the sketch and its console run on that hart (core 1,
    /// V5F; core 0 wakes it and waits in wfi), so `monitor --source dmseq` got no answer
    /// (2026-11-12).
    /// ja: target を走らせたまま持つ session(monitor、Arduino のモニタ、WCH-Link のブローカー)用。
    /// attach が走っている program にしたことを今すぐ戻す(単発のコマンドは drop で)。GPR を壊す
    /// chip では program を起動し直し、CH32H417 では、選ばなかった hart が走っていたなら resume する
    /// (AttachChip が止める。ArduinoCore の既定のビルドでは sketch とコンソールはその hart = core 1
    /// の V5F で動き、monitor の dmseq が答えなかった)。
    pub fn keep_running(&mut self) {
        if self.restart_if_attach_corrupted() {
            return;
        }
        self.resume_other_harts();
    }

    /// en: On a CH32H417, resume every hart that was running before the attach stopped it
    /// ([`hart_ran`]), and make the program's hart the session's: core 1 (V5F) when it ran, as
    /// with ArduinoCore-CH32RV's default build, where core 0 (V3F) only wakes it and waits in wfi.
    /// The halts, memory reads and resumes of the session then go to the hart that runs the
    /// sketch: a memory read passes through DATA0, which the running V5F's DMSeq console writes,
    /// so a read through core 0 returned the console's words instead of the flash (2026-11-12).
    /// ja: CH32H417 で、attach の前に走っていた hart をすべて resume し、program の hart を session
    /// の hart にする(走っていれば core 1 = V5F。ArduinoCore の既定のビルドでは core 0 = V3F は起こして
    /// wfi で待つだけ)。メモリの読み出しは DATA0 を通り、走っている V5F の DMSeq がそこを書くので、
    /// core 0 越しの読み出しは flash ではなくコンソールの語を返した。
    fn resume_other_harts(&mut self) {
        if self.attach.family_byte != 0xc6
            || !RESUME_OTHER_HARTS.load(std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        let keep = self.link.selected_hart();
        for hart in 0..2 {
            if self.ran_before[hart as usize] && matches!(hart_ran(&mut self.link, hart), Ok(true))
            {
                let _ = resume_hart(&mut self.link, hart);
            }
        }
        let program = if self.ran_before[1] { 1 } else { keep };
        let _ = self.link.select_hart(program);
    }

    /// Note that a reset which runs the target was done (for `--release-dm` on drop).
    pub fn mark_reset_run(&mut self) {
        self.reset_run = true;
    }

    /// en: The chip's unique ID as hex: the probe's ChipInfo UUID, else (a family the Link does not
    /// answer for: CH32M030) the ESIG UNIID1/UNIID2 words read from memory, as `oep::read_uid`.
    /// Both give `<word0><word1>` in big-endian hex. The hart is left as it was.
    /// ja: チップ固有 ID。ChipInfo の UUID、無ければ ESIG をメモリから読む(M030 は Link が返さない)。
    pub fn uid(&mut self) -> Option<String> {
        if let Some(c) = &self.chip {
            return Some(c.uuid.iter().map(|b| format!("{b:02x}")).collect());
        }
        let at = ch32rv_target::uid_addr(&self.family()).unwrap_or(0x1FFF_F7E8);
        let was_running = !self.dm().is_halted().unwrap_or(true);
        if was_running && self.dm().halt().is_err() {
            return None;
        }
        let bytes = self.dm().read_mem(at, 8);
        if was_running {
            let _ = self.dm().resume();
        }
        let bytes = bytes.ok()?;
        let [w0, w1] = [0, 4].map(|i| {
            u32::from_le_bytes(
                bytes
                    .get(i..i + 4)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0; 4]),
            )
        });
        if (w0 == 0 && w1 == 0) || (w0 == u32::MAX && w1 == u32::MAX) {
            return None;
        }
        Some(format!("{w0:08x}{w1:08x}"))
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
    // Parts that share an ID share their geometry too (db-gen refuses anything else).
    match db.resolve_by_chip_id(chip_id).geometry() {
        Some((flash, _)) if flash > 0 => (Some(flash), "db"),
        _ => (None, "unavailable"),
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // en: CH32V103: the attach overwrote s1, and DetachChip lets the program run, which then
        // faults on its next use of s1. Restart it unless a reset already did.
        // ja: V103 は attach が s1 を壊し、DetachChip で走り出して落ちる。reset 済みでなければ起動し直す。
        if !self.reset_run {
            self.restart_if_attach_corrupted();
        }
        if self.attach.family_byte == 0xc6
            && RESUME_OTHER_HARTS.load(std::sync::atomic::Ordering::Relaxed)
        {
            // Hart 0 under its own selection is the LinkE's to let go (DetachChip does).
            let keep = self.link.selected_hart();
            for hart in (0..2).filter(|&h| !(h == 0 && keep == 0)) {
                if self.ran_before[hart as usize]
                    && matches!(hart_ran(&mut self.link, hart), Ok(true))
                {
                    let _ = resume_hart(&mut self.link, hart);
                }
            }
        }
        if RELEASE_DM.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = self.link.dmi_write(DMCONTROL, 0);
            if self.reset_run {
                // en: Reset once more with the DM off, through NRST (the LinkE's RstOut; nothing
                // happens where RST is not wired, and the CH549 Link refuses it), and send nothing
                // after it: after the LinkE's soft reset the DM was active again and core 0 was
                // found halted, while DMCONTROL = 0 left both H417 cores running and core 1 back
                // in its deep sleep (ArduinoCore-CH32RV, 2026-10-22, when the RstOut frame was
                // still malformed, so the NRST pulse itself is not measured on the H417).
                // ja: DM を止めたまま NRST でもう一度 reset し、その後は何も送らない(LinkE の
                // soft reset の後は DM が戻り core 0 が止まっていた)。H417 で NRST の効果は未測定。
                if self.link.rst_out(true).is_ok() {
                    std::thread::sleep(Duration::from_millis(20));
                    let _ = self.link.rst_out(false);
                }
                return;
            }
        }
        // Always release the core, on every path.
        let _ = self.link.detach_chip();
    }
}

/// en: Whether `hart` was running before a debugger stopped it: running now, or halted at a pc
/// other than 0. The LinkE's AttachChip stops the H417 hart a command did not select, and
/// DetachChip does not let it go, so without this `--core 0 read` left core 1 (V5F) halted and
/// `--core 1 dbg resume` left core 0 (V3F) halted. A hart halted out of a reset must not be
/// resumed: core 1 sits at pc 0, which is V3F's image, and resuming it ran both cores on core 0's
/// sketch. DMSTATUS `allhavereset` does not tell that case apart: core 1 woken by core 0's WAKEIP
/// after a reset keeps it set with nobody to acknowledge it (ArduinoCore-CH32RV, 2026-10-22).
/// The resume acknowledges it.
/// ja: hart が debugger に止められる前に走っていたか。走っている、または pc が 0 でない所で
/// 止まっている。reset から止まっている hart(core 1 は pc 0 = V3F の image)は resume しない。
/// `allhavereset` では見分けられない(reset の後に WAKEIP で起きた core 1 は立てたまま走る)。
fn hart_ran(link: &mut WchLink, hart: u32) -> Result<bool, ch32rv_dmi::DmiError> {
    link.select_hart(hart)?;
    let mut dm = DebugModule::new(link);
    if !dm.is_halted()? {
        return Ok(true);
    }
    Ok(dm.read_reg(ch32rv_dmi::RegName::Pc)? != 0)
}

/// en: Whether the WCH-Link's fast memory read is unreliable on this family once the program was
/// restarted after the attach (CH32V103: it runs on what the Link set up at the attach, and
/// returned zeros after the restart, WCH-Link CH549 2.12, 2026-11-12): read through the Debug
/// Module there.
/// ja: attach の後に program を起動し直すと、WCH-Link の高速読み出しが当てにならない family か
/// (V103: Link が attach で用意したものの上で動き、起動し直しの後は 0 を返した)。DM で読む。
pub(crate) fn attach_corrupts_regs(family_byte: u8) -> bool {
    ch32rv_flash::flash_controller_profile(family_byte).is_some_and(|p| p.attach_corrupts_regs)
}

/// en: Whether the Link's fast memory read is to be left alone on this family for a session that
/// let the target run again after the attach: the CH32V103 ([`attach_corrupts_regs`]) and the
/// CH32H417, whose fast read returned words that were not in the flash (`90 0a 00 00` repeated
/// where the image starts `73 70 04 30`) once core 1 (V5F) was resumed under a broker
/// (2026-11-12). Those read through the Debug Module.
/// ja: attach の後に target を走らせ直した session で、Link の高速読み出しを使わない family か。
/// V103 と、CH32H417(core 1 を resume した後のブローカーで、flash に無い語を返した)。DM で読む。
pub(crate) fn link_fast_read_unreliable(family_byte: u8) -> bool {
    attach_corrupts_regs(family_byte) || family_byte == 0xc6
}

/// en: Whether the Link's fast read of `[addr, addr + len)` leaves a running program intact on this
/// probe. The WCH-Link (CH549) 2.12's fast read of the top of SRAM overwrites the halted hart's s1
/// (x9) with the end of the read and a0 (x10) with 0xE0000000 (CH32V203RBT6: reading
/// 0x2000F000+4 KiB; flash, the bottom of SRAM and the WCH-LinkE left every GPR alone), so a
/// program it then lets go hung in an `if (s1) ...` loop: `monitor --source rtt` scans the whole
/// SRAM and got no answer from ArduinoCore-CH32RV's console-rtt (2026-11-13). SRAM is read through
/// the Debug Module on that Link; together with [`link_fast_read_unreliable`].
/// ja: この probe で Link の高速読み出しが走っている program を壊さないか。WCH-Link(CH549)2.12 は
/// SRAM の上端を読むと、止めた hart の s1 を読み出しの終わりの番地に、a0 を 0xE0000000 にする
/// (V203RB で実測。flash・SRAM の下側・LinkE は GPR を変えない)。その Link では SRAM を DM で読む。
pub(crate) fn link_fast_read_ok(
    variant: ch32rv_wchlink::Variant,
    family_byte: u8,
    addr: u32,
    len: u32,
) -> bool {
    !link_fast_read_unreliable(family_byte) && !link_fast_read_clobbers(variant, addr, len)
}

/// en: The WCH-Link (CH549) part of [`link_fast_read_ok`]: a fast read reaching into SRAM.
/// ja: [`link_fast_read_ok`] のうち CH549 の Link の分(SRAM にかかる高速読み出し)。
pub(crate) fn link_fast_read_clobbers(
    variant: ch32rv_wchlink::Variant,
    addr: u32,
    len: u32,
) -> bool {
    const SRAM_BASE: u32 = 0x2000_0000;
    variant == ch32rv_wchlink::Variant::Ch549 && addr.saturating_add(len) > SRAM_BASE
}

/// en: The live GPR the WCH-Link's AttachChip overwrites on this chip, saved nowhere, so a
/// program that goes on running faults at its next use of it:
/// - CH32V103: s1 (x9), with 0x0297df3f through a WCH-Link CH549 2.12 and the chip id through a
///   WCH-LinkE; the attach also halts the hart (mcause 4 on `lw s0, 0(s1)`, ArduinoCore-CH32RV's
///   Clang build, 2026-11-12).
/// - CH32V205 family (CH32V205 and CH32V203CCT6 in the DB): s0 (x8) and dscratch0, with
///   0xE000D004 through a WCH-LinkE 2.22, whatever chip type byte goes before the attach
///   (`mtval` 0xE000D008; ArduinoCore-CH32RV's DMSeq console stopped answering, 2026-11-12).
///
/// wch-protocols `protocols/pc-to-link.ja.md` "V103 attach quirk".
/// ja: WCH-Link の AttachChip が上書きする生きた GPR(どこにも保存されない)。V103 は s1、
/// V205 の family(DB では CH32V205 と CH32V203CCT6)は s0(と dscratch0。0xE000D004)。
pub(crate) fn attach_corrupted_register(family_byte: u8, chip_id: u32) -> Option<&'static str> {
    if attach_corrupts_regs(family_byte) {
        return Some("s1");
    }
    let family = match ch32rv_target::Db::builtin().resolve_by_chip_id(chip_id) {
        ch32rv_target::Resolution::Sku(s) => s.family.clone(),
        ch32rv_target::Resolution::Family(f, _) => f,
        ch32rv_target::Resolution::Unknown => return None,
    };
    family_attach_corrupts(&family)
}

/// en: [`attach_corrupted_register`] by the chip id's DB family name (the OEP view of a WCH-Link
/// broker gives the family). ja: DB の family 名から(WCH-Link のブローカーの OEP の側で使う)。
pub(crate) fn family_attach_corrupts(family: &str) -> Option<&'static str> {
    match family {
        "CH32V103" => Some("s1"),
        "CH32V205" => Some("s0"),
        _ => None,
    }
}

/// en: The warning a command that attaches to such a chip gives: the program is restarted.
/// ja: そうした chip に attach するコマンドが出す警告(program は起動し直す)。
pub(crate) fn attach_corrupts_register_warning(register: &str) -> Warning {
    Warning {
        code: "attach-corrupts-register".to_owned(),
        msg: format!(
            "attaching through a WCH-Link overwrites this chip's {register} register, which a \
             running program cannot survive: ch32rv restarts the program before it lets the \
             target go (the probe lets it run when it detaches, even after `dbg halt`)"
        ),
    }
}

/// en: H417: which harts were running before the attach stopped them ([`hart_ran`]), hart 0
/// selected again after. `[false; 2]` on any other chip.
/// ja: H417 で、attach が止める前に走っていた hart(終わったら hart 0 を選び直す)。他は `[false; 2]`。
pub(crate) fn harts_that_ran(link: &mut WchLink, family_byte: u8) -> [bool; 2] {
    let mut ran = [false; 2];
    if family_byte == 0xc6 {
        for (hart, r) in ran.iter_mut().enumerate() {
            *r = hart_ran(link, hart as u32).unwrap_or(false);
        }
        let _ = link.select_hart(0);
    }
    ran
}

/// en: [`attach_corrupted_register`], and on a CH32H417 the GPR the attach overwrote in the
/// program given which harts ran (`ran`, [`harts_that_ran`]): the LinkE's AttachChip zeroes core
/// 0's (V3F) a0 (x10) about every other time (LinkE 2.22 and 2.23: 5 of 9 and 7 of 15 attaches;
/// core 1 untouched), and DMI reads nothing before the attach, so it cannot be saved. The program
/// is core 0's when core 1 did not run (ArduinoCore-CH32RV's V3F build): with `this` in a0 at
/// `SerialDMSeq.available()`'s entry, the sketch read a byte that was not there and got `\xff`
/// in front of its next line (2026-11-13). With core 1 running too (the default V5F build, where
/// core 0 only waits in wfi for work) it is left alone: see [`h417_core0_a0_warning`].
/// ja: [`attach_corrupted_register`] に加え、H417 では走っていた hart から決める。LinkE の
/// AttachChip は core 0(V3F)の a0 をおよそ 2 回に 1 回 0 にする(2.22 / 2.23 とも。core 1 は無事)。
/// attach の前は DMI が読めないので退避できない。core 1 が走っていなければ program は core 0 の
/// もの(V3F ビルド)で、`available()` の入口で a0 = this が 0 になり、行の頭に `\xff` が入った。
pub(crate) fn attach_corrupted_register_in(
    family_byte: u8,
    chip_id: u32,
    ran: [bool; 2],
) -> Option<&'static str> {
    attach_corrupted_register(family_byte, chip_id)
        .or_else(|| (family_byte == 0xc6 && ran[0] && !ran[1]).then_some("a0 (core 0)"))
}

/// en: A CH32H417 with both cores running: the attach may have zeroed core 0's a0, and the
/// program is not restarted (core 1 runs it, core 0 usually only waits for work).
/// ja: 両方の core が走っていた H417(attach が core 0 の a0 を 0 にしたかもしれないが、起動し直さない)。
pub(crate) fn h417_both_cores_ran(family_byte: u8, ran: [bool; 2]) -> bool {
    family_byte == 0xc6 && ran[0] && ran[1]
}

/// The warning for [`h417_both_cores_ran`].
pub(crate) fn h417_core0_a0_warning() -> Warning {
    Warning {
        code: "attach-may-corrupt-register".to_owned(),
        msg: "attaching through a WCH-LinkE may zero core 0's a0 register on a CH32H417; both \
              cores were running, so ch32rv leaves the program running (core 0 waiting for work, \
              as in ArduinoCore-CH32RV's default build, is not affected): reset the board if code \
              running on core 0 misbehaves"
            .to_owned(),
    }
}

/// en: For a raw link after AttachChip on a CH32H417 (the SDI forwarding, which keeps no
/// Session): resume every hart that was running before the attach stopped it, then select hart 0
/// again (the LinkE forwards SDI from what it set up at the attach). See
/// [`Session::keep_running`].
/// ja: CH32H417 で AttachChip の後の raw link 用(Session を持たない SDI 転送): attach の前に走って
/// いた hart をすべて resume し、hart 0 を選び直す。
pub(crate) fn resume_running_harts(link: &mut WchLink, family_byte: u8) {
    if family_byte != 0xc6 {
        return;
    }
    for hart in 0..2 {
        if matches!(hart_ran(link, hart), Ok(true)) {
            let _ = resume_hart(link, hart);
        }
    }
    let _ = link.select_hart(0);
}

/// Resume `hart` (selected first).
fn resume_hart(link: &mut WchLink, hart: u32) -> Result<(), ch32rv_dmi::DmiError> {
    link.select_hart(hart)?;
    DebugModule::new(link).resume()
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

/// en: `--connect-under-reset` for this process (set once by `main`; every WCH-Link session
/// attaches through [`attach_once`]).
/// ja: この process の `--connect-under-reset`(main が 1 回立てる)。
pub(crate) static CONNECT_UNDER_RESET: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// en: `--attach-wait` for this process, in seconds (0: attach once).
/// ja: この process の `--attach-wait`(秒。0 なら 1 回だけ attach)。
pub(crate) static ATTACH_WAIT_S: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// en: The `--attach-wait` deadline, said once on stderr: what the user should do meanwhile.
/// ja: `--attach-wait` の期限。待つ間にすることを stderr に 1 度だけ言う。
pub(crate) fn attach_wait_deadline() -> Option<std::time::Instant> {
    let s = ATTACH_WAIT_S.load(std::sync::atomic::Ordering::Relaxed);
    if s == 0 {
        return None;
    }
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "waiting up to {s} s for the target to answer: power-cycle it or press its RESET now"
        )
    });
    Some(std::time::Instant::now() + Duration::from_secs(s))
}

/// en: Resume, at the end of a session, the H417 hart the command did not select (set by `main`:
/// every command but `dbg`, which leaves the other core as it is unless `dbg resume --all`).
/// ja: session の終わりに、選ばなかった H417 の hart を resume する(`dbg` 以外。`dbg` は
/// `dbg resume --all` のときだけ)。
pub(crate) static RESUME_OTHER_HARTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// en: `--release-dm`: leave the debug module inactive (DMCONTROL = 0) at the end of a session,
/// and before a reset that lets the target run.
/// ja: `--release-dm`。session の終わりと、走らせる reset の前に DMCONTROL を 0 にする。
pub(crate) static RELEASE_DM: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

const DMCONTROL: u8 = 0x10;
const DMSTATUS: u8 = 0x11;
const DMCONTROL_HALT: u32 = 0x8000_0001; // haltreq | dmactive

/// en: Attach with NRST held (the probe's RST wired to it): hold it through the LinkE's RstOut,
/// attach and request a halt while it is held, release it, and attach again at once - the
/// wire needs AttachChip after the reset - with haltreq set, so the hart stops before firmware
/// that turns the debug pins into something else can run. Measured on a CH32H417 whose firmware
/// hands PB8 / PB9 to USB a few ms after boot (2026-10-20): the second AttachChip came back in
/// about 95 ms with the hart halted, and the flash could be erased from there. That run sent a
/// malformed RstOut (read as GetProbeInfo), so the halt came from haltreq and the re-attach;
/// the NRST hold is not measured yet.
/// ja: NRST を押さえて接続する。押さえたまま attach して haltreq を立て、離してすぐ attach し直す
/// (reset の後は AttachChip が要る)。起動直後にデバッグのピンを取る firmware が走る前に止める。
fn attach_under_reset(link: &mut WchLink) -> Result<AttachInfo, SessionError> {
    link.rst_out(true)
        .map_err(|e| SessionError::Attach(format!("RstOut failed: {e}")))?;
    std::thread::sleep(Duration::from_millis(20));
    let _ = link.attach_chip();
    let _ = link.dmi_op(DMCONTROL, DMCONTROL_HALT, 2);
    link.rst_out(false)
        .map_err(|e| SessionError::Attach(format!("RstOut failed: {e}")))?;
    let mut last = None;
    for k in 0..12 {
        if k > 0 {
            std::thread::sleep(Duration::from_millis(200));
        }
        match link.attach_chip() {
            Ok(a) => {
                let _ = link.dmi_op(DMCONTROL, DMCONTROL_HALT, 2);
                let _ = link.dmi_op(DMSTATUS, 0, 1);
                return Ok(a);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(WchLinkError::Protocol { reason: 0x55, .. }) | None => SessionError::NoTarget,
        Some(e) => SessionError::Attach(format!("attach under reset failed: {e}")),
    })
}

/// en: `--attach-wait`: repeat AttachChip until the target answers or the deadline passes, and
/// request a halt at once, before firmware that sleeps or takes the debug pins after its boot wait
/// gets that far. Measured on a WCH-Link (CH549) built into a CH32V103 board, whose NRST it does
/// not drive, running a sketch that went into standby 1.5 s after each boot (2026-10-30): with
/// RESET pressed by hand, attempt 123 (about 5 s in, about 40 ms each) answered and halted.
/// ja: `--attach-wait`。答えるか期限まで AttachChip を繰り返し、答えたらすぐ haltreq を立てる。
/// 起動待ちの後に眠る・デバッグのピンを取る firmware がそこへ行く前に止める。
fn attach_waiting(
    link: &mut WchLink,
    deadline: std::time::Instant,
) -> Result<AttachInfo, SessionError> {
    loop {
        match link.attach_chip() {
            Ok(a) => {
                for _ in 0..3 {
                    let _ = link.dmi_op(DMCONTROL, DMCONTROL_HALT, 2);
                }
                let _ = link.dmi_op(DMSTATUS, 0, 1);
                return Ok(a);
            }
            Err(
                WchLinkError::Protocol { reason: 0x55, .. } | WchLinkError::UnexpectedResponse(_),
            ) if std::time::Instant::now() < deadline => {}
            Err(
                WchLinkError::Protocol { reason: 0x55, .. } | WchLinkError::UnexpectedResponse(_),
            ) => {
                return Err(SessionError::NoTarget);
            }
            Err(e) => return Err(SessionError::Attach(format!("attach failed: {e}"))),
        }
    }
}

fn attach_once(link: &mut WchLink, speed: Speed) -> Result<AttachInfo, SessionError> {
    link.set_speed_default(speed)
        .map_err(|e| SessionError::Attach(format!("SetSpeed failed: {e}")))?;
    if CONNECT_UNDER_RESET.load(std::sync::atomic::Ordering::Relaxed) {
        return attach_under_reset(link);
    }
    if let Some(deadline) = attach_wait_deadline() {
        return attach_waiting(link, deadline);
    }
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

#[cfg(test)]
mod attach_corruption_tests {
    use super::*;

    /// H417: a0 is restarted for only when core 0 alone ran (the V3F build); with core 1 running
    /// too it is a warning, and nothing when core 0 did not run.
    #[test]
    fn h417_restarts_only_a_program_on_core_0_alone() {
        let id = 0x4170_053d;
        assert_eq!(
            attach_corrupted_register_in(0xc6, id, [true, false]),
            Some("a0 (core 0)")
        );
        assert_eq!(attach_corrupted_register_in(0xc6, id, [true, true]), None);
        assert!(h417_both_cores_ran(0xc6, [true, true]));
        assert_eq!(attach_corrupted_register_in(0xc6, id, [false, false]), None);
        assert_eq!(attach_corrupted_register_in(0xc6, id, [false, true]), None);
        assert!(!h417_both_cores_ran(0xc6, [false, true]));
    }

    /// Other chips do not depend on the harts.
    #[test]
    fn other_chips_ignore_the_harts() {
        assert_eq!(
            attach_corrupted_register_in(0x19, 0x2030_0500, [true, false]),
            None
        );
        assert!(!h417_both_cores_ran(0x19, [true, true]));
    }
}

#[cfg(test)]
mod fast_read_tests {
    use super::*;
    use ch32rv_wchlink::Variant;

    /// The CH549 Link's fast read is kept off SRAM (it overwrote s1 / a0 there), not off flash;
    /// the LinkE keeps it everywhere.
    #[test]
    fn ch549_reads_sram_through_the_debug_module() {
        assert!(link_fast_read_clobbers(Variant::Ch549, 0x2000_f000, 0x1000));
        assert!(link_fast_read_clobbers(Variant::Ch549, 0x1fff_fff0, 0x20));
        assert!(!link_fast_read_clobbers(
            Variant::Ch549,
            0x0800_0000,
            0x1_0000
        ));
        assert!(!link_fast_read_clobbers(
            Variant::LinkE,
            0x2000_f000,
            0x1000
        ));
        assert!(!link_fast_read_ok(Variant::Ch549, 0x19, 0x2000_0000, 4));
        assert!(link_fast_read_ok(Variant::Ch549, 0x19, 0x0800_0000, 4));
        assert!(!link_fast_read_ok(Variant::LinkE, 0xc6, 0x0800_0000, 4));
    }
}
