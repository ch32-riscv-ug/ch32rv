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
    /// en: An RV32E hart (x0..x15 only): GDB then works with 16 GPRs and pc (68 bytes, as it does
    /// for an RV32E ELF and as [`RV32E_TARGET_XML`] says), not 33 registers.
    /// ja: RV32E の hart(x0..x15 のみ)。GDB は GPR 16 本と pc(68 byte)で扱う。
    pub rv32e: bool,
}

/// en: The target description of an RV32E hart: x0..x15 and pc (remote number 32, as GDB numbers
/// the RISC-V pc), so GDB expects 17 registers whether or not it has the ELF.
/// ja: RV32E の hart の target description。x0..x15 と pc(GDB の RISC-V の pc の番号 32)。ELF の
/// 有無にかかわらず GDB は 17 本と見る。
pub const RV32E_TARGET_XML: &str = concat!(
    r#"<?xml version="1.0"?><!DOCTYPE target SYSTEM "gdb-target.dtd">"#,
    r#"<target version="1.0"><architecture>riscv:rv32</architecture>"#,
    r#"<feature name="org.gnu.gdb.riscv.cpu">"#,
    r#"<reg name="zero" bitsize="32" type="int" regnum="0"/>"#,
    r#"<reg name="ra" bitsize="32" type="code_ptr"/>"#,
    r#"<reg name="sp" bitsize="32" type="data_ptr"/>"#,
    r#"<reg name="gp" bitsize="32" type="data_ptr"/>"#,
    r#"<reg name="tp" bitsize="32" type="data_ptr"/>"#,
    r#"<reg name="t0" bitsize="32" type="int"/>"#,
    r#"<reg name="t1" bitsize="32" type="int"/>"#,
    r#"<reg name="t2" bitsize="32" type="int"/>"#,
    r#"<reg name="fp" bitsize="32" type="data_ptr"/>"#,
    r#"<reg name="s1" bitsize="32" type="int"/>"#,
    r#"<reg name="a0" bitsize="32" type="int"/>"#,
    r#"<reg name="a1" bitsize="32" type="int"/>"#,
    r#"<reg name="a2" bitsize="32" type="int"/>"#,
    r#"<reg name="a3" bitsize="32" type="int"/>"#,
    r#"<reg name="a4" bitsize="32" type="int"/>"#,
    r#"<reg name="a5" bitsize="32" type="int"/>"#,
    r#"<reg name="pc" bitsize="32" type="code_ptr" regnum="32"/>"#,
    r#"</feature></target>"#,
);

impl Registers for Rv32CoreRegs {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        let gprs = if self.rv32e { 16 } else { 32 };
        for r in self.x[..gprs].iter().chain(core::iter::once(&self.pc)) {
            for b in r.to_le_bytes() {
                write_byte(Some(b));
            }
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        // 33 registers x 4 bytes = 132 bytes, or on an RV32E hart 17 x 4 = 68.
        let gprs = if bytes.len() >= 33 * 4 {
            32
        } else if bytes.len() >= 17 * 4 {
            16
        } else {
            return Err(());
        };
        self.rv32e = gprs == 16;
        for (i, chunk) in bytes.as_chunks::<4>().0.iter().enumerate().take(gprs + 1) {
            let v = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if i < gprs {
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
        assert!(regs.gdb_deserialize(&[0u8; 67]).is_err());
    }

    #[test]
    fn rv32e_has_16_gprs_and_pc() {
        let mut regs = Rv32CoreRegs {
            rv32e: true,
            pc: 0x3cc,
            ..Default::default()
        };
        regs.x[15] = 0xf;
        let mut bytes = Vec::new();
        regs.gdb_serialize(|b| bytes.extend(b));
        assert_eq!(bytes.len(), 17 * 4);
        assert_eq!(&bytes[60..64], &0xfu32.to_le_bytes()); // x15
        assert_eq!(&bytes[64..68], &0x3ccu32.to_le_bytes()); // pc
        let mut back = Rv32CoreRegs::default();
        back.gdb_deserialize(&bytes).unwrap();
        assert_eq!(back, regs);
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
