//! en: Flash programming data & image parsing for CH32 RISC-V parts. This crate holds what the
//! programming path needs, while the actual erase/program/verify orchestration lives in the CLI:
//!
//! - [`Image::parse`] turns an ELF / Intel HEX / UF2 / raw bin into flash [`Segment`]s, mapping
//!   link-time addresses into the flash window (like wlink's `fix_code_flash_start`). ELF uses
//!   `object`; Intel HEX and UF2 have small in-house parsers.
//! - [`params_for_family`] returns the WCH-Link write parameters (loader stub + packet sizes) for
//!   an AttachChip family byte, ready to hand to `ch32rv_wchlink::WchLink::write_flash`.
//! - [`flash_controller_profile`] gives the direct-FLASH-controller page size and programming
//!   mode ([`ch32rv_dmi::FlashProgMode`]) for `erase --range` / flash software breakpoints.
//! - [`stub`] holds the loader blobs (interim: transcribed from wlink, to be built from source per
//!   docs/architecture.ja.md §3); [`CODE_FLASH_START`] is the universal 0x0800_0000 flash base.
//!
//! ```
//! use ch32rv_flash::{Image, CODE_FLASH_START};
//! use ch32rv_contract::policy::ImageFormat;
//!
//! // A raw bin loads at the flash base by default.
//! let img = Image::parse(&[0xde, 0xad, 0xbe, 0xef], ImageFormat::Bin, None, CODE_FLASH_START).unwrap();
//! assert_eq!(img.segments[0].addr, CODE_FLASH_START);
//! ```
//!
//! ja: CH32 RISC-V 用の flash 書込データと image 解析。erase/program/verify の編成自体は CLI 側で、
//! この crate は書込経路が必要とするものを持つ: [`Image::parse`](ELF/HEX/UF2/bin → flash
//! [`Segment`])、[`params_for_family`](WCH-Link 書込パラメータ)、[`flash_controller_profile`]
//! (直接 FLASH controller の page/mode)、[`stub`](loader blob。暫定: wlink 転記)、
//! [`CODE_FLASH_START`](共通 flash 先頭 0x0800_0000)。

pub mod image;
pub mod stub;

pub use image::{Image, ImageError, Segment};

use ch32rv_contract::policy::{ConfirmRunMode, EraseMode, Region, ResetPolicy, VerifyMode};
use ch32rv_dmi::FlashProgMode;

/// en: Start of code flash on every CH32 RISC-V part (the bin default load address). Universal, so
/// it is a constant rather than a per-family field.
/// ja: 全 CH32 RISC-V の code flash 先頭(bin 既定ロード番地)。共通なので family 別でなく定数。
pub const CODE_FLASH_START: u32 = 0x0800_0000;

/// en: Resolve the WCH-Link flash write parameters (loader stub + packet sizes + capability flags)
/// from the AttachChip family byte. Returns the canonical [`ch32rv_wchlink::FlashParams`] consumed
/// by [`ch32rv_wchlink::WchLink::write_flash`] directly (no rebuild). None for families not yet
/// covered by this interim table (the caller reports "unsupported for flashing").
/// ja: AttachChip family byte から WCH-Link の flash 書込パラメータ(loader stub + packet サイズ +
/// capability)を引く。`write_flash` がそのまま消費する正準 [`ch32rv_wchlink::FlashParams`] を返す。
pub fn params_for_family(family_byte: u8) -> Option<ch32rv_wchlink::FlashParams> {
    // Values from wlink: data_packet_size, write_pack_size, stub selection.
    let (stub, data_packet_size, write_pack_size): (&'static [u8], usize, usize) = match family_byte
    {
        0x09 | 0x49 => (&stub::CH32V003, 64, 1024), // CH32V003 / CH641 (single-wire SWIO)
        0x01 => (&stub::CH32V103, 128, 4096),       // CH32V103
        0x05 | 0x06 => (&stub::CH32V307, 256, 4096), // CH32V20x / CH32V30x
        0x0D | 0x0C => (&stub::CH643, 256, 4096),   // CH32X035 / CH643
        0x0E => (&stub::CH32L103, 256, 4096),       // CH32L103
        _ => return None,
    };
    // support_special_erase: everything except the CH56x/57x/58x/59x BLE families.
    let supports_special_erase = !matches!(family_byte, 0x02 | 0x03 | 0x07 | 0x0B);
    // support_flash_protect families (from probe-rs): V103/V20x/V30x/V003/V00x/CH643/L103/X035/CH641/V317/H4.
    let supports_protect = matches!(
        family_byte,
        0x01 | 0x05 | 0x06 | 0x09 | 0x4E | 0x0C | 0x0E | 0x0D | 0x49 | 0x86 | 0xC6
    );
    Some(ch32rv_wchlink::FlashParams {
        stub,
        data_packet_size,
        write_pack_size,
        supports_protect,
        supports_special_erase,
    })
}

/// en: The direct FLASH-controller programming profile for a family: the page size that
/// `erase --range` / flash software breakpoints work at, and the programming mechanism
/// ([`FlashProgMode`]). Used to drive [`ch32rv_dmi::DebugModule::flash_page_erase`] /
/// `flash_program_page`.
/// ja: family の直接 FLASH-controller profile: `erase --range` / flash SW breakpoint が使う page
/// サイズと、programming 方式([`FlashProgMode`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashCtrlProfile {
    /// Page size (bytes) that `erase --range` and flash software breakpoints operate at.
    pub page_size: u32,
    /// The FLASH-controller programming mechanism this family uses.
    pub mode: FlashProgMode,
    /// en: Whether gdb flash software breakpoints are supported on this family (all verified
    /// profiles). CH32V103 also needs [`Self::attach_corrupts_regs`] handled by the gdb server.
    /// ja: この family で gdb flash SW breakpoint を使えるか(全 verified profile で true)。
    /// CH32V103 は [`Self::attach_corrupts_regs`] の対処が前提。
    pub gdb_breakpoints: bool,
    /// en: True when the WCH-Link AttachChip corrupts a live GPR (CH32V103: it overwrites s1/x9
    /// with the chip id and saves the original nowhere, so resuming the user program faults on its
    /// next use of s1). Workaround: soft-reset the target after attach so the program re-runs and
    /// re-establishes its registers before we halt it.
    /// ja: WCH-Link の AttachChip が生きた GPR を壊す family か(CH32V103 は s1/x9 を chip id で
    /// 上書きし復元不可 → resume で s1 使用時に fault)。対処: attach 後に soft-reset して program に
    /// レジスタを再構築させてから halt する。
    pub attach_corrupts_regs: bool,
    /// en: True when an erased flash cell reads back as the real `0xff` over the debug link. On
    /// V20x/V30x the WCH-Link returns a `0xe339e339` placeholder for erased cells instead, so a
    /// read-modify-write of a page (e.g. `--restore-unwritten`) cannot tell a blank byte from
    /// real data and would program the placeholder into it - hence such features are gated on this.
    /// ja: 消去済みセルが debug read で本来の `0xff` を返す family か。V20x/V30x は placeholder
    /// `0xe339e339` を返すため、page の read-modify-write(`--restore-unwritten` 等)で blank と
    /// 実データを区別できず placeholder を焼き込んでしまう → この種の機能はこのフラグで gate する。
    pub erased_reads_ff: bool,
}

/// en: Resolve the FLASH-controller profile from the AttachChip family byte. Returns None for
/// families whose controller sequence is not capture-verified yet.
/// ja: family byte から FLASH-controller profile を引く。未検証 family は None。
pub fn flash_controller_profile(family_byte: u8) -> Option<FlashCtrlProfile> {
    // (page_size, mode, gdb_breakpoints, attach_corrupts_regs, erased_reads_ff)
    let (page_size, mode, gdb_breakpoints, attach_corrupts_regs, erased_reads_ff) =
        match family_byte {
            0x05 | 0x06 => (256, FlashProgMode::PgStart, true, false, false), // CH32V20x / V30x - verified (erased reads 0xe339e339)
            0x09 | 0x49 => (64, FlashProgMode::Buffered, true, false, true), // CH32V003 / CH641 - verified
            0x0C | 0x0D => (256, FlashProgMode::Buffered, true, false, true), // CH643 / CH32X035 - verified
            0x0E => (256, FlashProgMode::Buffered, true, false, true), // CH32L103 - verified live
            // CH32V103: FTER 128B erase + standard halfword program + commit. AttachChip corrupts s1,
            // so gdb needs a reset-after-attach; then flash breakpoints work (verified).
            0x01 => (128, FlashProgMode::V103, true, true, true),
            _ => return None,
        };
    Some(FlashCtrlProfile {
        page_size,
        mode,
        gdb_breakpoints,
        attach_corrupts_regs,
        erased_reads_ff,
    })
}

/// en: Policy set for one `flash` invocation. Defaults match docs/cli.ja.md §4.1.
/// ja: `flash` 1 回分の方針。既定値は docs/cli.ja.md §4.1 と一致させる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashOptions {
    pub region: Region,
    pub erase: EraseMode,
    pub verify: VerifyMode,
    pub reset: ResetPolicy,
    pub confirm_run: Option<ConfirmRunMode>,
}

impl Default for FlashOptions {
    fn default() -> Self {
        Self {
            region: Region::Code,
            erase: EraseMode::Auto,
            verify: VerifyMode::Readback,
            reset: ResetPolicy::Run,
            confirm_run: None,
        }
    }
}
