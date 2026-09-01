//! en: RISC-V Debug Module operations over a [`DtmAccess`] transport. Implemented against the
//! RISC-V External Debug Support spec and cross-checked with the QingKe debug manual; the
//! abstract-command encodings are transcribed from wlink (MIT/Apache-2.0) and verified on
//! live targets. Memory access uses program-buffer instructions (`lw`/`sw` + `ebreak`) the
//! same way wlink and minichlink do.
//!
//! ja: [`DtmAccess`] 上の RISC-V Debug Module 操作。RISC-V External Debug Support 仕様に
//! 沿い、QingKe debug manual と突き合わせた。abstract command の符号は wlink から転記し
//! 実機で裏を取る。memory access は program buffer 命令(`lw`/`sw` + `ebreak`)を使う。

use crate::{DmiError, DtmAccess};

/// en: Encode up to 3 host->target bytes into a data0 word with bit7 clear (the target's
/// poll() takes them). Empty input encodes to 0 (a bare ACK).
/// ja: host→target の最大 3 byte を bit7 クリアの data0 word に符号化(空なら 0=ただの ACK)。
fn encode_host_input(input: &[u8]) -> u32 {
    let n = input.len().min(3);
    let mut word = 0u32;
    for (i, &b) in input.iter().take(3).enumerate() {
        word |= u32::from(b) << (8 * (i + 1));
    }
    // Low byte carries the count (no bit7: this is a host->target frame).
    word | (n as u32)
}

// Debug Module register addresses (DMI address space).
const DMDATA0: u8 = 0x04;
const DMDATA1: u8 = 0x05;
const DMCONTROL: u8 = 0x10;
const DMSTATUS: u8 = 0x11;
const DMABSTRACTCS: u8 = 0x16;
const DMCOMMAND: u8 = 0x17;
const DMPROGBUF0: u8 = 0x20;
const DMPROGBUF1: u8 = 0x21;

// DMSTATUS bit masks.
const DMSTATUS_ALLRUNNING: u32 = 1 << 11;
const DMSTATUS_ANYRUNNING: u32 = 1 << 10;
const DMSTATUS_ALLHALTED: u32 = 1 << 9;
const DMSTATUS_ANYHALTED: u32 = 1 << 8;

// ABSTRACTCS fields.
const ABSTRACTCS_BUSY: u32 = 1 << 12;
const ABSTRACTCS_CMDERR_SHIFT: u32 = 8;
const ABSTRACTCS_CMDERR_MASK: u32 = 0x7 << ABSTRACTCS_CMDERR_SHIFT;

/// en: A named register for `dbg reg` (docs/cli.ja.md §4.4).
/// ja: `dbg reg` 用の名前付きレジスタ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegName {
    /// General-purpose register x0..x31 (abstract regno 0x1000 + n).
    Gpr(u8),
    /// Program counter, read as CSR dpc (0x7b1).
    Pc,
    /// Control/status register by number.
    Csr(u16),
}

impl RegName {
    /// The abstract-command register number.
    fn abstract_regno(self) -> u16 {
        match self {
            RegName::Gpr(n) => 0x1000 + u16::from(n),
            // dpc holds the PC while halted.
            RegName::Pc => 0x7b1,
            RegName::Csr(c) => c,
        }
    }
}

/// en: Debug Module driver. Borrows a [`DtmAccess`] transport for the duration of a session.
/// ja: Debug Module ドライバ。セッション中 [`DtmAccess`] transport を借用する。
pub struct DebugModule<'a, T: DtmAccess> {
    dtm: &'a mut T,
}

impl<'a, T: DtmAccess> DebugModule<'a, T> {
    pub fn new(dtm: &'a mut T) -> Self {
        Self { dtm }
    }

    fn write(&mut self, addr: u8, value: u32) -> Result<(), DmiError> {
        self.dtm.dmi_write(addr, value)
    }

    fn read(&mut self, addr: u8) -> Result<u32, DmiError> {
        self.dtm.dmi_read(addr)
    }

    /// Raw DMI read for diagnostics (`dbg dmi read`).
    pub fn raw_dmi_read(&mut self, addr: u8) -> Result<u32, DmiError> {
        self.read(addr)
    }

    /// Clear a sticky abstract-command error (cmderr) by writing 1s to the field.
    fn clear_cmderr(&mut self) -> Result<(), DmiError> {
        self.write(DMABSTRACTCS, ABSTRACTCS_CMDERR_MASK)
    }

    /// en: Wait until the abstract command is no longer busy, then surface cmderr.
    /// ja: abstract command の busy 解除を待ち、cmderr を返す。
    fn wait_abstract(&mut self) -> Result<(), DmiError> {
        for _ in 0..64 {
            let cs = self.read(DMABSTRACTCS)?;
            if cs & ABSTRACTCS_BUSY == 0 {
                let cmderr = (cs & ABSTRACTCS_CMDERR_MASK) >> ABSTRACTCS_CMDERR_SHIFT;
                if cmderr != 0 {
                    self.clear_cmderr()?;
                    return Err(DmiError::OperationFailed(format!("cmderr {cmderr}")));
                }
                return Ok(());
            }
        }
        Err(DmiError::Timeout)
    }

    /// en: True if the hart is halted (DMSTATUS all/any-halted).
    /// ja: hart が halt 状態か(DMSTATUS all/any-halted)。
    pub fn is_halted(&mut self) -> Result<bool, DmiError> {
        let s = self.read(DMSTATUS)?;
        Ok(s & DMSTATUS_ALLHALTED != 0 && s & DMSTATUS_ANYHALTED != 0)
    }

    /// en: True if the hart is running (DMSTATUS all/any-running). Used by `--confirm-run`.
    /// ja: hart が実行中か(DMSTATUS all/any-running)。`--confirm-run` で使う。
    pub fn is_running(&mut self) -> Result<bool, DmiError> {
        let s = self.read(DMSTATUS)?;
        Ok(s & DMSTATUS_ALLRUNNING != 0 && s & DMSTATUS_ANYRUNNING != 0)
    }

    /// en: Resume a halted hart (DMCONTROL resumereq). Best-effort ack check.
    /// ja: halt 中の hart を resume する(DMCONTROL resumereq)。
    pub fn resume(&mut self) -> Result<(), DmiError> {
        self.write(DMCONTROL, 0x4000_0001)?;
        Ok(())
    }

    /// en: Request a halt and wait for it (wlink `ensure_mcu_halt`). Idempotent.
    /// ja: halt を要求して待つ(wlink `ensure_mcu_halt`)。冪等。
    pub fn halt(&mut self) -> Result<(), DmiError> {
        if !self.is_halted()? {
            for _ in 0..64 {
                self.write(DMCONTROL, 0x8000_0001)?;
                if self.is_halted()? {
                    break;
                }
            }
        }
        // Clear the halt-request bit.
        self.write(DMCONTROL, 0x0000_0001)?;
        if self.is_halted()? {
            Ok(())
        } else {
            Err(DmiError::OperationFailed("hart did not halt".to_owned()))
        }
    }

    /// en: Read an abstract register (GPR/CSR/PC). The hart must be halted.
    /// ja: abstract register(GPR/CSR/PC)を読む。hart は halt 済みであること。
    pub fn read_reg(&mut self, reg: RegName) -> Result<u32, DmiError> {
        self.clear_cmderr()?;
        self.write(DMDATA0, 0)?;
        // access register, transfer, read (0x0022_0000 | regno).
        self.write(DMCOMMAND, 0x0022_0000 | u32::from(reg.abstract_regno()))?;
        self.wait_abstract()?;
        self.read(DMDATA0)
    }

    /// en: Write an abstract register (GPR/CSR/PC). The hart must be halted.
    /// ja: abstract register(GPR/CSR/PC)へ書く。hart は halt 済みであること。
    pub fn write_reg(&mut self, reg: RegName, value: u32) -> Result<(), DmiError> {
        self.clear_cmderr()?;
        self.write(DMDATA0, value)?;
        // access register, transfer, write (0x0023_0000 | regno).
        self.write(DMCOMMAND, 0x0023_0000 | u32::from(reg.abstract_regno()))?;
        self.wait_abstract()
    }

    /// en: Single-step one instruction (dcsr.step). The hart must be halted; it stays halted
    /// afterwards. dcsr is CSR 0x7b0, step is bit 2.
    /// ja: 1 命令 single-step(dcsr.step)。hart は halt 済みで、実行後も halt のまま。
    pub fn step(&mut self) -> Result<(), DmiError> {
        const DCSR: u16 = 0x7b0;
        const STEP_BIT: u32 = 1 << 2;
        let dcsr = self.read_reg(RegName::Csr(DCSR))?;
        self.write_reg(RegName::Csr(DCSR), dcsr | STEP_BIT)?;
        // Resume: the core executes exactly one instruction, then re-halts.
        self.resume()?;
        let mut halted = false;
        for _ in 0..64 {
            if self.is_halted()? {
                halted = true;
                break;
            }
        }
        // Clear the step bit regardless, so a later resume runs normally.
        let _ = self.write_reg(RegName::Csr(DCSR), dcsr & !STEP_BIT);
        if halted {
            Ok(())
        } else {
            Err(DmiError::OperationFailed(
                "did not re-halt after step".to_owned(),
            ))
        }
    }

    /// en: Read one 32-bit word from target memory via program buffer (`lw x6,0(x5)`).
    /// The hart must be halted. Transcribed from wlink `read_mem32`.
    /// ja: program buffer 経由で target メモリの 32bit を 1 word 読む(`lw x6,0(x5)`)。
    /// hart は halt 済みであること。wlink `read_mem32` から転記。
    pub fn read_mem32(&mut self, addr: u32) -> Result<u32, DmiError> {
        self.write(DMPROGBUF0, 0x0002_a303)?; // lw x6, 0(x5)
        self.write(DMPROGBUF1, 0x0010_0073)?; // ebreak
        self.write(DMDATA0, addr)?; // data0 <- address
        self.clear_cmderr()?;
        // x5 <- data0, then execute progbuf (postexec).
        self.write(DMCOMMAND, 0x0027_1005)?;
        self.wait_abstract()?;
        // data0 <- x6
        self.write(DMCOMMAND, 0x0022_1006)?;
        self.wait_abstract()?;
        self.read(DMDATA0)
    }

    /// en: One receive cycle of the ch32fun/minichlink DMDATA terminal (SerialDMDATA).
    /// The core keeps RUNNING - this only reads the DM data registers. Frame layout
    /// (target->host): data0 low byte = `0x80 | (count+4)`, upper 3 bytes = payload[0..3];
    /// data1 = payload[3..7]. Returns the payload and ACKs by clearing data0 (with the given
    /// host-input bytes, up to 3, for the reverse direction).
    /// ja: SerialDMDATA(minichlink -T)の受信 1 周期。core は running のまま DM data レジスタ
    /// のみ読む。frame は target→host: data0 下位 byte=`0x80|(count+4)`、上位 3B=payload、
    /// data1=残り。ACK は data0 を書いてクリア(host→target の入力を最大 3 byte 同載)。
    pub fn dmdata_poll(&mut self, host_input: &[u8]) -> Result<Option<Vec<u8>>, DmiError> {
        let d0 = self.read(DMDATA0)?;
        if d0 & 0x80 == 0 {
            // No target frame pending. If we have input to send, place it (bit7 clear).
            if !host_input.is_empty() {
                self.write(DMDATA0, encode_host_input(host_input))?;
            }
            return Ok(None);
        }
        // count is biased by 4 in the low 6 bits.
        let count = ((d0 & 0x3f).saturating_sub(4)) as usize;
        let d1 = self.read(DMDATA1)?;
        let bytes = [
            (d0 >> 8) as u8,
            (d0 >> 16) as u8,
            (d0 >> 24) as u8,
            d1 as u8,
            (d1 >> 8) as u8,
            (d1 >> 16) as u8,
            (d1 >> 24) as u8,
        ];
        let out = bytes[..count.min(7)].to_vec();
        // ACK: clear bit7; carry host input (if any) in the same word.
        self.write(DMDATA0, encode_host_input(host_input))?;
        Ok(Some(out))
    }

    /// en: Write one 32-bit word to target memory via program buffer (`sw x7,0(x5)`). The hart
    /// must be halted. Transcribed from wlink `write_mem32`.
    /// ja: program buffer 経由で target メモリへ 32bit を 1 word 書く(`sw x7,0(x5)`)。
    /// hart は halt 済みであること。wlink `write_mem32` から転記。
    pub fn write_mem32(&mut self, addr: u32, data: u32) -> Result<(), DmiError> {
        self.write(DMPROGBUF0, 0x0072_a023)?; // sw x7, 0(x5)
        self.write(DMPROGBUF1, 0x0010_0073)?; // ebreak
        self.write(DMDATA0, addr)?; // data0 <- address
        self.clear_cmderr()?;
        self.write(DMCOMMAND, 0x0023_1005)?; // x5 <- data0
        self.wait_abstract()?;
        self.write(DMDATA0, data)?; // data0 <- data
        self.clear_cmderr()?;
        self.write(DMCOMMAND, 0x0027_1007)?; // x7 <- data0 + postexec (sw)
        self.wait_abstract()
    }

    /// en: Write `data` to target memory starting at `addr`. Reads-modifies-writes the head
    /// and tail words to keep byte granularity. The hart must be halted.
    /// ja: `addr` から `data` を書く。端の word は read-modify-write で byte 単位を保つ。
    pub fn write_mem(&mut self, addr: u32, data: &[u8]) -> Result<(), DmiError> {
        if data.is_empty() {
            return Ok(());
        }
        let mut a = addr;
        let mut rest = data;
        // Head: if misaligned, patch within the first word.
        while !rest.is_empty() {
            let word_addr = a & !3;
            let off = (a - word_addr) as usize;
            if off == 0 && rest.len() >= 4 {
                let w = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
                self.write_mem32(word_addr, w)?;
                a = a
                    .checked_add(4)
                    .ok_or(DmiError::OperationFailed("overflow".into()))?;
                rest = &rest[4..];
            } else {
                // Partial word: read, splice, write.
                let mut w = self.read_mem32(word_addr)?.to_le_bytes();
                let n = (4 - off).min(rest.len());
                w[off..off + n].copy_from_slice(&rest[..n]);
                self.write_mem32(word_addr, u32::from_le_bytes(w))?;
                a = word_addr
                    .checked_add(4)
                    .ok_or(DmiError::OperationFailed("overflow".into()))?;
                rest = &rest[n..];
            }
        }
        Ok(())
    }

    /// en: Read `len` bytes starting at `addr` (word-aligned reads; caller trims). Halts first
    /// is the caller's responsibility.
    /// ja: `addr` から `len` byte を読む(word 単位。端数は呼び出し側で調整)。
    pub fn read_mem(&mut self, addr: u32, len: u32) -> Result<Vec<u8>, DmiError> {
        let mut out = Vec::with_capacity(len as usize);
        let start = addr & !3;
        let end = addr.saturating_add(len);
        let mut a = start;
        while a < end {
            let word = self.read_mem32(a)?;
            out.extend_from_slice(&word.to_le_bytes());
            a = a
                .checked_add(4)
                .ok_or(DmiError::OperationFailed("address overflow".to_owned()))?;
        }
        let head = (addr - start) as usize;
        out.drain(0..head);
        out.truncate(len as usize);
        Ok(out)
    }
}
