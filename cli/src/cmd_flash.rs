//! en: `flash`, `erase`, `reset`, and `recover` (docs/cli.ja.md §4.1). These write to the
//! target. `flash` erases (per policy), programs via the flash loader stub, verifies by
//! readback, resets to run, and (with `--confirm-run`) checks the target is actually running.
//! `recover --method power-off|nrst` is the "Clear All Code Flash" recovery for a target whose
//! debug pins were repurposed.
//! ja: `flash` / `erase` / `reset` / `recover`。target へ書き込む。flash は消去→stub 経由で
//! 書き込み→readback verify→reset run→(--confirm-run で)走行確認。recover は debug ピンを
//! 他用途に使った target の「Clear All Code Flash」復旧。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::event::Event;
use ch32rv_contract::policy::{
    ConfirmRunMode, EraseMode, MonitorSource, RecoverMethod, ResetPolicy, VerifyMode,
};
use ch32rv_contract::progress::ProgressSink;
use ch32rv_contract::{ErrorKind, ResultEnvelope, Warning};
use ch32rv_flash::{CODE_FLASH_START, Image, Segment, params_for_family};

use std::path::Path;

use crate::args::{Cli, FlashArgs, RecoverArgs, SwitchState};
use crate::cmd_probe::{confirm_destructive, fail, mode_str, select_entry};
use crate::parse;
use crate::session::Session;

/// en: Parse an image, treating a magic-less `.bin`/extensionless file as raw bin under
/// `--format auto` (ELF/HEX/UF2 are still detected by magic; anything else still errors).
/// ja: `--format auto` で magic の無い `.bin`/拡張子無しは raw bin 扱いにする。
pub(crate) fn parse_image(
    bytes: &[u8],
    format: ch32rv_contract::policy::ImageFormat,
    path: &Path,
    bin_offset: Option<u32>,
    code_flash_start: u32,
) -> Result<Image, ch32rv_flash::ImageError> {
    use ch32rv_contract::policy::ImageFormat;
    match Image::parse(bytes, format, bin_offset, code_flash_start) {
        Err(ch32rv_flash::ImageError::UnknownFormat) if format == ImageFormat::Auto => {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if matches!(ext.as_deref(), Some("bin") | None) {
                Image::parse(bytes, ImageFormat::Bin, bin_offset, code_flash_start)
            } else {
                Err(ch32rv_flash::ImageError::UnknownFormat)
            }
        }
        other => other,
    }
}

/// en: Read `expected.len()` bytes at `addr` for verification. Uses the fast WCH-Link bulk read,
/// but when that disagrees with `expected` (or errors) it re-reads via the authoritative DMI path
/// and returns that instead. The fast read can return stale/garbage right after stub execution on
/// some probes - observed on the CH549 Link (fw 2.12): a false verify-mismatch even though the write
/// landed (capture: the readback was a `00 01 02 ..` ramp, not flash). DMI word reads (progbuf) are
/// the source of truth, so this makes verify robust without paying the DMI cost on the common match.
/// The hart must already be halted.
/// ja: verify 用に `addr` から `expected.len()` byte 読む。まず fast bulk read、`expected` と食い違う
/// (またはエラー)なら権威ある DMI 読みで読み直して返す。fast read は stub 実行直後に一部 probe
/// (CH549 Link fw2.12 で実測)で stale/ゴミを返し false verify-mismatch を起こす。DMI が真実。/// en: How a family gets programmed. Most families upload WCH's flash loader stub to the probe,
/// which then streams the image; the V00x line has no stub, so its own FLASH controller is driven
/// directly over DMI instead (what minichlink calls "direct mode" - the only route WCH's tooling
/// uses for these parts either).
/// ja: family ごとの書込経路。多くは probe へ WCH の flash loader stub を載せて流すが、V00x 系は
/// stub が無いので target の FLASH controller を DMI で直接叩く(minichlink の "direct mode")。
enum Programmer {
    /// The probe runs WCH's loader stub in target RAM.
    Stub(Box<ch32rv_wchlink::FlashParams>),
    /// The host drives the target's FLASH controller over DMI, page by page.
    Controller(ch32rv_flash::FlashCtrlProfile),
}

fn verify_read(session: &mut Session, addr: u32, expected: &[u8]) -> Result<Vec<u8>, String> {
    let len = expected.len() as u32;
    match session.link().read_mem(addr, len) {
        Ok(d) if d == expected => Ok(d),
        // Mismatch or transport error: re-read via the authoritative DMI path (progbuf word reads),
        // which goes through the real bus and reflects the just-programmed flash. The fast bulk read
        // is only an optimization, so silently preferring the authoritative source on disagreement is
        // correct - a real mismatch is still caught (DMI confirms it), a stale one is corrected.
        _ => session.dm().read_mem(addr, len).map_err(|e| e.to_string()),
    }
}

/// en: The set of flash pages (page-aligned start addresses) that the given `(addr, len)`
/// segments touch, for a `page`-byte page size. `--erase sector` erases exactly these pages, so
/// it never wipes flash outside the image. A segment that starts mid-page pulls in its whole
/// page; segments that share a page collapse to one entry.
/// ja: `(addr, len)` セグメント群が触れる flash page(page 境界の開始番地)の集合。`--erase sector`
/// はこの page だけを消すので image 外の flash を消さない。page 途中開始はその page 全体を含む。
/// en: What the chosen programmer will actually write, for a given image. Both the erase plan and
/// the program step go through this, so the pages that get erased are exactly the pages that get
/// programmed. The stub path must hand the probe ONE contiguous region ([`Image::program_span`] -
/// only the first `write_flash` of a session takes effect); the FLASH-controller path drives whole
/// pages itself and takes the image's natural blocks, gaps and all. Either way a linked ELF's
/// `.data` initialiser - its own segment, starting right behind `.text` - stops being a write the
/// programmer cannot start (the stub would drop it silently, the controller would re-program a page
/// it had already written).
/// ja: 選ばれた programmer が実際に書く単位。消去計画も書込も同じこれを通すので、**消される page と
/// 焼かれる page が一致する**。stub 経路は 1 つの連続 region を渡さなければならない
/// ([`Image::program_span`]。1 セッションで効くのは最初の `write_flash` だけ)。controller 経路は
/// page 単位で自分で駆動するので image 本来の block のままでよい。どちらでも、link 済み ELF の
/// `.data` 初期値(`.text` 直後から始まる独立 segment)が「programmer が書き始められない write」で
/// なくなる(stub は黙って捨て、controller は書いた page を二度焼きしていた)。
fn planned_writes(programmer: &Programmer, image: &Image) -> Vec<Segment> {
    match programmer {
        // The stub programs one data packet at a time; that packet is the family's flash page.
        Programmer::Stub(fp) => image
            .program_span(fp.data_packet_size as u32)
            .into_iter()
            .collect(),
        Programmer::Controller(profile) => image.program_blocks(profile.page_size),
    }
}

/// en: Before writing, put down mode bits a stopped earlier write left in FLASH_CTLR (see
/// `DebugModule::flash_clear_stale_mode`), with a `flash-controller-reset` warning when it found
/// any. A hart that was running runs again afterwards; a check that cannot run is skipped.
/// ja: 書く前に、止まった前の書き込みが FLASH_CTLR に残したビットを下ろす。見つけたら warning。
/// 走っていた hart は走らせ直す。
pub(crate) fn clear_stale_flash_mode(session: &mut Session, warnings: &mut Vec<Warning>) {
    // The Link's own flash work follows: leave it the program buffer it needs.
    let keep = ch32rv_flash::erase_plan::link_uses_program_buffer(session.attach.family_byte);
    let mut dm = session.dm();
    let Ok(was_halted) = dm.is_halted() else {
        return;
    };
    if !was_halted && dm.halt().is_err() {
        return;
    }
    if let Ok(stale) = dm.flash_clear_stale_mode(keep)
        && stale != 0
    {
        warnings.push(Warning {
            code: "flash-controller-reset".to_owned(),
            msg: format!(
                "FLASH_CTLR held mode bits 0x{stale:08x} from a write that was stopped; cleared them"
            ),
        });
    }
    if !was_halted {
        let _ = dm.resume();
    }
}

pub(crate) fn covered_pages(
    segments: impl IntoIterator<Item = (u32, u32)>,
    page: u32,
) -> std::collections::BTreeSet<u32> {
    let mut pages = std::collections::BTreeSet::new();
    for (addr, len) in segments {
        let first = addr - (addr % page);
        let end = addr.saturating_add(len); // exclusive
        let mut p = first;
        while p < end {
            pages.insert(p);
            p = p.saturating_add(page);
        }
    }
    pages
}

/// en: The erase scope a flash reports: `result.flash.scope`, and with keep-store
/// `result.flash.store`.
/// ja: flash が報告する消去の範囲。keep-store では `store` も。
pub(crate) struct EraseReport {
    pub scope: &'static str,
    pub store: Option<serde_json::Value>,
}

impl EraseReport {
    /// The `erase:` line of the normal output.
    fn human(&self) -> String {
        let Some(st) = &self.store else {
            return self.scope.to_owned();
        };
        match (st["method"].as_str(), st["address"].as_str()) {
            (Some("none-present"), _) | (_, None) => {
                format!("{} (no STORE in the image: as sector)", self.scope)
            }
            (Some(m), Some(a)) => format!(
                "{} (STORE {} B at {a} {m})",
                self.scope,
                st["bytes"].as_u64().unwrap_or(0)
            ),
            (None, Some(_)) => self.scope.to_owned(),
        }
    }
}

/// en: `result.flash.store` under keep-store: the STORE's address and size (null / 0 when the
/// image names none) and `method` - untouched (no erase reached it), restored (chip erase, then
/// written back through the RAM loader and verified), blank (chip erase over a STORE that held
/// only the erased value: nothing to write back) or none-present.
/// ja: keep-store の `store`。address / bytes と method(untouched / restored / blank / none-present)。
pub(crate) fn store_json(
    store: Option<ch32rv_flash::StoreRegion>,
    method: &str,
) -> serde_json::Value {
    match store {
        Some(s) => serde_json::json!({
            "address": format!("0x{:08x}", s.addr),
            "bytes": s.len,
            "method": method,
        }),
        None => serde_json::json!({
            "address": null,
            "bytes": 0,
            "method": "none-present",
        }),
    }
}

/// en: keep-store by chip erase on the stub path: the STORE range rounded out to the stub's
/// pages, its backup, where the backup sits on the host until the STORE is back, and the RAM
/// loader that writes it back.
/// ja: chip erase で STORE を残すときの範囲・退避した中身・host に置いたファイル。
struct StoreRestore {
    lo: u32,
    hi: u32,
    backup: Vec<u8>,
    saved: Option<std::path::PathBuf>,
    /// The RAM loader that writes the STORE back.
    plan: ch32rv_flash::loader::LoaderPlan,
}

impl StoreRestore {
    /// The hint of a failure between the chip erase and the STORE written back.
    fn hint(&self) -> Option<String> {
        self.saved.as_ref().map(|p| {
            format!(
                "the STORE (0x{:08x}..0x{:08x}) was erased; its previous content is saved in {} - write it back with `ch32rv flash --erase sector --at 0x{:08x} {}`",
                self.lo,
                self.hi,
                p.display(),
                self.lo,
                p.display()
            )
        })
    }
}

/// en: Keep a STORE backup at `$XDG_RUNTIME_DIR/ch32rv/store-<id>.bin` (the runtime directory).
/// ja: STORE の退避を runtime dir の `store-<id>.bin` に置く。
fn save_store_backup(id: &str, data: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = ch32rv_usb::create_runtime_dir()?.join(format!("store-{safe}.bin"));
    std::fs::write(&path, data)?;
    Ok(path)
}

/// en: Overlay `segments` onto one page's pre-read `content` (the page starts at `page_addr` and
/// is `content.len()` bytes): bytes a segment covers take the segment's value, the rest keep their
/// original `content`. Used by `--restore-unwritten` so a page can be re-programmed whole without
/// losing bytes the image does not touch.
/// ja: `content`(page 先頭 `page_addr`、長さ=page サイズ)に `segments` を上書き合成する。segment
/// が覆う byte はその値、他は元の `content` のまま。`--restore-unwritten` で page 全体を再 program
/// するのに使う。
pub(crate) fn overlay_page(page_addr: u32, content: &mut [u8], segments: &[Segment]) {
    ch32rv_flash::overlay(page_addr, content, segments);
}

/// en: The flash flags ch32rv's RAM loader cannot honour, on either path that uses it (an OEP
/// probe, a WCH-Link family with no WCH stub). The loader erases the pages the image touches,
/// keeps the bytes around the image in them, and reads every page back, so `--erase
/// sector|keep-store` (the STORE lies outside the image), `--verify readback` and
/// `--restore-unwritten` say what it does, and `--erase chip` runs a mass erase over the DM first
/// ([`loader_chip_erase`]); `--erase none` (the loader always erases what it writes) and the rest
/// would be ignored.
/// ja: ch32rv の RAM loader(OEP の probe、WCH の stub が無い WCH-Link の family)が守れない flash の
/// フラグ。loader は image が触れる page を消し、その中の image の外の byte を残し、全 page を読み
/// 戻す。`--erase chip` は先に DM で全消去する。`--erase none` と残りは守れない。
pub(crate) fn loader_refusals(args: &FlashArgs) -> Vec<String> {
    let mut v = Vec::new();
    if args.erase == EraseMode::None {
        v.push("--erase none".to_owned());
    }
    if args.verify == VerifyMode::None {
        v.push("--verify none".to_owned());
    }
    if args.sdi.is_some() {
        v.push("--sdi".to_owned());
    }
    if args.monitor.is_some() {
        v.push("--monitor".to_owned());
    }
    v
}

/// en: `--erase chip` on the RAM-loader paths: the whole code flash erased over the DM before the
/// loader writes. A family whose FLASH controller has a mass erase gets MER + STRT; one with a
/// controller profile but no MER (CH32H417) gets every erase page of the code flash, `flash_size`
/// bytes from the code flash base (5.9 s for the H417's 960 KiB over a LinkE, measured). A family
/// with neither, or with no known flash size for the page loop, is refused, exit 24, before
/// anything is erased. The hart must be halted. Err is the failure to report (kind, message).
/// ja: RAM loader の経路の `--erase chip`。loader が書く前に DM で全消去する。MER のある family は
/// MER、MER の無い family(H417)は code flash 全体の page を順に消す。どちらもできなければ exit 24
/// で断る(何も消す前)。
pub(crate) fn loader_chip_erase<T: ch32rv_dmi::DtmAccess>(
    dtm: &mut T,
    family: &str,
    flash_size: Option<u32>,
) -> Result<(), (ErrorKind, String)> {
    if let Some(mode) = ch32rv_flash::erase_plan::mass_erase_mode(family) {
        return ch32rv_dmi::DebugModule::new(dtm)
            .flash_mass_erase(mode)
            .map_err(|e| (ErrorKind::TransferFailed, format!("chip erase failed: {e}")));
    }
    let Some(profile) = ch32rv_flash::flash_controller_profile_for(family) else {
        return Err((
            ErrorKind::CapabilityUnsupported,
            format!("--erase chip: ch32rv has no FLASH controller profile for {family}"),
        ));
    };
    let Some(size) = flash_size.filter(|&n| n > 0) else {
        return Err((
            ErrorKind::CapabilityUnsupported,
            format!(
                "--erase chip: the code flash size of this {family} is not known, so the page erase cannot be sized"
            ),
        ));
    };
    let page = profile.page_size;
    let mut dm = ch32rv_dmi::DebugModule::new(dtm);
    for i in 0..size.div_ceil(page) {
        let addr = CODE_FLASH_START + i * page;
        dm.flash_page_erase(addr, profile.mode).map_err(|e| {
            (
                ErrorKind::TransferFailed,
                format!("chip erase failed at 0x{addr:08x}: {e}"),
            )
        })?;
    }
    Ok(())
}

/// en: The `scope` / `store` a RAM-loader flash reports: the loader never touches flash outside
/// the image, so a STORE stays untouched.
/// ja: loader の経路の報告。loader は image の外に触れないので STORE は untouched。
pub(crate) fn loader_erase_report(
    erase: EraseMode,
    bytes: &[u8],
) -> (&'static str, Option<serde_json::Value>) {
    let store = (erase == EraseMode::KeepStore).then(|| {
        store_json(
            ch32rv_flash::store_region(bytes, CODE_FLASH_START),
            "untouched",
        )
    });
    (erase.as_str(), store)
}

/// en: Refuse (exit 24) the flags a flash path cannot honour, before anything is written.
/// ja: その経路で守れないフラグを、何か書く前に断る(exit 24)。
pub(crate) fn refuse_flags(cli: &Cli, path: &str, flags: &[String]) -> Option<ExitCode> {
    (!flags.is_empty()).then(|| {
        fail(
            cli,
            "flash",
            ErrorKind::CapabilityUnsupported,
            format!("{path} cannot honour {}", flags.join(", ")),
            Some("leave them out (docs/cli.ja.md §4.1 lists what each path does)"),
        )
    })
}

pub fn flash(cli: &Cli, args: &FlashArgs) -> ExitCode {
    // en: An OEP probe's path is known from the address alone: refuse what it cannot do before
    // connecting (`--repeat` too, whose wait for the next target only looks through a WCH-Link).
    // ja: OEP の経路はアドレスだけで分かるので、つなぐ前に断る(`--repeat` も。次の target を待つ
    // のは WCH-Link 越しだけ)。
    let addr = match crate::oep::addr(cli, "flash") {
        Ok(a) => a,
        Err(c) => return c,
    };
    if addr
        .as_ref()
        .is_some_and(|a| !matches!(a, crate::oep::OepAddr::Wch(_)))
        && let Some(c) = refuse_flags(cli, "an OEP probe", &crate::oep::flash_refusals(cli, args))
    {
        return c;
    }
    if args.repeat {
        flash_repeat(cli, args)
    } else {
        flash_once(cli, args, addr)
    }
}

/// en: `--repeat` (production): program the current target, then wait for the operator to remove it
/// and insert the next one, and program that too - looping until interrupted (Ctrl-C). A failed
/// board is reported and the loop moves on to the next, matching a production line's flow.
/// ja: `--repeat`(量産): 今の target を焼き、operator が外して次を挿すのを待って焼く、を Ctrl-C まで
/// 繰り返す。失敗 board は報告して次へ進む(産線の流れに合わせる)。
fn flash_repeat(cli: &Cli, args: &FlashArgs) -> ExitCode {
    let mut count = 0u32;
    loop {
        count += 1;
        eprintln!("repeat: programming target #{count} (Ctrl-C to stop)");
        // A WCH-Link's broker may have started or ended since the last board: look again.
        let _ = match crate::oep::addr(cli, "flash") {
            Ok(a) => flash_once(cli, args, a),
            Err(c) => c,
        };
        eprintln!("repeat: remove the programmed target ...");
        wait_for_chip(cli, false);
        eprintln!("repeat: insert the next target ...");
        wait_for_chip(cli, true);
    }
}

/// en: Poll the selected probe until a target chip is present (`want == true`) or absent
/// (`want == false`). The probe (WCH-Link) stays enumerated across a target swap, so this attaches
/// to the chip through it; only Ctrl-C (process signal) breaks the wait.
/// ja: 選択 probe に target chip が有る(want=true)/無い(want=false)になるまで poll。probe 自体は
/// target 交換で再列挙されないので、それ越しに chip へ attach を試す。抜けるのは Ctrl-C のみ。
fn wait_for_chip(cli: &Cli, want: bool) {
    loop {
        if chip_present(cli) == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Whether a target chip currently answers AttachChip on the selected probe.
fn chip_present(cli: &Cli) -> bool {
    let Ok(entry) = select_entry(cli, "flash") else {
        return false;
    };
    let Ok(mut link) = ch32rv_wchlink::WchLink::open(&entry.dev) else {
        return false;
    };
    let _ = link.probe_info();
    let present = link.attach_chip().is_ok();
    let _ = link.detach_chip();
    present
}

/// `flash` once, at `addr` (`crate::oep::addr`: an OEP probe, a WCH-Link's broker, or None).
fn flash_once(cli: &Cli, args: &FlashArgs, addr: Option<crate::oep::OepAddr>) -> ExitCode {
    const CMD: &str = "flash";
    let bytes = match std::fs::read(&args.file) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("read {}: {e}", args.file.display()),
                None,
            );
        }
    };

    // An OEP probe (`oep://`, a serial port no WCH-Link owns) takes the OEP path. A
    // WCH-Link whose broker runs (an open monitor) is borrowed from it for the direct path below -
    // WCH's stub stays - and handed back when this returns.
    let _lend = match addr {
        Some(crate::oep::OepAddr::Wch(t)) => match crate::broker::lend(&t) {
            Ok(l) => Some(crate::broker::LendScope::new(l)),
            Err(e) if e.other_version => return crate::oep::broker_fail(cli, CMD, e),
            Err(e) => return fail(cli, CMD, ErrorKind::DeviceBusy, e.msg, None),
        },
        Some(a) => return crate::oep::flash(cli, args, &bytes, &a),
        None => None,
    };

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; flashing requires RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let mut session = match Session::attach(
        &entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        cli.db.as_deref(),
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(e) => return crate::cmd_probe::session_error(cli, CMD, e),
    };
    clear_stale_flash_mode(&mut session, &mut warnings);

    if args.programmer == crate::args::ProgrammerChoice::Loader {
        return flash_via_loader(cli, args, &bytes, session, warnings);
    }

    let family = session.attach.family_byte;
    // en: No WCH stub: ch32rv's RAM loader when it has a plan - several times faster than driving
    // the FLASH controller word by word over DMI (64 KiB on a CH32H417: 8 s against 61 s) - else
    // the DMI-driven controller.
    // ja: WCH の stub が無ければ、plan のある RAM ローダー(DMI で 1 語ずつ書くより数倍速い。H417 の
    // 64 KiB で 8 秒と 61 秒)、無ければ DMI の FLASH controller。
    let loader_plan = params_for_family(family).is_none()
        && ch32rv_flash::loader::plan_for_family(&crate::cmd_target::db_family_of(&mut session))
            .is_some();
    if loader_plan {
        return flash_via_loader(cli, args, &bytes, session, warnings);
    }
    let programmer = match params_for_family(family) {
        Some(fp) => Programmer::Stub(Box::new(fp)),
        None => match ch32rv_flash::flash_controller_profile(family) {
            Some(profile) => Programmer::Controller(profile),
            None => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    format!(
                        "flashing family 0x{family:02x} ({}) is not supported yet (no flash stub and no FLASH-controller profile)",
                        session.family()
                    ),
                    Some("`capabilities` lists what this probe/target pair can do"),
                );
            }
        },
    };

    // Parse the input into flash segments (ELF / Intel HEX / UF2 / raw bin).
    let bin_offset = match &args.at {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => Some(a),
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => None,
    };
    let image = match parse_image(
        &bytes,
        args.format,
        &args.file,
        bin_offset,
        CODE_FLASH_START,
    ) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
    };
    // The probe reports flash size; use it to reject out-of-range segments.
    let flash_size = session.flash_capacity().0.unwrap_or(0);
    if flash_size > 0
        && let Err(e) = image.check_within_flash(CODE_FLASH_START, flash_size)
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            e.to_string(),
            Some("wrong --chip or --offset?"),
        );
    }
    let total = image.total_len() as u64;
    // The STORE (ELF `.ch32rv_store`) `--erase keep-store` keeps, and the name its host backup
    // takes (the chip's UID, else the probe's serial).
    let store = ch32rv_flash::store_region(&bytes, CODE_FLASH_START);
    let backup_id = match &session.chip {
        Some(c) => c
            .uuid
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        None => entry
            .dev
            .serial()
            .map(str::to_owned)
            .unwrap_or_else(|| entry.dev.topology()),
    };

    let sink = crate::progress::sink(cli);

    // --preverify: if the target already holds this exact image, skip erase+program entirely (saves
    // a flash cycle and its wear). Read the image region and compare before doing anything
    // destructive; on a mismatch, reset the link state and fall through to a normal flash.
    if args.preverify {
        sink.event(&Event::Phase {
            name: "preverify".into(),
            total: Some(total),
        });
        let already_matches = {
            if let Err(e) = session.dm().halt() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::AttachFailed,
                    format!("halt for preverify failed: {e}"),
                    None,
                );
            }
            let mut all = true;
            for seg in &image.segments {
                // Fast bulk read via the WCH-Link; fall back to DMI word reads on error.
                let readback = match session.link().read_mem(seg.addr, seg.data.len() as u32) {
                    Ok(d) => d,
                    Err(_) => match session.dm().read_mem(seg.addr, seg.data.len() as u32) {
                        Ok(d) => d,
                        Err(e) => {
                            return fail(
                                cli,
                                CMD,
                                ErrorKind::TransferFailed,
                                format!("preverify read failed at {:#010x}: {e}", seg.addr),
                                None,
                            );
                        }
                    },
                };
                if readback != seg.data {
                    all = false;
                    break;
                }
            }
            all
        };
        // We halted the core to read; reset the link/target state before whatever comes next.
        session.link().detach_chip().ok();
        let _ = session.link().attach_chip();
        if already_matches {
            return finish_flash(
                cli,
                CMD,
                session,
                total,
                &EraseReport {
                    scope: "none",
                    store: None,
                },
                true,
                Some(true),
                warnings,
                args.reset,
                args.confirm_run,
                args.sdi,
                args.monitor,
            );
        }
    }

    // Erase per policy (docs/cli.ja.md §4.1).
    //   chip       - one fast whole-chip erase (the Link's MER).
    //   sector     - only the pages the image covers. Where WCH's stub erases every page it
    //                programs itself (`stub_erases_pages`), ch32rv erases nothing first; the
    //                controller path, and a stub that does not (V103), erase them over DMI.
    //   keep-store - the STORE (ELF `.ch32rv_store`) is kept. On the stub path, the cheaper of the
    //                stub's own erase (= sector) and backup -> chip erase -> program -> STORE
    //                written back ([`ch32rv_flash::erase_plan::choose_keep_store`]). No STORE:
    //                the same as sector.
    //   none       - no erase: not on a stub that erases what it writes.
    let erase = args.erase;
    let stub_packet = match &programmer {
        Programmer::Stub(fp) => Some(fp.data_packet_size as u32),
        Programmer::Controller(_) => None,
    };
    let stub_erases = stub_packet.is_some() && ch32rv_flash::erase_plan::stub_erases_pages(family);
    if erase == EraseMode::None && stub_erases {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "WCH's flash stub for {} erases every page it writes, so it cannot honour --erase none",
                session.family()
            ),
            Some("use --erase sector or keep-store (the default)"),
        );
    }
    // `--restore-unwritten` re-programs whole pages, so it needs a page-granular erase.
    if args.restore_unwritten && matches!(erase, EraseMode::Chip | EraseMode::None) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!(
                "--restore-unwritten needs page-granular erase, but --erase is {}",
                erase.as_str()
            ),
            Some("use --erase sector or keep-store"),
        );
    }
    let planned = planned_writes(&programmer, &image);
    // keep-store on the stub path: which way, and the STORE range rounded out to the stub's pages.
    let mut restore: Option<StoreRestore> = None;
    // The STORE goes back through ch32rv's RAM loader (a second stub run after the image does not
    // take: V307 bench 2026-11-01), so way (b) needs the family's loader plan.
    let loader_plan = if erase == EraseMode::KeepStore && store.is_some() && stub_packet.is_some() {
        ch32rv_flash::loader::plan_for_family(&crate::cmd_target::db_family_of(&mut session))
    } else {
        None
    };
    if erase == EraseMode::KeepStore
        && !args.restore_unwritten
        && let (Some(packet), Some(st), Some(plan)) = (stub_packet, store, loader_plan)
    {
        let lo = st.addr - st.addr % packet;
        let hi = st.end().div_ceil(packet).saturating_mul(packet);
        let overlaps = planned
            .iter()
            .any(|s| s.addr < hi && s.addr.saturating_add(s.data.len() as u32) > lo);
        let written: u32 = planned.iter().map(|s| s.data.len() as u32).sum();
        let choice = ch32rv_flash::erase_plan::choose_keep_store(family, written, hi - lo);
        if !overlaps && choice.way == ch32rv_flash::erase_plan::KeepStoreWay::ChipAndRestore {
            restore = Some(StoreRestore {
                lo,
                hi,
                backup: Vec::new(),
                saved: None,
                plan,
            });
        }
    }
    let mut store_method = match (erase, store, &restore) {
        (EraseMode::KeepStore, None, _) => Some("none-present"),
        (EraseMode::KeepStore, Some(_), Some(_)) => Some("restored"),
        (EraseMode::KeepStore, Some(_), None) => Some("untouched"),
        _ => None,
    };
    // Full pages to program instead of the sparse image, populated only under --restore-unwritten
    // (each covered page read before it is erased, with the image overlaid, so unwritten bytes
    // survive).
    let mut restored: Option<Vec<Segment>> = None;
    match erase {
        EraseMode::None => {}
        EraseMode::Chip => {
            sink.event(&Event::Phase {
                name: "erase".into(),
                total: None,
            });
            if let Err(e) = session.link().erase_flash() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("erase failed: {e}"),
                    None,
                );
            }
        }
        EraseMode::KeepStore if restore.is_some() => {
            let Some(r) = restore.as_mut() else {
                unreachable!("checked above")
            };
            sink.event(&Event::Phase {
                name: "erase".into(),
                total: None,
            });
            if let Err(e) = session.dm().halt() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::AttachFailed,
                    format!("halt for the STORE backup failed: {e}"),
                    None,
                );
            }
            let len = r.hi - r.lo;
            r.backup = match session.link().read_mem(r.lo, len) {
                Ok(d) if d.len() == len as usize => d,
                _ => match session.dm().read_mem(r.lo, len) {
                    Ok(d) => d,
                    Err(e) => {
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::TransferFailed,
                            format!("STORE backup read at 0x{:08x} failed: {e}", r.lo),
                            None,
                        );
                    }
                },
            };
            let erased_word =
                ch32rv_target::flash_geometry(&crate::cmd_target::db_family_of(&mut session))
                    .and_then(|g| g.erased_word);
            if ch32rv_flash::erase_plan::all_erased(&r.backup, erased_word) {
                // Nothing to put back: the chip erase leaves the STORE as it was.
                store_method = Some("blank");
            } else {
                // Kept on the host until the STORE is back and verified, so a write that stops
                // in between loses nothing.
                match save_store_backup(&backup_id, &r.backup) {
                    Ok(p) => r.saved = Some(p),
                    Err(e) => {
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::TransferFailed,
                            format!(
                                "could not keep the STORE backup on the host ({e}); nothing was erased"
                            ),
                            Some("use --erase sector to leave the STORE in place"),
                        );
                    }
                }
            }
            session.link().detach_chip().ok();
            let _ = session.link().attach_chip();
            if let Err(e) = session.link().erase_flash() {
                let hint = r.hint();
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("erase failed: {e}"),
                    hint.as_deref(),
                );
            }
        }
        EraseMode::Sector | EraseMode::KeepStore => {
            let cprofile = ch32rv_flash::flash_controller_profile(family);
            let controller = matches!(programmer, Programmer::Controller(_));
            // The controller path erases the covered pages itself, and --restore-unwritten reads
            // them first: both need the family's verified page size.
            // ch32rv erases the covered pages over DMI unless the stub does it.
            let pre_erase = controller || !stub_erases;
            let cprofile = match cprofile {
                Some(p) => Some(p),
                None if pre_erase || args.restore_unwritten => {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::CapabilityUnsupported,
                        format!(
                            "--erase {} is not supported for {} (0x{family:02x}) yet",
                            erase.as_str(),
                            session.family()
                        ),
                        Some("use --erase chip to erase the whole chip"),
                    );
                }
                None => None,
            };
            // restore-unwritten needs a true 0xff readback for erased cells, so a blank byte in a
            // page is not confused with real data and re-programmed as a placeholder.
            if args.restore_unwritten && cprofile.is_some_and(|p| !p.erased_reads_ff) {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    format!(
                        "--restore-unwritten is not supported on {} (erased cells do not read back as 0xff)",
                        session.family()
                    ),
                    Some("omit --restore-unwritten; sector erase clears whole covered pages"),
                );
            }
            if let Some(cprofile) = cprofile
                && (pre_erase || args.restore_unwritten)
            {
                let page = cprofile.page_size;
                // The planned writes, not the raw segments: the stub path programs one contiguous
                // region, so a gap inside it is programmed too (`--restore-unwritten` puts its
                // previous content back).
                let pages =
                    covered_pages(planned.iter().map(|s| (s.addr, s.data.len() as u32)), page);
                if let Err(e) = session.dm().halt() {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::AttachFailed,
                        format!("halt for sector erase failed: {e}"),
                        None,
                    );
                }
                // restore-unwritten: capture each page's current content (before it is erased)
                // and overlay the image on it, so the program step rewrites whole pages and
                // unwritten bytes survive.
                if args.restore_unwritten {
                    let mut dm = session.dm();
                    let mut merged = Vec::with_capacity(pages.len());
                    for &pg in &pages {
                        let mut buf = match dm.read_mem(pg, page) {
                            Ok(b) => b,
                            Err(e) => {
                                return fail(
                                    cli,
                                    CMD,
                                    ErrorKind::TransferFailed,
                                    format!(
                                        "restore-unwritten read of page 0x{pg:08x} failed: {e}"
                                    ),
                                    None,
                                );
                            }
                        };
                        buf.resize(page as usize, 0xff);
                        overlay_page(pg, &mut buf, &image.segments);
                        merged.push(Segment {
                            addr: pg,
                            data: buf,
                        });
                    }
                    restored = Some(merged);
                }
                // Erase first unless the stub does (a DMI pre-erase is pure overhead there).
                if pre_erase {
                    let total_pages = pages.len() as u64;
                    sink.event(&Event::Phase {
                        name: "erase".into(),
                        total: Some(total_pages),
                    });
                    let mode = cprofile.mode;
                    let mut dm = session.dm();
                    for (i, pg) in pages.iter().enumerate() {
                        if let Err(e) = dm.flash_page_erase(*pg, mode) {
                            return fail(
                                cli,
                                CMD,
                                ErrorKind::TransferFailed,
                                format!("sector erase failed at 0x{pg:08x}: {e}"),
                                None,
                            );
                        }
                        sink.event(&Event::Progress {
                            phase: "erase".into(),
                            done: (i + 1) as u64,
                            total: Some(total_pages),
                        });
                    }
                }
                // The stub gets a clean slate: the reads and the erase halted the core and used
                // the program buffer, which the Link fills again at attach.
                if !controller {
                    session.link().detach_chip().ok();
                    let _ = session.link().attach_chip();
                }
            }
        }
    }
    let store_hint = restore.as_ref().and_then(StoreRestore::hint);

    // Program each segment. Under --restore-unwritten we program the merged whole pages instead of
    // the sparse image (so unwritten bytes in a partially-filled page keep their original values).
    // Either way what goes out is `planned_writes`, not the raw segments.
    let program_segments = match &restored {
        Some(segs) => planned_writes(
            &programmer,
            &Image {
                segments: segs.clone(),
            },
        ),
        None => planned,
    };
    let program_segments: &[Segment] = &program_segments;
    let program_total: u64 = program_segments.iter().map(|s| s.data.len() as u64).sum();
    sink.event(&Event::Phase {
        name: "program".into(),
        total: Some(program_total),
    });
    match &programmer {
        Programmer::Stub(fp) => {
            let s = &sink;
            let mut base = 0u64;
            for seg in program_segments {
                let seg_len = seg.data.len() as u64;
                if let Err(e) = session.link().write_flash(seg.addr, &seg.data, fp, |done| {
                    s.event(&Event::Progress {
                        phase: "program".into(),
                        done: base + done,
                        total: Some(program_total),
                    });
                }) {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TransferFailed,
                        format!("program failed at {:#010x}: {e}", seg.addr),
                        store_hint.as_deref(),
                    );
                }
                base += seg_len;
            }
        }
        // No stub for this family: drive the target's own FLASH controller over DMI, page by page.
        // Uncovered bytes of a partially-filled page go out as 0xff, which is what the page holds
        // after the erase step - the same result the stub path produces. `--restore-unwritten` has
        // already merged the old content into `program_segments` when the caller asked for it.
        Programmer::Controller(profile) => {
            if let Err(e) = session.dm().halt() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::AttachFailed,
                    format!("halt for controller programming failed: {e}"),
                    None,
                );
            }
            let page = profile.page_size;
            let mode = profile.mode;
            let mut dm = session.dm();
            let mut done = 0u64;
            for seg in program_segments {
                let one = std::slice::from_ref(seg);
                for pg in covered_pages([(seg.addr, seg.data.len() as u32)], page) {
                    let mut buf = vec![0xffu8; page as usize];
                    overlay_page(pg, &mut buf, one);
                    if let Err(e) = dm.flash_program_page(pg, &buf, mode) {
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::TransferFailed,
                            format!("program failed at page {pg:#010x}: {e}"),
                            None,
                        );
                    }
                    done = (done + u64::from(page)).min(program_total);
                    sink.event(&Event::Progress {
                        phase: "program".into(),
                        done,
                        total: Some(program_total),
                    });
                }
            }
        }
    }

    // keep-store by chip erase: the STORE goes back through ch32rv's RAM loader over DMI (it
    // erases, programs and reads back each page). A second stub run after the image did not take
    // effect (V307, LinkE 2.22, 2026-11-01: the STORE read back blank).
    if let Some(r) = &restore
        && r.saved.is_some()
    {
        sink.event(&Event::Phase {
            name: "store".into(),
            total: Some(r.backup.len() as u64),
        });
        if let Err(e) = session.dm().reset_halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("reset-halt for the STORE write-back failed: {e}"),
                store_hint.as_deref(),
            );
        }
        let seg = [Segment {
            addr: r.lo,
            data: r.backup.clone(),
        }];
        let result = {
            let mut t = ch32rv_dmi::DmTarget::new(session.link());
            ch32rv_flash::loader::program(&mut t, r.plan, &seg, &mut |_, _| {})
        };
        if let Err(e) = result {
            return fail(
                cli,
                CMD,
                loader_error_kind(&e),
                format!("writing the STORE back at {:#010x} failed: {e}", r.lo),
                store_hint.as_deref(),
            );
        }
    }

    // Verify (readback), unless disabled. A STORE written back is always checked.
    let mut verified = None;
    let mut checks: Vec<(u32, &[u8])> = Vec::new();
    if !matches!(args.verify, VerifyMode::None) {
        checks.extend(image.segments.iter().map(|s| (s.addr, s.data.as_slice())));
    }
    if let Some(r) = &restore
        && r.saved.is_some()
    {
        checks.push((r.lo, r.backup.as_slice()));
    }
    if !checks.is_empty() {
        sink.event(&Event::Phase {
            name: "verify".into(),
            total: Some(checks.iter().map(|(_, d)| d.len() as u64).sum()),
        });
        session.link().detach_chip().ok();
        let _ = session.link().attach_chip();
        if let Err(e) = session.dm().halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt for verify failed: {e}"),
                store_hint.as_deref(),
            );
        }
        for (addr, data) in checks {
            // Fast bulk readback, re-checked via the authoritative DMI read on mismatch/error
            // (fast read can return stale data right after stub execution on the CH549 Link).
            let readback = match verify_read(&mut session, addr, data) {
                Ok(d) => d,
                Err(e) => {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TransferFailed,
                        format!("readback failed: {e}"),
                        store_hint.as_deref(),
                    );
                }
            };
            if readback != data {
                let at = readback
                    .iter()
                    .zip(data)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                return fail(
                    cli,
                    CMD,
                    ErrorKind::VerifyMismatch,
                    format!("verify mismatch at {:#010x}", addr as usize + at),
                    store_hint.as_deref(),
                );
            }
        }
        if !matches!(args.verify, VerifyMode::None) {
            verified = Some(true);
        }
    }
    // The STORE is back and verified: the host copy is no longer needed.
    if let Some(p) = restore.as_ref().and_then(|r| r.saved.as_ref()) {
        let _ = std::fs::remove_file(p);
    }

    let report = EraseReport {
        scope: erase.as_str(),
        store: store_method.map(|m| store_json(store, m)),
    };
    finish_flash(
        cli,
        CMD,
        session,
        total,
        &report,
        false,
        verified,
        warnings,
        args.reset,
        args.confirm_run,
        args.sdi,
        args.monitor,
    )
}

/// en: Apply the reset policy (run/halt/none, with `--confirm-run`), optionally set the SDI print
/// state (`--sdi`) and hand off to a monitor session (`--monitor`), then print/emit the result.
/// Shared by the normal end of `flash` and the `--preverify` "already matches" skip path. Takes the
/// session by value so it can be dropped (releasing the probe's USB handle) before a monitor
/// session re-opens the probe.
/// ja: reset 方針を適用し、必要なら SDI print 状態を設定(`--sdi`)して monitor へ移行(`--monitor`)、
/// 結果を出す。`flash` の通常終了と `--preverify` スキップ経路で共有。monitor が probe を開き直せる
/// よう session を値で受け、drop で USB を解放してから渡す。
#[allow(clippy::too_many_arguments)]
fn finish_flash(
    cli: &Cli,
    cmd: &str,
    mut session: Session,
    total: u64,
    erase: &EraseReport,
    skipped: bool,
    verified: Option<bool>,
    warnings: Vec<Warning>,
    reset: ResetPolicy,
    confirm: Option<ConfirmRunMode>,
    sdi: Option<SwitchState>,
    monitor: Option<MonitorSource>,
) -> ExitCode {
    // Which writer programmed it: WCH's stub where the family has one, else the FLASH controller.
    let programmer = if params_for_family(session.attach.family_byte).is_some() {
        "stub"
    } else {
        "controller"
    };
    let mut running = None;
    // en: A Link borrowed from its broker (a monitor is open) is handed back with the reset: the
    // broker reopens its consoles first, so the firmware's first output is polled.
    // ja: ブローカーから借りた Link は reset ごと返す(ブローカーが console を開き直してから reset
    // するので、firmware の最初の出力を汲める)。
    if reset == ResetPolicy::Run && crate::broker::lend_active() {
        let family = session.family();
        drop(session);
        running = crate::broker::hand_back_with_reset();
        if confirm.is_some() && running != Some(true) {
            return finish_lent(
                cli, cmd, &family, programmer, total, erase, skipped, verified, running, warnings,
            );
        }
        return finish_lent_ok(
            cli, cmd, &family, programmer, total, erase, skipped, verified, running, warnings,
            monitor,
        );
    }
    match reset {
        ResetPolicy::Run => {
            if let Err(msg) = soft_reset_and_run(&mut session) {
                return fail(cli, cmd, ErrorKind::TransferFailed, msg, None);
            }
            if let Some(mode) = confirm {
                std::thread::sleep(Duration::from_millis(200));
                running = Some(confirm_run(&mut session, mode));
                if running == Some(false) {
                    return finish(
                        cli,
                        cmd,
                        &session.family(),
                        programmer,
                        total,
                        erase,
                        skipped,
                        verified,
                        running,
                        warnings,
                        Some((
                            ErrorKind::NotRunningAfterWrite,
                            "programmed and verified, but the target is not running after reset"
                                .to_owned(),
                        )),
                    );
                }
            }
        }
        ResetPolicy::Halt => {
            let mut dm = session.dm();
            let _ = dm.reset_halt();
        }
        ResetPolicy::None => {}
    }

    // --sdi: set the probe's SDI-print forwarding after the target is (re)started. A failure here
    // must not fail the flash (programming already succeeded) - surface it as a warning.
    if let Some(state) = sdi {
        let on = matches!(state, SwitchState::On);
        if let Err(e) = session.link().set_sdi_print_enabled(on) {
            crate::warn(
                "sdi",
                format!(
                    "could not set SDI print {}: {e}",
                    if on { "on" } else { "off" }
                ),
            );
        }
    }

    let exit = finish(
        cli,
        cmd,
        &session.family(),
        programmer,
        total,
        erase,
        skipped,
        verified,
        running,
        warnings,
        None,
    );

    // --monitor: hand off to a monitor session (runs until Ctrl-C). Drop the flash session first so
    // its USB handle is released and the monitor backend can open the probe / its CDC port.
    if let Some(source) = monitor {
        drop(session);
        let margs = crate::args::MonitorArgs {
            cmd: None,
            source: Some(source),
            port: None,
            baud: 115_200,
        };
        return crate::cmd_monitor::monitor(cli, &margs);
    }
    exit
}

/// The flash's result after a reset the broker did, failing (exit 50) when it was not seen running.
#[allow(clippy::too_many_arguments)]
fn finish_lent(
    cli: &Cli,
    cmd: &str,
    family: &str,
    programmer: &str,
    total: u64,
    erase: &EraseReport,
    skipped: bool,
    verified: Option<bool>,
    running: Option<bool>,
    warnings: Vec<Warning>,
) -> ExitCode {
    finish(
        cli,
        cmd,
        family,
        programmer,
        total,
        erase,
        skipped,
        verified,
        running,
        warnings,
        Some((
            ErrorKind::NotRunningAfterWrite,
            "programmed and verified, but the target is not running after the broker's reset"
                .to_owned(),
        )),
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_lent_ok(
    cli: &Cli,
    cmd: &str,
    family: &str,
    programmer: &str,
    total: u64,
    erase: &EraseReport,
    skipped: bool,
    verified: Option<bool>,
    running: Option<bool>,
    warnings: Vec<Warning>,
    monitor: Option<MonitorSource>,
) -> ExitCode {
    let exit = finish(
        cli, cmd, family, programmer, total, erase, skipped, verified, running, warnings, None,
    );
    if let Some(source) = monitor {
        let margs = crate::args::MonitorArgs {
            cmd: None,
            source: Some(source),
            port: None,
            baud: 115_200,
        };
        return crate::cmd_monitor::monitor(cli, &margs);
    }
    exit
}

/// en: Soft-reset the target and make sure it is actually executing afterwards. The probe's
/// "reset and run" (`0x0b 0x01`) puts the hart back at its reset vector, but when the probe still
/// holds the halt request from attach - always the case for a standalone `reset`, which attaches
/// and immediately resets - the hart re-enters Debug Mode on reset, and DetachChip only drops the
/// request; a halted hart needs an explicit resume to leave. Two steps make that decision sound:
/// first acknowledge the reset (DMCONTROL `ackhavereset`) - on the CH32V00x Debug Module the
/// halt/running bits in DMSTATUS stay frozen at their reset-time values until the pending
/// `havereset` is acknowledged, so without the ack a running hart still reads as halted (and the
/// reverse could happen) - then read DMSTATUS and resume if halted. Measured 2026-09-16 on a
/// CH32V006 with a UART heartbeat: before, `reset` printed "running" and the UART stayed silent
/// (3/3) with DMSTATUS `0x004c0382` (halted, havereset); after, the heartbeat appears 5/5 and
/// DMSTATUS reads `0x00430c82` (allrunning). `flash` never showed the symptom because its verify
/// step halts through the DM, whose `halt` clears the request before the reset.
/// ja: soft reset 後、target が本当に走っていることを保証する。probe の「reset して実行」
/// (`0x0b 0x01`)は hart をリセットベクタへ戻すが、attach 由来の halt 要求を probe が保持したまま
/// だと(attach 直後に reset する standalone `reset` は常にこれ)hart はリセット時に Debug Mode へ
/// 再突入し、DetachChip は要求を落とすだけで、halt した hart は明示的な resume がなければ走らない。
/// 判定を確実にするために 2 段: まず reset を ack する(DMCONTROL `ackhavereset`) — CH32V00x の DM は
/// 保留中の `havereset` を ack するまで DMSTATUS の halt/running bit がリセット時の値で固着し、走って
/// いても halted と読める — 次に DMSTATUS を読み、halt なら resume。2026-09-16 CH32V006 + UART
/// heartbeat で実測: 修正前は `reset` が "running" と出しつつ UART 無音(3/3)・DMSTATUS `0x004c0382`
/// (halted, havereset)、修正後は heartbeat 5/5・DMSTATUS `0x00430c82`(allrunning)。`flash` は verify
/// で DM 経由の `halt`(reset 前に要求をクリア)を通るため症状が出なかった。
/// en: `flash --programmer loader`: program through ch32rv's own RAM loader (the OEP path) over
/// the WCH-Link's plain DMI, to check the loader and its procedure on real silicon. The family is
/// resolved from the chip id (not the family byte), as on an OEP probe.
/// ja: `flash --programmer loader`: ch32rv 自前の RAM loader(OEP の経路)で、WCH-Link の素の DMI 越し
/// に書く。loader と手順を実機で確かめるため。family は OEP と同じく chip id から引く。
fn flash_via_loader(
    cli: &Cli,
    args: &FlashArgs,
    bytes: &[u8],
    mut session: Session,
    warnings: Vec<Warning>,
) -> ExitCode {
    const CMD: &str = "flash";
    // en: `--confirm-run` is not refused although nothing checks it here: the Arduino recipes
    // always pass it, and a check that halts would fail a CH32H417 whose sketch takes the debug
    // pins (PB8 / PB9) for USB. ja: `--confirm-run` は断らない(Arduino の recipe が常に渡す。止めて
    // 確かめると、debug の pin を USB に取る H417 の sketch で失敗する)。
    let mut refused = loader_refusals(args);
    if args.preverify {
        refused.push("--preverify".to_owned());
    }
    if let Some(c) = refuse_flags(cli, "the ch32rv loader path (no WCH stub)", &refused) {
        return c;
    }
    let family = crate::cmd_target::db_family_of(&mut session);
    let Some(plan) = ch32rv_flash::loader::plan_for_family(&family) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!("the device DB has no loader plan for {family}"),
            None,
        );
    };
    let bin_offset = match &args.at {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => Some(a),
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => None,
    };
    let image = match parse_image(bytes, args.format, &args.file, bin_offset, CODE_FLASH_START) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
    };
    let flash_size = session.flash_capacity().0.unwrap_or(0);
    if flash_size > 0
        && let Err(e) = image.check_within_flash(CODE_FLASH_START, flash_size)
    {
        return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None);
    }
    // Halt before the first instruction, so a running watchdog cannot reset the part mid-write.
    // en: Not on a CH32H417: its reset-halt does not stop at the reset vector (the boot copies
    // the code to RAM and it runs before the halt lands, measured 2026-10-20), so it only gives
    // that code a fresh start - and code that turns PB8 / PB9 (SWCLK / SWDIO) into USB cuts the
    // link mid-write. The hart is halted where it is instead.
    // ja: H417 では reset-halt しない(リセット直後に止まらず、RAM に写したコードが走ってから止まる。
    // そのコードが PB8 / PB9 を USB にすると書き込みの途中で線が切れる)。今いる所で止める。
    let halted = if family.eq_ignore_ascii_case("CH32H417") {
        session.dm().halt()
    } else {
        session.dm().reset_halt()
    };
    if let Err(e) = halted {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("reset-halt: {e}"),
            None,
        );
    }
    let started = std::time::Instant::now();
    if args.erase == EraseMode::Chip {
        // en: A family with no MER (CH32H417): the Link's own chip erase does it whole (0.3 s on
        // the H417 against 5.9 s for the page loop, measured 2026-11-11).
        // ja: MER の無い family(H417)は Link の全消去を使う(page を回すより速い)。
        let erased = if ch32rv_flash::erase_plan::mass_erase_mode(&family).is_none()
            && ch32rv_flash::flash_controller_profile_for(&family).is_some()
        {
            session
                .link()
                .erase_flash()
                .map_err(|e| (ErrorKind::TransferFailed, format!("chip erase failed: {e}")))
        } else {
            loader_chip_erase(
                session.link(),
                &family,
                (flash_size > 0).then_some(flash_size),
            )
        };
        if let Err((kind, msg)) = erased {
            return fail(cli, CMD, kind, msg, None);
        }
    }
    let result = {
        let mut t = ch32rv_dmi::DmTarget::new(session.link());
        ch32rv_flash::loader::program(&mut t, plan, &image.segments, &mut |_, _| {})
    };
    let report = match result {
        Ok(r) => r,
        Err(e) => return fail(cli, CMD, loader_error_kind(&e), e.to_string(), None),
    };
    let secs = started.elapsed().as_secs_f64();
    let reset = match args.reset {
        ResetPolicy::Run => soft_reset_and_run(&mut session),
        // Out of reset, at the reset vector: the loader's own ebreak is not where a resume
        // should start the program.
        ResetPolicy::Halt => session
            .dm()
            .reset_halt()
            .map_err(|e| format!("reset-halt failed: {e}")),
        ResetPolicy::None => Ok(()),
    };
    if let Err(e) = reset {
        return fail(cli, CMD, ErrorKind::TransferFailed, e, None);
    }
    // The Link's soft reset gives no word on whether the program runs.
    let running: Option<bool> = None;
    let total = image.total_len();
    let (scope, store) = loader_erase_report(args.erase, bytes);
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "flash": {
                "bytes": total,
                "programmer": "loader",
                "family": family,
                "pages": report.pages,
                "rewritten": report.rewritten,
                "restarted_runs": report.restarted_runs,
                // Every page is read back after it is written: verified, or the run failed.
                "verified": true,
                "skipped": false,
                "scope": scope,
                "running": running,
                "seconds": secs,
            }
        }));
        if let (Some(st), Some(r)) = (store, env.result.as_mut()) {
            r["flash"]["store"] = st;
        }
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        println!(
            "flashed {total} bytes to {family} with the ch32rv loader in {secs:.2} s: {} page(s), {} rewritten, {} run(s) re-issued, verified",
            report.pages, report.rewritten, report.restarted_runs
        );
        ExitCode::SUCCESS
    }
}

pub(crate) fn soft_reset_and_run(session: &mut Session) -> Result<(), String> {
    session.mark_reset_run();
    session
        .link()
        .soft_reset()
        .map_err(|e| format!("reset failed: {e}"))?;
    // Give the hart a moment to come out of reset before judging its state.
    std::thread::sleep(Duration::from_millis(20));
    let mut dm = session.dm();
    dm.ack_have_reset()
        .map_err(|e| format!("reset acknowledge failed: {e}"))?;
    match dm.is_running() {
        Ok(true) => Ok(()),
        // Halted on reset (or the DM could not tell): a resume request is what releases it.
        Ok(false) | Err(_) => dm
            .resume()
            .map_err(|e| format!("resume after reset failed: {e}")),
    }
}

/// en: confirm-run: sample whether the target is actually executing. `status` checks the
/// running bit; `pc` additionally halts, reads dpc, checks it lies in flash, and resumes.
/// ja: confirm-run。`status` は running bit を見る。`pc` はさらに halt→dpc→flash 判定→resume。
fn confirm_run(session: &mut Session, mode: ConfirmRunMode) -> bool {
    let flash = session
        .flash_capacity()
        .0
        .filter(|&n| n > 0)
        .unwrap_or(0x10_0000);
    let h417 = crate::cmd_target::db_family_of(session).eq_ignore_ascii_case("CH32H417");
    let mut dm = session.dm();
    match mode {
        ConfirmRunMode::Status => dm.is_running().unwrap_or(false),
        ConfirmRunMode::Pc => {
            if dm.halt().is_err() {
                return false;
            }
            let pc = dm.read_reg(ch32rv_dmi::RegName::Pc).ok();
            let _ = dm.resume();
            pc.is_some_and(|p| pc_runs_the_image(p, flash, h417))
        }
    }
}

/// en: Whether `pc` lies where a flashed image runs: code flash, through its low alias or the
/// 0x0800_0000 window (`flash` bytes of it); on a CH32H417 also RAM_CODE (0x2010_0000, where
/// the boot copies the code and it runs - ch32rv's loader at 0x2012_0000 is not that). SRAM
/// elsewhere means the program never left a reset stub or loader.
/// ja: pc がイメージの走る場所か。code flash(別名と 0x0800_0000)、H417 は RAM_CODE も。
fn pc_runs_the_image(p: u32, flash: u32, h417: bool) -> bool {
    p < flash
        || (0x0800_0000..0x0800_0000u32.saturating_add(flash)).contains(&p)
        || (h417 && (0x2010_0000..0x2012_0000).contains(&p))
}

#[allow(clippy::too_many_arguments)]
fn finish(
    cli: &Cli,
    cmd: &str,
    family: &str,
    programmer: &str,
    total_bytes: u64,
    erase: &EraseReport,
    skipped: bool,
    verified: Option<bool>,
    running: Option<bool>,
    warnings: Vec<Warning>,
    error: Option<(ErrorKind, String)>,
) -> ExitCode {
    let ok = error.is_none();
    if cli.json {
        let mut env = if let Some((kind, msg)) = &error {
            ResultEnvelope::failure(cmd, *kind, msg.clone())
        } else {
            ResultEnvelope::success(cmd)
        };
        env.result = Some(serde_json::json!({
            "flash": {
                "bytes": total_bytes,
                "family": family,
                "programmer": programmer,
                "skipped": skipped,
                "scope": erase.scope,
                "verified": verified,
                "running": running,
            }
        }));
        if let (Some(store), Some(r)) = (&erase.store, env.result.as_mut()) {
            r["flash"]["store"] = store.clone();
        }
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        if ok {
            if skipped {
                // --preverify: the target already held the image, so nothing was erased/programmed.
                println!("preverify: target already matches - skipped");
            } else {
                println!("flashed {total_bytes} bytes to {family}");
                println!("erase:   {}", erase.human());
                if let Some(v) = verified {
                    println!(
                        "verify:  {}",
                        if v {
                            "OK (readback matches)"
                        } else {
                            "MISMATCH"
                        }
                    );
                }
            }
            if let Some(r) = running {
                println!("running: {}", if r { "yes" } else { "NO" });
            }
        }
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        if let Some((kind, msg)) = &error {
            eprintln!("ch32rv: error[{}]: {msg}", kind.as_str());
            return kind.exit_code().into();
        }
        ExitCode::SUCCESS
    }
}

pub fn erase(cli: &Cli, args: &crate::args::EraseArgs) -> ExitCode {
    const CMD: &str = "erase";
    match crate::oep::addr(cli, CMD) {
        Ok(Some(_)) => return erase_oep(cli, args),
        Ok(None) => {}
        Err(c) => return c,
    }
    if args.region.is_some() || args.range.is_some() {
        return erase_range(cli, args);
    }
    // --all: whole-chip erase.
    let mut session = match crate::cmd_probe::attach(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if let Err(why) = confirm_destructive(cli, "Erase the entire chip flash?") {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            why,
            Some("pass --yes to confirm"),
        );
    }
    let mut warnings = Vec::new();
    clear_stale_flash_mode(&mut session, &mut warnings);
    if let Err(e) = session.link().erase_flash() {
        return fail(
            cli,
            CMD,
            ErrorKind::TransferFailed,
            format!("erase failed: {e}"),
            None,
        );
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "scope": "chip", "family": session.family() }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        println!("erased entire chip flash ({})", session.family());
        ExitCode::SUCCESS
    }
}
/// en: `erase` through an OEP probe: page erases by the FLASH controller over the Debug Module,
/// for `--all` the whole code flash (the size from the DB by the chip id), for `--range` /
/// `--region` the page-aligned span. A WCH-Link's own chip erase is not there.
/// ja: OEP の probe での `erase`。Debug Module で FLASH controller の page 消去を回す。`--all` は
/// code flash 全体(大きさは chip id から DB で引く)、`--range` / `--region` は page 境界の範囲。
fn erase_oep(cli: &Cli, args: &crate::args::EraseArgs) -> ExitCode {
    const CMD: &str = "erase";
    crate::cmd_target::on_target(cli, CMD, |t, family, _db_family| {
        let Some(profile) = ch32rv_flash::flash_controller_profile_for(family) else {
            return fail(
                cli,
                CMD,
                ErrorKind::CapabilityUnsupported,
                format!("erasing {family} through the FLASH controller is not supported yet"),
                None,
            );
        };
        let page = profile.page_size;
        let flash_bytes = t
            .chip_id()
            .and_then(
                |id| match ch32rv_target::Db::builtin().resolve_by_chip_id(id) {
                    ch32rv_target::Resolution::Sku(s) => Some(s.flash_bytes),
                    _ => None,
                },
            )
            .unwrap_or(0);
        let (start, len, scope) = if let Some(r) = &args.range {
            match parse::range(r) {
                Ok((s, l)) => (s, l, "range"),
                Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
            }
        } else if let Some(region) = &args.region {
            match parse::resolve_region(region, flash_bytes, flash_bytes, None) {
                Ok((start, _)) if !(CODE_FLASH_START..0x1000_0000).contains(&start) => {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::Usage,
                        "erase operates on the flash (code) region only",
                        Some("to erase a sub-range use --region code+<off>+<len> or --range"),
                    );
                }
                Ok((s, l)) => (s, l, "range"),
                Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
            }
        } else {
            if flash_bytes == 0 {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TargetNotInDb,
                    "the code flash size is not known for this chip id, so --all cannot be sized",
                    Some("erase a span with --range instead"),
                );
            }
            (CODE_FLASH_START, flash_bytes, "chip")
        };
        if len == 0 {
            return fail(cli, CMD, ErrorKind::Usage, "empty range", None);
        }
        if start % page != 0 || len % page != 0 {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!(
                    "range 0x{start:08x}+0x{len:x} is not aligned to the {page}-byte flash page"
                ),
                Some("align both the start and the length to the page size"),
            );
        }
        let pages = len / page;
        let end = start.saturating_add(len);
        if let Err(why) = confirm_destructive(
            cli,
            &format!("Erase {len} bytes ({pages} page(s)) at 0x{start:08x}..0x{end:08x}?"),
        ) {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                why,
                Some("pass --yes to confirm"),
            );
        }
        if let Err(e) = t.halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt failed: {e}"),
                None,
            );
        }
        // en: A page erase that fails is tried again after halting anew (an erase is idempotent):
        // on CH32L103 over OEP one in many stopped with a DMI error now and then.
        // ja: 失敗した page erase は、止め直してやり直す(消去は何度やっても同じ)。
        for i in 0..pages {
            let addr = start + i * page;
            let mut last = None;
            for _ in 0..3 {
                let r = {
                    let mut d = t.dtm();
                    ch32rv_dmi::DebugModule::new(&mut d).flash_page_erase(addr, profile.mode)
                };
                match r {
                    Ok(()) => {
                        last = None;
                        break;
                    }
                    Err(e) => {
                        last = Some(e);
                        let _ = t.halt();
                    }
                }
            }
            if let Some(e) = last {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("page erase failed at 0x{addr:08x}: {e}"),
                    None,
                );
            }
        }
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({
                "scope": scope,
                "addr": format!("0x{start:08x}"),
                "len": len,
                "pages": pages,
                "page_size": page,
                "family": family,
            }));
            crate::print_envelope(&env)
        } else {
            println!(
                "erased {pages} page(s) ({len} bytes) at 0x{start:08x}..0x{end:08x} ({family})"
            );
            ExitCode::SUCCESS
        }
    })
}

/// en: `erase --range <a+len|a..b>` / `--region code[+off+len]`: page-granular erase via the
/// direct FLASH controller (docs/cli.ja.md §4.1). Requires page alignment (fail-closed) since
/// erase is per-page. `code` resolves to the probe-reported code-flash window.
/// ja: `erase --range/--region` を FLASH controller の page 単位消去で実装。page 境界必須。
fn erase_range(cli: &Cli, args: &crate::args::EraseArgs) -> ExitCode {
    const CMD: &str = "erase";
    let mut session = match crate::cmd_probe::attach(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    clear_stale_flash_mode(&mut session, &mut Vec::new());
    let family = session.attach.family_byte;
    let Some(profile) = ch32rv_flash::flash_controller_profile(family) else {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "range/region erase is not yet supported for {} (0x{family:02x})",
                session.family()
            ),
            Some(
                "verified so far: V20x/V30x, V003/CH641, V00x, X035/CH643, L103 (CH32V103 is a follow-up)",
            ),
        );
    };
    let page = profile.page_size;

    // Resolve (start, len) from --range or --region.
    let (start, len) = if let Some(r) = &args.range {
        match parse::range(r) {
            Ok(v) => v,
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        }
    } else if let Some(region) = &args.region {
        let flash_bytes = session.flash_capacity().0.unwrap_or(0);
        // erase is flash-only; pass flash_bytes for the SRAM size too so a `ram` spec still resolves
        // to a non-empty range and is then caught by the flash-window check below with a clear error.
        // erase rejects non-flash regions below, so the option base is irrelevant here.
        match parse::resolve_region(region, flash_bytes, flash_bytes, None) {
            // erase operates on flash pages only, so reject a non-flash region (e.g. ram/option).
            Ok((start, _)) if !(CODE_FLASH_START..0x1000_0000).contains(&start) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "erase operates on the flash (code) region only",
                    Some("to erase a sub-range use --region code+<off>+<len> or --range"),
                );
            }
            Ok(v) => v,
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        }
    } else {
        return fail(cli, CMD, ErrorKind::Usage, "no --range or --region", None);
    };

    if len == 0 {
        return fail(cli, CMD, ErrorKind::Usage, "empty range", None);
    }
    // Erase is per-page: demand page alignment so we never silently wipe neighbours.
    if start % page != 0 || len % page != 0 {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("range 0x{start:08x}+0x{len:x} is not aligned to the {page}-byte flash page"),
            Some("align both the start and the length to the page size"),
        );
    }
    let pages = len / page;
    let end = start.saturating_add(len);

    if let Err(why) = confirm_destructive(
        cli,
        &format!("Erase {len} bytes ({pages} page(s)) at 0x{start:08x}..0x{end:08x}?"),
    ) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            why,
            Some("pass --yes to confirm"),
        );
    }

    if let Err(e) = session.dm().halt() {
        return fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }
    let mode = profile.mode;
    let mut dm = session.dm();
    for i in 0..pages {
        let addr = start + i * page;
        if let Err(e) = dm.flash_page_erase(addr, mode) {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("page erase failed at 0x{addr:08x}: {e}"),
                None,
            );
        }
    }

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "scope": "range",
            "addr": format!("0x{start:08x}"),
            "len": len,
            "pages": pages,
            "page_size": page,
            "family": family,
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "erased {pages} page(s) ({len} bytes) at 0x{start:08x}..0x{end:08x} ({})",
            session.family()
        );
        ExitCode::SUCCESS
    }
}

pub fn reset(cli: &Cli, args: &crate::args::ResetArgs) -> ExitCode {
    // An OEP probe (`oep://`, or a serial port no WCH-Link owns) takes the OEP path.
    match crate::oep::addr(cli, "reset") {
        Ok(Some(a)) => return crate::oep::reset(cli, args, &a),
        Ok(None) => {}
        Err(c) => return c,
    }
    const CMD: &str = "reset";
    let mut session = match crate::cmd_probe::attach(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    if args.dm {
        // Reset the debug module only (no target reset).
        // Best-effort: DMCONTROL dmactive toggle is inside the DM layer's halt path; here we
        // just detach/re-attach which re-initializes the DM.
        let _ = session.link().detach_chip();
        let _ = session.link().attach_chip();
    } else if args.halt {
        // Reset and halt: soft reset, then halt immediately.
        if let Err(e) = session.link().soft_reset() {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("reset failed: {e}"),
                None,
            );
        }
        let mut dm = session.dm();
        if let Err(e) = dm.halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt after reset failed: {e}"),
                None,
            );
        }
    } else if let Err(msg) = soft_reset_and_run(&mut session) {
        return fail(cli, CMD, ErrorKind::TransferFailed, msg, None);
    }

    let mut running = None;
    if let Some(mode) = args.confirm_run
        && !args.halt
        && !args.dm
    {
        std::thread::sleep(Duration::from_millis(200));
        running = Some(confirm_run(&mut session, mode));
    }

    if cli.json {
        let mode = if args.dm {
            "dm"
        } else if args.halt {
            "halt"
        } else {
            "run"
        };
        let mut env = if running == Some(false) {
            ResultEnvelope::failure(
                CMD,
                ErrorKind::NotRunningAfterWrite,
                "target not running after reset",
            )
        } else {
            ResultEnvelope::success(CMD)
        };
        env.result = Some(serde_json::json!({ "mode": mode, "running": running }));
        crate::print_envelope(&env)
    } else {
        let what = if args.dm {
            "debug module reset"
        } else if args.halt {
            "reset and halted"
        } else {
            "reset, running"
        };
        println!("{what}");
        if running == Some(false) {
            eprintln!("ch32rv: error[not-running-after-write]: target not running after reset");
            return ErrorKind::NotRunningAfterWrite.exit_code().into();
        }
        ExitCode::SUCCESS
    }
}

pub fn verify(cli: &Cli, args: &crate::args::VerifyArgs) -> ExitCode {
    const CMD: &str = "verify";
    let bytes = match std::fs::read(&args.file) {
        Ok(d) => d,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("read {}: {e}", args.file.display()),
                None,
            );
        }
    };
    match crate::oep::addr(cli, CMD) {
        Ok(Some(a)) => return crate::oep::verify(cli, args, &bytes, &a),
        Ok(None) => {}
        Err(c) => return c,
    }
    let bin_offset = match &args.at {
        Some(s) => match parse::u32_addr(s) {
            Ok(a) => Some(a),
            Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
        },
        None => None,
    };
    let mut session = match crate::cmd_probe::attach(cli, CMD) {
        Ok(s) => s,
        Err(c) => return c,
    };
    // Verify only reads the flash back, which needs nothing of the family (it used to be gated
    // on a write path, which left CH32H417 / CH32M030 out).
    let image = match parse_image(
        &bytes,
        args.format,
        &args.file,
        bin_offset,
        CODE_FLASH_START,
    ) {
        Ok(i) => i,
        Err(e) => return fail(cli, CMD, ErrorKind::Usage, e.to_string(), None),
    };
    if let Err(e) = session.dm().halt() {
        return fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }
    for seg in &image.segments {
        // Fast bulk read via the WCH-Link; fall back to DMI word reads on error.
        let read = match session.link().read_mem(seg.addr, seg.data.len() as u32) {
            Ok(d) => Ok(d),
            Err(_) => session.dm().read_mem(seg.addr, seg.data.len() as u32),
        };
        match read {
            Ok(readback) => {
                if readback != seg.data {
                    let at = readback
                        .iter()
                        .zip(&seg.data)
                        .position(|(a, b)| a != b)
                        .unwrap_or(0);
                    if cli.json {
                        let env = ResultEnvelope::failure(
                            CMD,
                            ErrorKind::VerifyMismatch,
                            format!("mismatch at {:#010x}", seg.addr as usize + at),
                        );
                        return crate::print_envelope(&env);
                    }
                    eprintln!("verify: MISMATCH at {:#010x}", seg.addr as usize + at);
                    return ErrorKind::VerifyMismatch.exit_code().into();
                }
            }
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("readback failed: {e}"),
                    None,
                );
            }
        }
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({ "bytes": image.total_len(), "verified": true }));
        crate::print_envelope(&env)
    } else {
        println!("verify: OK ({} bytes match)", image.total_len());
        ExitCode::SUCCESS
    }
}

pub fn recover(cli: &Cli, args: &RecoverArgs) -> ExitCode {
    // en: power-off / nrst / unbrick are WCH-Link commands (its special erase, its power switch):
    // an OEP probe has neither, and says so rather than looking for a WCH-Link. The diagnosis,
    // auto and unprotect work through the Debug Module on either.
    // ja: power-off / nrst / unbrick は WCH-Link の命令(特殊消去、電源)なので OEP では断る。
    // en: `--image` is written only by an OEP probe's `--method nrst`; anywhere else it is refused.
    // ja: `--image` を書くのは OEP の `--method nrst` だけ。ほかでは断る。
    if args.image.is_some() {
        let oep_nrst = args.method == Some(RecoverMethod::Nrst)
            && match crate::oep::addr(cli, "recover") {
                Ok(a) => a.is_some_and(|a| !matches!(a, crate::oep::OepAddr::Wch(_))),
                Err(c) => return c,
            };
        if !oep_nrst {
            return fail(
                cli,
                "recover",
                ErrorKind::CapabilityUnsupported,
                "--image is written only by `--method nrst` on an OEP probe",
                Some("flash the image after the recovery: `ch32rv flash <FILE>`"),
            );
        }
    }
    if matches!(
        args.method,
        Some(RecoverMethod::PowerOff | RecoverMethod::Nrst | RecoverMethod::Unbrick)
    ) {
        match crate::oep::addr(cli, "recover") {
            Ok(Some(a)) if args.method == Some(RecoverMethod::Nrst) => {
                return recover_nrst_oep(cli, &a, args.image.as_deref());
            }
            Ok(Some(_)) => {
                return fail(
                    cli,
                    "recover",
                    ErrorKind::CapabilityUnsupported,
                    "power-off and unbrick use a WCH-Link's own commands; an OEP probe has none of them",
                    Some(
                        "on an OEP probe: `ch32rv recover` (diagnose), `--method nrst`, `--method unprotect`, or `target protect off`",
                    ),
                );
            }
            Ok(None) => {}
            Err(c) => return c,
        }
    }
    match args.method {
        None => crate::cmd_recover::diagnose_only(cli),
        Some(RecoverMethod::Auto) => crate::cmd_recover::auto(cli),
        Some(m @ (RecoverMethod::PowerOff | RecoverMethod::Nrst)) => recover_special_erase(cli, m),
        Some(RecoverMethod::Unprotect) => recover_unprotect(cli),
        Some(RecoverMethod::Unbrick) => recover_unbrick(cli),
    }
}

/// en: `recover --method nrst` through an OEP probe: hold the port's NRST line (its label,
/// oep-spec host guide §5.3) through a halting attach, so the hart stops before the firmware runs
/// (that firmware may have turned the debug pins into GPIOs), then clear what keeps a normal flash
/// out: read protection (writing it off mass-erases), or else the whole code flash by page erases.
/// A stop away from the reset vector is reported: the NRST line may be unwired, or turned into a
/// GPIO by the option bytes (RST_MODE).
/// ja: OEP の probe での `recover --method nrst`。スロットの NRST の線(label)をかけながら止める
/// attach で、firmware が走る前に止め、保護を外す(全消去)か、code flash を page 消去する。
/// リセットベクタ以外で止まったら、NRST が配線されていないか RST_MODE で GPIO にされている可能性を言う。
fn recover_nrst_oep(
    cli: &Cli,
    a: &crate::oep::OepAddr,
    image_path: Option<&std::path::Path>,
) -> ExitCode {
    const CMD: &str = "recover";
    // The image to leave behind, read before anything is touched.
    let image = match image_path {
        Some(path) => match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| {
                parse_image(
                    &b,
                    ch32rv_contract::policy::ImageFormat::Auto,
                    path,
                    None,
                    CODE_FLASH_START,
                )
                .map_err(|e| e.to_string())
            }) {
            Ok(i) => Some(i),
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    format!("--image {}: {e}", path.display()),
                    None,
                );
            }
        },
        None => None,
    };
    if let Err(why) = confirm_destructive(
        cli,
        "ERASE the target's code flash, attaching under its NRST line?",
    ) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            why,
            Some("pass --yes to confirm"),
        );
    }
    crate::oep::with_attached_reset(cli, CMD, a, true, true, |x| {
        let stopped = x.halted_at;
        let mut warnings = Vec::new();
        if stopped != Some(0) {
            let at = stopped.map_or("(not halted)".to_owned(), |d| format!("0x{d:08x}"));
            warnings.push(format!(
                "the hart stopped at {at}, not at the reset vector: the NRST line may not be wired, or the option bytes may have turned it into a GPIO (RST_MODE)"
            ));
        }
        let family = x.family.clone().unwrap_or_else(|| "unknown".to_owned());
        let flash_bytes = x
            .chip_id
            .and_then(
                |id| match ch32rv_target::Db::builtin().resolve_by_chip_id(id) {
                    ch32rv_target::Resolution::Sku(s) => Some(s.flash_bytes),
                    _ => None,
                },
            )
            .unwrap_or(0);
        let option_base = match crate::cmd_target::option_base(&family) {
            Ok(b) => b,
            Err(msg) => return fail(cli, CMD, ErrorKind::CapabilityUnsupported, msg, None),
        };
        let mut t = crate::cmd_target::OnTarget::Oep(x);
        let protected = {
            let mut d = t.dtm();
            let mut dm = ch32rv_dmi::DebugModule::new(&mut d);
            let cur = match dm.read_mem(option_base, 16) {
                Ok(v) if v.len() == 16 => {
                    let mut c = [0u8; 16];
                    c.copy_from_slice(&v);
                    Some(c)
                }
                _ => None,
            };
            let protected = cur.is_none_or(|c| c[0] != 0xA5);
            if protected {
                let image = match cur.filter(crate::cmd_target::option_bytes_plausible) {
                    Some(c) => crate::cmd_target::unprotect_image(&c),
                    None => crate::cmd_target::BLANKET_FACTORY,
                };
                if let Err(e) = dm.flash_program_option_bytes(option_base, &image) {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TransferFailed,
                        format!("clearing read protection failed: {e}"),
                        None,
                    );
                }
            } else {
                let Some(profile) = ch32rv_flash::flash_controller_profile_for(&family) else {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::CapabilityUnsupported,
                        format!(
                            "erasing {family} through the FLASH controller is not supported yet"
                        ),
                        None,
                    );
                };
                if flash_bytes == 0 {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TargetNotInDb,
                        "the code flash size is not known for this chip id",
                        None,
                    );
                }
                // en: The reset vector's page first, so the firmware that kept the debugger out
                // no longer starts whatever happens next (a few tries: the DMI page erase was
                // seen to stop now and then on CH32L103 over OEP).
                // ja: 先にリセットベクタのページを消し、何があっても壊れた firmware が起動しない
                // ようにする(L103 で DMI の page erase が時々止まったので数回試す)。
                let mut first = Err(ch32rv_dmi::DmiError::OperationFailed("not tried".into()));
                for _ in 0..3 {
                    first = dm.flash_page_erase(CODE_FLASH_START, profile.mode);
                    if first.is_ok() {
                        break;
                    }
                }
                if let Err(e) = first {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::TransferFailed,
                        format!("erasing the reset vector's page failed: {e}"),
                        None,
                    );
                }
            }
            protected
        };
        // en: The rest through the RAM loader, which erases and writes whole pages and reads
        // them back: the image given, else the erased word over the whole code flash.
        // ja: 残りは RAM loader で(ページを消して書き、読み返す)。イメージか、消去済みの値。
        if !protected {
            let Some(plan) = ch32rv_flash::loader::plan_for_family(&family) else {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::CapabilityUnsupported,
                    format!("the device DB has no loader plan for {family}"),
                    None,
                );
            };
            let segments = match &image {
                Some(i) => i.segments.clone(),
                None => {
                    let word = ch32rv_target::flash_geometry(&family)
                        .and_then(|g| g.erased_word)
                        .unwrap_or(0xFFFF_FFFF);
                    vec![ch32rv_flash::Segment {
                        addr: CODE_FLASH_START,
                        data: word.to_le_bytes().repeat(flash_bytes as usize / 4),
                    }]
                }
            };
            let crate::cmd_target::OnTarget::Oep(x) = &mut t else {
                unreachable!("an OEP connection");
            };
            if let Err(e) = ch32rv_flash::loader::program(&mut x.t, plan, &segments, &mut |_, _| {})
            {
                return fail(
                    cli,
                    CMD,
                    loader_error_kind(&e),
                    format!("writing the code flash failed: {e}"),
                    None,
                );
            }
        }
        t.system_reset();
        let how = match (protected, image.is_some()) {
            (true, _) => "read protection cleared (mass erase)",
            (false, true) => "image written",
            (false, false) => "code flash erased",
        };
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.warnings = warnings
                .iter()
                .map(|m| ch32rv_contract::Warning {
                    code: "nrst-not-at-reset-vector".to_owned(),
                    msg: m.clone(),
                })
                .collect();
            env.result = Some(serde_json::json!({
                "method": "nrst",
                "family": family,
                "stopped_at": stopped.map(|d| format!("0x{d:08x}")),
                "unprotected": protected,
                "erased": image.is_none(),
                "image": image.as_ref().map(|i| i.total_len()),
            }));
            crate::print_envelope(&env)
        } else {
            for w in &warnings {
                eprintln!("warning[nrst-not-at-reset-vector]: {w}");
            }
            println!("recover nrst: {how} on {family} (attached under NRST)");
            ExitCode::SUCCESS
        }
    })
}

/// en: `recover --method unprotect`: clear read protection by writing factory-default option bytes
/// (RDPR=0xA5). On a read-protected target this triggers the chip's mass erase, unbricking it. The
/// target must still attach over DMI (a fully-dead target needs power-off/nrst/unbrick instead).
/// ja: `recover --method unprotect`: 工場 option bytes(RDPR=0xA5)を書いて読み出し保護を解除。保護
/// 済み target ではこれが chip の mass erase を誘発して復旧する。attach は要る(完全死は power-off 等)。
pub(crate) fn recover_unprotect(cli: &Cli) -> ExitCode {
    const CMD: &str = "recover";
    if let Err(why) = confirm_destructive(
        cli,
        "Remove read protection? This ERASES ALL FLASH on a protected target.",
    ) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            why,
            Some("pass --yes to confirm"),
        );
    }
    // On a WCH-Link session or an OEP connection alike (the Debug Module does the work).
    crate::cmd_target::on_target(cli, CMD, |t, family, db_family| {
        // The option block is not at the same address on every part (CH32M030: 0x1FFF_F300), so
        // take it from the device DB rather than assuming - writing factory bytes to the wrong
        // address would program arbitrary memory.
        let option_base = match crate::cmd_target::option_base(db_family) {
            Ok(b) => b,
            Err(msg) => return fail(cli, CMD, ErrorKind::CapabilityUnsupported, msg, None),
        };
        if let Err(e) = t.halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt failed: {e}"),
                None,
            );
        }
        let preserved = {
            let mut d = t.dtm();
            let mut dm = ch32rv_dmi::DebugModule::new(&mut d);
            // Clear read protection without touching anything else the part carries: read the
            // current bytes and replace only RDPR. A blanket USER=0xff would repartition SRAM on
            // V20x/V307 (`RAM_CODE_MOD`) and change the NRST pin function on V003 (`RST_MODE`) -
            // neither is part of "remove read protection". A protected part may not hand its
            // option bytes back, so fall back to the blanket image and say so.
            let (image, preserved) = match dm.read_mem(option_base, 16) {
                Ok(v) if v.len() == 16 => {
                    let mut cur = [0u8; 16];
                    cur.copy_from_slice(&v);
                    if crate::cmd_target::option_bytes_plausible(&cur) {
                        (crate::cmd_target::unprotect_image(&cur), true)
                    } else {
                        (crate::cmd_target::BLANKET_FACTORY, false)
                    }
                }
                _ => (crate::cmd_target::BLANKET_FACTORY, false),
            };
            if !preserved {
                crate::warn(
                    "option-defaults",
                    "the target's current option bytes could not be read back, so family-specific USER bits (SRAM split, NRST mode) are set to 0xff rather than preserved",
                );
            }
            if let Err(e) = dm.flash_program_option_bytes(option_base, &image) {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("writing option bytes failed: {e}"),
                    None,
                );
            }
            preserved
        };
        // Apply the option change with a system reset.
        t.system_reset();
        if cli.json {
            let mut env = ResultEnvelope::success(CMD);
            env.result = Some(serde_json::json!({
                "method": "unprotect", "family": family, "preserved_user_bits": preserved, "note": "read protection cleared (RDPR=0xA5); applies after reset",
            }));
            crate::print_envelope(&env)
        } else {
            println!("recover unprotect: read protection cleared (RDPR=0xA5) on {family}");
            println!("note: a protected target is mass-erased; re-flash your firmware");
            ExitCode::SUCCESS
        }
    })
}

/// en: `recover --method unbrick`: attach-based escalation. Attaches the target, then clears
/// whatever keeps it from a normal flash - read protection (write factory option bytes, which
/// triggers the chip's mass erase) if set, otherwise a whole-chip erase - and applies it with a
/// reset. A target that cannot attach at all (its app repurposed SWDIO/SWCLK) is beyond this:
/// point the user at the no-attach `--method power-off --chip <family>` instead.
/// ja: `recover --method unbrick`: attach ベースのエスカレーション復旧。attach して、通常 flash を
/// 妨げているもの(保護 ON なら工場 option bytes 書込=mass erase、そうでなければ chip erase)を
/// クリアし reset で反映。全く attach できない target(app が SWD ピンを転用)は対象外で、非 attach の
/// `--method power-off --chip <family>` を案内する。
fn recover_unbrick(cli: &Cli) -> ExitCode {
    const CMD: &str = "recover";
    if let Err(why) = confirm_destructive(
        cli,
        "Unbrick this target? This ERASES ALL FLASH (and clears read protection if set).",
    ) {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            why,
            Some("pass --yes to confirm"),
        );
    }
    // Quiet attach: on failure give the tailored "use power-off" hint rather than a generic error.
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; this needs RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(m) => return fail(cli, CMD, ErrorKind::Usage, m, None),
    };
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let mut session = match Session::attach(
        &entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        cli.db.as_deref(),
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(_) => {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                "unbrick needs the target to attach over DMI, but attach failed",
                Some(
                    "if the app sleeps or repurposed SWDIO/SWCLK: retry with --attach-wait 10 and press the target's RESET while it waits, or use recover --method power-off --chip <family>",
                ),
            );
        }
    };
    let family = session.family();
    // Per-family option-byte block base from the device DB (not universal: CH32M030 uses
    // 0x1FFF_F300), used for both the RDPR probe and the factory write below.
    let db_family = crate::cmd_target::db_family_of(&mut session);
    let option_base = match crate::cmd_target::option_base(&db_family) {
        Ok(b) => b,
        Err(msg) => return fail(cli, CMD, ErrorKind::CapabilityUnsupported, msg, None),
    };

    // Decide and apply under a halted hart: read protection off -> chip erase; on -> factory
    // option bytes (mass erase). Scope the DM borrow so we can reach the probe afterwards.
    let protected;
    {
        let mut dm = session.dm();
        if let Err(e) = dm.halt() {
            return fail(
                cli,
                CMD,
                ErrorKind::AttachFailed,
                format!("halt failed: {e}"),
                None,
            );
        }
        // Option byte 0 (RDPR) at the family's option base; 0xA5 = read protection disabled.
        protected = match dm.read_mem(option_base, 1) {
            Ok(b) => b.first().copied() != Some(0xA5),
            Err(_) => false,
        };
        // Same reasoning as `--method unprotect`: keep every USER bit the part carries.
        let image = match dm.read_mem(option_base, 16) {
            Ok(v) if v.len() == 16 => {
                let mut cur = [0u8; 16];
                cur.copy_from_slice(&v);
                if crate::cmd_target::option_bytes_plausible(&cur) {
                    crate::cmd_target::unprotect_image(&cur)
                } else {
                    crate::cmd_target::BLANKET_FACTORY
                }
            }
            _ => crate::cmd_target::BLANKET_FACTORY,
        };
        if protected && let Err(e) = dm.flash_program_option_bytes(option_base, &image) {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("clearing read protection failed: {e}"),
                None,
            );
        }
    }
    let action = if protected {
        "read protection cleared (RDPR=0xA5, mass erase)"
    } else {
        // en: Attach afresh first, as `erase --all` erases straight after its attach. A WCH-Link
        // (CH549) fw 2.12 on a CH32V103 left the chip erase unanswered (timeout, then a stalled
        // endpoint until a replug) after the halt and option-byte reads above (2026-10-30).
        // ja: `erase --all` と同じく attach し直してから消す。上の halt と option byte の読み出しの
        // 後だと、WCH-Link(CH549)fw 2.12 + V103 で chip erase が返らなかった。
        let link = session.link();
        let _ = link.detach_chip();
        let erased = link.attach_chip().and_then(|_| link.erase_flash());
        if let Err(e) = erased {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("chip erase failed: {e}"),
                None,
            );
        }
        "whole-chip erase"
    };
    // Apply (a protection change needs a system reset; a plain erase just re-runs blank flash).
    let _ = session.link().soft_reset();

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.result = Some(serde_json::json!({
            "method": "unbrick", "family": family, "protected": protected, "action": action,
        }));
        crate::print_envelope(&env)
    } else {
        println!("recover unbrick: {action} on {family}");
        println!("note: flash is now blank; re-flash your firmware");
        ExitCode::SUCCESS
    }
}

/// en: "Clear All Code Flash - By Power off / RST pin". Does NOT attach the target first
/// (the point is to recover a target you cannot attach). Needs `--chip` to know the family
/// byte, since we cannot read it without attaching.
/// ja: 「Clear All Code Flash」。attach しない(attach できない target の復旧が目的)。
/// family byte を読めないため `--chip` が要る。
pub(crate) fn recover_special_erase(cli: &Cli, method: RecoverMethod) -> ExitCode {
    const CMD: &str = "recover";
    let Some(chip) = cli.chip.as_deref() else {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "special erase needs --chip <family> (the target cannot be probed for it)",
            Some("e.g. --chip CH32V203 or the family name; see `ch32rv db list`"),
        );
    };
    let Some(family_byte) = family_byte_from_name(chip) else {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            format!("unknown --chip `{chip}` for special erase (family not recognized)"),
            None,
        );
    };
    // en: The special erase is the WCH-Link's own command, keyed by the family byte. It answers
    // `0x0f` when the erase took and `0x00` when it did not, so every family whose byte is known
    // may try it. It used to be gated on wlink's stub table, which was never a measurement (the
    // only one is X035's `0x0f`, wch-protocols E162 / E164; E167 found the "power-off" does not
    // cut 3V3 at all) and left CH32H417 and the newer parts out. A family not seen confirming is
    // reported as unconfirmed below.
    // ja: 特殊消去は WCH-Link 自身の命令(family byte で指定)。効けば `0x0f`、効かなければ `0x00`
    // を返すので、family byte の分かるすべての family で試せる。以前は wlink の stub 表で絞っていた
    // が、実測の裏付けは無かった(X035 の `0x0f` だけ。E167 では「電源断」で 3V3 は切れていない)。
    // en: Measured to do harm on a CH32H417 (2026-10-20, WCH-LinkE 2.22): the Link's own erase
    // routine for it left half-erased words in the code flash and rewrote the option bytes (USER
    // 0xff -> 0x3f, dropping USB / USART download; DATA and WRP left at the erased 0xe339). An
    // attach under reset reaches such a chip without it.
    // ja: H417 では実害を確かめた(LinkE の消去が code flash に消え残りを作り、option byte の USER /
    // DATA / WRP を崩した)。reset を押さえた attach で代わりに届く。
    if family_byte == 0xC6 {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "the WCH-Link's special erase damages a {chip}: it leaves half-erased flash and rewrites the option bytes"
            ),
            Some(
                "attach under reset instead (RST wired to NRST) and let ch32rv erase: `--connect-under-reset flash <image>` or `--connect-under-reset erase --range <a>..<b>`",
            ),
        );
    }
    if !params_for_family(family_byte).is_some_and(|p| p.supports_special_erase) {
        crate::warn(
            "special-erase-untried",
            format!(
                "the special erase has not been seen working on {chip} here; the probe's answer tells"
            ),
        );
    }

    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(c) => return c,
    };
    // Only LinkE/LinkW implement the "power-off" special erase.
    let mut link = match ch32rv_wchlink::WchLink::open(&entry.dev) {
        Ok(l) => l,
        Err(e) => return fail(cli, CMD, ErrorKind::DeviceOpenFailed, e.to_string(), None),
    };
    link.set_timeout(Duration::from_millis(
        cli.timeout.map(|s| s * 1000).unwrap_or(5000),
    ));

    let variant = link.probe_info().ok().map(|i| i.variant);
    let power_capable = matches!(
        variant,
        Some(ch32rv_wchlink::Variant::LinkE) | Some(ch32rv_wchlink::Variant::LinkW)
    );
    if method == RecoverMethod::PowerOff && !power_capable {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            "only WCH-LinkE / LinkW implement the power-off special erase",
            Some("use --method nrst with the RST pin wired, or a LinkE/LinkW probe"),
        );
    }

    // en: The probe answers `0x0f` when the erase took and `0x00` when it did not: on a CH32X035
    // the first attempt after the target stopped answering has come back `0x00` (~2.1 s) with the
    // flash untouched, and the second `0x0f` (~0.2 s) with it erased (wch-protocols E162 / E164;
    // E165 got `0x0f` first time). A `0x00` is an attempt that did not get the halt; in the stopped
    // state a reset without a power loss follows and the next attempt erases (E168) - so repeat
    // on `0x00`. Other families' answers are not recorded, so a run that never sees `0x0f` is
    // reported as unconfirmed rather than failed.
    // ja: probe は消えたら `0x0f`、消えていなければ `0x00` を返す(X035 で 1 回目が `00`、2 回目
    // `0f`。E162 / E164)。`00` なら繰り返す。他 family の応答は未記録なので、`0f` が一度も来なくても
    // 失敗ではなく「未確認」として報告する。
    const ATTEMPTS: usize = 3;
    let mut answers: Vec<u8> = Vec::new();
    for _ in 0..ATTEMPTS {
        let res = match method {
            RecoverMethod::PowerOff => link.erase_code_flash_by_power_off(family_byte),
            RecoverMethod::Nrst => link.erase_code_flash_by_rst(family_byte),
            _ => unreachable!(),
        };
        match res {
            Ok(b) => {
                answers.push(b);
                if b != 0x00 {
                    break;
                }
            }
            Err(e) => {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::TransferFailed,
                    format!("special erase failed: {e}"),
                    None,
                );
            }
        }
    }
    let confirmed = answers.last() == Some(&0x0f);
    let answers_hex: Vec<String> = answers.iter().map(|b| format!("0x{b:02x}")).collect();

    // en: The special erase leaves the probe holding a stale (corrupted) readback of the target;
    // clear it so a follow-up attach/read is clean (board-identify's re-detect recovery).
    // ja: 特殊消去の後は probe が壊れ読み値を保持するため、redetect でクリアして次の attach を綺麗にする。
    let _ = link.redetect_chip();
    let _ = link.detach_chip();

    let mut warnings = Vec::new();
    if !confirmed {
        warnings.push(Warning {
            code: "special-erase-unconfirmed".to_owned(),
            msg: format!(
                "the probe did not confirm the erase (answers: {}; 0x0f is the confirmation seen on CH32X035): check with `ch32rv read --blank-check`",
                answers_hex.join(" ")
            ),
        });
    }
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.warnings = warnings;
        env.result = Some(serde_json::json!({
            "method": method.as_str(), "family": chip, "attempts": answers.len(),
            "answers": answers_hex, "confirmed": confirmed,
        }));
        crate::print_envelope(&env)
    } else {
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        println!(
            "special erase ({}) issued for {chip}: {} attempt(s), probe answered {}",
            method.as_str(),
            answers.len(),
            answers_hex.join(" ")
        );
        if confirmed {
            println!("the target code flash is cleared; re-flash normally now");
        } else {
            println!(
                "the erase is not confirmed; check with `ch32rv read --blank-check` before re-flashing"
            );
        }
        ExitCode::SUCCESS
    }
}

/// en: Map a --chip name to its AttachChip family byte (interim; the DB will replace this).
/// ja: --chip 名を AttachChip family byte へ(暫定。将来は DB)。
pub(crate) fn family_byte_from_name(name: &str) -> Option<u8> {
    let n = name.to_ascii_uppercase();
    let n = n.strip_prefix("CH32").unwrap_or(&n);
    Some(match n {
        s if s.starts_with("V103") => 0x01,
        s if s.starts_with("V205") || s.starts_with("V203CC") => 0xCE,
        s if s.starts_with("V20") => 0x05,
        s if s.starts_with("V30") || s.starts_with("V317") => 0x06,
        s if s.starts_with("V003") => 0x09,
        s if s.starts_with("V00") => 0x4E,
        s if s.starts_with("X03") || s.starts_with("X035") => 0x0D,
        s if s.starts_with("X315") || s.starts_with("X305") => 0xE6,
        s if s.starts_with("L103") => 0x0E,
        s if s.starts_with("643") || s.starts_with("CH643") => 0x0C,
        s if s.starts_with("641") || s.starts_with("CH641") => 0x49,
        s if s.starts_with("H4") => 0xC6,
        // Measured 2026-10-10 (docs/data-requests/measured/device-id-{v407,m030}-2026-10-10.md).
        s if s.starts_with("V40") => 0xA6,
        s if s.starts_with("M03") => 0x8E,
        _ => return None,
    })
}

/// en: The error kind of a RAM-loader failure: only a page that still differs after rewriting is
/// a verify mismatch; the probe or its transport failing is a transfer failure (a broker that went
/// away is not a corrupted write), a protect error is the target's protection.
/// ja: RAM loader の失敗の種別。書き直しても違う page だけが verify-mismatch。probe や transport の
/// 失敗は transfer の失敗(ブローカーが消えたのは書き込みの破損ではない)、書き込み保護は target の保護。
pub(crate) fn loader_error_kind(e: &ch32rv_flash::loader::LoaderError) -> ErrorKind {
    use ch32rv_flash::loader::LoaderError as L;
    match e {
        L::Verify { .. } => ErrorKind::VerifyMismatch,
        L::WriteProtected { .. } | L::StillLocked(_) => ErrorKind::TargetProtected,
        L::Probe(d) => crate::source::dmi_error_kind(d),
        _ => ErrorKind::TransferFailed,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_pc_runs_the_image_where_the_part_puts_it() {
        use super::pc_runs_the_image as runs;
        // Past 128 KiB on a big part (V307: 288 KiB of zero-wait flash).
        assert!(runs(0x0003_0000, 288 * 1024, false));
        assert!(runs(0x0803_0000, 288 * 1024, false));
        // A CH32H417 runs its code from RAM_CODE; elsewhere in SRAM is a stub or the loader.
        assert!(runs(0x2010_03b0, 960 * 1024, true));
        assert!(!runs(0x2012_0004, 960 * 1024, true));
        assert!(!runs(0x2010_03b0, 64 * 1024, false));
    }

    #[test]
    fn v205_names_use_the_measured_family_byte() {
        for name in ["CH32V205", "CH32V205RCT6", "ch32v205cct6", "CH32V203CCT6"] {
            assert_eq!(super::family_byte_from_name(name), Some(0xce), "{name}");
        }
        assert_eq!(super::family_byte_from_name("CH32V203C8T6"), Some(0x05));
        assert_eq!(super::family_byte_from_name("CH32V208RBT6"), Some(0x05));
        assert_eq!(super::family_byte_from_name("CH32X315MCU6"), Some(0xe6));
        assert_eq!(super::family_byte_from_name("CH32X305RCT6"), Some(0xe6));
    }
    /// en: What the RAM-loader paths do is accepted (what the Arduino recipes pass, the erase and
    /// verify that match the loader); what they would ignore is listed for refusal.
    /// ja: loader の経路がすることは受け、無視することになるものは断る一覧に入る。
    #[test]
    #[allow(clippy::expect_used, clippy::unreachable)]
    fn loader_paths_refuse_only_what_they_would_ignore() {
        use clap::Parser;
        let parse = |extra: &[&str]| {
            let mut argv = vec!["ch32rv", "flash", "a.elf"];
            argv.extend_from_slice(extra);
            crate::args::Cli::try_parse_from(argv).expect("parses")
        };
        let flash_of = |cli: &crate::args::Cli| match &cli.command {
            crate::args::Command::Flash(a) => (super::loader_refusals(a), {
                crate::oep::flash_refusals(cli, a)
            }),
            _ => unreachable!(),
        };
        // The Arduino recipe's flags, and the values that say what the loader does.
        for ok in [
            &[
                "--format",
                "elf",
                "--reset",
                "run",
                "--confirm-run",
                "--progress",
                "none",
            ][..],
            &[
                "--erase",
                "sector",
                "--verify",
                "readback",
                "--restore-unwritten",
            ][..],
            &["--erase", "keep-store", "--preverify", "--reset", "none"][..],
            // chip: a mass erase over the DM first (a family without one is refused after attach).
            &["--erase", "chip"][..],
        ] {
            let cli = parse(ok);
            let (loader, oep) = flash_of(&cli);
            assert!(loader.is_empty(), "{ok:?}: {loader:?}");
            assert!(oep.is_empty(), "{ok:?}: {oep:?}");
        }
        let cli = parse(&["--erase", "none", "--verify", "none", "--sdi", "on"]);
        assert_eq!(flash_of(&cli).0, ["--erase none", "--verify none", "--sdi"]);
        // auto is gone.
        assert!(
            crate::args::Cli::try_parse_from(["ch32rv", "flash", "a.elf", "--erase", "auto"])
                .is_err()
        );
        let cli = parse(&["--erase", "none", "--monitor", "dmdata"]);
        assert_eq!(flash_of(&cli).0, ["--erase none", "--monitor"]);
        let cli = crate::args::Cli::try_parse_from([
            "ch32rv",
            "--connect-under-reset",
            "--attach-wait",
            "5",
            "flash",
            "a.elf",
            "--reset",
            "halt",
            "--repeat",
        ])
        .expect("parses");
        assert_eq!(
            flash_of(&cli).1,
            [
                "--connect-under-reset",
                "--attach-wait",
                "--reset halt",
                "--repeat"
            ]
        );
        // Values no path implements are not accepted at all.
        for gone in [&["--verify", "crc"][..], &["--region", "system"][..]] {
            assert!(
                crate::args::Cli::try_parse_from(["ch32rv", "flash", "a.elf"].iter().chain(gone))
                    .is_err(),
                "{gone:?}"
            );
        }
        assert!(
            crate::args::Cli::try_parse_from(["ch32rv", "verify", "a.elf", "--region", "code"])
                .is_err()
        );
    }

    use super::{
        EraseReport, Segment, covered_pages, loader_erase_report, overlay_page, store_json,
    };
    use ch32rv_contract::policy::EraseMode;

    #[test]
    fn keep_store_reports_the_store() {
        let st = ch32rv_flash::StoreRegion {
            addr: 0x0803_f000,
            len: 4096,
        };
        let v = store_json(Some(st), "restored");
        assert_eq!(v["address"], "0x0803f000");
        assert_eq!(v["bytes"], 4096);
        assert_eq!(v["method"], "restored");
        // A chip erase over an all-erased STORE writes nothing back.
        assert_eq!(store_json(Some(st), "blank")["method"], "blank");
        // No STORE in the image: none-present, whatever the path did.
        assert_eq!(store_json(None, "untouched")["method"], "none-present");
        let r = EraseReport {
            scope: "keep-store",
            store: Some(v),
        };
        assert_eq!(
            r.human(),
            "keep-store (STORE 4096 B at 0x0803f000 restored)"
        );
    }

    #[test]
    fn loader_paths_report_the_requested_scope() {
        // A bin names no STORE: keep-store reports none-present; other modes report no store.
        let (scope, store) = loader_erase_report(EraseMode::KeepStore, &[0xff; 16]);
        assert_eq!(scope, "keep-store");
        assert_eq!(
            store.map(|s| s["method"].clone()),
            Some("none-present".into())
        );
        assert_eq!(
            loader_erase_report(EraseMode::Chip, &[0xff; 16]),
            ("chip", None)
        );
        assert_eq!(
            loader_erase_report(EraseMode::Sector, &[0xff; 16]),
            ("sector", None)
        );
    }

    // en: The pages a sector erase must clear for a given image layout.
    fn pages(segs: &[(u32, u32)], page: u32) -> Vec<u32> {
        covered_pages(segs.iter().copied(), page)
            .into_iter()
            .collect()
    }

    fn seg(addr: u32, data: &[u8]) -> Segment {
        Segment {
            addr,
            data: data.to_vec(),
        }
    }

    #[test]
    fn single_aligned_segment_one_page() {
        // A 256-byte segment exactly on a page boundary touches exactly one 256-byte page.
        assert_eq!(pages(&[(0x0800_ff00, 256)], 256), vec![0x0800_ff00]);
    }

    #[test]
    fn segment_spanning_two_pages() {
        // 260 bytes at a page start spills 4 bytes into the next page -> two pages.
        assert_eq!(
            pages(&[(0x0800_0000, 260)], 256),
            vec![0x0800_0000, 0x0800_0100]
        );
    }

    #[test]
    fn unaligned_start_pulls_in_whole_first_page() {
        // Starting mid-page erases from that page's base, not the segment's address.
        assert_eq!(
            pages(&[(0x0800_0080, 256)], 256),
            vec![0x0800_0000, 0x0800_0100]
        );
    }

    #[test]
    fn segments_sharing_a_page_collapse() {
        // Two segments in the same page must not produce a duplicate erase of that page.
        assert_eq!(
            pages(&[(0x0800_0000, 16), (0x0800_0040, 16)], 256),
            vec![0x0800_0000]
        );
    }

    #[test]
    fn full_image_covers_every_page() {
        // A 1 KiB image at the code base covers four 256-byte pages, contiguous and sorted.
        assert_eq!(
            pages(&[(0x0800_0000, 1024)], 256),
            vec![0x0800_0000, 0x0800_0100, 0x0800_0200, 0x0800_0300]
        );
    }

    #[test]
    fn honours_a_128_byte_page_size() {
        // V103 uses 128-byte pages; a 256-byte segment then spans two pages.
        assert_eq!(
            pages(&[(0x0800_0000, 256)], 128),
            vec![0x0800_0000, 0x0800_0080]
        );
    }

    // --- overlay_page (restore-unwritten merge) ---

    #[test]
    fn overlay_writes_only_the_segment_span() {
        // A page pre-read as all-0xff, image writes 4 bytes at the start -> rest stays 0xff.
        let mut page = vec![0xff_u8; 8];
        overlay_page(0x0800_0000, &mut page, &[seg(0x0800_0000, &[1, 2, 3, 4])]);
        assert_eq!(page, [1, 2, 3, 4, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn overlay_preserves_pre_read_bytes_outside_the_image() {
        // The unwritten-byte-preservation case: original content survives where the image is absent.
        let mut page = vec![0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7];
        overlay_page(0x0800_0000, &mut page, &[seg(0x0800_0002, &[0xBB, 0xCC])]);
        assert_eq!(page, [0xA0, 0xA1, 0xBB, 0xCC, 0xA4, 0xA5, 0xA6, 0xA7]);
    }

    #[test]
    fn overlay_clips_a_segment_to_the_page() {
        // A segment that starts before and ends after the page only writes the in-page slice.
        // page covers [0x100, 0x108); segment covers [0x0FE, 0x106) -> writes page[0..6].
        let mut page = vec![0u8; 8];
        overlay_page(
            0x0800_0100,
            &mut page,
            &[seg(0x0800_00fe, &[10, 11, 12, 13, 14, 15, 16, 17])],
        );
        // segment bytes at offsets 2..8 land in the page (0x100-0x0FE = 2).
        assert_eq!(page, [12, 13, 14, 15, 16, 17, 0, 0]);
    }

    #[test]
    fn overlay_ignores_a_segment_in_another_page() {
        // A segment entirely outside the page leaves it untouched.
        let mut page = vec![0x55_u8; 4];
        overlay_page(0x0800_0000, &mut page, &[seg(0x0800_1000, &[1, 2, 3, 4])]);
        assert_eq!(page, [0x55, 0x55, 0x55, 0x55]);
    }
}
