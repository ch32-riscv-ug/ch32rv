//! en: `recover` without a method (diagnose) and `recover --method auto` (diagnose, then apply the
//! recommendation). The diagnosis writes nothing: it attaches (falling back to low speed), reads the
//! option bytes twice, and classifies what keeps the target from a normal flash. The fixed methods
//! (`power-off` / `nrst` / `unprotect` / `unbrick`) live in `cmd_flash`.
//! ja: method 無しの `recover`(診断)と `recover --method auto`(診断→推奨を適用)。診断は何も
//! 書かない。attach(low speed へ fallback)→ option bytes を 2 回読む→ 通常 flash を妨げている
//! 要因を分類する。固定 method の実体は `cmd_flash` 側。

use std::process::ExitCode;
use std::time::Duration;

use ch32rv_contract::policy::RecoverMethod;
use ch32rv_contract::{ErrorKind, ResultEnvelope, Warning};
use ch32rv_wchlink::Variant;

use crate::args::Cli;
use crate::cmd_probe::{confirm_destructive, fail, mode_str, select_entry};
use crate::parse;
use crate::session::{Session, SessionError};

const CMD: &str = "recover";

/// What the diagnosis found, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// Attached, option bytes sane and at their documented defaults: nothing to recover.
    Healthy,
    /// RDPR is not 0xA5: flash is read-protected (clearing it mass-erases).
    ReadProtected,
    /// USER bits the DB documents differ from their reset values (e.g. a watchdog forced on).
    OptionNonstandard,
    /// The option bytes read back inconsistent or implausible (complements do not match).
    OptionUnreadable,
    /// Nothing answered on the debug pins, at any speed.
    Unreachable,
}

impl State {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::ReadProtected => "read-protected",
            Self::OptionNonstandard => "option-nonstandard",
            Self::OptionUnreadable => "option-unreadable",
            Self::Unreachable => "unreachable",
        }
    }
}

/// The recommended next step for a [`State`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Unprotect,
    OptionReset,
    PowerOff,
    /// Nothing ch32rv can do from here (e.g. unreachable behind a probe that cannot switch power).
    Manual,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unprotect => "unprotect",
            Self::OptionReset => "option-reset",
            Self::PowerOff => "power-off",
            Self::Manual => "manual",
        }
    }
}

/// en: Classify option bytes read twice. Pure, so the decision table is unit-tested without a
/// target. ja: 2 回読んだ option bytes を分類する純関数(target 無しで判定表を試験できる)。
pub(crate) fn classify(db_family: &str, first: &[u8; 16], second: &[u8; 16]) -> State {
    if first != second {
        return State::OptionUnreadable;
    }
    // RDPR first: a protected part may not hand the rest back, so its complement pairs are not
    // evidence of anything.
    if first[0] != 0xA5 {
        return State::ReadProtected;
    }
    if !crate::cmd_target::option_bytes_plausible(first) {
        return State::OptionUnreadable;
    }
    if crate::cmd_target::reset_user_byte(db_family, first[2]) != first[2] {
        return State::OptionNonstandard;
    }
    State::Healthy
}

fn recommend(state: State, power_capable: bool) -> Action {
    match state {
        State::Healthy => Action::None,
        State::ReadProtected | State::OptionUnreadable => Action::Unprotect,
        State::OptionNonstandard => Action::OptionReset,
        State::Unreachable if power_capable => Action::PowerOff,
        State::Unreachable => Action::Manual,
    }
}

struct Diagnosis {
    state: State,
    action: Action,
    probe: String,
    /// The speed class the target answered at (`None` when it never did).
    speed: Option<&'static str>,
    family: Option<String>,
    db_family: Option<String>,
    chip_id: Option<u32>,
    option_bytes: Option<[u8; 16]>,
    notes: Vec<String>,
    warnings: Vec<Warning>,
}

fn try_attach(
    cli: &Cli,
    entry: &crate::cmd_probe::Entry,
    spec: &str,
    warnings: &mut Vec<Warning>,
) -> Result<Session, SessionError> {
    let (speed, w) = parse::speed(spec).map_err(SessionError::Attach)?;
    warnings.extend(w);
    Session::attach(
        entry,
        speed,
        Duration::from_millis(cli.timeout.map(|s| s * 1000).unwrap_or(3000)),
        Duration::from_secs(cli.lock_timeout),
        cli.chip.as_deref(),
        cli.db.as_deref(),
        warnings,
    )
}

fn diagnose(cli: &Cli) -> Result<Diagnosis, ExitCode> {
    let entry = select_entry(cli, CMD)?;
    if entry.mode != ch32rv_contract::ProbeMode::Riscv {
        return Err(fail(
            cli,
            CMD,
            ErrorKind::CapabilityUnsupported,
            format!(
                "probe is in {} mode; recovering a target needs RISC-V mode",
                mode_str(entry.mode)
            ),
            Some("switch it with `ch32rv probe mode set riscv`"),
        ));
    }
    let mut warnings = Vec::new();
    let mut notes = Vec::new();

    // en: Identify the probe before any attach: whether power-off is on the table depends on it,
    // and asking afterwards means reopening a device the failed attach has only just released.
    // ja: attach より前に probe を特定する(power-off の可否がこれで決まる)。失敗した attach の
    // 直後に開き直すと、解放直後の device を開くことになる。
    let (probe, power_capable) = match crate::session::open_with_retry(&entry).and_then(|mut l| {
        // A LinkE next to a wedged target has been seen answering GetProbeInfo with an
        // error once (`81 55 01 02`) and normally the next time, so ask more than once.
        let mut last = l.probe_info();
        for _ in 0..2 {
            if last.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
            last = l.probe_info();
        }
        last
    }) {
        Ok(i) => (
            i.variant.name(),
            matches!(i.variant, Variant::LinkE | Variant::LinkW),
        ),
        Err(e) => {
            notes.push(format!("could not identify the probe: {e}"));
            ("unknown".to_owned(), false)
        }
    };

    // en: The requested speed first, then low: a target running at a slow or odd clock, or with a
    // long cable, can answer at low speed when it does not at high. Only "no target" falls back;
    // `--chip` conflicts and lock timeouts are real answers and are rendered as such.
    // ja: 指定 speed → low の順。遅い/変なクロックや長い配線の target は low なら応答することが
    // ある。fallback するのは「応答なし」だけで、`--chip` 矛盾や lock timeout はそのまま返す。
    let requested = cli.speed.as_str();
    let mut tries: Vec<&'static str> = Vec::new();
    let mut session = None;
    for spec in [Some(requested), (requested != "low").then_some("low")]
        .into_iter()
        .flatten()
    {
        match try_attach(cli, &entry, spec, &mut warnings) {
            Ok(s) => {
                let label: &'static str = if spec == "low" { "low" } else { "requested" };
                tries.push(label);
                session = Some((s, label));
                break;
            }
            Err(SessionError::NoTarget | SessionError::Attach(_)) => {
                tries.push(if spec == "low" { "low" } else { "requested" });
            }
            Err(e) => return Err(crate::cmd_probe::session_error(cli, CMD, e)),
        }
    }

    let Some((mut session, label)) = session else {
        // Nothing answered. Whether power-off is on the table depends on the probe.
        notes.push(
            "check first that the target is powered and SWDIO/SWCLK (or SWIO) and GND are wired: that is the common cause"
                .to_owned(),
        );
        if cli.connect_under_reset {
            notes.push("--connect-under-reset was already tried".to_owned());
        } else {
            notes.push(
                "if the probe's RST line is wired to the target's NRST, retry with --connect-under-reset, or use --method nrst"
                    .to_owned(),
            );
        }
        return Ok(Diagnosis {
            state: State::Unreachable,
            action: recommend(State::Unreachable, power_capable),
            probe,
            speed: None,
            family: None,
            db_family: None,
            chip_id: None,
            option_bytes: None,
            notes,
            warnings,
        });
    };
    let speed: &'static str = if label == "low" {
        "low"
    } else {
        speed_class(requested)
    };
    if label == "low" {
        notes.push(format!(
            "the target did not answer at --speed {requested}, only at low: pass --speed low to other commands"
        ));
    }

    let probe = session.probe_info.variant.name();
    let family = session.family();
    let db_family = crate::cmd_target::db_family_of(&mut session);
    let chip_id = session.attach.chip_id;
    let base = crate::cmd_target::option_base(&db_family)
        .map_err(|msg| fail(cli, CMD, ErrorKind::CapabilityUnsupported, msg, None))?;
    let mut dm = session.dm();
    dm.halt().map_err(|e| {
        fail(
            cli,
            CMD,
            ErrorKind::AttachFailed,
            format!("halt failed: {e}"),
            None,
        )
    })?;
    let mut reads = [[0u8; 16]; 2];
    let mut read_ok = true;
    for r in &mut reads {
        match dm.read_mem(base, 16) {
            Ok(v) if v.len() == 16 => r.copy_from_slice(&v),
            _ => read_ok = false,
        }
    }
    // Leave the target running as it was found: the diagnosis only looks.
    let _ = dm.resume();
    let state = if read_ok {
        classify(&db_family, &reads[0], &reads[1])
    } else {
        State::OptionUnreadable
    };
    if state == State::OptionNonstandard {
        let user = reads[0][2];
        let want = crate::cmd_target::reset_user_byte(&db_family, user);
        notes.push(format!(
            "USER 0x{user:02x}, documented defaults give 0x{want:02x} (fields: {})",
            ch32rv_target::option_user_fields(&db_family)
                .into_iter()
                .filter(|f| f.bit < 8 && (user ^ want) & (1 << f.bit) != 0)
                .map(|f| f.field)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Diagnosis {
        state,
        action: recommend(state, true),
        probe,
        speed: Some(speed),
        family: Some(family),
        db_family: Some(db_family),
        chip_id: Some(chip_id),
        option_bytes: read_ok.then_some(reads[0]),
        notes,
        warnings,
    })
}

fn speed_class(spec: &str) -> &'static str {
    match parse::speed(spec) {
        Ok((ch32rv_wchlink::Speed::Low, _)) => "low",
        Ok((ch32rv_wchlink::Speed::Medium, _)) => "medium",
        _ => "high",
    }
}

/// The command line that applies `action` (what a person would type next).
fn command_for(cli: &Cli, d: &Diagnosis, action: Action) -> Option<String> {
    let speed = match d.speed {
        Some("low") => " --speed low",
        _ => "",
    };
    let chip = d
        .db_family
        .clone()
        .or_else(|| cli.chip.clone())
        .unwrap_or_else(|| "<family>".to_owned());
    Some(match action {
        Action::None | Action::Manual => return None,
        Action::Unprotect => format!("ch32rv recover --method unprotect{speed}"),
        Action::OptionReset => format!("ch32rv target option reset{speed}"),
        Action::PowerOff => format!("ch32rv recover --method power-off --chip {chip}"),
    })
}

/// The hedged caveats of a power-off recovery, shown whenever it is recommended or run.
const POWER_OFF_CAVEATS: [&str; 3] = [
    "power-off works only when the target is powered from the probe's 3V3/5V pin; a board with its own supply never loses power",
    "the target may stay partly powered through other connections (UART TX->RX, the SWDIO/SWCLK pull-ups), so the power cycle may not take",
    "confirm the result with `ch32rv read --blank-check` (or a normal attach) afterwards",
];

fn render(cli: &Cli, d: &Diagnosis) -> ExitCode {
    let cmd = command_for(cli, d, d.action);
    if cli.json {
        let mut env = ResultEnvelope::success(CMD);
        env.warnings = d.warnings.clone();
        env.result = Some(serde_json::json!({
            "state": d.state.as_str(),
            "probe": d.probe,
            "speed": d.speed,
            "family": d.family,
            "db_family": d.db_family,
            "chip_id": d.chip_id.map(|c| format!("0x{c:08x}")),
            "option_bytes": d.option_bytes.map(|b| crate::cmd_target::hex(&b)),
            "recommendation": { "action": d.action.as_str(), "command": cmd },
            "notes": d.notes,
            "caveats": if d.action == Action::PowerOff { POWER_OFF_CAVEATS.to_vec() } else { Vec::new() },
        }));
        return crate::print_envelope(&env);
    }
    for w in &d.warnings {
        eprintln!("warning[{}]: {}", w.code, w.msg);
    }
    println!("probe:  {}", d.probe);
    match (&d.family, d.chip_id) {
        (Some(f), Some(id)) => println!(
            "target: {} (chip_id 0x{id:08x}), attached at {} speed",
            d.db_family.as_deref().unwrap_or(f),
            d.speed.unwrap_or("?")
        ),
        _ => println!(
            "target: no response on the debug pins (tried --speed {})",
            if cli.speed == "low" {
                "low".to_owned()
            } else {
                format!("{} and low", cli.speed)
            }
        ),
    }
    if let Some(b) = d.option_bytes {
        println!(
            "option: {} (RDPR 0x{:02x}, USER 0x{:02x})",
            crate::cmd_target::hex(&b),
            b[0],
            b[2]
        );
    }
    println!("state:  {}", d.state.as_str());
    for n in &d.notes {
        println!("note:   {n}");
    }
    match (d.action, cmd) {
        (Action::None, _) => println!(
            "nothing to recover: the target attaches, read protection is off, and the documented USER bits are at their reset values"
        ),
        (Action::Manual, _) => println!(
            "recommended: {}",
            if d.probe == "unknown" {
                "the probe could not be identified; if it is a WCH-LinkE / LinkW, try `ch32rv recover --method power-off --chip <family>`, otherwise power-cycle the target by hand, or wire NRST and use --method nrst"
            } else {
                "this probe cannot switch the target's power; power-cycle it by hand, or wire NRST and use --method nrst"
            }
        ),
        (a, Some(c)) => {
            let auto_chip = if a == Action::PowerOff {
                " --chip <family>"
            } else {
                ""
            };
            println!("recommended: {c}   (or: ch32rv recover --method auto{auto_chip})");
            if a == Action::PowerOff {
                for c in POWER_OFF_CAVEATS {
                    println!("caution: {c}");
                }
            }
        }
        (_, None) => {}
    }
    ExitCode::SUCCESS
}

/// `recover` (no `--method`): diagnose and print the recommendation. Writes nothing.
pub fn diagnose_only(cli: &Cli) -> ExitCode {
    match diagnose(cli) {
        Ok(d) => render(cli, &d),
        Err(c) => c,
    }
}

/// `recover --method auto`: diagnose, then apply the recommended action (each action keeps its own
/// confirmation prompt; `--yes` skips them).
pub fn auto(cli: &Cli) -> ExitCode {
    let d = match diagnose(cli) {
        Ok(d) => d,
        Err(c) => return c,
    };
    if matches!(d.action, Action::None | Action::Manual) {
        return render(cli, &d);
    }
    if !cli.json {
        println!(
            "diagnosis: {}; applying {}",
            d.state.as_str(),
            d.action.as_str()
        );
    }
    let speed = d.speed.unwrap_or("low");
    match d.action {
        Action::Unprotect => {
            crate::cmd_probe::with_speed(speed, || crate::cmd_flash::recover_unprotect(cli))
        }
        Action::OptionReset => {
            crate::cmd_probe::with_speed(speed, || crate::cmd_target::option_reset(cli))
        }
        Action::PowerOff => {
            if cli.chip.is_none() {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    "the target does not answer; a power-off recovery needs --chip <family> (it cannot be read from the target)",
                    Some("e.g. ch32rv recover --method auto --chip CH32X035"),
                );
            }
            if !cli.json {
                for c in POWER_OFF_CAVEATS {
                    eprintln!("caution: {c}");
                }
            }
            if let Err(why) = confirm_destructive(
                cli,
                "Power-cycle the target and erase its code flash (WCH special erase)?",
            ) {
                return fail(
                    cli,
                    CMD,
                    ErrorKind::Usage,
                    why,
                    Some("pass --yes to confirm"),
                );
            }
            crate::cmd_flash::recover_special_erase(cli, RecoverMethod::PowerOff)
        }
        Action::None | Action::Manual => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEALTHY: [u8; 16] = [
        0xA5, 0x5A, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
        0x00,
    ];

    #[test]
    fn classify_table() {
        assert_eq!(classify("CH32L103", &HEALTHY, &HEALTHY), State::Healthy);
        let mut prot = HEALTHY;
        prot[0] = 0x00;
        prot[1] = 0xFF;
        assert_eq!(classify("CH32L103", &prot, &prot), State::ReadProtected);
        let mut bad = HEALTHY;
        bad[3] = 0x12;
        assert_eq!(classify("CH32L103", &bad, &bad), State::OptionUnreadable);
        assert_eq!(
            classify("CH32L103", &HEALTHY, &bad),
            State::OptionUnreadable
        );
    }

    #[test]
    fn nonstandard_user_bit() {
        // Clearing a documented USER bit whose reset value is 1 is "nonstandard"; which bit that is
        // comes from the DB, so find one rather than hard-code it.
        let Some(f) = ch32rv_target::option_user_fields("CH32L103")
            .into_iter()
            .find(|f| f.bit < 8 && f.default != 0)
        else {
            return;
        };
        let mut ob = HEALTHY;
        ob[2] &= !(1 << f.bit);
        ob[3] = !ob[2];
        assert_eq!(classify("CH32L103", &ob, &ob), State::OptionNonstandard);
    }

    #[test]
    fn recommendations() {
        assert_eq!(recommend(State::Unreachable, true), Action::PowerOff);
        assert_eq!(recommend(State::Unreachable, false), Action::Manual);
        assert_eq!(recommend(State::OptionUnreadable, true), Action::Unprotect);
    }
}
