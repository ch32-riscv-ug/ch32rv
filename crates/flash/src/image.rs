//! en: Firmware image parsing: ELF / Intel HEX / UF2 / raw bin into flash [`Segment`]s.
//! ELF32 program headers are parsed directly (we need p_paddr / LMA, which is where .data
//! init lives in flash while its VMA is RAM); Intel HEX and UF2 have small in-house parsers.
//! Addresses are normalized into the flash window like wlink's `fix_code_flash_start`, so a
//! 0x0000_0000-based ELF/HEX lands at 0x0800_0000.
//!
//! ja: firmware image のパース。ELF / Intel HEX / UF2 / 生 bin を flash の [`Segment`] 群へ。
//! ELF32 の program header を直接読む(必要なのは p_paddr = LMA。.data の初期値は flash 上の
//! LMA にあり VMA は RAM)。HEX/UF2 は自前 parser。アドレスは wlink の fix_code_flash_start と
//! 同様に flash 窓へ正規化するので、0x0000_0000 ベースの ELF/HEX は 0x0800_0000 に載る。

use ch32rv_contract::policy::ImageFormat;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImageError {
    #[error("empty image")]
    Empty,
    #[error("could not determine image format; pass --format")]
    UnknownFormat,
    #[error("truncated or malformed {0} image")]
    Malformed(&'static str),
    #[error("Intel HEX: bad record at line {line}: {reason}")]
    Hex { line: usize, reason: String },
    #[error("no loadable flash content found")]
    NoFlashContent,
    #[error("segment at {addr:#010x} ({len} bytes) is outside the flash window")]
    OutOfFlash { addr: u32, len: usize },
}

/// One contiguous chunk of bytes to program at `addr` (absolute flash address).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub addr: u32,
    pub data: Vec<u8>,
}

/// A parsed image: one or more flash segments, sorted by address.
#[derive(Debug, Clone, Default)]
pub struct Image {
    pub segments: Vec<Segment>,
}

impl Image {
    /// Total programmed byte count across all segments.
    pub fn total_len(&self) -> usize {
        self.segments.iter().map(|s| s.data.len()).sum()
    }

    /// Lowest segment address, if any.
    pub fn base_addr(&self) -> Option<u32> {
        self.segments.iter().map(|s| s.addr).min()
    }

    /// en: Parse `bytes` in the given format. `bin_offset` is the load address for raw bin
    /// (defaults to `code_flash_start`). `code_flash_start` is the family's flash base.
    /// ja: `bytes` を format に従いパースする。bin の load 先は `bin_offset`(既定は flash 先頭)。
    pub fn parse(
        bytes: &[u8],
        format: ImageFormat,
        bin_offset: Option<u32>,
        code_flash_start: u32,
    ) -> Result<Image, ImageError> {
        if bytes.is_empty() {
            return Err(ImageError::Empty);
        }
        let fmt = match format {
            ImageFormat::Auto => detect(bytes).ok_or(ImageError::UnknownFormat)?,
            other => other,
        };
        let img = match fmt {
            ImageFormat::Elf => parse_elf(bytes, code_flash_start)?,
            ImageFormat::Hex => parse_ihex(bytes, code_flash_start)?,
            ImageFormat::Uf2 => parse_uf2(bytes, code_flash_start)?,
            ImageFormat::Bin => Image {
                segments: vec![Segment {
                    addr: bin_offset.unwrap_or(code_flash_start),
                    data: bytes.to_vec(),
                }],
            },
            ImageFormat::Auto => unreachable!("resolved above"),
        };
        if img.segments.is_empty() || img.total_len() == 0 {
            return Err(ImageError::NoFlashContent);
        }
        Ok(img)
    }

    /// en: The blocks to actually program, derived from [`Self::segments`]: runs that are
    /// contiguous, overlapping, or merely share an `align`-sized programming unit are merged into
    /// one, and every block is grown to whole `align` units (padded with `0xff`). Programming a
    /// `0xff` byte cannot clear a cell, so the padding is a no-op on the silicon - but without this
    /// the programmer would be handed a block at an address the flash cannot start a write at.
    ///
    /// This is what a linked ELF needs: the `.data` initialiser lives at its own LMA immediately
    /// after `.text` (`LOAD 0x08000000+0x2bac` then `LOAD` at `0x08002bac`, 16 bytes), so it
    /// arrives as a second segment starting mid-page. Handed to the WCH-Link flash stub as-is, that
    /// write is silently dropped and the image verifies as mismatched at exactly the `.data` LMA;
    /// on the FLASH-controller path it would re-program a page that the previous segment had
    /// already written. Merged, both paths see one block per page and program it once.
    ///
    /// ja: 実際に書き込む単位。[`Self::segments`] のうち連続・重複・同じ `align` 単位に載るものを
    /// 1 つにまとめ、各 block を `align` の整数倍へ広げる(埋めは `0xff`)。`0xff` の書込はセルを
    /// 落とせないので silicon 上は no-op だが、これが無いと flash が書き始められない番地の block を
    /// programmer に渡すことになる。link 済み ELF ではこれが必要: `.data` の初期値は `.text` 直後の
    /// LMA に置かれ、page 途中から始まる 2 つ目の segment として届く。WCH-Link の flash stub に
    /// そのまま渡すとその書込は黙って捨てられ、`.data` の LMA ちょうどで verify mismatch になる。
    /// FLASH-controller 経路では、前の segment が書いた page を二度書きすることになる。
    pub fn program_blocks(&self, align: u32) -> Vec<Segment> {
        let align = align.max(1) as u64;
        let unit_start = |a: u32| (u64::from(a) / align) * align;
        let unit_end = |a: u64| a.div_ceil(align) * align;

        let mut sorted = self.segments.clone();
        sorted.sort_by_key(|s| s.addr);
        let mut out: Vec<Segment> = Vec::new();
        for seg in sorted {
            if seg.data.is_empty() {
                continue;
            }
            let seg_end = u64::from(seg.addr) + seg.data.len() as u64;
            match out.last_mut() {
                // The next segment starts inside the block this one already occupies (including
                // its 0xff tail padding): extend the current block and overlay the bytes.
                Some(last)
                    if unit_start(seg.addr)
                        <= unit_end(u64::from(last.addr) + last.data.len() as u64) =>
                {
                    let base = u64::from(last.addr);
                    let want = (unit_end(seg_end) - base) as usize;
                    if last.data.len() < want {
                        last.data.resize(want, 0xff);
                    }
                    let at = (u64::from(seg.addr) - base) as usize;
                    last.data[at..at + seg.data.len()].copy_from_slice(&seg.data);
                }
                _ => {
                    let base = unit_start(seg.addr);
                    let mut data = vec![0xffu8; (unit_end(seg_end) - base) as usize];
                    let at = (u64::from(seg.addr) - base) as usize;
                    data[at..at + seg.data.len()].copy_from_slice(&seg.data);
                    out.push(Segment {
                        // `base` is derived from a u32 address rounded down, so it fits.
                        addr: base as u32,
                        data,
                    });
                }
            }
        }
        out
    }

    /// en: The whole image as ONE contiguous block of whole `align` units, gaps between segments
    /// filled with `0xff`. This is what the WCH-Link flash-stub path must program: the stub is a
    /// full-region programmer and only the *first* `write_flash` of a session takes effect - a
    /// second one is acknowledged by the probe (`41 01 01 04`, and the stub upload and every
    /// Program sub-command reply as usual) and programs nothing at all, so every block after the
    /// first is silently lost. Measured on a CH32V307VCT6 + WCH-LinkE with a two-block image
    /// (0x08000000 and 0x08004600, both page-aligned): the second block's page read back erased,
    /// and the data was not written anywhere else either. A detach/attach or a soft reset between
    /// the two writes does not help. wch-protocols `protocols/pc-to-link.ja.md#flash` records the same boundary
    /// from the other side: the stub path is for full-region programming after an erase.
    ///
    /// The filler cannot damage anything it covers - programming `0xff` clears no cell - but the
    /// caller must erase the whole span, not just the segments, because the span is what gets
    /// programmed. Returns None for an empty image.
    ///
    /// ja: image 全体を **1 つの連続 block**(`align` の整数倍、segment 間の隙間は `0xff`)にする。
    /// WCH-Link の flash stub 経路はこれが必須: stub は全 region 書込器で、**1 セッションで効くのは
    /// 最初の `write_flash` だけ**。2 本目は probe が ack を返し(`41 01 01 04`、stub upload も各
    /// Program サブコマンドも正常応答)ながら**何も焼かない**ので、2 つ目以降の block が黙って消える。
    /// CH32V307VCT6 + WCH-LinkE で 2 block image(0x08000000 と 0x08004600、ともに page 整列)で実測:
    /// 2 つ目の page は消去状態のまま、データはどこにも書かれていない。間に detach/attach や soft
    /// reset を挟んでも変わらない。wch-protocols `protocols/pc-to-link.ja.md#flash` が逆側から同じ境界を記録して
    /// いる(stub 経路は消去後の全 region 書込用)。埋めの `0xff` はセルを落とせないので覆った領域を
    /// 壊さないが、**焼くのは span 全体なので、呼び出し側は segment ではなく span を消去する**こと。
    pub fn program_span(&self, align: u32) -> Option<Segment> {
        let blocks = self.program_blocks(align);
        let (first, last) = (blocks.first()?, blocks.last()?);
        let base = first.addr;
        let end = u64::from(last.addr) + last.data.len() as u64;
        let mut data = vec![0xffu8; (end - u64::from(base)) as usize];
        for b in &blocks {
            let at = (b.addr - base) as usize;
            data[at..at + b.data.len()].copy_from_slice(&b.data);
        }
        Some(Segment { addr: base, data })
    }

    /// en: Reject segments that fall outside `[code_flash_start, code_flash_start+size)`.
    /// ja: flash 範囲外の segment を弾く。
    pub fn check_within_flash(
        &self,
        code_flash_start: u32,
        flash_size: u32,
    ) -> Result<(), ImageError> {
        let end = code_flash_start.saturating_add(flash_size);
        for s in &self.segments {
            let seg_end = s.addr.saturating_add(s.data.len() as u32);
            if s.addr < code_flash_start || seg_end > end {
                return Err(ImageError::OutOfFlash {
                    addr: s.addr,
                    len: s.data.len(),
                });
            }
        }
        Ok(())
    }
}

/// Detect the image format from a magic prefix.
fn detect(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(b"\x7fELF") {
        Some(ImageFormat::Elf)
    } else if bytes.len() >= 8
        && bytes[0] == b':'
        && bytes[1..].iter().take(8).all(|b| is_hex_or_crlf(*b))
    {
        Some(ImageFormat::Hex)
    } else if bytes.len() >= 8 && bytes[0..4] == [0x55, 0x46, 0x32, 0x0a] {
        Some(ImageFormat::Uf2)
    } else {
        // No recognizable magic: caller must pass --format for raw bin.
        None
    }
}

fn is_hex_or_crlf(b: u8) -> bool {
    b.is_ascii_hexdigit() || b == b'\r' || b == b'\n'
}

/// en: Map a link-time address into the flash window (wlink `fix_code_flash_start`).
/// 0x0000_0000-based -> code_flash_start; already-absolute flash stays put; RAM stays RAM.
/// ja: link アドレスを flash 窓へ写す。0 ベースは flash 先頭へ、絶対 flash はそのまま、RAM は RAM。
fn fix_flash_addr(addr: u32, code_flash_start: u32) -> u32 {
    let a = code_flash_start.wrapping_add(addr);
    if a >= 0x1000_0000 {
        a.wrapping_sub(0x0800_0000)
    } else {
        a
    }
}

/// Keep only segments whose fixed address lies in the flash window (drop RAM segments).
fn flash_only(mut segs: Vec<Segment>, code_flash_start: u32) -> Image {
    segs.retain(|s| !s.data.is_empty() && s.addr >= code_flash_start && s.addr < 0x1000_0000);
    segs.sort_by_key(|s| s.addr);
    Image { segments: segs }
}

// ---- ELF32 ----

fn u16le(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(o)?,
        *b.get(o + 1)?,
        *b.get(o + 2)?,
        *b.get(o + 3)?,
    ]))
}

fn parse_elf(bytes: &[u8], code_flash_start: u32) -> Result<Image, ImageError> {
    // ELF32, little-endian only (CH32 RISC-V). e_ident: 0x7f 'E' 'L' 'F', class=1 (32), data=1 (LE).
    if bytes.len() < 52 || &bytes[0..4] != b"\x7fELF" {
        return Err(ImageError::Malformed("ELF"));
    }
    if bytes[4] != 1 || bytes[5] != 1 {
        return Err(ImageError::Malformed("ELF (need 32-bit little-endian)"));
    }
    let e_phoff = u32le(bytes, 0x1c).ok_or(ImageError::Malformed("ELF"))? as usize;
    let e_phentsize = u16le(bytes, 0x2a).ok_or(ImageError::Malformed("ELF"))? as usize;
    let e_phnum = u16le(bytes, 0x2c).ok_or(ImageError::Malformed("ELF"))? as usize;
    if e_phentsize < 32 {
        return Err(ImageError::Malformed("ELF program header"));
    }
    let mut segs = Vec::new();
    for i in 0..e_phnum {
        let ph = e_phoff
            .checked_add(
                i.checked_mul(e_phentsize)
                    .ok_or(ImageError::Malformed("ELF"))?,
            )
            .ok_or(ImageError::Malformed("ELF"))?;
        let p_type = u32le(bytes, ph).ok_or(ImageError::Malformed("ELF program header"))?;
        if p_type != 1 {
            continue; // PT_LOAD only
        }
        let p_offset = u32le(bytes, ph + 4).ok_or(ImageError::Malformed("ELF"))? as usize;
        let p_paddr = u32le(bytes, ph + 12).ok_or(ImageError::Malformed("ELF"))?;
        let p_filesz = u32le(bytes, ph + 16).ok_or(ImageError::Malformed("ELF"))? as usize;
        if p_filesz == 0 {
            continue;
        }
        let end = p_offset
            .checked_add(p_filesz)
            .ok_or(ImageError::Malformed("ELF"))?;
        let data = bytes
            .get(p_offset..end)
            .ok_or(ImageError::Malformed("ELF segment out of file"))?
            .to_vec();
        segs.push(Segment {
            addr: fix_flash_addr(p_paddr, code_flash_start),
            data,
        });
    }
    Ok(flash_only(segs, code_flash_start))
}

/// en: The STORE region an ArduinoCore-CH32RV ELF keeps against the end of flash: the
/// `.ch32rv_store` section (NOLOAD, so it is in no PT_LOAD segment), else the
/// `_ch32rv_store_start` / `_ch32rv_store_end` symbols. `addr` is normalized into the flash window
/// like the segments. ja: ArduinoCore-CH32RV の ELF が flash の末尾に置く STORE の範囲。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreRegion {
    pub addr: u32,
    pub len: u32,
}

impl StoreRegion {
    /// One past the last byte.
    pub fn end(&self) -> u32 {
        self.addr.saturating_add(self.len)
    }
}

/// en: The STORE region of an ELF image (see [`StoreRegion`]). `None` for anything else - a
/// bin / hex / UF2 carries no section table - and for an ELF without the section or symbols, or
/// whose STORE is 0 bytes (the core links it only when something uses it).
/// ja: ELF の STORE。bin / hex / UF2、section も記号も無い ELF、0 byte の STORE は `None`。
pub fn store_region(bytes: &[u8], code_flash_start: u32) -> Option<StoreRegion> {
    if !bytes.starts_with(b"\x7fELF") || bytes.get(4) != Some(&1) || bytes.get(5) != Some(&1) {
        return None;
    }
    let shoff = u32le(bytes, 0x20)? as usize;
    let shentsize = u16le(bytes, 0x2e)? as usize;
    let shnum = u16le(bytes, 0x30)? as usize;
    let shstrndx = u16le(bytes, 0x32)? as usize;
    if shentsize < 40 || shoff == 0 {
        return None;
    }
    let sh = |i: usize| -> Option<usize> { shoff.checked_add(i.checked_mul(shentsize)?) };
    let cstr = |off: usize| -> Option<&[u8]> {
        let tail = bytes.get(off..)?;
        Some(&tail[..tail.iter().position(|&b| b == 0)?])
    };
    let region = |addr: u32, len: u32| {
        (len > 0).then(|| StoreRegion {
            addr: fix_flash_addr(addr, code_flash_start),
            len,
        })
    };
    // The section, by name.
    let strtab_off = u32le(bytes, sh(shstrndx)? + 16)? as usize;
    for i in 0..shnum {
        let h = sh(i)?;
        let name = u32le(bytes, h)? as usize;
        if cstr(strtab_off.checked_add(name)?)? == b".ch32rv_store" {
            return region(u32le(bytes, h + 12)?, u32le(bytes, h + 20)?);
        }
    }
    // The symbols (a linker script that names the bounds without the section).
    let (mut start, mut end) = (None, None);
    for i in 0..shnum {
        let h = sh(i)?;
        if u32le(bytes, h + 4)? != 2 {
            continue; // SHT_SYMTAB only
        }
        let off = u32le(bytes, h + 16)? as usize;
        let size = u32le(bytes, h + 20)? as usize;
        let link = u32le(bytes, h + 24)? as usize;
        let str_off = u32le(bytes, sh(link)? + 16)? as usize;
        for s in (off..off.checked_add(size)?).step_by(16) {
            let name = cstr(str_off.checked_add(u32le(bytes, s)? as usize)?)?;
            let value = u32le(bytes, s + 4)?;
            match name {
                b"_ch32rv_store_start" => start = Some(value),
                b"_ch32rv_store_end" => end = Some(value),
                _ => {}
            }
        }
    }
    let (s, e) = (start?, end?);
    region(s, e.checked_sub(s)?)
}

// ---- Intel HEX ----

fn parse_ihex(bytes: &[u8], code_flash_start: u32) -> Result<Image, ImageError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| ImageError::Malformed("Intel HEX (not ASCII)"))?;
    let mut upper: u32 = 0; // extended linear address (record 04) high 16 bits
    let mut runs: Vec<Segment> = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let ln = idx + 1;
        let rec = line.strip_prefix(':').ok_or_else(|| ImageError::Hex {
            line: ln,
            reason: "record does not start with ':'".into(),
        })?;
        let raw = decode_hex(rec).ok_or_else(|| ImageError::Hex {
            line: ln,
            reason: "odd length or non-hex".into(),
        })?;
        if raw.len() < 5 {
            return Err(ImageError::Hex {
                line: ln,
                reason: "record too short".into(),
            });
        }
        let len = raw[0] as usize;
        if raw.len() != len + 5 {
            return Err(ImageError::Hex {
                line: ln,
                reason: "length byte mismatch".into(),
            });
        }
        let sum = raw.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        if sum != 0 {
            return Err(ImageError::Hex {
                line: ln,
                reason: "checksum mismatch".into(),
            });
        }
        let offset = u16::from_be_bytes([raw[1], raw[2]]) as u32;
        let rtype = raw[3];
        let data = &raw[4..4 + len];
        match rtype {
            0x00 => {
                let addr = fix_flash_addr(upper | offset, code_flash_start);
                append_run(&mut runs, addr, data);
            }
            0x01 => break, // EOF
            0x04 => {
                if len != 2 {
                    return Err(ImageError::Hex {
                        line: ln,
                        reason: "type 04 needs 2 bytes".into(),
                    });
                }
                upper = (u16::from_be_bytes([data[0], data[1]]) as u32) << 16;
            }
            0x02 => {
                if len != 2 {
                    return Err(ImageError::Hex {
                        line: ln,
                        reason: "type 02 needs 2 bytes".into(),
                    });
                }
                upper = (u16::from_be_bytes([data[0], data[1]]) as u32) << 4;
            }
            0x03 | 0x05 => {} // start address records: ignored for flashing
            other => {
                return Err(ImageError::Hex {
                    line: ln,
                    reason: format!("unsupported record type {other:#04x}"),
                });
            }
        }
    }
    Ok(flash_only(runs, code_flash_start))
}

/// Append `data` at `addr`, extending the last run if contiguous, else starting a new one.
fn append_run(runs: &mut Vec<Segment>, addr: u32, data: &[u8]) {
    if let Some(last) = runs.last_mut()
        && last.addr.wrapping_add(last.data.len() as u32) == addr
    {
        last.data.extend_from_slice(data);
        return;
    }
    runs.push(Segment {
        addr,
        data: data.to_vec(),
    });
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 2);
    for pair in b.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

// ---- UF2 ----

fn parse_uf2(bytes: &[u8], code_flash_start: u32) -> Result<Image, ImageError> {
    // 512-byte blocks. Layout: magic0=0x0A324655, magic1=0x9E5D5157, flags, targetAddr,
    // payloadSize, blockNo, numBlocks, familyID/size, data[476], magicEnd=0x0AB16F30.
    if !bytes.len().is_multiple_of(512) {
        return Err(ImageError::Malformed("UF2 (size not a multiple of 512)"));
    }
    let mut runs: Vec<Segment> = Vec::new();
    // Length is already validated as a multiple of 512, so chunks() yields full blocks.
    for block in bytes.chunks(512) {
        let magic0 = u32le(block, 0).ok_or(ImageError::Malformed("UF2"))?;
        let magic1 = u32le(block, 4).ok_or(ImageError::Malformed("UF2"))?;
        let magic_end = u32le(block, 508).ok_or(ImageError::Malformed("UF2"))?;
        if magic0 != 0x0A32_4655 || magic1 != 0x9E5D_5157 || magic_end != 0x0AB1_6F30 {
            return Err(ImageError::Malformed("UF2 (bad magic)"));
        }
        let flags = u32le(block, 8).ok_or(ImageError::Malformed("UF2"))?;
        if flags & 0x0000_0001 != 0 {
            continue; // "not main flash" block
        }
        let target = u32le(block, 12).ok_or(ImageError::Malformed("UF2"))?;
        let payload = u32le(block, 16).ok_or(ImageError::Malformed("UF2"))? as usize;
        if payload > 476 {
            return Err(ImageError::Malformed("UF2 (payload too large)"));
        }
        let data = &block[32..32 + payload];
        append_run(&mut runs, fix_flash_addr(target, code_flash_start), data);
    }
    Ok(flash_only(runs, code_flash_start))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The defect this exists for: a linked ELF hands over `.text` and the `.data` initialiser at
    /// its LMA as two segments that meet mid-page. They must come out as one page-aligned block.
    #[test]
    fn program_blocks_merges_a_data_lma_onto_the_text_tail() {
        let img = Image {
            segments: vec![
                Segment {
                    addr: 0x0800_0000,
                    data: vec![0xaa; 0x2bac],
                },
                Segment {
                    addr: 0x0800_2bac,
                    data: vec![0x11; 0x10],
                },
            ],
        };
        let blocks = img.program_blocks(256);
        assert_eq!(blocks.len(), 1, "contiguous segments must merge");
        assert_eq!(blocks[0].addr, 0x0800_0000);
        assert_eq!(blocks[0].data.len(), 0x2c00, "grown to whole 256-B units");
        assert_eq!(&blocks[0].data[0x2ba0..0x2bac], &[0xaa; 12]);
        assert_eq!(&blocks[0].data[0x2bac..0x2bbc], &[0x11; 16]);
        assert_eq!(
            &blocks[0].data[0x2bbc..0x2c00],
            &[0xff; 0x44],
            "0xff padding"
        );
    }

    /// The second half of the same defect: when `.text` ends exactly on a page boundary the two
    /// segments only touch, and a merge that required them to overlap left the `.data` page as a
    /// separate write - which the stub path then loses entirely (see [`Image::program_span`]).
    #[test]
    fn program_blocks_merges_segments_that_only_touch() {
        let img = Image {
            segments: vec![
                Segment {
                    addr: 0x0800_0000,
                    data: vec![0xaa; 0x4600],
                },
                Segment {
                    addr: 0x0800_4600,
                    data: vec![0x11; 0x58],
                },
            ],
        };
        let blocks = img.program_blocks(256);
        assert_eq!(blocks.len(), 1, "touching segments must merge");
        assert_eq!(blocks[0].data.len(), 0x4700);
        assert_eq!(&blocks[0].data[0x4600..0x4658], &[0x11; 0x58]);
    }

    /// The stub path gets one region, gaps filled, because only its first write takes effect.
    #[test]
    fn program_span_is_one_contiguous_region() {
        let img = Image {
            segments: vec![
                Segment {
                    addr: 0x0800_0000,
                    data: vec![1; 4],
                },
                Segment {
                    addr: 0x0800_1000,
                    data: vec![2; 4],
                },
            ],
        };
        let span = img.program_span(256).expect("non-empty image has a span");
        assert_eq!(span.addr, 0x0800_0000);
        assert_eq!(span.data.len(), 0x1100, "0x08000000 .. 0x08001100");
        assert_eq!(&span.data[0..4], &[1; 4]);
        assert_eq!(&span.data[4..0x1000], &[0xff; 0xffc], "gap is filled");
        assert_eq!(&span.data[0x1000..0x1004], &[2; 4]);
        assert!(Image::default().program_span(256).is_none());
    }

    /// A block that does not start on a programming unit is grown down to one, and a gap inside
    /// the same unit is filled - a HEX image may start at any address.
    #[test]
    fn program_blocks_aligns_the_start_and_fills_a_same_unit_gap() {
        let img = Image {
            segments: vec![
                Segment {
                    addr: 0x0800_0004,
                    data: vec![1, 2, 3, 4],
                },
                Segment {
                    addr: 0x0800_0010,
                    data: vec![5, 6],
                },
            ],
        };
        let blocks = img.program_blocks(64);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].addr, 0x0800_0000);
        assert_eq!(blocks[0].data.len(), 64);
        assert_eq!(&blocks[0].data[0..4], &[0xff; 4]);
        assert_eq!(&blocks[0].data[4..8], &[1, 2, 3, 4]);
        assert_eq!(&blocks[0].data[8..16], &[0xff; 8], "gap stays erased");
        assert_eq!(&blocks[0].data[16..18], &[5, 6]);
    }

    /// Segments in different programming units stay separate blocks (no 0xff flood between them).
    #[test]
    fn program_blocks_keeps_distant_segments_apart() {
        let img = Image {
            segments: vec![
                Segment {
                    addr: 0x0800_0000,
                    data: vec![1; 4],
                },
                Segment {
                    addr: 0x0800_1000,
                    data: vec![2; 4],
                },
            ],
        };
        let blocks = img.program_blocks(256);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].addr, 0x0800_0000);
        assert_eq!(blocks[0].data.len(), 256);
        assert_eq!(blocks[1].addr, 0x0800_1000);
        assert_eq!(blocks[1].data.len(), 256);
    }

    #[test]
    fn ihex_basic_maps_to_flash() {
        // Two data records at 0x0000/0x0010, then EOF. 0-based -> 0x0800_0000.
        let hex = ":0400000001020304F2\n:00000001FF\n";
        let img = Image::parse(hex.as_bytes(), ImageFormat::Hex, None, 0x0800_0000).unwrap();
        assert_eq!(img.segments.len(), 1);
        assert_eq!(img.segments[0].addr, 0x0800_0000);
        assert_eq!(img.segments[0].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn ihex_checksum_error() {
        let bad = ":0400000001020304FF\n";
        assert!(matches!(
            Image::parse(bad.as_bytes(), ImageFormat::Hex, None, 0x0800_0000),
            Err(ImageError::Hex { .. })
        ));
    }

    #[test]
    fn bin_uses_offset() {
        let img = Image::parse(
            &[0xaa; 16],
            ImageFormat::Bin,
            Some(0x0800_1000),
            0x0800_0000,
        )
        .unwrap();
        assert_eq!(img.segments[0].addr, 0x0800_1000);
        assert_eq!(img.total_len(), 16);
    }

    #[test]
    fn detect_formats() {
        assert_eq!(detect(b"\x7fELFxxxx"), Some(ImageFormat::Elf));
        assert_eq!(detect(b":10000000ab"), Some(ImageFormat::Hex));
        assert_eq!(detect(&[0x00, 0x11]), None);
    }

    #[test]
    fn fix_addr_windows() {
        assert_eq!(fix_flash_addr(0x0000_0000, 0x0800_0000), 0x0800_0000);
        assert_eq!(fix_flash_addr(0x0000_045c, 0x0800_0000), 0x0800_045c);
        assert_eq!(fix_flash_addr(0x0800_0000, 0x0800_0000), 0x0800_0000);
        assert_eq!(fix_flash_addr(0x2000_0000, 0x0800_0000), 0x2000_0000);
    }

    #[test]
    fn ihex_extended_linear_address() {
        // Type-04 sets the upper 16 bits; a low data record then lands high in flash.
        // :02000004 0800 F2  (upper = 0x0800) then 4 bytes at offset 0x0100.
        let hex = ":020000040800F2\n:04010000AABBCCDDED\n:00000001FF\n";
        let img = Image::parse(hex.as_bytes(), ImageFormat::Hex, None, 0x0800_0000).unwrap();
        assert_eq!(img.segments.len(), 1);
        assert_eq!(img.segments[0].addr, 0x0800_0100);
        assert_eq!(img.segments[0].data, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    /// Build one 512-byte UF2 block at `target` carrying `payload` (<=476 bytes), with `flags`.
    fn uf2_block(target: u32, payload: &[u8], flags: u32) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[0..4].copy_from_slice(&0x0A32_4655u32.to_le_bytes());
        b[4..8].copy_from_slice(&0x9E5D_5157u32.to_le_bytes());
        b[8..12].copy_from_slice(&flags.to_le_bytes());
        b[12..16].copy_from_slice(&target.to_le_bytes());
        b[16..20].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        b[32..32 + payload.len()].copy_from_slice(payload);
        b[508..512].copy_from_slice(&0x0AB1_6F30u32.to_le_bytes());
        b
    }

    #[test]
    fn uf2_two_contiguous_blocks_merge() {
        let mut bytes = uf2_block(0x0800_0000, &[1, 2, 3, 4], 0);
        bytes.extend(uf2_block(0x0800_0004, &[5, 6, 7, 8], 0));
        let img = Image::parse(&bytes, ImageFormat::Uf2, None, 0x0800_0000).unwrap();
        assert_eq!(img.segments.len(), 1); // contiguous -> one run
        assert_eq!(img.segments[0].addr, 0x0800_0000);
        assert_eq!(img.segments[0].data, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn uf2_skips_not_main_flash_and_rejects_bad_magic() {
        // A block flagged "not main flash" (bit0) is skipped; with only that block the image is empty.
        let skip = uf2_block(0x0800_0000, &[1, 2, 3, 4], 0x0000_0001);
        assert!(Image::parse(&skip, ImageFormat::Uf2, None, 0x0800_0000).is_err());
        // Corrupt the end magic -> malformed.
        let mut bad = uf2_block(0x0800_0000, &[1, 2, 3, 4], 0);
        bad[508] ^= 0xff;
        assert!(matches!(
            Image::parse(&bad, ImageFormat::Uf2, None, 0x0800_0000),
            Err(ImageError::Malformed(_))
        ));
        // Not a multiple of 512.
        assert!(Image::parse(&[0u8; 100], ImageFormat::Uf2, None, 0x0800_0000).is_err());
    }

    #[test]
    fn check_within_flash_bounds() {
        let img = Image::parse(&[0xaa; 16], ImageFormat::Bin, None, 0x0800_0000).unwrap();
        // Fits in an 4 KiB flash.
        assert!(img.check_within_flash(0x0800_0000, 4096).is_ok());
        // A 8-byte flash is too small for the 16-byte image.
        assert!(img.check_within_flash(0x0800_0000, 8).is_err());
    }

    /// en: A minimal ELF32 LE with a section table: `sections` are (name, type, addr, size,
    /// link, payload); the name string table is appended as the last section.
    type Section<'a> = (&'a str, u32, u32, u32, u32, Vec<u8>);

    fn tiny_elf(sections: &[Section<'_>]) -> Vec<u8> {
        let mut shstr = vec![0u8];
        let mut names = Vec::new();
        for (n, ..) in sections {
            names.push(shstr.len() as u32);
            shstr.extend_from_slice(n.as_bytes());
            shstr.push(0);
        }
        let shstr_name = shstr.len() as u32;
        shstr.extend_from_slice(b".shstrtab\0");
        let mut body = vec![0u8; 52];
        body[..6].copy_from_slice(b"\x7fELF\x01\x01");
        let mut offs = Vec::new();
        for (.., data) in sections {
            offs.push(body.len() as u32);
            body.extend_from_slice(data);
        }
        let shstr_off = body.len() as u32;
        body.extend_from_slice(&shstr);
        let shoff = body.len() as u32;
        let mut sh = |name: u32, ty: u32, addr: u32, off: u32, size: u32, link: u32| {
            for v in [name, ty, 0, addr, off, size, link, 0, 0, 0] {
                body.extend_from_slice(&v.to_le_bytes());
            }
        };
        sh(0, 0, 0, 0, 0, 0);
        for (i, (_, ty, addr, size, link, data)) in sections.iter().enumerate() {
            let size = if *ty == 8 { *size } else { data.len() as u32 };
            sh(names[i], *ty, *addr, offs[i], size, *link);
        }
        sh(shstr_name, 3, 0, shstr_off, shstr.len() as u32, 0);
        let n = sections.len() as u16 + 2;
        body[0x20..0x24].copy_from_slice(&shoff.to_le_bytes());
        body[0x2e..0x30].copy_from_slice(&40u16.to_le_bytes());
        body[0x30..0x32].copy_from_slice(&n.to_le_bytes());
        body[0x32..0x34].copy_from_slice(&(n - 1).to_le_bytes());
        body
    }

    #[test]
    fn store_region_from_the_section() {
        let elf = tiny_elf(&[
            (".text", 1, 0, 0, 0, vec![0x13; 8]),
            (".ch32rv_store", 8, 0x0003_fc00, 0x400, 0, vec![]),
        ]);
        // The 0-based link address lands in the flash window, like the segments.
        assert_eq!(
            store_region(&elf, 0x0800_0000),
            Some(StoreRegion {
                addr: 0x0803_fc00,
                len: 0x400
            })
        );
        // A 0-byte STORE (nothing uses it) is no STORE.
        let empty = tiny_elf(&[(".ch32rv_store", 8, 0x0800_4000, 0, 0, vec![])]);
        assert_eq!(store_region(&empty, 0x0800_0000), None);
        // No section, no symbols; and a bin is never one.
        let none = tiny_elf(&[(".text", 1, 0, 0, 0, vec![0x13; 8])]);
        assert_eq!(store_region(&none, 0x0800_0000), None);
        assert_eq!(store_region(&[0xffu8; 64], 0x0800_0000), None);
    }

    #[test]
    fn store_region_from_the_symbols() {
        let strtab = b"\0_ch32rv_store_start\0_ch32rv_store_end\0".to_vec();
        let mut symtab = vec![0u8; 16];
        for (name, value) in [(1u32, 0x0800_3c00u32), (21, 0x0800_4000)] {
            let mut e = [0u8; 16];
            e[..4].copy_from_slice(&name.to_le_bytes());
            e[4..8].copy_from_slice(&value.to_le_bytes());
            symtab.extend_from_slice(&e);
        }
        // Section 1 = .symtab (link 2 = .strtab).
        let elf = tiny_elf(&[
            (".symtab", 2, 0, 0, 2, symtab),
            (".strtab", 3, 0, 0, 0, strtab),
        ]);
        assert_eq!(
            store_region(&elf, 0x0800_0000),
            Some(StoreRegion {
                addr: 0x0800_3c00,
                len: 0x400
            })
        );
    }
}
