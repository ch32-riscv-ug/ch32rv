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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

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
}
