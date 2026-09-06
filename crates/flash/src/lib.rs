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
        0x0E => (&stub::CH643, 256, 4096),          // CH32L103 (byte-identical to X035/CH643)
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
    /// en: True when an erased flash cell reads back as `0xff`. This is a property of the silicon,
    /// not of the link: the reference manuals split CH32 into two groups - group A erases to
    /// `0xFFFFFFFF` (V003 / V103 / V205 / V006 / X035 / L103 / M030) and group B to `0xe339e339`
    /// (V20x / V30x / V407 / X315 / H417). On group B a read-modify-write of a page (e.g.
    /// `--restore-unwritten`) cannot tell a blank byte from real data and would program the erase
    /// pattern into it - hence such features are gated on this.
    /// ja: 消去済みセルが `0xff` を返す family か。**link でなくシリコンの特性**で、RM が 2 系統に
    /// 書き分けている: 系統 A = `0xFFFFFFFF`(V003/V103/V205/V006/X035/L103/M030)、系統 B =
    /// `0xe339e339`(V20x/V30x/V407/X315/H417)。系統 B では page の read-modify-write
    /// (`--restore-unwritten` 等)が blank と実データを区別できず消去パターンを焼き込むため gate する。
    ///
    /// Source: the generated DB's `erased_word` (`ch32-device-data` `evidence/flash_geometry.csv`,
    /// R-31: the reference manuals and WCH's own IAP blank checks - and for CH32V103, which no
    /// manual states, this project's silicon read, which the data repo took as the row's basis).
    pub erased_reads_ff: bool,
}

/// en: The `ch32-device-data` family string for an AttachChip family byte, for the families whose
/// controller path this project has driven on real silicon. CH641 and CH643 have no DB row of their
/// own; they share the CH32V003 / CH32X035 controller (same core generation and geometry), which is
/// how they were verified here. Bytes that are absent are unsupported by design, not by omission:
/// the DB knows more families (V006 / V205 / M030 / V407 / X315 / H417), but nothing here has been
/// run against that silicon, and this path erases and programs flash.
/// ja: family byte → ch32-device-data の family 文字列(実機で往復検証した family のみ)。CH641 /
/// CH643 は DB に行が無く、V003 / X035 と同じ controller を共有する。未掲載の byte は「DB に無い」
/// のではなく「実機未検証だから載せていない」(この経路は flash を消して書くため)。
fn db_family(family_byte: u8) -> Option<&'static str> {
    Some(match family_byte {
        0x01 => "CH32V103",
        0x05 => "CH32V20x",
        0x06 => "CH32V307",
        0x09 | 0x49 => "CH32V003", // CH641 shares the V003 controller
        0x0C | 0x0D => "CH32X035", // CH643 shares the X035 controller
        0x0E => "CH32L103",
        _ => return None,
    })
}

/// en: Resolve the FLASH-controller profile from the AttachChip family byte, from the generated DB
/// (`cargo xtask db-gen` <- `ch32-device-data`): the page size is the family's fast-erase
/// granularity, the mode comes from the RM/EVT programming procedure, and the erase pattern from the
/// erased-cell read value. Returns None when the family is unsupported (see [`db_family`]), when the
/// data repo marks the procedure `conflict`, when the family has no per-page fast erase, or when the
/// procedure is one this crate cannot drive over DMI - all fail-closed, because the caller uses this
/// to erase and program flash.
/// ja: family byte から FLASH-controller profile を引く(生成 DB 由来)。page サイズ = fast erase 粒度、
/// mode = RM/EVT の編程手順、消去パターン = 消去済み読み出し値。未対応・`conflict`・page 消去なし・
/// DMI で駆動できない手順はすべて None(fail-closed)。
pub fn flash_controller_profile(family_byte: u8) -> Option<FlashCtrlProfile> {
    let family = db_family(family_byte)?;
    let geometry = ch32rv_target::flash_geometry(family)?;
    let method = ch32rv_target::flash_program_method(family)?;
    // The data repo flags rows whose RM and EVT driver disagree; do not guess on a flash writer.
    if method.confidence == "conflict" {
        return None;
    }
    // `erase --range` and flash breakpoints work at the fast-erase page; 0 = block erase only.
    let page_size = geometry.fast_erase;
    if page_size == 0 {
        return None;
    }
    let mode = match (method.mode.as_str(), method.buffer_load_bits) {
        // FTPG, write the words, then PG_STRT.
        ("direct", _) => FlashProgMode::PgStart,
        // FTPG + BUFRST, one word per BUFLOAD, then STRT - drivable word by word over DMI.
        ("buffered", Some(32)) => FlashProgMode::Buffered,
        // A wider buffer load (V103 = 128 bit, M030 = 64 bit) cannot be fed by the word-at-a-time
        // DMI writer - it corrupts the page. CH32V103 is the one such family with a standard
        // half-word path implemented here (plus its mandatory commit side effect); anything else
        // fails closed until that path is generalised.
        ("buffered", Some(_)) if family == "CH32V103" => FlashProgMode::V103,
        _ => return None,
    };
    Some(FlashCtrlProfile {
        page_size,
        mode,
        // Verified on this project's bench for every family `db_family` lists.
        gdb_breakpoints: true,
        // CH32V103 only (its AttachChip overwrites s1/x9).
        attach_corrupts_regs: family == "CH32V103",
        erased_reads_ff: geometry.erased_word == Some(0xFFFF_FFFF),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    /// en: The DB-driven profile must still produce exactly what this project verified on the
    /// bench (these were hard-coded until the data repo delivered the programming procedure).
    /// A change in the generated tables that moves any of these is a regression, not an update.
    /// ja: DB 由来になった profile が実機検証済みの値と一致すること(納品前は手書きだった値)。
    #[test]
    fn profile_matches_bench_verified_values() {
        // (family_byte, page_size, mode, attach_corrupts_regs, erased_reads_ff)
        let cases = [
            (0x05u8, 256u32, FlashProgMode::PgStart, false, false), // CH32V20x
            (0x06, 256, FlashProgMode::PgStart, false, false),      // CH32V30x
            (0x09, 64, FlashProgMode::Buffered, false, true),       // CH32V003
            (0x49, 64, FlashProgMode::Buffered, false, true),       // CH641 (shares V003)
            (0x0C, 256, FlashProgMode::Buffered, false, true),      // CH643 (shares X035)
            (0x0D, 256, FlashProgMode::Buffered, false, true),      // CH32X035
            (0x0E, 256, FlashProgMode::Buffered, false, true),      // CH32L103
            (0x01, 128, FlashProgMode::V103, true, true),           // CH32V103
        ];
        for (byte, page_size, mode, corrupts, reads_ff) in cases {
            let p = flash_controller_profile(byte).unwrap_or_else(|| {
                panic!("family byte 0x{byte:02x}: no controller profile from the DB")
            });
            assert_eq!(p.page_size, page_size, "0x{byte:02x} page_size");
            assert_eq!(p.mode, mode, "0x{byte:02x} mode");
            assert_eq!(
                p.attach_corrupts_regs, corrupts,
                "0x{byte:02x} attach_corrupts_regs"
            );
            assert_eq!(p.erased_reads_ff, reads_ff, "0x{byte:02x} erased_reads_ff");
            assert!(p.gdb_breakpoints, "0x{byte:02x} gdb_breakpoints");
        }
    }

    /// Families the bench has never run must stay unsupported, even though the DB knows them:
    /// this path erases and programs flash, so it fails closed.
    #[test]
    fn unverified_families_have_no_profile() {
        for byte in [0x4Eu8, 0x86, 0xC6, 0x02, 0x00, 0xFF] {
            assert!(
                flash_controller_profile(byte).is_none(),
                "family byte 0x{byte:02x} must not resolve to a controller profile"
            );
        }
    }

    /// The two silicon erase groups must both be represented, and a group-B family must never be
    /// reported as blank-checkable with 0xff (that would program the erase pattern into a page).
    #[test]
    fn erase_groups_are_distinguished() {
        let group_b = flash_controller_profile(0x06).expect("CH32V30x profile");
        assert!(!group_b.erased_reads_ff, "V30x erases to 0xe339e339");
        let group_a = flash_controller_profile(0x0E).expect("CH32L103 profile");
        assert!(group_a.erased_reads_ff, "L103 erases to 0xffffffff");
    }
}
