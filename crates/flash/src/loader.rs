//! en: Programming CH32 flash with ch32rv's own RAM loader over any [`TargetAccess`] probe
//! (docs/oep-host.ja.md §5): the probe only writes RAM and runs the loader to its ebreak; what to
//! write and how is decided here from the device DB. Used for OEP probes; the WCH-Link keeps
//! WCH's stub path.
//!
//! The loader (`loader/ch32_loader.S`, RV32EC, position independent) erases and programs one fast
//! page per run and always stops at offset 4. Pages are read first and the image laid over them, so
//! bytes outside the image survive. Everything is read back; pages that fail or read back wrong
//! are written again (up to two passes, placing the loader again each time, since a garbled
//! loader still reaches its ebreak).
//! ja: ch32rv 自前の RAM loader で、任意の [`TargetAccess`] の probe から CH32 の flash に書く。probe
//! は RAM に書いて loader を ebreak まで走らせるだけで、何をどう書くかは DB からここで決める。

use std::time::Duration;

use ch32rv_dmi::access::regno;
use ch32rv_dmi::{DmiError, TargetAccess};

use crate::Segment;

/// The committed loader binary (`cargo xtask loader-gen` / `loader-check`).
pub const LOADER: &[u8] = include_bytes!("../loader/ch32_loader.bin");
/// Every run ends at this ebreak.
pub const DONE_OFFSET: u32 = 4;

const SRAM: u32 = 0x2000_0000;
/// The page buffer, after the loader (which is well under 512 bytes).
const BUFFER: u32 = SRAM + 0x200;
const FLASH_BASE: u32 = 0x4002_2000;
const FLASH_KEYR: u32 = FLASH_BASE + 0x04;
const FLASH_CTLR: u32 = FLASH_BASE + 0x10;
const FLASH_MODEKEYR: u32 = FLASH_BASE + 0x24;
const KEY1: u32 = 0x4567_0123;
const KEY2: u32 = 0xCDEF_89AB;
const CTLR_LOCK: u32 = 1 << 7;
const CTLR_FLOCK: u32 = 1 << 15;
/// en: Bounds a controller that never clears BSY. A page is not always quick: measured over a
/// WCH-Link (host-side, DMI polling included), a CH32V307 page took about 22 ms and a CH32V103
/// page about 0.2 s, so a 200 ms bound cut the V103 short. Kept well under the session lease.
/// ja: BSY が落ちない controller の上限。page は速いとは限らない(WCH-Link 越しの host 側の実測で
/// V307 は約 22 ms、V103 は約 0.2 s。200 ms では V103 が途中で切られた)。lease より十分短く。
const RUN_TIMEOUT: Duration = Duration::from_millis(1000);

/// The loader's programming mechanism (its `a3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderMode {
    /// BUFRST, per word store + BUFLOAD, then STRT (V003, V00x, X035, L103).
    Buffered = 0,
    /// FTPG, per word store, then PGSTART (V20x, V30x).
    PgStart = 1,
    /// Halfword programming + the V103 commit.
    V103 = 2,
}

/// How to program one family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoaderPlan {
    pub mode: LoaderMode,
    /// The fast page the loader erases and programs per run.
    pub page: u32,
}

/// The plan for a DB family (`CH32V20x`, `CH32X035`, ...), from its programming method and
/// geometry. None when the DB does not know enough to program it safely.
pub fn plan_for_family(family: &str) -> Option<LoaderPlan> {
    let geo = ch32rv_target::flash_geometry(family)?;
    let page = geo.fast_program;
    if page == 0 || page != geo.fast_erase || page > 256 || !page.is_multiple_of(4) {
        return None;
    }
    let mode = if family.eq_ignore_ascii_case("CH32V103") {
        LoaderMode::V103
    } else {
        match ch32rv_target::flash_program_method(family)?.mode.as_str() {
            "buffered" => LoaderMode::Buffered,
            "direct" => LoaderMode::PgStart,
            _ => return None,
        }
    };
    Some(LoaderPlan { mode, page })
}

#[derive(Debug, thiserror::Error)]
pub enum LoaderError {
    #[error("probe: {0}")]
    Probe(#[from] DmiError),
    #[error("the FLASH controller stayed locked after the unlock keys (CTLR 0x{0:08x})")]
    StillLocked(u32),
    #[error("the loader did not read back as written after 3 tries")]
    LoaderGarbled,
    #[error("{} page(s) still differ after rewriting, first at 0x{first:08x}", .count)]
    Verify { first: u32, count: usize },
    #[error("the controller reported a write-protect error at 0x{addr:08x} (STATR 0x{statr:08x})")]
    WriteProtected { addr: u32, statr: u32 },
}

/// What programming did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoaderReport {
    pub pages: usize,
    /// Pages written again after a failed run or a readback mismatch.
    pub rewritten: usize,
    /// Loader runs that did not start (dpc still at the entry) and were issued again.
    pub restarted_runs: usize,
}

fn words(b: &[u8]) -> Vec<u32> {
    b.chunks(4)
        .map(|c| {
            let mut w = [0xFFu8; 4];
            w[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(w)
        })
        .collect()
}

fn bytes(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn unlock<T: TargetAccess + ?Sized>(t: &mut T) -> Result<(), LoaderError> {
    let ctlr = t.read_words(FLASH_CTLR, 1)?.first().copied().unwrap_or(0);
    if ctlr & (CTLR_LOCK | CTLR_FLOCK) != 0 {
        for (reg, key) in [
            (FLASH_KEYR, KEY1),
            (FLASH_KEYR, KEY2),
            (FLASH_MODEKEYR, KEY1),
            (FLASH_MODEKEYR, KEY2),
        ] {
            t.write_words(reg, &[key])?;
        }
    }
    let ctlr = t.read_words(FLASH_CTLR, 1)?.first().copied().unwrap_or(0);
    if ctlr & (CTLR_LOCK | CTLR_FLOCK) != 0 {
        return Err(LoaderError::StillLocked(ctlr));
    }
    Ok(())
}

fn lock<T: TargetAccess + ?Sized>(t: &mut T) -> Result<(), LoaderError> {
    let ctlr = t.read_words(FLASH_CTLR, 1)?.first().copied().unwrap_or(0);
    t.write_words(FLASH_CTLR, &[ctlr | CTLR_LOCK | CTLR_FLOCK])?;
    Ok(())
}

/// Write the loader and read it back until it matches (3 tries).
fn place_loader<T: TargetAccess + ?Sized>(t: &mut T) -> Result<(), LoaderError> {
    let w = words(LOADER);
    for _ in 0..3 {
        t.write_words(SRAM, &w)?;
        if t.read_words(SRAM, w.len())? == w {
            return Ok(());
        }
    }
    Err(LoaderError::LoaderGarbled)
}

/// The outcome of one page's run.
enum PageRun {
    Ok,
    /// Did not complete (timeout, stopped elsewhere): write again.
    Retry,
    WriteProtected(u32),
}

fn run_page<T: TargetAccess + ?Sized>(
    t: &mut T,
    plan: LoaderPlan,
    addr: u32,
    content: &[u8],
    report: &mut LoaderReport,
) -> Result<PageRun, LoaderError> {
    t.write_words(BUFFER, &words(content))?;
    let regs = [
        (regno::A0, addr),
        (regno::A1, BUFFER),
        (regno::A2, plan.page),
        (regno::A3, plan.mode as u32),
        (regno::A4, FLASH_BASE),
        (regno::MSTATUS, 0),
    ];
    // The probe issues a run once and never again: a run that did not start (dpc still at the
    // entry) is safe to issue again, since erase + program of one page is idempotent.
    for _ in 0..3 {
        let r = t.run_until_halt(SRAM, &regs, &[regno::A0], RUN_TIMEOUT)?;
        let a0 = r.outs.first().copied().unwrap_or(u32::MAX);
        if r.stopped && r.dpc == SRAM + DONE_OFFSET {
            return Ok(match a0 {
                0 => PageRun::Ok,
                v if v & 0x8000_0000 != 0 => PageRun::WriteProtected(v & 0x7FFF_FFFF),
                _ => PageRun::Retry,
            });
        }
        if r.dpc == SRAM {
            report.restarted_runs += 1;
            continue;
        }
        return Ok(PageRun::Retry);
    }
    Ok(PageRun::Retry)
}

/// en: Program `segments` (flash addresses) with `plan`. The hart must be halted - reset-halt it
/// first so a running watchdog cannot reset the part mid-write. `progress(done, total)` counts
/// pages. The FLASH controller is locked again at the end, on every path.
/// ja: `segments` を `plan` で書く。hart は止めておくこと(走っている IWDG に途中で reset させない
/// よう reset-halt してから)。最後に、どの経路でも FLASH controller を lock し直す。
pub fn program<T: TargetAccess + ?Sized>(
    t: &mut T,
    plan: LoaderPlan,
    segments: &[Segment],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<LoaderReport, LoaderError> {
    unlock(t)?;
    let r = program_unlocked(t, plan, segments, progress);
    let relock = lock(t);
    let report = r?;
    relock?;
    Ok(report)
}

fn program_unlocked<T: TargetAccess + ?Sized>(
    t: &mut T,
    plan: LoaderPlan,
    segments: &[Segment],
    progress: &mut dyn FnMut(usize, usize),
) -> Result<LoaderReport, LoaderError> {
    let page = plan.page;
    // The pages the image touches, each as its final content: what the flash holds now with the
    // image laid over it.
    let mut addrs: Vec<u32> = segments
        .iter()
        .flat_map(|s| {
            let end = s.addr + s.data.len() as u32;
            (s.addr / page..end.div_ceil(page)).map(move |p| p * page)
        })
        .collect();
    addrs.sort_unstable();
    addrs.dedup();
    let mut pages: Vec<(u32, Vec<u8>)> = Vec::with_capacity(addrs.len());
    for &a in &addrs {
        let mut content = bytes(&t.read_words(a, (page / 4) as usize)?);
        crate::overlay(a, &mut content, segments);
        pages.push((a, content));
    }

    let mut report = LoaderReport {
        pages: pages.len(),
        ..LoaderReport::default()
    };
    place_loader(t)?;
    let total = pages.len();
    let mut todo: Vec<usize> = (0..total).collect();
    for pass in 0..3 {
        if pass > 0 {
            report.rewritten += todo.len();
            place_loader(t)?;
        }
        let mut again = Vec::new();
        for (n, &i) in todo.iter().enumerate() {
            let (a, ref c) = pages[i];
            match run_page(t, plan, a, c, &mut report)? {
                PageRun::Ok => {}
                PageRun::Retry => again.push(i),
                PageRun::WriteProtected(statr) => {
                    return Err(LoaderError::WriteProtected { addr: a, statr });
                }
            }
            if pass == 0 {
                progress(n + 1, total);
            }
        }
        // Read everything written this pass back; a page that differs is written again.
        for &i in &todo {
            let (a, ref c) = pages[i];
            if !again.contains(&i) && bytes(&t.read_words(a, (page / 4) as usize)?) != *c {
                again.push(i);
            }
        }
        again.sort_unstable();
        again.dedup();
        if again.is_empty() {
            return Ok(report);
        }
        todo = again;
    }
    Err(LoaderError::Verify {
        first: pages[todo[0]].0,
        count: todo.len(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn plans_come_from_the_db() {
        assert_eq!(
            plan_for_family("CH32V20x"),
            Some(LoaderPlan {
                mode: LoaderMode::PgStart,
                page: 256
            })
        );
        assert_eq!(
            plan_for_family("CH32X035").unwrap().mode,
            LoaderMode::Buffered
        );
        assert_eq!(plan_for_family("CH32V003").unwrap().page, 64);
        assert_eq!(
            plan_for_family("CH32V103"),
            Some(LoaderPlan {
                mode: LoaderMode::V103,
                page: 128
            })
        );
        assert_eq!(plan_for_family("CH32NOPE"), None);
    }

    #[test]
    fn the_loader_stops_at_its_done_offset() {
        // j main ; ebreak at +4
        assert_eq!(&LOADER[4..8], &0x0010_0073u32.to_le_bytes());
        assert!(LOADER.len() < (BUFFER - SRAM) as usize);
    }
}
