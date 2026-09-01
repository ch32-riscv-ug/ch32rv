//! en: A gdbstub `Target` for a CH32 RISC-V core reached over a [`DtmAccess`] transport.
//! Supports registers, memory read/write, halt/continue/single-step, and breakpoints. A `break`
//! GDB requests as software (Z0) is placed by the cheapest working mechanism, in order: a RAM
//! `ebreak` memory-patch; else a hardware execute trigger when the core has a free slot (no
//! wear); else a flash software breakpoint that rewrites the containing flash page (works on
//! triggerless cores, at the cost of flash wear). Hardware breakpoints use the RISC-V trigger
//! module (measured: 4 slots on QingKe V4F/CH32V307 and V4C/CH32X035, live-fire confirmed; 0 on
//! V4B/CH32V203, V2A/CH32V003, V3/CH32V103 - detected dynamically). Flash software breakpoints
//! need a verified FLASH-controller page profile (256-byte families for now; V003/V103 are a
//! follow-up). Attach does not modify flash; flash breakpoints are removed and pages restored on
//! detach (docs/cli.ja.md §4.6).
//! ja: [`DtmAccess`] 上の CH32 RISC-V core 用 gdbstub Target。register・memory R/W・
//! halt/continue/step・breakpoint。GDB が software(Z0)で要求した `break` は、動く中で最も安い
//! 手段の順(RAM `ebreak` patch → 空き HW trigger〔摩耗なし〕→ flash page 書き換えの flash SW
//! breakpoint〔trigger 無し core でも効くが flash 摩耗あり〕)で張る。hardware breakpoint は
//! RISC-V trigger module(実測: V4F/V307・V4C/X035 は 4 slot 実発火、V4B/V203・V2A/V003・V3/V103 は
//! 0。動的検出)。flash SW breakpoint は検証済み FLASH-controller profile が必要(現状 256byte
//! family、V003/V103 は後続)。attach で flash を書き換えず、flash breakpoint は detach 時に外して
//! page を復元する。

use ch32rv_dmi::{DebugModule, DmiError, DtmAccess, FlashProgMode, RegName};
use gdbstub::common::Signal;
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep,
    SingleThreadSingleStepOps,
};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwBreakpoint, HwBreakpointOps, SwBreakpoint, SwBreakpointOps,
};
use gdbstub::target::{Target, TargetError, TargetResult};

use crate::arch::{Rv32, Rv32CoreRegs};

/// A software breakpoint: the address and the original bytes we overwrote with `ebreak`.
struct SwBp {
    addr: u32,
    original: Vec<u8>,
}

/// A hardware breakpoint: the trigger slot it occupies and the address it watches.
struct HwBp {
    slot: u32,
    addr: u32,
}

/// A flash software breakpoint: an `ebreak` patched into flash-resident code by rewriting its
/// page. `len` is the patched instruction size (2 for RVC, 4 otherwise).
struct FlashBp {
    addr: u32,
    len: usize,
}

/// A flash page we manage: its pristine content (before any breakpoint) and what we last
/// programmed into it, so repeated set/clear that yields identical content skips the rewrite.
struct FlashPage {
    page_addr: u32,
    pristine: Vec<u8>,
    current: Vec<u8>,
}

/// en: gdbstub target that OWNS its transport `T` (owning, not borrowing, keeps the type free
/// of a lifetime so it fits `BlockingEventLoop::Target`). Recover the transport with
/// [`Ch32Target::into_inner`] to detach cleanly afterwards.
/// ja: transport `T` を所有する gdbstub target(所有にすることでライフタイムが付かず
/// `BlockingEventLoop::Target` に収まる)。後始末は [`Ch32Target::into_inner`] で回収する。
pub struct Ch32Target<T: DtmAccess> {
    dtm: T,
    breakpoints: Vec<SwBp>,
    hw_breakpoints: Vec<HwBp>,
    hw_trigger_count: u32,
    /// Number of integer GPRs the core exposes: 16 on RV32E (misa.E, e.g. CH32V003), else 32.
    /// Reading a non-existent GPR via an abstract command raises cmderr, so we must not touch
    /// x16..x31 on an RV32E hart.
    gpr_count: u8,
    /// Fast-page size for flash software breakpoints, or None when this family's FLASH-controller
    /// profile is not verified (so a flash breakpoint is refused rather than risked).
    flash_page_size: Option<u32>,
    /// The family's fast-program mechanism (meaningful only when `flash_page_size` is Some).
    flash_prog_mode: FlashProgMode,
    flash_bps: Vec<FlashBp>,
    flash_pages: Vec<FlashPage>,
}

impl<T: DtmAccess> Ch32Target<T> {
    /// en: Find a free hardware trigger slot (none used twice). Returns None when the core has
    /// no trigger module or all slots are taken.
    /// ja: 空いている HW trigger slot を探す。trigger 無し/全 slot 使用中なら None。
    fn alloc_hw_slot(&self) -> Option<u32> {
        (0..self.hw_trigger_count).find(|s| !self.hw_breakpoints.iter().any(|b| b.slot == *s))
    }

    /// en: Wrap a transport and halt the core so GDB attaches to a stopped target. `flash` is the
    /// FLASH-controller profile for this family (fast-page size + program mode, from
    /// `ch32rv_flash::flash_controller_profile`), or None to refuse flash software breakpoints.
    /// ja: transport を包んで halt。`flash` はこの family の FLASH-controller profile(fast page
    /// サイズ + program mode。None なら flash SW breakpoint を拒否)。
    pub fn new(dtm: T, flash: Option<(u32, FlashProgMode)>) -> Result<Self, DmiError> {
        let mut t = Self {
            dtm,
            breakpoints: Vec::new(),
            hw_breakpoints: Vec::new(),
            hw_trigger_count: 0,
            gpr_count: 32,
            flash_page_size: flash.map(|(p, _)| p),
            flash_prog_mode: flash.map(|(_, m)| m).unwrap_or(FlashProgMode::PgStart),
            flash_bps: Vec::new(),
            flash_pages: Vec::new(),
        };
        t.dm().halt()?;
        // Make `ebreak` halt into debug mode so software breakpoints stop the core.
        let _ = t.dm().enable_ebreak_debug();
        t.hw_trigger_count = t.dm().hw_trigger_count();
        // misa.E (bit 4) marks RV32E: only x0..x15 exist. Reading x16.. would raise cmderr.
        const MISA: u16 = 0x301;
        const MISA_E: u32 = 1 << 4;
        if let Ok(misa) = t.dm().read_reg(RegName::Csr(MISA))
            && misa & MISA_E != 0
        {
            t.gpr_count = 16;
        }
        Ok(t)
    }

    /// Number of hardware trigger slots the core exposes (0 on V003/V2A).
    pub fn hw_trigger_count(&self) -> u32 {
        self.hw_trigger_count
    }

    fn dm(&mut self) -> DebugModule<'_, T> {
        DebugModule::new(&mut self.dtm)
    }

    /// True if the core is halted right now.
    pub fn is_halted(&mut self) -> Result<bool, DmiError> {
        self.dm().is_halted()
    }

    /// Request a halt (used for Ctrl-C).
    pub fn halt(&mut self) -> Result<(), DmiError> {
        self.dm().halt()
    }

    /// Recover the owned transport (to detach after the session).
    pub fn into_inner(self) -> T {
        self.dtm
    }

    /// True if this core supports flash software breakpoints (verified FLASH-controller profile).
    pub fn flash_breakpoints_supported(&self) -> bool {
        self.flash_page_size.is_some()
    }

    /// en: Restore every managed flash page to its pristine content (removing any `ebreak` still
    /// patched in). Call before detaching so an interrupted session never leaves a breakpoint
    /// baked into flash. Best-effort.
    /// ja: 管理中の flash page をすべて pristine に戻す(残った `ebreak` を消す)。detach 前に
    /// 呼び、途中終了しても flash に breakpoint が焼き付かないようにする。best-effort。
    pub fn restore_flash_breakpoints(&mut self) {
        let Some(page) = self.flash_page_size else {
            return;
        };
        self.flash_bps.clear();
        let addrs: Vec<u32> = self.flash_pages.iter().map(|p| p.page_addr).collect();
        for page_addr in addrs {
            let _ = self.reprogram_flash_page(page, page_addr);
        }
        self.flash_pages.clear();
    }

    /// en: Map a code address into the physical code-flash window. Programs run from the low
    /// alias (0x0000_0000 mirrors flash), but the FLASH controller must be given the real flash
    /// address (0x0800_0000+off). Reads work through either mirror; writes must use the physical.
    /// ja: コード番地を物理 code-flash 窓へ写す。実行は低位 alias(0x0000_0000=flash の鏡)だが、
    /// FLASH controller には実 flash 番地(0x0800_0000+off)を渡す必要がある。
    fn flash_phys(addr: u32) -> u32 {
        const FLASH_BASE: u32 = 0x0800_0000;
        if addr < FLASH_BASE {
            FLASH_BASE + addr
        } else {
            addr
        }
    }

    /// The `ebreak` patch bytes for an instruction of size `len` (2 = RVC c.ebreak, else ebreak).
    fn ebreak_patch(len: usize) -> &'static [u8] {
        if len == 2 {
            &[0x02, 0x90] // c.ebreak (0x9002, little-endian)
        } else {
            &[0x73, 0x00, 0x10, 0x00] // ebreak (0x00100073)
        }
    }

    /// en: Rewrite `page_addr` to hold its pristine content plus every currently-active flash
    /// breakpoint in that page. Skips the erase/program when the page already matches (so a
    /// redundant set/clear round-trip costs no flash wear). Returns Ok(true) if it wrote.
    /// ja: `page_addr` を「pristine + その page の全 flash breakpoint」の内容に書き直す。既に一致
    /// なら erase/program を省く(無駄な書き換え=摩耗を避ける)。書いたら Ok(true)。
    fn reprogram_flash_page(&mut self, page: u32, page_addr: u32) -> Result<bool, DmiError> {
        let Some(idx) = self
            .flash_pages
            .iter()
            .position(|p| p.page_addr == page_addr)
        else {
            return Ok(false);
        };
        let mut desired = self.flash_pages[idx].pristine.clone();
        for bp in &self.flash_bps {
            if bp.addr & !(page - 1) == page_addr {
                let off = (bp.addr - page_addr) as usize;
                let patch = Self::ebreak_patch(bp.len);
                if off + bp.len <= desired.len() {
                    desired[off..off + bp.len].copy_from_slice(patch);
                }
            }
        }
        if self.flash_pages[idx].current == desired {
            return Ok(false); // no net change: skip the flash write
        }
        let phys = Self::flash_phys(page_addr);
        let mode = self.flash_prog_mode;
        {
            let mut dm = self.dm();
            dm.flash_page_erase(phys)?;
            dm.flash_program_page(phys, &desired, mode)?;
        }
        self.flash_pages[idx].current = desired;
        Ok(true)
    }

    /// en: Add a flash software breakpoint by rewriting the containing page. Returns Ok(false)
    /// if this family has no verified flash profile. The hart must be halted.
    /// ja: 該当 page を書き換えて flash SW breakpoint を張る。未対応 family は Ok(false)。
    fn add_flash_breakpoint(&mut self, addr: u32, len: usize) -> TargetResult<bool, Self> {
        let Some(page) = self.flash_page_size else {
            return Ok(false);
        };
        let page_addr = addr & !(page - 1);
        // Load the pristine page the first time we touch it (it has no breakpoint yet).
        if !self.flash_pages.iter().any(|p| p.page_addr == page_addr) {
            let content = self
                .dm()
                .read_mem(page_addr, page)
                .map_err(TargetError::Fatal)?;
            self.flash_pages.push(FlashPage {
                page_addr,
                pristine: content.clone(),
                current: content,
            });
        }
        self.flash_bps.push(FlashBp { addr, len });
        self.reprogram_flash_page(page, page_addr)
            .map_err(TargetError::Fatal)?;
        // Verify the ebreak actually landed (flash programming can silently fail on protect).
        let back = self
            .dm()
            .read_mem(addr, len as u32)
            .map_err(TargetError::Fatal)?;
        if back != Self::ebreak_patch(len) {
            // Roll back: drop this bp and restore the page.
            self.flash_bps.pop();
            let _ = self.reprogram_flash_page(page, page_addr);
            return Ok(false);
        }
        Ok(true)
    }

    /// en: Remove a flash software breakpoint, restoring the page (dropping the managed page once
    /// it holds no more breakpoints). Returns Ok(false) if `addr` was not a flash breakpoint.
    /// ja: flash SW breakpoint を外して page を復元(その page の breakpoint が無くなれば管理解除)。
    fn remove_flash_breakpoint(&mut self, addr: u32) -> TargetResult<bool, Self> {
        let Some(page) = self.flash_page_size else {
            return Ok(false);
        };
        let Some(pos) = self.flash_bps.iter().position(|b| b.addr == addr) else {
            return Ok(false);
        };
        let page_addr = addr & !(page - 1);
        self.flash_bps.remove(pos);
        self.reprogram_flash_page(page, page_addr)
            .map_err(TargetError::Fatal)?;
        // If nothing else lives in this page, stop managing it (it is now pristine again).
        if !self
            .flash_bps
            .iter()
            .any(|b| b.addr & !(page - 1) == page_addr)
        {
            self.flash_pages.retain(|p| p.page_addr != page_addr);
        }
        Ok(true)
    }
}

impl<T: DtmAccess> Target for Ch32Target<T> {
    type Arch = Rv32;
    type Error = DmiError;

    #[inline(always)]
    fn base_ops(&mut self) -> gdbstub::target::ext::base::BaseOps<'_, Self::Arch, Self::Error> {
        gdbstub::target::ext::base::BaseOps::SingleThread(self)
    }

    #[inline(always)]
    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }
}

impl<T: DtmAccess> SingleThreadBase for Ch32Target<T> {
    fn read_registers(&mut self, regs: &mut Rv32CoreRegs) -> TargetResult<(), Self> {
        let gpr_count = self.gpr_count;
        let mut dm = self.dm();
        regs.x = [0; 32];
        // On RV32E only x0..x15 exist; leave x16..x31 as zero (touching them raises cmderr).
        for i in 1..gpr_count {
            regs.x[i as usize] = dm.read_reg(RegName::Gpr(i)).map_err(TargetError::Fatal)?;
        }
        regs.pc = dm.read_reg(RegName::Pc).map_err(TargetError::Fatal)?;
        Ok(())
    }

    fn write_registers(&mut self, regs: &Rv32CoreRegs) -> TargetResult<(), Self> {
        let gpr_count = self.gpr_count;
        let mut dm = self.dm();
        for i in 1..gpr_count {
            dm.write_reg(RegName::Gpr(i), regs.x[i as usize])
                .map_err(TargetError::Fatal)?;
        }
        dm.write_reg(RegName::Pc, regs.pc)
            .map_err(TargetError::Fatal)?;
        Ok(())
    }

    fn read_addrs(&mut self, start: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        let bytes = self
            .dm()
            .read_mem(start, data.len() as u32)
            .map_err(TargetError::Fatal)?;
        let n = bytes.len().min(data.len());
        data[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn write_addrs(&mut self, start: u32, data: &[u8]) -> TargetResult<(), Self> {
        self.dm()
            .write_mem(start, data)
            .map_err(TargetError::Fatal)?;
        Ok(())
    }

    #[inline(always)]
    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl<T: DtmAccess> SingleThreadResume for Ch32Target<T> {
    fn resume(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.dm().resume()
    }

    #[inline(always)]
    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl<T: DtmAccess> SingleThreadSingleStep for Ch32Target<T> {
    fn step(&mut self, _signal: Option<Signal>) -> Result<(), Self::Error> {
        self.dm().step()
    }
}

impl<T: DtmAccess> Breakpoints for Ch32Target<T> {
    #[inline(always)]
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    #[inline(always)]
    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        // Only advertise HW breakpoints when the core actually has trigger slots.
        if self.hw_trigger_count > 0 {
            Some(self)
        } else {
            None
        }
    }
}

impl<T: DtmAccess> HwBreakpoint for Ch32Target<T> {
    fn add_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let Some(slot) = self.alloc_hw_slot() else {
            return Ok(false); // out of trigger slots
        };
        self.dm()
            .set_hw_breakpoint(slot, addr)
            .map_err(TargetError::Fatal)?;
        self.hw_breakpoints.push(HwBp { slot, addr });
        Ok(true)
    }

    fn remove_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if let Some(pos) = self.hw_breakpoints.iter().position(|b| b.addr == addr) {
            let bp = self.hw_breakpoints.remove(pos);
            self.dm()
                .clear_hw_breakpoint(bp.slot)
                .map_err(TargetError::Fatal)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl<T: DtmAccess> SwBreakpoint for Ch32Target<T> {
    /// en: Set a breakpoint GDB asked for as "software" (Z0). We first try a RAM memory-patch
    /// (`ebreak`); if the write does not stick - the classic case being a flash address, where
    /// the DM `sw` silently no-ops - we transparently fall back to a hardware execute trigger
    /// when the core has a free slot. That makes plain `break` work on flash-resident code for
    /// V4-class cores (which expose triggers) without the risk of rewriting flash. Cores with no
    /// trigger module (V003/V103) can still set RAM breakpoints; a flash breakpoint there returns
    /// unsupported (a flash-patch approach is a follow-up).
    /// ja: GDB が Z0(software)で要求した breakpoint。まず RAM の memory patch(`ebreak`)を
    /// 試し、書き込みが定着しない(典型は flash 番地。DM の `sw` が無反応)場合は、空き HW slot が
    /// あれば HW execute trigger へ透過的にフォールバックし(flash 書き換えの危険なし)、trigger が
    /// 無ければ page 書き換えの flash software breakpoint へフォールバックする。これで trigger を
    /// 持たない core(V203 等)でも flash 上のコードに通常 `break` が効く(flash 書き換えの摩耗あり)。
    /// 順序は RAM → HW trigger → flash-patch。どれも不可なら未対応を返す。
    fn add_sw_breakpoint(&mut self, addr: u32, kind: usize) -> TargetResult<bool, Self> {
        // kind is the instruction size GDB expects (2 for RVC, 4 otherwise).
        let (patch, len): (&[u8], usize) = if kind == 2 {
            (&[0x02, 0x90], 2) // c.ebreak (0x9002, little-endian)
        } else {
            (&[0x73, 0x00, 0x10, 0x00], 4) // ebreak (0x00100073)
        };
        // Try a RAM memory-patch first. Scope the DM borrow so we can touch other fields after.
        let (stuck, original) = {
            let mut dm = self.dm();
            let original = dm.read_mem(addr, len as u32).map_err(TargetError::Fatal)?;
            dm.write_mem(addr, patch).map_err(TargetError::Fatal)?;
            // The patch lands only in writable memory; a flash `sw` silently no-ops.
            let back = dm.read_mem(addr, len as u32).map_err(TargetError::Fatal)?;
            let stuck = back == patch;
            if !stuck {
                let _ = dm.write_mem(addr, &original); // restore best-effort
            }
            (stuck, original)
        };
        if stuck {
            self.breakpoints.push(SwBp { addr, original });
            return Ok(true);
        }
        // Flash (or otherwise unwritable): prefer a free hardware trigger (no wear)...
        if let Some(slot) = self.alloc_hw_slot() {
            self.dm()
                .set_hw_breakpoint(slot, addr)
                .map_err(TargetError::Fatal)?;
            self.hw_breakpoints.push(HwBp { slot, addr });
            return Ok(true);
        }
        // ...otherwise fall back to a flash software breakpoint (page rewrite) when supported.
        self.add_flash_breakpoint(addr, len)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if let Some(pos) = self.breakpoints.iter().position(|b| b.addr == addr) {
            let bp = self.breakpoints.remove(pos);
            self.dm()
                .write_mem(addr, &bp.original)
                .map_err(TargetError::Fatal)?;
            return Ok(true);
        }
        // It may have been satisfied by a hardware-trigger fallback (flash address).
        if let Some(pos) = self.hw_breakpoints.iter().position(|b| b.addr == addr) {
            let bp = self.hw_breakpoints.remove(pos);
            self.dm()
                .clear_hw_breakpoint(bp.slot)
                .map_err(TargetError::Fatal)?;
            return Ok(true);
        }
        // Or by a flash software breakpoint (page rewrite).
        self.remove_flash_breakpoint(addr)
    }
}
