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

    /// en: Make `ebreak` enter Debug Mode (halt) instead of trapping, by setting dcsr
    /// ebreakm/ebreaks/ebreaku. Required for `ebreak`-based software breakpoints to stop the
    /// core. The hart must be halted.
    /// ja: `ebreak` を例外 trap でなく Debug Mode 突入(halt)にする(dcsr の
    /// ebreakm/ebreaks/ebreaku を立てる)。ebreak ベースの SW breakpoint に必須。halt 済みで。
    pub fn enable_ebreak_debug(&mut self) -> Result<(), DmiError> {
        const DCSR: u16 = 0x7b0;
        const EBREAK_BITS: u32 = (1 << 15) | (1 << 13) | (1 << 12); // ebreakm | ebreaks | ebreaku
        let dcsr = self.read_reg(RegName::Csr(DCSR))?;
        self.write_reg(RegName::Csr(DCSR), dcsr | EBREAK_BITS)
    }

    // ---- Hardware breakpoints (RISC-V trigger module; QingKe V3/V4, not V2A/V003) ----

    /// en: Number of usable hardware triggers, probed by writing `tselect` and reading it back
    /// (the CSR saturates at the highest index). Returns 0 when there is no trigger module
    /// (e.g. CH32V003) or it is inaccessible. The hart must be halted.
    /// ja: 使える HW trigger 数を `tselect` の書き戻しで調べる(最大 index で飽和する)。
    /// trigger module が無い(V003 等)場合は 0。hart は halt 済みであること。
    pub fn hw_trigger_count(&mut self) -> u32 {
        const TSELECT: u16 = 0x7a0;
        const TDATA1: u16 = 0x7a1;
        // A slot is usable only if tselect accepts the index AND tdata1 reports a non-zero
        // trigger type (type field [31:28]). On cores without a real trigger module the CSRs
        // read back 0, so this avoids advertising breakpoints that never fire.
        let mut count = 0u32;
        for i in 0..16u32 {
            if self.write_reg(RegName::Csr(TSELECT), i).is_err() {
                break;
            }
            let sel_ok = matches!(self.read_reg(RegName::Csr(TSELECT)), Ok(v) if v == i);
            if !sel_ok {
                break;
            }
            let type_field = self
                .read_reg(RegName::Csr(TDATA1))
                .map(|t| t >> 28)
                .unwrap_or(0);
            if type_field == 0 || type_field == 0xf {
                // type 0 = none, 0xf = disabled/unavailable.
                break;
            }
            count = i + 1;
        }
        let _ = self.write_reg(RegName::Csr(TSELECT), 0);
        count
    }

    /// en: Program hardware trigger `slot` as an execute breakpoint at `addr` (mcontrol type 2:
    /// action=enter-debug, match=exact, execute, m/s/u). The hart must be halted.
    /// ja: HW trigger `slot` を `addr` の実行 breakpoint に設定(mcontrol type2)。halt 済みで。
    pub fn set_hw_breakpoint(&mut self, slot: u32, addr: u32) -> Result<(), DmiError> {
        const TSELECT: u16 = 0x7a0;
        const TDATA1: u16 = 0x7a1;
        const TDATA2: u16 = 0x7a2;
        // mcontrol: type=2, dmode=1, action=1 (enter debug), match=0 (exact),
        // m/s/u = 1, execute = 1.
        const MCONTROL: u32 = 0x2800_0000 // type(2)<<28 | dmode<<27
            | 0x0000_1000 // action=1
            | 0x0000_0040 // m
            | 0x0000_0010 // s
            | 0x0000_0008 // u
            | 0x0000_0004; // execute
        self.write_reg(RegName::Csr(TSELECT), slot)?;
        // Clear before reprogramming.
        self.write_reg(RegName::Csr(TDATA1), 0)?;
        self.write_reg(RegName::Csr(TDATA2), addr)?;
        self.write_reg(RegName::Csr(TDATA1), MCONTROL)?;
        Ok(())
    }

    /// Read back a trigger slot's tdata1/tdata2 (for diagnostics). The hart must be halted.
    pub fn read_trigger(&mut self, slot: u32) -> Result<(u32, u32), DmiError> {
        const TSELECT: u16 = 0x7a0;
        const TDATA1: u16 = 0x7a1;
        const TDATA2: u16 = 0x7a2;
        self.write_reg(RegName::Csr(TSELECT), slot)?;
        let t1 = self.read_reg(RegName::Csr(TDATA1))?;
        let t2 = self.read_reg(RegName::Csr(TDATA2))?;
        Ok((t1, t2))
    }

    /// Clear hardware trigger `slot`. The hart must be halted.
    pub fn clear_hw_breakpoint(&mut self, slot: u32) -> Result<(), DmiError> {
        const TSELECT: u16 = 0x7a0;
        const TDATA1: u16 = 0x7a1;
        self.write_reg(RegName::Csr(TSELECT), slot)?;
        self.write_reg(RegName::Csr(TDATA1), 0)?;
        Ok(())
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

    /// en: Store one 16-bit halfword to target memory via program buffer (`sh x7,0(x5)`). The
    /// hart must be halted. Needed for CH32V103 standard flash programming, which latches the
    /// FLASH controller on each 16-bit store (a 32-bit `sw` does not program it correctly).
    /// ja: program buffer 経由で 16bit halfword を store(`sh x7,0(x5)`)。CH32V103 の標準 flash
    /// programming は 16bit store ごとに controller が latch するため必要(32bit `sw` では不可)。
    pub fn write_mem16(&mut self, addr: u32, data: u16) -> Result<(), DmiError> {
        self.write(DMPROGBUF0, 0x0072_9023)?; // sh x7, 0(x5)
        self.write(DMPROGBUF1, 0x0010_0073)?; // ebreak
        self.write(DMDATA0, addr)?; // data0 <- address
        self.clear_cmderr()?;
        self.write(DMCOMMAND, 0x0023_1005)?; // x5 <- data0
        self.wait_abstract()?;
        self.write(DMDATA0, u32::from(data))?; // data0 <- data
        self.clear_cmderr()?;
        self.write(DMCOMMAND, 0x0027_1007)?; // x7 <- data0 + postexec (sh)
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

    // ---- Direct FLASH-controller programming over DMI (fast page erase / program) ----
    //
    // en: These drive the memory-mapped FLASH controller (0x4002_2000) through `read_mem32`/
    // `write_mem32`, exactly as the QingKe reference manual and wlink's reference block
    // describe. Unlike the WCH-Link stub write path, this is page-granular (256-byte fast
    // pages on V20x/V30x/X035/CH643/L103), so it supports surgical read-modify-write of a
    // single page - the basis for `erase --range` and flash software breakpoints. Verified
    // live on CH32V203/V307/X035 (2026-09-01). Note: an erased cell reads back as the
    // `0xe339e339` placeholder over the LinkE, not 0xff, so callers must not blank-check for
    // 0xff; trust the controller's completion status instead. The hart must be halted.
    // ja: memory-mapped FLASH controller(0x4002_2000)を read_mem32/write_mem32 で駆動する
    // (QingKe manual と wlink 参照ブロックの手順)。WCH-Link stub 経路と違い page 単位
    // (V20x/V30x/X035/CH643/L103 は 256byte fast page)なので 1 page の read-modify-write が
    // でき、`erase --range` と flash SW breakpoint の土台になる。CH32V203/V307/X035 で実機確認。
    // 消去済みセルは LinkE 経由だと 0xff でなく `0xe339e339` を返すので 0xff の blank-check は
    // 不可、controller の完了ステータスで判定する。hart は halt 済みであること。

    /// en: Unlock the FLASH controller (LOCK + FLOCK) with the standard key sequence. Idempotent.
    /// ja: FLASH controller を鍵手順で unlock(LOCK + FLOCK)。冪等。
    fn flash_unlock(&mut self) -> Result<(), DmiError> {
        let ctlr = self.read_mem32(FLASH_CTLR)?;
        if ctlr & (FLASH_LOCK | FLASH_FLOCK) == 0 {
            return Ok(()); // already unlocked
        }
        self.write_mem32(FLASH_KEYR, FLASH_KEY1)?;
        self.write_mem32(FLASH_KEYR, FLASH_KEY2)?;
        self.write_mem32(FLASH_MODEKEYR, FLASH_KEY1)?;
        self.write_mem32(FLASH_MODEKEYR, FLASH_KEY2)?;
        Ok(())
    }

    /// Re-lock the FLASH controller (LOCK + FLOCK).
    fn flash_lock(&mut self) -> Result<(), DmiError> {
        let ctlr = self.read_mem32(FLASH_CTLR)?;
        self.write_mem32(FLASH_CTLR, ctlr | FLASH_LOCK | FLASH_FLOCK)
    }

    /// Spin until every bit in `mask` reads 0 in the FLASH status register, then return it.
    fn flash_wait(&mut self, mask: u32) -> Result<u32, DmiError> {
        for _ in 0..4000 {
            let v = self.read_mem32(FLASH_STATR)?;
            if v & mask == 0 {
                return Ok(v);
            }
        }
        Err(DmiError::Timeout)
    }

    /// en: CH32V103-only "commit" side effect the EVT driver performs after every fast erase /
    /// buffer load: read the word at `(addr & !3) ^ 0x1000` and write it to the undocumented
    /// FLASH register 0x4002_2034. Without it, a V103 erase/program silently does nothing.
    /// ja: CH32V103 専用。EVT ドライバが fast erase / buffer load の後に必ず行う "commit" 副作用:
    /// `(addr & !3) ^ 0x1000` の word を読み、未文書 FLASH レジスタ 0x4002_2034 へ書く。これが
    /// 無いと V103 の erase/program は無反応になる。
    fn flash_v103_commit(&mut self, addr: u32) -> Result<(), DmiError> {
        let v = self.read_mem32((addr & 0xFFFF_FFFC) ^ 0x0000_1000)?;
        self.write_mem32(FLASH_MAGIC_V103, v)
    }

    /// en: Fast-page-erase the page at `addr` (page-aligned) with FTER + STRT. `mode` selects the
    /// family quirks: [`FlashProgMode::V103`] adds the mandatory commit side effect. The hart
    /// must be halted.
    /// ja: `addr` の fast page を FTER + STRT で消去。`mode` で family 差を選ぶ(V103 は commit 副作用
    /// が必須)。halt 済みで。
    pub fn flash_page_erase(&mut self, addr: u32, mode: FlashProgMode) -> Result<(), DmiError> {
        self.flash_unlock()?;
        if self.read_mem32(FLASH_STATR)? & FLASH_BUSY != 0 {
            return Err(DmiError::OperationFailed("flash busy".to_owned()));
        }
        let ctlr = self.read_mem32(FLASH_CTLR)?;
        self.write_mem32(FLASH_CTLR, ctlr | FLASH_FTER)?;
        self.write_mem32(FLASH_ADDR, addr)?;
        let ctlr = self.read_mem32(FLASH_CTLR)?;
        self.write_mem32(FLASH_CTLR, ctlr | FLASH_STRT)?;
        let statr = self.flash_wait(FLASH_BUSY)?;
        // Clear FTER regardless, then surface a write-protect error.
        let ctlr = self.read_mem32(FLASH_CTLR)?;
        self.write_mem32(FLASH_CTLR, ctlr & !FLASH_FTER)?;
        self.write_mem32(FLASH_STATR, statr)?; // write 1s to clear EOP/WPRERR
        if mode == FlashProgMode::V103 {
            self.flash_v103_commit(addr)?;
        }
        self.flash_lock()?;
        if statr & FLASH_WPRERR != 0 {
            return Err(DmiError::OperationFailed(
                "flash write-protected".to_owned(),
            ));
        }
        Ok(())
    }

    /// en: Fast-page-program `data` at `addr` (page-aligned, `data.len()` == the page size, and
    /// the page already erased), using the family's programming `mode`. The hart must be halted.
    /// Three programming mechanisms exist: [`FlashProgMode::PgStart`] (V20x/V30x - load words,
    /// then PGSTART), [`FlashProgMode::Buffered`] (V003/X035/L103 - buffer reset, then per-word
    /// write+BUFLOAD, then STRT), and [`FlashProgMode::V103`] (CH32V103 - standard 16-bit halfword
    /// programming via CR_PG with the mandatory commit side effect per word). All verified live.
    /// ja: `addr` へ `data` を program(page 境界・page サイズ長・消去済みが前提)。`mode` で分岐:
    /// PgStart(V20x/V30x)、Buffered(V003/X035/L103)、V103(標準 16bit halfword + commit)。全て実機確認済み。
    pub fn flash_program_page(
        &mut self,
        addr: u32,
        data: &[u8],
        mode: FlashProgMode,
    ) -> Result<(), DmiError> {
        if !data.len().is_multiple_of(4) {
            return Err(DmiError::OperationFailed(
                "page data length must be a multiple of 4".to_owned(),
            ));
        }
        self.flash_unlock()?;
        if self.read_mem32(FLASH_STATR)? & FLASH_BUSY != 0 {
            return Err(DmiError::OperationFailed("flash busy".to_owned()));
        }
        if mode == FlashProgMode::V103 {
            // Standard programming via 16-bit halfword stores. Set PG once for the whole page,
            // store each halfword (waiting for BSY), then clear PG and do the commit once - this
            // is verified equivalent to the EVT per-word sequence but far fewer DMI round-trips,
            // which keeps a single Z0 insert under GDB's remote timeout.
            let ctlr = self.read_mem32(FLASH_CTLR)?;
            self.write_mem32(FLASH_CTLR, ctlr | FLASH_PG)?;
            for (i, word) in data.chunks(4).enumerate() {
                let a = addr + i as u32 * 4;
                self.write_mem16(a, u16::from_le_bytes([word[0], word[1]]))?;
                self.flash_wait(FLASH_BUSY)?;
                self.write_mem16(a + 2, u16::from_le_bytes([word[2], word[3]]))?;
                self.flash_wait(FLASH_BUSY)?;
            }
            let ctlr = self.read_mem32(FLASH_CTLR)?;
            self.write_mem32(FLASH_CTLR, ctlr & !FLASH_PG)?;
            self.flash_v103_commit(addr)?;
            self.flash_lock()?;
            return Ok(());
        }
        match mode {
            FlashProgMode::V103 => unreachable!("handled above"),
            FlashProgMode::PgStart => {
                self.write_mem32(FLASH_CTLR, FLASH_FTPG)?;
                for (i, word) in data.chunks(4).enumerate() {
                    let w = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
                    self.write_mem32(addr + i as u32 * 4, w)?;
                    self.flash_wait(FLASH_WRBUSY)?;
                }
                self.write_mem32(FLASH_CTLR, FLASH_FTPG | FLASH_PGSTART)?;
            }
            FlashProgMode::Buffered => {
                // Reset the page buffer, then load each word (write + BUFLOAD), then start.
                self.write_mem32(FLASH_CTLR, FLASH_FTPG)?;
                self.write_mem32(FLASH_CTLR, FLASH_FTPG | FLASH_BUFRST)?;
                self.flash_wait(FLASH_BUSY)?;
                for (i, word) in data.chunks(4).enumerate() {
                    let w = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
                    self.write_mem32(addr + i as u32 * 4, w)?;
                    self.write_mem32(FLASH_CTLR, FLASH_FTPG | FLASH_BUFLOAD)?;
                    self.flash_wait(FLASH_BUSY)?;
                }
                self.write_mem32(FLASH_ADDR, addr)?;
                self.write_mem32(FLASH_CTLR, FLASH_FTPG | FLASH_STRT)?;
            }
        }
        let statr = self.flash_wait(FLASH_BUSY)?;
        self.write_mem32(FLASH_CTLR, 0)?; // clear FTPG (and any BUF bits)
        self.write_mem32(FLASH_STATR, statr)?;
        self.flash_lock()?;
        if statr & FLASH_WPRERR != 0 {
            return Err(DmiError::OperationFailed(
                "flash write-protected".to_owned(),
            ));
        }
        Ok(())
    }

    /// en: Program the 16 option bytes (0x1FFF_F800: 8 halfwords of value+complement). The hart must
    /// be halted. `bytes` is the full 16-byte image exactly as `read_mem(0x1FFF_F800, 16)` returns
    /// it; each halfword is written verbatim, so the CALLER owns the complement bytes. CAUTION: a
    /// wrong RDPR (byte 0) or WRPR here enables read/write protection - an all-0xff option area means
    /// read protection ON, so RDPR is programmed first. Transcribed from minichlink's option path.
    /// ja: 16 byte の option bytes(0x1FFF_F800、value+complement の 8 halfword)を program。hart は
    /// halt 済み。`bytes` は `read_mem(0x1FFF_F800,16)` の 16 byte をそのまま。各 halfword を verbatim で
    /// 書くので complement は呼び出し側の責任。注意: RDPR(byte0)/WRPR を誤ると保護 ON(全 0xff = 読み
    /// 出し保護 ON)なので RDPR を最初に書く。minichlink の option 書込経路から転記。
    pub fn flash_program_option_bytes(&mut self, bytes: &[u8; 16]) -> Result<(), DmiError> {
        const OB_BASE: u32 = 0x1FFF_F800;
        const FLASH_OBKEYR: u32 = 0x4002_2008; // option-write unlock (STM32F1-style OPTKEYR)
        const OPTPG: u32 = 1 << 4;
        const OPTER: u32 = 1 << 5;
        const OPTWRE: u32 = 1 << 9;
        // Unlock main flash and the option-write enable (OBKEYR sets OPTWRE); MODEKEYR is harmless.
        self.write_mem32(FLASH_KEYR, FLASH_KEY1)?;
        self.write_mem32(FLASH_KEYR, FLASH_KEY2)?;
        self.write_mem32(FLASH_OBKEYR, FLASH_KEY1)?;
        self.write_mem32(FLASH_OBKEYR, FLASH_KEY2)?;
        self.write_mem32(FLASH_MODEKEYR, FLASH_KEY1)?;
        self.write_mem32(FLASH_MODEKEYR, FLASH_KEY2)?;
        if self.read_mem32(FLASH_CTLR)? & OPTWRE == 0 {
            return Err(DmiError::OperationFailed(
                "option-byte unlock failed (OPTWRE not set)".to_owned(),
            ));
        }
        // Erase all option bytes (they must be blank before programming).
        self.write_mem32(FLASH_CTLR, OPTER | OPTWRE)?;
        self.write_mem32(FLASH_CTLR, OPTER | OPTWRE | FLASH_STRT)?;
        let statr = self.flash_wait(FLASH_BUSY)?;
        if statr & FLASH_WPRERR != 0 {
            self.write_mem32(FLASH_CTLR, 0)?;
            return Err(DmiError::OperationFailed(
                "option-byte erase: write-protect error".to_owned(),
            ));
        }
        // Program the 8 halfwords; RDPR (halfword 0) first, so read protection is re-established
        // immediately after the erase blanked it.
        for i in 0..8u32 {
            self.write_mem32(FLASH_CTLR, OPTPG | OPTWRE)?;
            self.write_mem32(FLASH_CTLR, OPTPG | OPTWRE | FLASH_STRT)?;
            let lo = bytes[(i * 2) as usize];
            let hi = bytes[(i * 2 + 1) as usize];
            self.write_mem16(OB_BASE + i * 2, u16::from_le_bytes([lo, hi]))?;
            let statr = self.flash_wait(FLASH_BUSY)?;
            if statr & FLASH_WPRERR != 0 {
                self.write_mem32(FLASH_CTLR, 0)?;
                return Err(DmiError::OperationFailed(format!(
                    "option-byte program: write-protect error at halfword {i}"
                )));
            }
        }
        self.write_mem32(FLASH_CTLR, 0)?; // clear OPTPG / OPTWRE
        Ok(())
    }
}

/// en: Which fast-program mechanism a family uses (see [`DebugModule::flash_program_page`]).
/// ja: family が使う fast-program 方式([`DebugModule::flash_program_page`] 参照)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashProgMode {
    /// V20x/V30x: load words, then set PGSTART.
    PgStart,
    /// V003/X035/L103: buffer reset, per-word write + BUFLOAD, then STRT.
    Buffered,
    /// CH32V103: standard 16-bit halfword programming (CR_PG) + the mandatory commit side effect.
    V103,
}

// FLASH controller registers (memory-mapped at 0x4002_2000) and bit masks. Transcribed from
// the QingKe reference manual / wlink / WCH EVT drivers; see the FLASH-controller section above.
const FLASH_KEYR: u32 = 0x4002_2004;
const FLASH_STATR: u32 = 0x4002_200C;
const FLASH_CTLR: u32 = 0x4002_2010;
const FLASH_ADDR: u32 = 0x4002_2014;
const FLASH_MODEKEYR: u32 = 0x4002_2024;
// Undocumented CH32V103 "commit" register (EVT ch32v10x_flash.c writes it after each op).
const FLASH_MAGIC_V103: u32 = 0x4002_2034;
const FLASH_KEY1: u32 = 0x4567_0123;
const FLASH_KEY2: u32 = 0xCDEF_89AB;
// CTLR bits.
const FLASH_PG: u32 = 1 << 0; // standard programming enable (V103)
const FLASH_STRT: u32 = 1 << 6; // start erase
const FLASH_LOCK: u32 = 1 << 7;
const FLASH_FLOCK: u32 = 1 << 15; // fast-mode lock
const FLASH_FTPG: u32 = 1 << 16; // fast page program
const FLASH_FTER: u32 = 1 << 17; // fast page erase
const FLASH_BUFLOAD: u32 = 1 << 18; // load one word into the page buffer (buffered mode)
const FLASH_BUFRST: u32 = 1 << 19; // reset the page buffer (buffered mode)
const FLASH_PGSTART: u32 = 1 << 21; // start fast page program (PgStart mode)
// STATR bits.
const FLASH_BUSY: u32 = 1 << 0;
const FLASH_WRBUSY: u32 = 1 << 1;
const FLASH_WPRERR: u32 = 1 << 4;
