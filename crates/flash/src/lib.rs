//! en: Programming layer: orchestrates erase / program / verify / confirm-run
//! (docs/cli.ja.md §4.1). Flash loader stubs live in [`stub`] (interim: transcribed from
//! wlink, to be built from source per docs/architecture.ja.md §3). For input images, ELF uses
//! `object`; Intel HEX gets a small in-house parser.
//!
//! ja: 書き込み層。erase / program / verify / confirm-run の編成。flash loader stub は
//! [`stub`](暫定: wlink から転記、将来は source から build)。ELF は object、HEX は自前 parser。

pub mod stub;

use ch32rv_contract::policy::{ConfirmRunMode, EraseMode, Region, ResetPolicy, VerifyMode};

/// en: Flash geometry/protocol parameters selected by the AttachChip family byte. Interim
/// source: wlink `RiscvChip` methods. The eventual source is the generated target DB
/// (docs/architecture.ja.md §3); this table only covers what the connected hardware needs.
/// ja: AttachChip family byte で選ぶ flash パラメータ(暫定: wlink 由来。将来は生成 DB)。
#[derive(Debug, Clone, Copy)]
pub struct FlashParams {
    pub stub: &'static [u8],
    pub data_packet_size: usize,
    pub write_pack_size: usize,
    pub supports_protect: bool,
    pub supports_special_erase: bool,
    /// Start of code flash for this family (bin default load address).
    pub code_flash_start: u32,
}

/// en: Resolve flash parameters from the AttachChip family byte. Returns None for families
/// not yet covered by this interim table (the caller reports "unsupported for flashing").
/// ja: AttachChip family byte から flash パラメータを引く。未対応 family は None。
pub fn params_for_family(family_byte: u8) -> Option<FlashParams> {
    // Values from wlink: data_packet_size, write_pack_size, code_flash_start, stub selection.
    let (stub, data_packet_size, write_pack_size): (&'static [u8], usize, usize) = match family_byte
    {
        0x09 | 0x49 => (&stub::CH32V003, 64, 1024), // CH32V003 / CH641 (single-wire SWIO)
        0x01 => (&stub::CH32V103, 128, 4096),       // CH32V103
        0x05 | 0x06 => (&stub::CH32V307, 256, 4096), // CH32V20x / CH32V30x
        _ => return None,
    };
    // support_special_erase: everything except the CH56x/57x/58x/59x BLE families.
    let supports_special_erase = !matches!(family_byte, 0x02 | 0x03 | 0x07 | 0x0B);
    // support_flash_protect families (from probe-rs): V103/V20x/V30x/V003/V00x/CH643/L103/X035/CH641/V317/H4.
    let supports_protect = matches!(
        family_byte,
        0x01 | 0x05 | 0x06 | 0x09 | 0x4E | 0x0C | 0x0E | 0x0D | 0x49 | 0x86 | 0xC6
    );
    Some(FlashParams {
        stub,
        data_packet_size,
        write_pack_size,
        supports_protect,
        supports_special_erase,
        code_flash_start: 0x0800_0000,
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
