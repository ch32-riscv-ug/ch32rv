//! en: A gdbstub `Target` for a CH32 RISC-V core reached over a [`DtmAccess`] transport.
//! Supports registers, memory read/write, halt/continue/single-step, and software breakpoints
//! (memory-patched `ebreak`, so RAM only for now - flash breakpoints need the QingKe trigger
//! module and are a follow-up). Attach does not modify flash (docs/cli.ja.md §4.6).
//! ja: [`DtmAccess`] 上の CH32 RISC-V core 用 gdbstub Target。register・memory R/W・
//! halt/continue/step・software breakpoint(memory patch の `ebreak`。当面 RAM のみ)。
//! attach で flash を書き換えない。

use ch32rv_dmi::{DebugModule, DmiError, DtmAccess, RegName};
use gdbstub::common::Signal;
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep,
    SingleThreadSingleStepOps,
};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, SwBreakpoint, SwBreakpointOps,
};
use gdbstub::target::{Target, TargetError, TargetResult};

use crate::arch::{Rv32, Rv32CoreRegs};

/// A software breakpoint: the address and the original bytes we overwrote with `ebreak`.
struct SwBp {
    addr: u32,
    original: Vec<u8>,
}

/// en: gdbstub target that OWNS its transport `T` (owning, not borrowing, keeps the type free
/// of a lifetime so it fits `BlockingEventLoop::Target`). Recover the transport with
/// [`Ch32Target::into_inner`] to detach cleanly afterwards.
/// ja: transport `T` を所有する gdbstub target(所有にすることでライフタイムが付かず
/// `BlockingEventLoop::Target` に収まる)。後始末は [`Ch32Target::into_inner`] で回収する。
pub struct Ch32Target<T: DtmAccess> {
    dtm: T,
    breakpoints: Vec<SwBp>,
}

impl<T: DtmAccess> Ch32Target<T> {
    /// Wrap a transport and halt the core so GDB attaches to a stopped target.
    pub fn new(dtm: T) -> Result<Self, DmiError> {
        let mut t = Self {
            dtm,
            breakpoints: Vec::new(),
        };
        t.dm().halt()?;
        Ok(t)
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
        let mut dm = self.dm();
        regs.x[0] = 0;
        for i in 1..32u8 {
            regs.x[i as usize] = dm.read_reg(RegName::Gpr(i)).map_err(TargetError::Fatal)?;
        }
        regs.pc = dm.read_reg(RegName::Pc).map_err(TargetError::Fatal)?;
        Ok(())
    }

    fn write_registers(&mut self, regs: &Rv32CoreRegs) -> TargetResult<(), Self> {
        let mut dm = self.dm();
        for i in 1..32u8 {
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
}

impl<T: DtmAccess> SwBreakpoint for Ch32Target<T> {
    fn add_sw_breakpoint(&mut self, addr: u32, kind: usize) -> TargetResult<bool, Self> {
        // kind is the instruction size GDB expects (2 for RVC, 4 otherwise).
        let (patch, len): (&[u8], usize) = if kind == 2 {
            (&[0x02, 0x90], 2) // c.ebreak (0x9002, little-endian)
        } else {
            (&[0x73, 0x00, 0x10, 0x00], 4) // ebreak (0x00100073)
        };
        let mut dm = self.dm();
        let original = dm.read_mem(addr, len as u32).map_err(TargetError::Fatal)?;
        dm.write_mem(addr, patch).map_err(TargetError::Fatal)?;
        // Verify the patch landed (flash writes via write_mem silently no-op -> reject).
        let back = dm.read_mem(addr, len as u32).map_err(TargetError::Fatal)?;
        if back != patch {
            // Restore best-effort and report unsupported (e.g. flash address).
            let _ = dm.write_mem(addr, &original);
            return Ok(false);
        }
        self.breakpoints.push(SwBp { addr, original });
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if let Some(pos) = self.breakpoints.iter().position(|b| b.addr == addr) {
            let bp = self.breakpoints.remove(pos);
            self.dm()
                .write_mem(addr, &bp.original)
                .map_err(TargetError::Fatal)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
