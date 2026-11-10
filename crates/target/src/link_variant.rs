//! en: The one mapping from a WCH probe product as ch32-device-data names it (`device` and `mcu`
//! of `link_firmware` / `link_firmware_issues`) to the variant ID ch32rv compares
//! (`ch32rv_wchlink::Variant::id`, JSON `probe.variant`).
//! `cargo xtask db-gen` includes this file too (`#[path]`), and stops on a device it does not map;
//! the wchlink tests check every `Variant` against it.
//! ja: ch32-device-data の probe の名前(`device` + `mcu`)から ch32rv の variant ID への対応。
//! ここだけに書く。db-gen もこのファイルを読み、対応の無い device で生成を止める。

/// `(device, mcu, variant id)`.
pub const LINK_VARIANTS: &[(&str, &str, &str)] = &[
    ("WCH-Link", "CH549", "link-ch549"),
    ("WCH-LinkE", "CH32V305", "linke"),
    // The early WCH-LinkE built on a CH32V307 (MounRiver Studio 1.8.x shipped its
    // FIRMWARE_CH32V307.bin, 2.4-2.7). It reports the same variant byte as today's LinkE, so
    // ch32rv never detects it: its images are known by sha256 and refused on any LinkE.
    ("WCH-LinkE", "CH32V307", "linke-v307"),
    ("WCH-LinkS", "CH32V203", "links"),
    ("WCH-DAPLink", "CH32V203", "daplink"),
    ("WCH-LinkW", "CH32V208", "linkw"),
];

/// en: The variant ID of the probe `device` whose MCU is `mcu`, or None when unmapped.
/// ja: `device` + `mcu` の variant ID。対応が無ければ None。
pub fn variant_id(device: &str, mcu: &str) -> Option<&'static str> {
    LINK_VARIANTS
        .iter()
        .find(|(d, m, _)| *d == device && *m == mcu)
        .map(|(_, _, id)| *id)
}
