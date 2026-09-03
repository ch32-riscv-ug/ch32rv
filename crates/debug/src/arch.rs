//! en: A minimal RISC-V RV32 architecture for gdbstub: 32 integer GPRs plus the PC, all u32,
//! in the order GDB's `riscv:rv32` core.xml expects (x0..x31, then pc). FPU/CSR registers are
//! not exposed yet (docs/architecture.ja.md §1.3 notes V4F FPU needs a custom Arch later).
//! ja: gdbstub 用の最小 RISC-V RV32 定義。GPR 32 本 + PC(すべて u32)を GDB の core.xml 順
//! (x0..x31, pc)で並べる。FPU/CSR は未対応(V4F FPU は将来の課題)。

use core::num::NonZeroUsize;

use gdbstub::arch::{Arch, RegId, Registers};

/// RV32 core register file: x0..x31 and pc.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Rv32CoreRegs {
    pub x: [u32; 32],
    pub pc: u32,
}

impl Registers for Rv32CoreRegs {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for r in self.x.iter().chain(core::iter::once(&self.pc)) {
            for b in r.to_le_bytes() {
                write_byte(Some(b));
            }
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        // 33 registers x 4 bytes = 132 bytes expected.
        if bytes.len() < 33 * 4 {
            return Err(());
        }
        for (i, chunk) in bytes.chunks(4).enumerate().take(33) {
            if chunk.len() < 4 {
                break;
            }
            let v = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if i < 32 {
                self.x[i] = v;
            } else {
                self.pc = v;
            }
        }
        Ok(())
    }
}

/// Register identifier: a GPR index 0..31, or the PC (id 32).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rv32RegId {
    Gpr(u8),
    Pc,
}

impl RegId for Rv32RegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        let size = NonZeroUsize::new(4);
        match id {
            0..=31 => Some((Rv32RegId::Gpr(id as u8), size)),
            32 => Some((Rv32RegId::Pc, size)),
            _ => None,
        }
    }

    fn to_raw_id(&self) -> Option<usize> {
        Some(match self {
            Rv32RegId::Gpr(n) => *n as usize,
            Rv32RegId::Pc => 32,
        })
    }
}

/// The RV32 architecture marker for gdbstub (zero-variant, used at the type level only).
pub enum Rv32 {}

impl Arch for Rv32 {
    type Usize = u32;
    type Registers = Rv32CoreRegs;
    type BreakpointKind = usize;
    type RegId = Rv32RegId;

    fn target_description_xml() -> Option<&'static str> {
        // Lets GDB auto-detect the architecture without `set architecture`.
        Some(r#"<target version="1.0"><architecture>riscv:rv32</architecture></target>"#)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn core_regs_serialize_roundtrip() {
        let mut regs = Rv32CoreRegs::default();
        for (i, x) in regs.x.iter_mut().enumerate() {
            *x = 0x1000_0000 + i as u32;
        }
        regs.pc = 0x0800_0000;

        // Serialize: 33 registers x 4 bytes, little-endian, x0..x31 then pc.
        let mut bytes = Vec::new();
        regs.gdb_serialize(|b| {
            if let Some(b) = b {
                bytes.push(b);
            }
        });
        assert_eq!(bytes.len(), 33 * 4);
        assert_eq!(&bytes[0..4], &0x1000_0000u32.to_le_bytes()); // x0
        assert_eq!(&bytes[128..132], &0x0800_0000u32.to_le_bytes()); // pc last

        // Round-trip back.
        let mut back = Rv32CoreRegs::default();
        back.gdb_deserialize(&bytes).unwrap();
        assert_eq!(back, regs);
    }

    #[test]
    fn deserialize_rejects_short_input() {
        let mut regs = Rv32CoreRegs::default();
        assert!(regs.gdb_deserialize(&[0u8; 131]).is_err());
    }

    #[test]
    fn reg_id_mapping() {
        assert_eq!(
            Rv32RegId::from_raw_id(0).map(|(r, _)| r),
            Some(Rv32RegId::Gpr(0))
        );
        assert_eq!(
            Rv32RegId::from_raw_id(31).map(|(r, _)| r),
            Some(Rv32RegId::Gpr(31))
        );
        assert_eq!(
            Rv32RegId::from_raw_id(32).map(|(r, _)| r),
            Some(Rv32RegId::Pc)
        );
        assert!(Rv32RegId::from_raw_id(33).is_none());
    }
}
