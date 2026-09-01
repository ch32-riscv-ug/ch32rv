//! en: A gdbstub `Target` for a CH32 RISC-V core reached over a [`DtmAccess`] transport.
//! Supports registers, memory read/write, halt/continue/single-step, and breakpoints. Software
//! breakpoints are memory-patched `ebreak` in RAM; when the patch would target flash (the `sw`
//! silently no-ops) we transparently fall back to a hardware execute trigger if the core has a
//! free slot, so plain `break` works on flash for trigger-capable cores. Hardware breakpoints
//! use the RISC-V trigger module (measured: 4 slots on QingKe V4C/CH32X035, live-fire confirmed;
//! 0 on QingKe V2A/CH32V003 and V3/CH32V103, so those get RAM breakpoints only and a flash
//! breakpoint needs the flash-patch follow-up). Attach does not modify flash (docs/cli.ja.md §4.6).
//! ja: [`DtmAccess`] 上の CH32 RISC-V core 用 gdbstub Target。register・memory R/W・
//! halt/continue/step・breakpoint。software breakpoint は RAM への `ebreak` memory patch。
//! patch 先が flash(`sw` が無反応)なら、空き slot があれば HW execute trigger へ透過的に
//! フォールバックし、trigger を持つ core では flash でも通常 `break` が効く。hardware
//! breakpoint は RISC-V trigger module(実測: QingKe V4C/CH32X035 は 4 slot・実発火確認、
//! QingKe V2A/CH32V003 と V3/CH32V103 は 0 = RAM breakpoint のみ、flash は flash-patch 後続)。
//! attach で flash を書き換えない。

use ch32rv_dmi::{DebugModule, DmiError, DtmAccess, RegName};
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
}

impl<T: DtmAccess> Ch32Target<T> {
    /// en: Find a free hardware trigger slot (none used twice). Returns None when the core has
    /// no trigger module or all slots are taken.
    /// ja: 空いている HW trigger slot を探す。trigger 無し/全 slot 使用中なら None。
    fn alloc_hw_slot(&self) -> Option<u32> {
        (0..self.hw_trigger_count).find(|s| !self.hw_breakpoints.iter().any(|b| b.slot == *s))
    }

    /// Wrap a transport and halt the core so GDB attaches to a stopped target.
    pub fn new(dtm: T) -> Result<Self, DmiError> {
        let mut t = Self {
            dtm,
            breakpoints: Vec::new(),
            hw_breakpoints: Vec::new(),
            hw_trigger_count: 0,
            gpr_count: 32,
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
    /// 試し、書き込みが定着しない(典型は flash 番地。DM の `sw` が無反応)場合は、空き slot が
    /// あれば HW execute trigger へ透過的にフォールバックする。これで trigger を持つ V4 系では
    /// flash 上のコードにも通常 `break` が効く(flash 書き換えの危険なし)。trigger の無い
    /// V003/V103 は RAM breakpoint のみ可、flash は未対応を返す(flash-patch は後続)。
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
        // Flash (or otherwise unwritable): fall back to a hardware trigger if one is free.
        if let Some(slot) = self.alloc_hw_slot() {
            self.dm()
                .set_hw_breakpoint(slot, addr)
                .map_err(TargetError::Fatal)?;
            self.hw_breakpoints.push(HwBp { slot, addr });
            return Ok(true);
        }
        Ok(false)
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
        Ok(false)
    }
}
