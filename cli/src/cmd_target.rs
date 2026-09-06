//! en: `target info` (docs/cli.ja.md §4.3): attach, read the chip signature and factory
//! UUID/flash size, and detach. Read-only: nothing is written to the target, and the core is
//! always released (detach) on drop of the session, on every path. The LinkE corrupted-readback
//! bug is detected and recovered inside the shared session (board-identify, measured).
//! ja: `target info`。attach → chip 署名と工場 UUID・flash 容量の読み取り → detach。
//! 読み取り専用で、session の drop 時に必ず detach する。LinkE の壊れ読み値バグは共通 session で復旧。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::{ErrorKind, ProbeMode, ResultEnvelope, TargetReport, Warning};

use crate::args::{Cli, SwitchState};
use crate::cmd_probe::{
    apply_probe_info, base_report, fail, mode_str, print_probe_human, select_entry,
};
use crate::parse;
use crate::session::Session;

const CMD: &str = "target.info";

pub fn info(cli: &Cli) -> ExitCode {
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(code) => return code,
    };
    if entry.mode != ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; attaching to a target requires RISC-V mode",
                mode_str(entry.mode)
            ),
            Some("switch to RISC-V mode with `ch32rv probe mode set riscv` (WCH-LinkE only)"),
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(msg) => return fail(cli, CMD, ErrorKind::Usage, msg, None),
    };

    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let session = match Session::attach(
        &entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(e) => return crate::cmd_probe::session_error(cli, CMD, e),
    };

    let mut probe_report = base_report(&entry);
    apply_probe_info(&mut probe_report, &session.probe_info, &mut warnings);

    let family = session.family();
    if family.starts_with("unknown") {
        warnings.push(Warning {
            code: "family-unknown".to_owned(),
            msg: format!(
                "family byte 0x{:02x} is not in the known table (possibly a gap series) - worth recording for data request 0001",
                session.attach.family_byte
            ),
        });
    }
    // Resolve the SKU from the live chip_id against the generated DB (device_ids join, rev [7:4]
    // masked). Fail-closed: an unknown or cross-family-ambiguous id shows no SKU rather than a guess.
    let db = ch32rv_target::Db::builtin();
    let resolution = db.resolve_by_chip_id(session.attach.chip_id);
    let (sku, sku_verified, sku_line): (Option<String>, Option<bool>, String) = match &resolution {
        ch32rv_target::Resolution::Sku(s) => (
            Some(s.sku.clone()),
            Some(s.verified),
            format!(
                "{} ({})",
                s.sku,
                if s.verified {
                    "verified on silicon"
                } else {
                    "generated DB, datasheet reference"
                }
            ),
        ),
        ch32rv_target::Resolution::Family(fam, cands) => {
            let names: Vec<&str> = cands.iter().map(|c| c.sku.as_str()).collect();
            warnings.push(Warning {
                code: "sku-ambiguous".to_owned(),
                msg: format!(
                    "chip_id matches {} SKUs in family {fam}: {} - pass --chip to disambiguate",
                    cands.len(),
                    names.join(", ")
                ),
            });
            (
                None,
                None,
                format!("- ({} candidates in {fam})", cands.len()),
            )
        }
        ch32rv_target::Resolution::Unknown => {
            warnings.push(Warning {
                code: "sku-unknown".to_owned(),
                msg: format!(
                    "chip_id 0x{:08x} is not in the generated DB (a gap-series or new part) - worth recording for data request 0001",
                    session.attach.chip_id
                ),
            });
            (None, None, "- (chip_id not in DB)".to_owned())
        }
    };

    // Debug wiring (1-wire SWIO vs 2-wire RVSWD) for the resolved series (data request 0002).
    let wiring = match &resolution {
        ch32rv_target::Resolution::Sku(s) => ch32rv_target::debug_wiring(&s.series),
        _ => None,
    };

    let chip = session.chip;
    let target = TargetReport {
        sku,
        family: Some(family),
        chip_id: Some(format!("0x{:08x}", session.attach.chip_id)),
        uid: chip.as_ref().map(|c| hex(&c.uuid)),
        verified: sku_verified,
        provisional: None,
        protected: None,
        flash_bytes: chip.as_ref().map(|c| c.flash_bytes),
    };

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.probe = Some(probe_report);
        env.result = Some(serde_json::json!({
            "protection_raw": chip.as_ref().map(|c| hex(&c.protection_raw)),
            "chip_id_echo": chip.as_ref().map(|c| format!("0x{:08x}", c.chip_id_echo)),
            "debug_wiring": wiring.as_ref().map(|w| serde_json::json!({
                "wire": w.wire, "swdio": w.swdio, "swclk": w.swclk,
            })),
        }));
        env.target = Some(target);
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        print_probe_human(&probe_report);
        println!("---");
        println!("family:   {}", target.family.as_deref().unwrap_or("-"));
        println!(
            "chip id:  {}  (bits [7:4] = silicon revision)",
            target.chip_id.as_deref().unwrap_or("-")
        );
        println!("uid:      {}", target.uid.as_deref().unwrap_or("-"));
        match target.flash_bytes {
            Some(b) => println!("flash:    {} KiB", b / 1024),
            None => println!("flash:    -"),
        }
        println!("sku:      {sku_line}");
        if let Some(w) = &wiring {
            println!(
                "debug:    {} (SWDIO/DAT={}{})",
                w.wire,
                w.swdio,
                if w.swclk == "-" {
                    String::new()
                } else {
                    format!(", SWCLK={}", w.swclk)
                }
            );
        }
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

/// en: `target option get` (docs/cli.ja.md §4.3): read the option bytes (0x1FFF_F800, 16 bytes)
/// over DMI and decode the common fields (read protection, the USER byte's IWDG/STOP/STANDBY
/// bits, the Data0/Data1 user bytes, and the WRP write-protect mask). Read-only. Family-specific
/// USER bits need the generated target DB, so the raw bytes are always shown and the structured
/// decode is marked interim.
/// ja: `target option get`。option bytes(0x1FFF_F800、16 byte)を DMI で読み、共通フィールド
/// (読み出し保護・USER の IWDG/STOP/STANDBY・Data0/Data1・WRP)を復号。読み取り専用。family 固有の
/// USER ビットは DB 生成後。生バイトは常に表示し、構造化復号は暫定扱い。
pub fn option_get(cli: &Cli) -> ExitCode {
    const CMD: &str = "target.option.get";
    let entry = match select_entry(cli, CMD) {
        Ok(e) => e,
        Err(code) => return code,
    };
    if entry.mode != ProbeMode::Riscv {
        return fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; attaching to a target requires RISC-V mode",
                mode_str(entry.mode)
            ),
            None,
        );
    }
    let (speed, mut warnings) = match parse::speed(&cli.speed) {
        Ok(v) => v,
        Err(msg) => return fail(cli, CMD, ErrorKind::Usage, msg, None),
    };
    let timeout = Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000));
    let mut session = match Session::attach(
        &entry,
        speed,
        timeout,
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        &mut warnings,
    ) {
        Ok(s) => s,
        Err(e) => return crate::cmd_probe::session_error(cli, CMD, e),
    };

    let family = session.family();
    let db_family = db_family_of(&mut session);
    let option_base = match option_base(&db_family) {
        Ok(b) => b,
        Err(msg) => return fail(cli, CMD, ErrorKind::CapabilityUnsupported, msg, None),
    };
    let user_fields = ch32rv_target::option_user_fields(&db_family);
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
    let raw = match dm.read_mem(option_base, 16) {
        Ok(v) => v,
        Err(e) => {
            return fail(
                cli,
                CMD,
                ErrorKind::TransferFailed,
                format!("reading option bytes failed: {e}"),
                None,
            );
        }
    };

    // Layout (STM32F1-style, shared by CH32V0/V1/V2/V3/X0): each logical byte is stored with its
    // complement. [0]=RDPR [2]=USER [4]=Data0 [6]=Data1 [8/10/12/14]=WRPR0..3.
    let rdpr = raw[0];
    let user = raw[2];
    let data0 = raw[4];
    let data1 = raw[6];
    let wrpr = u32::from(raw[8])
        | u32::from(raw[10]) << 8
        | u32::from(raw[12]) << 16
        | u32::from(raw[14]) << 24;
    // RDPR == 0xA5 means read-out protection disabled (the factory/unprotected value).
    let unprotected = rdpr == 0xA5;

    // USER byte: decode per the family's named bits from the generated DB (request 0003). When the
    // family is not in the DB, fall back to the common STM32F1-style bits and flag it interim.
    let user_bits: Vec<(String, u8)> = if user_fields.is_empty() {
        [(0u8, "IWDGSW"), (1, "nRST_STOP"), (2, "nRST_STDBY")]
            .iter()
            .map(|(bit, name)| ((*name).to_owned(), (user >> bit) & 1))
            .collect()
    } else {
        user_fields
            .iter()
            .map(|f| (f.field.clone(), (user >> f.bit) & 1))
            .collect()
    };
    if user_fields.is_empty() {
        warnings.push(Warning {
            code: "option-decode-interim".to_owned(),
            msg: format!(
                "USER-byte fields for {db_family} are not in the DB; decoded with the common STM32F1-style bits only (data request 0003)"
            ),
        });
    }
    let user_str = user_bits
        .iter()
        .map(|(name, v)| format!("{name}={v}"))
        .collect::<Vec<_>>()
        .join("  ");

    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        let user_json: serde_json::Map<String, serde_json::Value> = user_bits
            .iter()
            .map(|(name, v)| (name.clone(), serde_json::json!(*v == 1)))
            .collect();
        env.result = Some(serde_json::json!({
            "family": family,
            "db_family": db_family,
            "raw": hex(&raw),
            "read_protected": !unprotected,
            "rdpr": format!("0x{rdpr:02x}"),
            "user": format!("0x{user:02x}"),
            "user_bits": user_json,
            "data0": format!("0x{data0:02x}"),
            "data1": format!("0x{data1:02x}"),
            "wrpr": format!("0x{wrpr:08x}"),
            "write_protected": wrpr != 0xFFFF_FFFF,
        }));
        env.warnings = warnings;
        crate::print_envelope(&env)
    } else {
        println!("family:          {family}");
        println!("raw:             {}", hex(&raw));
        println!(
            "read protection: {}  (RDPR=0x{rdpr:02x}{})",
            if unprotected { "off" } else { "ON" },
            if unprotected {
                ", 0xA5=unprotected"
            } else {
                ""
            }
        );
        println!("user (0x{user:02x}):     {user_str}");
        println!("data0/data1:     0x{data0:02x} / 0x{data1:02x}");
        println!(
            "write protect:   0x{wrpr:08x}  ({})",
            if wrpr == 0xFFFF_FFFF {
                "none (all pages writable)"
            } else {
                "some pages write-protected"
            }
        );
        for w in &warnings {
            eprintln!("warning[{}]: {}", w.code, w.msg);
        }
        ExitCode::SUCCESS
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// en: Resolve the DB family string for the attached target from its live chip id (the AttachChip
/// family byte is coarser: 0x06 covers CH32V30x while the DB keys on CH32V307).
/// ja: 生の chip_id から DB の family 文字列を引く(attach の family byte より細かい粒度)。
pub(crate) fn db_family_of(session: &mut Session) -> String {
    let db = ch32rv_target::Db::builtin();
    match db.resolve_by_chip_id(session.attach.chip_id) {
        ch32rv_target::Resolution::Sku(s) => s.family.clone(),
        ch32rv_target::Resolution::Family(fam, _) => fam,
        ch32rv_target::Resolution::Unknown => session.family(),
    }
}

/// en: The option-byte block base for a DB family, from the generated device DB. Fail-closed: the
/// block is **not** at a universal address - most parts put it at `0x1FFF_F800` but CH32M030 uses
/// `0x1FFF_F300` - so a family the DB does not carry gets an error instead of a guess that would
/// read, and worse program, the wrong memory.
/// ja: DB family の option byte ブロック先頭番地(生成 DB 由来)。**共通番地ではない**
/// (CH32M030 は `0x1FFF_F300`)ので、DB に無い family は推測せずエラーにする。
pub(crate) fn option_base(db_family: &str) -> Result<u32, String> {
    ch32rv_target::option_bytes_layout(db_family)
        .map(|l| l.base)
        .ok_or_else(|| {
            format!(
                "the option-byte block location for {db_family} is not in the device DB, and it is not the same address on every part (CH32M030 uses 0x1FFF_F300) - refusing to guess"
            )
        })
}

/// en: A warning when the family's reference manual describes a different option-byte programming
/// procedure than the half-word (OBPG) one implemented here. Not fatal: the OBPG path is verified
/// on CH32L103, which the DB classifies `ftpg`, so the classification alone must not gate the
/// write - but on an untested family it is worth saying out loud.
/// ja: RM が本実装(OBPG の half-word 書き)と別の手順を書いている family への警告。致命ではない
/// (`ftpg` 分類の L103 で OBPG 経路が実機検証済みのため)が、未検証 family では明示する。
fn option_method_warning(db_family: &str) -> Option<String> {
    let layout = ch32rv_target::option_bytes_layout(db_family)?;
    (layout.write_method == "ftpg" && db_family != "CH32L103").then(|| {
        format!(
            "{db_family}: the reference manual programs option bytes by the fast-page (FTPG) procedure, while this writes half-words (OPTPG). The half-word path is verified here on CH32L103 only"
        )
    })
}

/// en: The blanket option-byte image used only when the target's current bytes cannot be read
/// back: RDPR off, everything else `0xff`, each with its complement. It is a last resort - see
/// [`unprotect_image`] for why writing it unconditionally is wrong.
/// ja: 現在値が読めないときだけ使う一律 image(RDPR off + 他 `0xff`)。最後の手段。
pub(crate) const BLANKET_FACTORY: [u8; 16] = [
    0xA5, 0x5A, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00,
];

/// en: True when the 16 bytes read back look like real option bytes: RDPR and USER each agree with
/// their complement. Data/WRPR are deliberately not checked - CH32V103 ships those complements as
/// `0xff` rather than the inverse (measured, docs/data-requests/measured/option-bytes-2026-09-06.md),
/// so requiring all eight pairs would reject a healthy part.
/// ja: 読み戻した 16 byte が本物の option bytes に見えるか(RDPR と USER が補数と整合)。V103 は
/// Data/WRPR の補数を持たない実測があるので、そこは検査しない。
pub(crate) fn option_bytes_plausible(raw: &[u8; 16]) -> bool {
    raw[0] ^ raw[1] == 0xFF && raw[2] ^ raw[3] == 0xFF
}

/// en: The image that clears read protection **without changing anything else the part carries**.
/// Writing a blanket `USER=0xff` is wrong on real silicon: the reference manuals leave some USER
/// bits indeterminate or non-`1` at reset, and the bench measurements show they differ per part -
/// CH32V20x/V307 ship `RAM_CODE_MOD` (`[7:5]`, the SRAM/flash split) as `001`/`101`, and CH32V003
/// ships `RST_MODE` (`[4:3]`, the NRST pin function) as `10b`. Clearing read protection must not
/// silently repartition SRAM or turn NRST into GPIO, so only RDPR and its complement are replaced.
/// ja: **他を一切変えずに**読み出し保護だけ解除する image。一律 `USER=0xff` は実機と合わない
/// (V20x/V307 の `RAM_CODE_MOD`、V003 の `RST_MODE` は出荷値が `0xff` ではない)。保護解除が
/// SRAM 分割や NRST の機能を書き換えてはいけないので、RDPR と補数だけ差し替える。
pub(crate) fn unprotect_image(current: &[u8; 16]) -> [u8; 16] {
    let mut img = *current;
    img[0] = 0xA5;
    img[1] = 0x5A;
    img
}

/// en: The USER byte for `option reset`: every bit the device DB documents goes back to its
/// reference-manual reset value, and every other bit keeps what the part currently has. The DB does
/// not carry the multi-bit fields (CH32V003 `RST_MODE`, CH32V20x/V307 `RAM_CODE_MOD`), and the
/// manuals give `RAM_CODE_MOD` no reset value at all, so those cannot be reconstructed - keeping the
/// part's own value is the only answer that does not invent one.
/// ja: `option reset` の USER byte。DB が定義する bit は RM の復位値へ戻し、それ以外は現在値を保つ。
/// 多 bit フィールド(`RST_MODE` / `RAM_CODE_MOD`)は DB に無く、RM も `RAM_CODE_MOD` の復位値を
/// 書いていないので、再構成せず現在値を残す。
pub(crate) fn reset_user_byte(db_family: &str, current: u8) -> u8 {
    let mut user = current;
    for f in ch32rv_target::option_user_fields(db_family) {
        if f.bit < 8 {
            let mask = 1u8 << f.bit;
            if f.default == 0 {
                user &= !mask;
            } else {
                user |= mask;
            }
        }
    }
    user
}

/// Confirm a destructive option-byte write (the shared gate: `--yes` skips it,
/// `--non-interactive` without `--yes` refuses, otherwise prompt on the terminal).
fn ob_confirm(cli: &Cli, prompt: &str) -> bool {
    crate::cmd_probe::confirm_destructive(cli, prompt).is_ok()
}

/// Parse exactly 16 hex bytes (optionally space/`:`-separated) for `option write-raw`.
fn parse_hex16(s: &str) -> Result<[u8; 16], String> {
    let clean: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':' && *c != '_')
        .collect();
    if clean.len() != 32 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "expected 16 hex bytes (32 hex digits), got {} digit(s)",
            clean.len()
        ));
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// en: Read the 16 option bytes plus the DB family string (for USER-field names) - attach, halt,
/// read, detach. ja: 16 byte の option bytes と DB family(USER field 名用)を読む。
fn read_option_bytes(cli: &Cli, cmd: &str) -> Result<(String, [u8; 16]), ExitCode> {
    let mut session = crate::cmd_probe::attach(cli, cmd)?;
    let db_family = db_family_of(&mut session);
    let base = option_base(&db_family)
        .map_err(|msg| fail(cli, cmd, ErrorKind::CapabilityUnsupported, msg, None))?;
    let mut dm = session.dm();
    dm.halt().map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        )
    })?;
    let v = dm.read_mem(base, 16).map_err(|e| {
        fail(
            cli,
            cmd,
            ErrorKind::TransferFailed,
            format!("reading option bytes failed: {e}"),
            None,
        )
    })?;
    let mut a = [0u8; 16];
    a.copy_from_slice(&v[..16]);
    Ok((db_family, a))
}

/// en: Erase + program the 16 option bytes to `new` (value+complement pairs, as `option get`
/// returns), then read back and verify. `new[0]` (RDPR) is programmed first so read protection is
/// re-established immediately. The bytes take effect after a system reset. ja: option bytes を
/// `new` へ erase+program し read-back で検証。RDPR を最初に書く。反映は system reset 後。
fn program_option(cli: &Cli, cmd: &str, new: &[u8; 16]) -> ExitCode {
    let mut session = match crate::cmd_probe::attach(cli, cmd) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let family = session.family();
    let db_family = db_family_of(&mut session);
    let base = match option_base(&db_family) {
        Ok(b) => b,
        Err(msg) => return fail(cli, cmd, ErrorKind::CapabilityUnsupported, msg, None),
    };
    if let Some(w) = option_method_warning(&db_family) {
        eprintln!("warning[option-write-method]: {w}");
    }
    let mut dm = session.dm();
    if let Err(e) = dm.halt() {
        return fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        );
    }
    let before = match dm.read_mem(base, 16) {
        Ok(v) => v,
        Err(e) => {
            return fail(
                cli,
                cmd,
                ErrorKind::TransferFailed,
                format!("reading option bytes failed: {e}"),
                None,
            );
        }
    };
    if let Err(e) = dm.flash_program_option_bytes(base, new) {
        return fail(
            cli,
            cmd,
            ErrorKind::TransferFailed,
            format!(
                "programming option bytes failed: {e} - the target may be left with erased (read-protected) option bytes; recover with `ch32rv recover`"
            ),
            None,
        );
    }
    // en: Verify after a reset, not straight after the write. On CH32V103 the option area keeps
    // reading back the pre-write image until the target resets - `option set STOPRST=0` was
    // measured reporting a false verify-mismatch while the write had in fact taken, and a fresh
    // session showed the new value. Option bytes only take effect at reset anyway, so resetting
    // first is also what the caller wants. Re-read a couple of times in case the reset is still
    // settling; a mismatch that survives that is a genuine write failure (exit 30).
    // ja: 検証は reset 後に行う。V103 は reset するまで書込前の像を読み続け、書けているのに
    // verify-mismatch を返していた(実測)。option bytes はどのみち reset で反映されるので、
    // reset してから読むのが意味的にも正しい。落ち着くまで数回読み直す。
    // (the `dm` borrow above ends here, so the probe is reachable again)
    let _ = session.link().soft_reset();
    std::thread::sleep(Duration::from_millis(50));
    let mut dm = session.dm();
    if let Err(e) = dm.halt() {
        return fail(
            cli,
            cmd,
            ErrorKind::AttachFailed,
            format!("halt after the option reset failed: {e}"),
            None,
        );
    }
    let mut after = Vec::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        after = match dm.read_mem(base, 16) {
            Ok(v) => v,
            Err(e) => {
                return fail(
                    cli,
                    cmd,
                    ErrorKind::TransferFailed,
                    format!("verify read failed: {e}"),
                    None,
                );
            }
        };
        if (0..16).step_by(2).all(|i| after[i] == new[i]) {
            break;
        }
    }
    if let Some(i) = (0..16).step_by(2).find(|&i| after[i] != new[i]) {
        return fail(
            cli,
            cmd,
            ErrorKind::VerifyMismatch,
            format!(
                "option byte {i} reads back 0x{:02x}, not the requested 0x{:02x} (before={}, after={})",
                after[i],
                new[i],
                hex(&before),
                hex(&after)
            ),
            Some("the write did not take; check the target is not write-protected"),
        );
    }
    if cli.json {
        let mut env = ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({
            "family": family,
            "before": hex(&before),
            "after": hex(&after),
            "verified": true,
            "note": "option bytes take effect after a power-on / system reset",
        }));
        crate::print_envelope(&env)
    } else {
        println!(
            "option bytes: {} -> {} ({family})",
            hex(&before),
            hex(&after)
        );
        println!("note: option bytes take effect after a power-on / system reset");
        ExitCode::SUCCESS
    }
}

/// `target option write-raw <hex>`: overwrite the 16 option bytes with a raw value (expert).
pub fn option_write_raw(cli: &Cli, hexstr: &str) -> ExitCode {
    const CMD: &str = "target.option.write-raw";
    let bytes = match parse_hex16(hexstr) {
        Ok(b) => b,
        Err(m) => {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                m,
                Some("e.g. a55aff00ff00ff00ff00ff00ff00ff00 (16 bytes: RDPR nRDPR USER nUSER ...)"),
            );
        }
    };
    if bytes[0] != 0xA5
        && !ob_confirm(
            cli,
            &format!(
                "RDPR byte is 0x{:02x} (not 0xA5): this ENABLES read protection - flash becomes unreadable until you unprotect (which erases it). Continue?",
                bytes[0]
            ),
        )
    {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "aborted: RDPR would enable read protection (pass --yes to force)",
            None,
        );
    }
    if !ob_confirm(cli, "Overwrite the target's option bytes?") {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    program_option(cli, CMD, &bytes)
}

/// `target option reset`: restore factory-default option bytes (RDPR off, USER/Data/WRP cleared).
pub fn option_reset(cli: &Cli) -> ExitCode {
    const CMD: &str = "target.option.reset";
    // Read first: the USER bits the DB does not document (CH32V003 `RST_MODE`, CH32V20x/V307
    // `RAM_CODE_MOD`) have no reconstructable reset value, and the parts do not ship them as 1, so
    // they are carried over instead of being blanked. Data/WRPR are genuinely cleared.
    let (db_family, current) = match read_option_bytes(cli, CMD) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let user = reset_user_byte(&db_family, current[2]);
    let mut defaults = BLANKET_FACTORY;
    defaults[2] = user;
    defaults[3] = !user;
    if !ob_confirm(
        cli,
        &format!(
            "Restore factory-default option bytes (RDPR off, USER=0x{user:02x}, Data/WRP cleared)?"
        ),
    ) {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    if current[2] != user {
        println!("option reset: USER 0x{:02x} -> 0x{user:02x}", current[2]);
    }
    program_option(cli, CMD, &defaults)
}

/// en: `target option set <key=value ...>`: read-modify-write named option fields. Keys: a USER
/// bit field name from the DB (per family, e.g. `IWDGSW=0`), `rdp=on|off`, `data0=<hex>`,
/// `data1=<hex>`. Complement bytes are recomputed. `rdp=off` triggers a full mass erase.
/// ja: `target option set <key=value ...>`。DB の USER bit 名(family 別)/`rdp`/`data0`/`data1` を
/// read-modify-write。補数は再計算。`rdp=off` は全消去を伴う。
pub fn option_set(cli: &Cli, kv: &[String]) -> ExitCode {
    const CMD: &str = "target.option.set";
    let (db_family, mut ob) = match read_option_bytes(cli, CMD) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let fields = ch32rv_target::option_user_fields(&db_family);
    let mut changes: Vec<String> = Vec::new();
    let mut mass_erase = false;
    let mut protect_on = false;

    for pair in kv {
        let Some((key, val)) = pair.split_once('=') else {
            return fail(
                cli,
                CMD,
                ErrorKind::Usage,
                format!("expected key=value, got {pair:?}"),
                Some("e.g. IWDGSW=0 rdp=off data0=0x42"),
            );
        };
        let key_l = key.to_ascii_lowercase();
        match key_l.as_str() {
            "rdp" | "protect" => match val.to_ascii_lowercase().as_str() {
                "off" | "0" | "none" => {
                    if ob[0] != 0xA5 {
                        mass_erase = true;
                    }
                    ob[0] = 0xA5;
                    changes.push("rdp=off".to_owned());
                }
                "on" | "1" => {
                    ob[0] = 0xFF;
                    protect_on = true;
                    changes.push("rdp=on".to_owned());
                }
                other => {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::Usage,
                        format!("rdp must be on/off, got {other:?}"),
                        None,
                    );
                }
            },
            "data0" | "data1" => {
                let Some(byte) = parse_u8(val) else {
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::Usage,
                        format!("{key}: expected a byte 0..255 (e.g. 0x42), got {val:?}"),
                        None,
                    );
                };
                let idx = if key_l == "data0" { 4 } else { 6 };
                ob[idx] = byte;
                changes.push(format!("{key_l}=0x{byte:02x}"));
            }
            _ => {
                // A named USER-byte bit for this family.
                let Some(f) = fields.iter().find(|f| f.field.eq_ignore_ascii_case(key)) else {
                    let names: Vec<&str> = fields.iter().map(|f| f.field.as_str()).collect();
                    return fail(
                        cli,
                        CMD,
                        ErrorKind::Usage,
                        format!("unknown option field {key:?} for {db_family}"),
                        Some(&format!(
                            "known USER fields: {} (plus rdp, data0, data1)",
                            names.join(", ")
                        )),
                    );
                };
                let bit = match val {
                    "0" => 0u8,
                    "1" => 1,
                    other => {
                        return fail(
                            cli,
                            CMD,
                            ErrorKind::Usage,
                            format!("{key}: expected 0 or 1, got {other:?}"),
                            None,
                        );
                    }
                };
                if bit == 1 {
                    ob[2] |= 1 << f.bit;
                } else {
                    ob[2] &= !(1 << f.bit);
                }
                changes.push(format!("{}={bit}", f.field));
            }
        }
    }

    if changes.is_empty() {
        return fail(
            cli,
            CMD,
            ErrorKind::Usage,
            "no key=value pairs given".to_owned(),
            None,
        );
    }
    // Recompute every complement byte so the halfwords are valid regardless of what we touched.
    for i in (0..16).step_by(2) {
        ob[i + 1] = 0xFF ^ ob[i];
    }

    let prompt = if mass_erase {
        format!(
            "Apply option changes [{}]? rdp=off ERASES ALL FLASH (mass erase).",
            changes.join(", ")
        )
    } else if protect_on {
        format!(
            "Apply option changes [{}]? rdp=on makes the flash unreadable/undebuggable.",
            changes.join(", ")
        )
    } else {
        format!("Apply option changes [{}]?", changes.join(", "))
    };
    if !ob_confirm(cli, &prompt) {
        return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
    }
    program_option(cli, CMD, &ob)
}

/// Parse a byte as decimal or `0x`-hex.
fn parse_u8(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u8::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// `target protect on|off`: enable/disable flash read protection (RDPR). Turning it OFF triggers a
/// full mass erase of the target's flash.
/// Emit a no-op success ("read protection is already <state>") that still produces a JSON envelope.
fn already_in_state(cli: &Cli, cmd: &str, protected: bool) -> ExitCode {
    if cli.json {
        let mut env = ResultEnvelope::success(cmd);
        env.result = Some(serde_json::json!({ "changed": false, "read_protected": protected }));
        crate::print_envelope(&env)
    } else {
        println!(
            "read protection is already {}",
            if protected { "ON" } else { "OFF" }
        );
        ExitCode::SUCCESS
    }
}

pub fn protect(cli: &Cli, state: SwitchState) -> ExitCode {
    const CMD: &str = "target.protect";
    let mut ob = match read_option_bytes(cli, CMD) {
        Ok((_family, b)) => b,
        Err(c) => return c,
    };
    match state {
        SwitchState::On => {
            if ob[0] != 0xA5 {
                return already_in_state(cli, CMD, true);
            }
            if !ob_confirm(
                cli,
                "Enable read protection? The flash becomes unreadable/undebuggable until you turn it OFF (which ERASES all flash).",
            ) {
                return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
            }
            ob[0] = 0xFF;
            ob[1] = 0x00;
        }
        SwitchState::Off => {
            if ob[0] == 0xA5 {
                return already_in_state(cli, CMD, false);
            }
            if !ob_confirm(
                cli,
                "Disable read protection? This ERASES ALL FLASH (mass erase) on the target.",
            ) {
                return fail(cli, CMD, ErrorKind::Usage, "aborted (no --yes)", None);
            }
            ob[0] = 0xA5;
            ob[1] = 0x5A;
        }
    }
    program_option(cli, CMD, &ob)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::{
        BLANKET_FACTORY, option_base, option_bytes_plausible, option_method_warning, parse_hex16,
        reset_user_byte, unprotect_image,
    };

    /// en: Clearing read protection must not disturb the family-specific USER bits. These are the
    /// bytes actually read off the bench (docs/data-requests/measured/option-bytes-2026-09-06.md):
    /// CH32V20x ships `USER=0x3f` and CH32V003 `0xf7`, so a blanket 0xff would repartition SRAM
    /// (`RAM_CODE_MOD`) and change the NRST pin function (`RST_MODE`).
    /// ja: 保護解除で family 固有の USER bit を壊さないこと(実測値で固定)。
    #[test]
    fn unprotect_keeps_everything_but_rdpr() {
        // CH32V203C8T6 as shipped.
        let v20x = [
            0xa5, 0x5a, 0x3f, 0xc0, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00,
            0xff, 0x00,
        ];
        let out = unprotect_image(&v20x);
        assert_eq!(out[0], 0xA5, "RDPR cleared");
        assert_eq!(out[1], 0x5A, "RDPR complement");
        assert_eq!(
            out[2], 0x3f,
            "USER must survive (RAM_CODE_MOD = SRAM/flash split)"
        );
        assert_eq!(&out[4..], &v20x[4..], "Data/WRPR must survive");
        // A protected part reads back as something else; only RDPR changes either way.
        let protected = [
            0x00, 0xff, 0xf7, 0x08, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00,
            0xff, 0x00,
        ];
        let out = unprotect_image(&protected);
        assert_eq!((out[0], out[1]), (0xA5, 0x5A));
        assert_eq!(out[2], 0xf7, "CH32V003 RST_MODE must survive");
    }

    /// The plausibility gate decides whether the read-back bytes may be trusted. CH32V103 ships
    /// Data/WRPR complements as 0xff rather than the inverse, so those pairs must not be checked.
    #[test]
    fn plausibility_checks_rdpr_and_user_only() {
        let v103 = [
            0xa5, 0x5a, 0xff, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xff,
        ];
        assert!(
            option_bytes_plausible(&v103),
            "CH32V103 as shipped is valid"
        );
        let garbage = [0u8; 16];
        assert!(!option_bytes_plausible(&garbage));
        let half = [
            0xa5, 0x5a, 0xff, 0xff, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00, 0xff, 0x00,
            0xff, 0x00,
        ];
        assert!(!option_bytes_plausible(&half), "USER/complement disagree");
        assert!(option_bytes_plausible(&BLANKET_FACTORY));
    }

    /// `option reset` restores the bits the DB documents and keeps the ones it cannot know. On the
    /// bench parts every documented bit already sits at its reset value, so the USER byte must come
    /// back unchanged - a reset that "fixes" 0x3f into 0xff would be the bug this guards.
    #[test]
    fn reset_restores_known_bits_and_keeps_the_rest() {
        assert_eq!(reset_user_byte("CH32V20x", 0x3f), 0x3f);
        assert_eq!(reset_user_byte("CH32V307", 0xbf), 0xbf);
        assert_eq!(reset_user_byte("CH32V003", 0xf7), 0xf7);
        assert_eq!(reset_user_byte("CH32L103", 0xff), 0xff);
        // A cleared IWDGSW (bit0, reset value 1) is restored; the undocumented high bits are not.
        assert_eq!(reset_user_byte("CH32V20x", 0x3e), 0x3f);
        // An unknown family has no documented bits, so nothing is touched.
        assert_eq!(reset_user_byte("CH32V999", 0x12), 0x12);
    }

    /// en: The option-byte block is not at one universal address, and the writer must take it from
    /// the DB. CH32M030 is the counter-example that makes a hard-coded `0x1FFF_F800` wrong.
    /// ja: option byte の番地は共通ではない(M030 が反例)。DB から引けていることを固定する。
    #[test]
    fn option_base_comes_from_the_db_and_m030_differs() {
        assert_eq!(option_base("CH32V103").unwrap(), 0x1FFF_F800);
        assert_eq!(option_base("CH32L103").unwrap(), 0x1FFF_F800);
        assert_eq!(option_base("CH32V307").unwrap(), 0x1FFF_F800);
        assert_eq!(
            option_base("CH32M030").unwrap(),
            0x1FFF_F300,
            "CH32M030 keeps its option bytes at 0x1FFF_F300"
        );
    }

    /// A family the DB does not carry must fail closed rather than fall back to a guessed address.
    #[test]
    fn option_base_fails_closed_for_an_unknown_family() {
        assert!(option_base("CH32V999").is_err());
        assert!(option_base("unknown (family byte 0x4e)").is_err());
    }

    /// The DB classifies CH32L103's option programming as the fast-page procedure, but the
    /// half-word path is verified on that silicon here - so it must not warn, while an untested
    /// fast-page family must.
    #[test]
    fn option_method_warning_only_for_untested_fast_page_families() {
        assert!(option_method_warning("CH32L103").is_none());
        assert!(option_method_warning("CH32V103").is_none()); // classified obpg
        assert!(option_method_warning("CH32M030").is_some());
    }

    #[test]
    fn parses_16_contiguous_bytes() {
        let b = parse_hex16("a55aff00ff00ff00ff00ff00ff00ff00").unwrap();
        assert_eq!(b[0], 0xA5);
        assert_eq!(b[1], 0x5A);
        assert_eq!(b[15], 0x00);
    }

    #[test]
    fn accepts_separators() {
        let a = parse_hex16("a5:5a:ff:00:ff:00:ff:00:ff:00:ff:00:ff:00:ff:00").unwrap();
        let b = parse_hex16("a5 5a ff 00 ff 00 ff 00 ff 00 ff 00 ff 00 ff 00").unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0], 0xA5);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(parse_hex16("abcd").is_err());
        assert!(parse_hex16("a55aff00ff00ff00ff00ff00ff00ff0000").is_err()); // 17 bytes
    }

    #[test]
    fn rejects_non_hex() {
        assert!(parse_hex16("zz5aff00ff00ff00ff00ff00ff00ff00").is_err());
    }
}
