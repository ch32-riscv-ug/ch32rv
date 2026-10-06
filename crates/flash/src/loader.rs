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
use ch32rv_dmi::{DmiError, ResetMode, TargetAccess};

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
/// en: Bounds one run (up to [`MAX_BATCH_PAGES`] pages) and a controller that never clears BSY. A
/// page is not always quick: measured over a WCH-Link (host-side, DMI polling included), a
/// CH32V307 page took about 22 ms and a CH32V103 page about 0.2 s, so 8 V103 pages take about
/// 1.6 s. Kept under the 3 s session lease: the probe answers nothing while a run is in progress.
/// ja: 1 回の run(最大 [`MAX_BATCH_PAGES`] page)の上限。V103 は 1 page 約 0.2 s なので 8 page で
/// 約 1.6 s。run の間 probe は答えないので、3 s の lease より短く。
const RUN_TIMEOUT: Duration = Duration::from_millis(2500);
/// One run's buffer: its pages back to back after the loader (SRAM is 2 KiB on the smallest part).
const BATCH_BYTES: usize = 1024;
const MAX_BATCH_PAGES: usize = 8;

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

/// en: Unlock the FLASH controller (both key pairs) and read CTLR back. A key write lost on the
/// way (a link drop on rvswd makes a write vanish while the request still succeeds) leaves the
/// controller locked, and a key sequence out of order keeps it locked until a reset: so a CTLR
/// still locked is met with a reset-halt and the keys again, up to 3 times (seen on the L103
/// behind the RP2350, 2026-10-06). The hart is reset-halted on entry anyway (see [`program`]).
/// ja: FLASH controller の鍵を入れて CTLR を読み直す。鍵の書き込みが線の途切れで消えると lock の
/// まま残り、順の崩れた鍵は reset まで lock を保つので、lock のままなら reset-halt して鍵を入れ直す
/// (最大 3 回)。
fn unlock<T: TargetAccess + ?Sized>(t: &mut T) -> Result<(), LoaderError> {
    let mut ctlr = 0;
    for attempt in 0..3 {
        if attempt > 0 {
            t.reset(ResetMode::HaltAtReset)?;
        }
        ctlr = t.read_words(FLASH_CTLR, 1)?.first().copied().unwrap_or(0);
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
        ctlr = t.read_words(FLASH_CTLR, 1)?.first().copied().unwrap_or(0);
        if ctlr & (CTLR_LOCK | CTLR_FLOCK) == 0 {
            return Ok(());
        }
    }
    Err(LoaderError::StillLocked(ctlr))
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

/// The outcome of one run.
enum Run {
    Ok,
    /// Did not complete (timeout, stopped elsewhere): write again.
    Retry,
    WriteProtected(u32),
}

/// en: Write `count` consecutive pages starting at `addr` in one loader run: their content goes to
/// the buffer back to back, and the loader erases and programs each in turn (fewer round trips,
/// which is what bounds a slow link such as a 115200 UART bridge).
/// ja: `addr` から連続する `count` page を 1 回の run で書く(往復が減る。115200 の UART bridge の
/// ような遅いリンクの律速はここ)。
fn run_batch<T: TargetAccess + ?Sized>(
    t: &mut T,
    plan: LoaderPlan,
    addr: u32,
    content: &[u8],
    count: u32,
    report: &mut LoaderReport,
) -> Result<Run, LoaderError> {
    t.write_words(BUFFER, &words(content))?;
    let regs = [
        (regno::A0, addr),
        (regno::A1, BUFFER),
        (regno::A2, plan.page),
        (regno::A3, plan.mode as u32),
        (regno::A5, count),
        (regno::MSTATUS, 0),
    ];
    // The probe issues a run once and never again: a run that did not start (dpc still at the
    // entry) is safe to issue again, since erase + program of whole pages is idempotent.
    for _ in 0..3 {
        let r = t.run_until_halt(SRAM, &regs, &[regno::A0], RUN_TIMEOUT)?;
        let a0 = r.outs.first().copied().unwrap_or(u32::MAX);
        if r.stopped && r.dpc == SRAM + DONE_OFFSET {
            return Ok(match a0 {
                0 => Run::Ok,
                v if v & 0x8000_0000 != 0 => Run::WriteProtected(v & 0x7FFF_FFFF),
                _ => Run::Retry,
            });
        }
        if r.dpc == SRAM {
            report.restarted_runs += 1;
            continue;
        }
        return Ok(Run::Retry);
    }
    Ok(Run::Retry)
}

/// Runs of consecutive entries of `idx` (indices into `pages`), each at most `max` long.
fn batches(pages: &[(u32, Vec<u8>)], idx: &[usize], page: u32, max: usize) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    for &i in idx {
        match out.last_mut() {
            Some(b)
                if b.len() < max && b.last().is_some_and(|&j| pages[j].0 + page == pages[i].0) =>
            {
                b.push(i)
            }
            _ => out.push(vec![i]),
        }
    }
    out
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
    let words_per_page = (page / 4) as usize;
    // The pages the image touches, each as its final content. Only a page the image covers in
    // part is read first (to keep the bytes around the image); a whole page is the image's.
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
        let mut covered = vec![false; page as usize];
        for seg in segments {
            let lo = seg.addr.max(a);
            let hi = (seg.addr + seg.data.len() as u32).min(a + page);
            for x in lo..hi {
                covered[(x - a) as usize] = true;
            }
        }
        let mut content = if covered.iter().all(|&c| c) {
            vec![0xFF; page as usize]
        } else {
            bytes(&t.read_words(a, words_per_page)?)
        };
        crate::overlay(a, &mut content, segments);
        pages.push((a, content));
    }

    let mut report = LoaderReport {
        pages: pages.len(),
        ..LoaderReport::default()
    };
    place_loader(t)?;
    let total = pages.len();
    let per_run = (BATCH_BYTES / page as usize).clamp(1, MAX_BATCH_PAGES);
    let mut todo: Vec<usize> = (0..total).collect();
    let mut done = 0;
    for pass in 0..3 {
        if pass > 0 {
            report.rewritten += todo.len();
            place_loader(t)?;
        }
        let mut again = Vec::new();
        for b in batches(&pages, &todo, page, per_run) {
            let first = pages[b[0]].0;
            let content: Vec<u8> = b.iter().flat_map(|&i| pages[i].1.iter().copied()).collect();
            match run_batch(t, plan, first, &content, b.len() as u32, &mut report)? {
                Run::Ok => {}
                Run::Retry => again.extend(&b),
                Run::WriteProtected(statr) => {
                    return Err(LoaderError::WriteProtected { addr: first, statr });
                }
            }
            if pass == 0 {
                done += b.len();
                progress(done, total);
            }
        }
        // Read back what this pass wrote, a consecutive run at a time; a page that differs is
        // written again.
        for b in batches(&pages, &todo, page, usize::MAX) {
            let got = bytes(&t.read_words(pages[b[0]].0, words_per_page * b.len())?);
            for (k, &i) in b.iter().enumerate() {
                let off = k * page as usize;
                if !again.contains(&i) && got[off..off + page as usize] != pages[i].1[..] {
                    again.push(i);
                }
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

    /// A FLASH controller that loses the first `drop` writes (a link drop: the write vanishes,
    /// the request succeeds) and stays locked after keys out of order until a reset.
    struct Fpec {
        ctlr: u32,
        keyr: Vec<u32>,
        modekeyr: Vec<u32>,
        jammed: bool,
        drop: usize,
        resets: usize,
    }

    impl Fpec {
        fn new(drop: usize) -> Self {
            Fpec {
                ctlr: CTLR_LOCK | CTLR_FLOCK,
                keyr: Vec::new(),
                modekeyr: Vec::new(),
                jammed: false,
                drop,
                resets: 0,
            }
        }

        fn key(seq: &mut Vec<u32>, k: u32, jammed: &mut bool) -> bool {
            seq.push(k);
            match seq.as_slice() {
                [KEY1] => false,
                [KEY1, KEY2] => {
                    seq.clear();
                    !*jammed
                }
                _ => {
                    seq.clear();
                    *jammed = true;
                    false
                }
            }
        }
    }

    impl ch32rv_dmi::DtmAccess for Fpec {
        fn dmi_read(&mut self, _: u8) -> Result<u32, DmiError> {
            Ok(0)
        }
        fn dmi_write(&mut self, _: u8, _: u32) -> Result<(), DmiError> {
            Ok(())
        }
        fn dmi_nop(&mut self) -> Result<(), DmiError> {
            Ok(())
        }
    }

    impl TargetAccess for Fpec {
        fn max_block_words(&self) -> usize {
            64
        }
        fn read_words(&mut self, addr: u32, count: usize) -> Result<Vec<u32>, DmiError> {
            Ok(vec![if addr == FLASH_CTLR { self.ctlr } else { 0 }; count])
        }
        fn write_words(&mut self, addr: u32, words: &[u32]) -> Result<(), DmiError> {
            if self.drop > 0 {
                self.drop -= 1;
                return Ok(());
            }
            let k = words[0];
            if addr == FLASH_KEYR && Self::key(&mut self.keyr, k, &mut self.jammed) {
                self.ctlr &= !CTLR_LOCK;
            }
            if addr == FLASH_MODEKEYR && Self::key(&mut self.modekeyr, k, &mut self.jammed) {
                self.ctlr &= !CTLR_FLOCK;
            }
            Ok(())
        }
        fn run_until_halt(
            &mut self,
            pc: u32,
            _: &[(u16, u32)],
            _: &[u16],
            _: Duration,
        ) -> Result<ch32rv_dmi::RunResult, DmiError> {
            Ok(ch32rv_dmi::RunResult {
                stopped: true,
                dpc: pc,
                elapsed_us: 0,
                outs: Vec::new(),
            })
        }
        fn halt(&mut self) -> Result<(), DmiError> {
            Ok(())
        }
        fn resume_once(&mut self) -> Result<(), DmiError> {
            Ok(())
        }
        fn reset(&mut self, _: ResetMode) -> Result<ch32rv_dmi::ResetResult, DmiError> {
            *self = Fpec {
                resets: self.resets + 1,
                drop: self.drop,
                ..Fpec::new(0)
            };
            Ok(ch32rv_dmi::ResetResult { pc: 0 })
        }
    }

    #[test]
    fn a_lost_key_write_is_met_with_a_reset_and_the_keys_again() {
        let mut ok = Fpec::new(0);
        unlock(&mut ok).unwrap();
        assert_eq!((ok.ctlr, ok.resets), (0, 0));
        // KEY1 lost: KEY2 alone jams the controller until a reset.
        let mut lost = Fpec::new(1);
        unlock(&mut lost).unwrap();
        assert_eq!((lost.ctlr, lost.resets), (0, 1));
        // Never getting through: StillLocked after 3 tries.
        let mut dead = Fpec::new(usize::MAX);
        assert!(matches!(
            unlock(&mut dead),
            Err(LoaderError::StillLocked(_))
        ));
        assert_eq!(dead.resets, 2);
    }

    #[test]
    fn the_loader_stops_at_its_done_offset() {
        // j main ; ebreak at +4
        assert_eq!(&LOADER[4..8], &0x0010_0073u32.to_le_bytes());
        assert!(LOADER.len() < (BUFFER - SRAM) as usize);
    }
}
