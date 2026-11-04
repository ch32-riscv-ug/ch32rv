//! en: How `flash --erase keep-store` is carried out on the WCH-Link stub path, and the other
//! erase facts the paths share (docs/cli.ja.md §4.1).
//!
//! On the families measured ([`stub_erases_pages`]) the WCH flash stub erases every page it
//! programs by itself, and runs fast only on flash a chip erase (Link `Program 0x01`, MER) has just
//! cleared. So `keep-store` with a STORE in the image has two ways there:
//! - (a) no ch32rv erase: the stub erases the pages it writes; the STORE is never touched.
//!   Cost grows with the pages written ([`page_self_erase`]).
//! - (b) back up the STORE (fast read), chip erase, program the image, program the STORE back with
//!   ch32rv's RAM loader over DMI (a second stub run after the image does not take: V307 bench
//!   2026-11-01), verify both. Cost is about fixed ([`RESTORE_FIXED_MS`]).
//!
//! [`choose_keep_store`] picks (b) when the estimate for (a) is larger. On the other families
//! (CH32V103's stub does not erase: it stops on a page not erased) ch32rv erases the pages written
//! over DMI first, as on the controller path, and the STORE is not touched.
//!
//! ja: WCH-Link の stub 経路で `--erase keep-store` をどう行うか。stub は書く page を自分で消し、
//! chip erase 直後の flash でだけ速く書く。そこで STORE があるときは (a) ch32rv は消さない、(b) STORE
//! を退避 → chip erase → image → STORE を RAM loader で書き戻す、の 2 通り(2 回目の stub は効かなかった)。見積もりで速い方を選ぶ。

use ch32rv_dmi::FlashProgMode;

/// en: What the stub's own erase costs per page it writes, for one family.
/// ja: stub が書く page ごとに自分で消す時間。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelfErase {
    /// The stub's write unit (its data packet = the page it erases), bytes.
    pub page: u32,
    /// Milliseconds the stub's erase adds per page.
    pub ms: f64,
    /// Measured with the stub (true), or taken from the app-side erase timing (false, uncertain).
    pub measured: bool,
}

/// en: Per-family cost of the stub's own page erase, by AttachChip family byte. Measured: ch32rv
/// bench 2026-11-01, LinkE 2.22, release f7ca580, 3 runs each, wall time; per page = (sector -
/// chip) / pages written (`--erase sector` = no ch32rv erase on the stub path).
/// - V003 / CH641 (0x09 / 0x49): 64 B page, 3.1 ms. V003 16 KiB: sector 3.1 s, chip 2.25 s
///   = 3.3 ms (an earlier run: 3.0 s / 2.2 s = 3.1 ms).
/// - V30x (0x06): 256 B, 27 ms. V307 96 KiB: sector 14.3 s, chip 4.0 s = 26.8 ms.
/// - V20x (0x05): 256 B, 19.5 ms. V203C8T6 32 KiB: sector 4.05 s, chip 1.55 s = 19.5 ms.
/// - L103 (0x0E): 256 B, 13 ms. L103C8T6 32 KiB: sector 3.78 s, chip 2.1 s = 13 ms.
///
/// Only the families whose stub was measured erasing ([`stub_erases_pages`]) have a figure.
///
/// ja: family ごとの stub の page 消去の費用(V003 / V307 / V203 / L103 を stub で実測、LinkE 2.22、
/// (sector − chip) / page 数)。stub が消すと実測した family だけ。
pub fn page_self_erase(family_byte: u8) -> Option<SelfErase> {
    let (page, ms) = match family_byte {
        0x09 => (64, 3.1),
        0x06 => (256, 27.0),
        0x05 => (256, 19.5),
        0x0E => (256, 13.0),
        _ => return None,
    };
    Some(SelfErase {
        page,
        ms,
        measured: true,
    })
}

/// en: Whether the family's WCH stub erases the pages it writes by itself (measured: CH32V003,
/// V20x, V30x, L103). Elsewhere ch32rv erases them over DMI first: CH32V103's stub does not erase
/// (LinkE 2.12 and CH549 Link 2.12, `--erase sector` over a programmed chip: the stub times out),
/// and a family not measured is treated the same, since an erase it repeats only costs time.
/// ja: その family の stub が書く page を自分で消すか(実測: V003、V20x、V30x、L103)。それ以外は
/// ch32rv が先に DMI で消す。V103 の stub は消さない(書いた chip に sector で書くと timeout)。
pub fn stub_erases_pages(family_byte: u8) -> bool {
    page_self_erase(family_byte).is_some()
}

/// en: Whether the WCH-Link's own erase and stub on this family use the instructions the Link
/// loaded into PROGBUF0/1 at attach, so a DMI memory access before them must put those back
/// (measured: CH32V103, LinkE 2.12 and CH549 Link 2.12 answer `81 55 01 02` otherwise). On
/// CH32V003 putting them back makes the stub skip its page erase (LinkE 2.22, `--erase sector`
/// over a programmed chip: `transfer timed out`); V20x, V30x and L103 work either way.
/// ja: この family で Link の消去と stub が、attach で入れた PROGBUF0/1 の命令を使うか(実測: V103)。
/// V003 では戻すと stub が page を消さない。V20x・V30x・L103 はどちらでも動く。
pub fn link_uses_program_buffer(family_byte: u8) -> bool {
    family_byte == 0x01
}

/// en: Fixed cost of way (b) beyond a plain chip-erase flash, in ms: the backup read, the second
/// loader upload and run, the extra verify, the extra attach cycles. From the V307 measurement of
/// backup + chip + restore of a 4 KB STORE (~5.9 s against chip 4.0 s), less the per-byte part.
/// The loader's write-back of a 4 KiB STORE on V307 (~16 pages, ~0.4 s) is within this figure.
/// ja: (b) の固定費(退避の読み、loader での書き戻し、照合、attach のやり直し)。V307 の実測から。
pub const RESTORE_FIXED_MS: f64 = 1800.0;

/// Way (b)'s cost per STORE byte (read + write back + verify), in ms (~50 ms per KiB, estimate).
pub const RESTORE_PER_BYTE_MS: f64 = 50.0 / 1024.0;

/// Which way `keep-store` takes on the stub path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepStoreWay {
    /// (a) No ch32rv erase; the stub erases the pages it writes.
    StubErase,
    /// (b) Back up the STORE, chip erase, program, write the STORE back.
    ChipAndRestore,
}

/// en: The choice and the two estimates (ms) behind it.
/// ja: 選んだ方法と、その元になった 2 つの見積もり(ms)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeepStoreChoice {
    pub way: KeepStoreWay,
    pub est_stub_erase_ms: f64,
    pub est_restore_ms: f64,
}

/// en: Choose how `keep-store` runs on the stub path for a family, the bytes the stub will write
/// and the STORE's size: (b) when est_a = pages_written × page_self_erase_ms > est_b =
/// [`RESTORE_FIXED_MS`] + store_bytes × [`RESTORE_PER_BYTE_MS`]. A family with no figure takes (a)
/// (never erases what it was not asked to).
/// ja: est_a(書く page 数 × page の自己消去)が est_b(固定費 + STORE の byte 分)より大きければ (b)。
pub fn choose_keep_store(family_byte: u8, written_bytes: u32, store_bytes: u32) -> KeepStoreChoice {
    let est_restore_ms = RESTORE_FIXED_MS + f64::from(store_bytes) * RESTORE_PER_BYTE_MS;
    let Some(se) = page_self_erase(family_byte) else {
        return KeepStoreChoice {
            way: KeepStoreWay::StubErase,
            est_stub_erase_ms: 0.0,
            est_restore_ms,
        };
    };
    let pages = written_bytes.div_ceil(se.page);
    let est_stub_erase_ms = f64::from(pages) * se.ms;
    KeepStoreChoice {
        way: if est_stub_erase_ms > est_restore_ms {
            KeepStoreWay::ChipAndRestore
        } else {
            KeepStoreWay::StubErase
        },
        est_stub_erase_ms,
        est_restore_ms,
    }
}

/// en: The FLASH-controller mode [`ch32rv_dmi::DebugModule::flash_mass_erase`] runs with for a
/// DB family, or `None` where ch32rv does not mass-erase over the DM: a family with no verified
/// controller profile, and CH32H417 (its CTLR has no MER in the RM-derived register data and its
/// EVT driver has no FLASH_EraseAllPages).
/// ja: DM 越しの全消去に使う mode。profile の無い family と H417(MER が無い)は `None`。
pub fn mass_erase_mode(family: &str) -> Option<FlashProgMode> {
    let profile = crate::flash_controller_profile_for(family)?;
    (profile.mode != FlashProgMode::PgStartSectorErase8K).then_some(profile.mode)
}

/// en: Whether `bytes` are all the erased value: 0xff, or the family's erased word (0xe339e339 on
/// V20x / V30x / X315 / V407 / H417), little-endian from a word boundary.
/// ja: すべて消去済みの値か(0xff か、family の消去済み word)。
pub fn all_erased(bytes: &[u8], erased_word: Option<u32>) -> bool {
    if bytes.iter().all(|&b| b == 0xff) {
        return true;
    }
    match erased_word {
        Some(w) => {
            let pat = w.to_le_bytes();
            bytes.iter().enumerate().all(|(i, &b)| b == pat[i % 4])
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v003_16k_keeps_the_stub_erase() {
        // 16 KB / 64 B = 256 pages x 3.1 ms = 0.8 s < 1.8 s + 1 KiB.
        let c = choose_keep_store(0x09, 16 * 1024, 1024);
        assert_eq!(c.way, KeepStoreWay::StubErase);
        assert!(c.est_stub_erase_ms < c.est_restore_ms);
    }

    #[test]
    fn v307_97k_takes_chip_and_restore() {
        // 97 KB / 256 B = 388 pages x 27 ms = 10.5 s > 1.8 s + 4 KiB.
        let c = choose_keep_store(0x06, 97 * 1024, 4096);
        assert_eq!(c.way, KeepStoreWay::ChipAndRestore);
        assert!(c.est_stub_erase_ms > 10_000.0);
    }

    #[test]
    fn small_images_and_unknown_families_keep_the_stub_erase() {
        assert_eq!(
            choose_keep_store(0x06, 4096, 4096).way,
            KeepStoreWay::StubErase
        );
        assert_eq!(
            choose_keep_store(0xC6, 1 << 20, 4096).way,
            KeepStoreWay::StubErase
        );
    }

    #[test]
    fn every_family_with_a_self_erase_figure_has_a_loader_for_the_store() {
        // Way (b) writes the STORE back through the RAM loader: every stub family it can pick
        // must have a loader plan, or (b) is never taken there.
        for (byte, family) in [
            (0x09, "CH32V003"),
            (0x06, "CH32V307"),
            (0x05, "CH32V20x"),
            (0x0E, "CH32L103"),
        ] {
            assert!(page_self_erase(byte).is_some(), "{family}");
            assert!(crate::loader::plan_for_family(family).is_some(), "{family}");
        }
        // V103's stub does not erase (measured); X035's was never measured: ch32rv erases first.
        for byte in [0x01, 0x0C, 0x0D, 0x49] {
            assert!(!stub_erases_pages(byte), "0x{byte:02x}");
        }
    }

    #[test]
    fn only_v103_keeps_the_links_program_buffer() {
        // V103's Link needs its PROGBUF0/1 back; V003's stub skips its erase with them back.
        assert!(link_uses_program_buffer(0x01));
        for byte in [0x09, 0x05, 0x06, 0x0E] {
            assert!(!link_uses_program_buffer(byte), "0x{byte:02x}");
        }
    }

    #[test]
    fn erased_values() {
        assert!(all_erased(&[0xff; 8], None));
        assert!(all_erased(&[0x39, 0xe3, 0x39, 0xe3], Some(0xe339_e339)));
        assert!(!all_erased(&[0x39, 0xe3, 0x00, 0xe3], Some(0xe339_e339)));
        assert!(!all_erased(&[0x00], None));
    }

    #[test]
    fn mass_erase_where_the_controller_has_mer() {
        assert!(mass_erase_mode("CH32V307").is_some());
        assert!(mass_erase_mode("CH32X315").is_some());
        assert!(mass_erase_mode("CH32V103").is_some());
        assert_eq!(mass_erase_mode("CH32H417"), None);
        assert_eq!(mass_erase_mode("CH32Q999"), None);
    }
}
