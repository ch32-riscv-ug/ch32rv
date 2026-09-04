//! en: ch32rv CLI. A thin layer that only composes the library crates (docs/architecture.ja.md §2).
//! The WCH-LinkE-route surface is implemented (flash/verify/read/write/erase/reset/recover, dbg +
//! gdb, monitor, target/probe/db/capabilities, arduino); a few routes (run/isp/boot/dap) still
//! return exit 70 (unimplemented).
//!
//! ja: ch32rv CLI。library crate 群を組み合わせるだけの薄い層(docs/architecture.ja.md §2)。
//! WCH-LinkE 経路の機能は実装済み(flash/verify/read/write/erase/reset/recover、dbg+gdb、monitor、
//! target/probe/db/capabilities、arduino)。一部経路(run/isp/boot/dap)は exit 70(unimplemented)。

mod args;
mod cmd_arduino;
mod cmd_capabilities;
mod cmd_db;
mod cmd_dbg;
mod cmd_doctor;
mod cmd_flash;
mod cmd_gdb;
mod cmd_monitor;
mod cmd_probe;
mod cmd_run;
mod cmd_target;
mod cmd_write;
mod config;
mod parse;
mod progress;
mod session;

use clap::Parser;

use args::*;
use ch32rv_contract::{self as contract, ErrorKind, ResultEnvelope};

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // --replay: run against a recorded capture instead of hardware (mutually exclusive with --capture).
    if let Some(path) = cli.replay.as_deref() {
        if cli.capture.is_some() {
            eprintln!("error: --replay and --capture cannot be used together");
            return ErrorKind::Usage.exit_code().into();
        }
        if let Err(e) = ch32rv_usb::replay::start(path) {
            eprintln!("error: --replay: cannot load {}: {e}", path.display());
            return ErrorKind::Usage.exit_code().into();
        }
    }
    // Start USB transaction capture if requested (diagnostic; a failure to open the file only warns).
    if let Some(path) = cli.capture.as_deref()
        && let Err(e) = ch32rv_usb::capture::start(path)
    {
        eprintln!(
            "warning: --capture disabled: cannot write {}: {e}",
            path.display()
        );
    }
    let code = run_command(&cli);
    // After a replay, note if the run diverged from the recording (a protocol change) or ran short.
    if let Some((divergences, underruns)) = ch32rv_usb::replay::summary()
        && (divergences > 0 || underruns > 0)
    {
        eprintln!(
            "warning: replay diverged from the recording ({divergences} write mismatch(es), {underruns} short read(s)) - the code may produce a different protocol than the capture"
        );
    }
    code
}

fn run_command(cli: &Cli) -> std::process::ExitCode {
    match &cli.command {
        Command::Version => cmd_version(cli),
        Command::Probe(ProbeCmd::List { watch }) => cmd_probe::list(cli, *watch),
        Command::Probe(ProbeCmd::Info) => cmd_probe::info(cli),
        Command::Probe(ProbeCmd::Mode(ModeCmd::Get)) => cmd_probe::mode_get(cli),
        Command::Probe(ProbeCmd::Mode(ModeCmd::Set { mode })) => cmd_probe::mode_set(cli, *mode),
        Command::Probe(ProbeCmd::Power(p)) => cmd_probe::power(cli, p),
        Command::Probe(ProbeCmd::Firmware(FirmwareCmd::Info)) => cmd_probe::firmware_info(cli),
        Command::Probe(ProbeCmd::Firmware(FirmwareCmd::Check { min })) => {
            cmd_probe::firmware_check(cli, min.as_deref())
        }
        Command::Probe(ProbeCmd::Firmware(FirmwareCmd::ExitIap)) => {
            cmd_probe::firmware_exit_iap(cli)
        }
        Command::Probe(ProbeCmd::Firmware(FirmwareCmd::Update { image })) => {
            cmd_probe::firmware_update(cli, image)
        }
        Command::Target(TargetCmd::Info) => cmd_target::info(cli),
        Command::Target(TargetCmd::Opt(OptionCmd::Get)) => cmd_target::option_get(cli),
        Command::Target(TargetCmd::Opt(OptionCmd::WriteRaw { hex })) => {
            cmd_target::option_write_raw(cli, hex)
        }
        Command::Target(TargetCmd::Opt(OptionCmd::Set { kv })) => cmd_target::option_set(cli, kv),
        Command::Target(TargetCmd::Opt(OptionCmd::Reset)) => cmd_target::option_reset(cli),
        Command::Target(TargetCmd::Protect { state }) => cmd_target::protect(cli, *state),
        Command::Dbg(DbgCmd::Regs) => cmd_dbg::regs(cli),
        Command::Dbg(DbgCmd::Halt { reset }) => cmd_dbg::halt(cli, *reset),
        Command::Dbg(DbgCmd::Resume) => cmd_dbg::resume(cli),
        Command::Dbg(DbgCmd::Step { n }) => cmd_dbg::step(cli, *n),
        Command::Dbg(DbgCmd::Reg(sub)) => cmd_dbg::reg(cli, sub),
        Command::Dbg(DbgCmd::Dmi(sub)) => cmd_dbg::dmi(cli, sub),
        Command::Read(args) => cmd_dbg::read(cli, args),
        Command::Flash(args) => cmd_flash::flash(cli, args),
        Command::Verify(args) => cmd_flash::verify(cli, args),
        Command::Erase(args) => cmd_flash::erase(cli, args),
        Command::Reset(args) => cmd_flash::reset(cli, args),
        Command::Recover(args) => cmd_flash::recover(cli, args),
        Command::Doctor(args) => cmd_doctor::doctor(cli, args),
        Command::Monitor(args) => cmd_monitor::monitor(cli, args),
        Command::Gdb(args) => cmd_gdb::gdb(cli, args),
        Command::Db(DbCmd::List {
            family,
            verified_only,
        }) => cmd_db::list(cli, family.as_deref(), *verified_only),
        Command::Db(DbCmd::Info { sku }) => cmd_db::info(cli, sku),
        Command::Capabilities => cmd_capabilities::capabilities(cli),
        Command::Write(args) => cmd_write::write(cli, args),
        Command::Arduino(ArduinoCmd::Discovery) => cmd_arduino::discovery(cli),
        Command::Arduino(ArduinoCmd::Monitor) => cmd_arduino::monitor(cli),
        Command::Run(args) => cmd_run::run(cli, args),
        Command::Complete(a) => cmd_complete(a.shell),
        other => unimplemented_cmd(cli, canonical_name(other)),
    }
}

/// `complete <shell>`: print a shell completion script to stdout (redirect it into your shell's
/// completion dir). Generated from the clap command tree, so it always matches the CLI.
fn cmd_complete(shell: Shell) -> std::process::ExitCode {
    use clap::CommandFactory;
    let target = match shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
        Shell::Powershell => clap_complete::Shell::PowerShell,
    };
    let mut cmd = Cli::command();
    clap_complete::generate(target, &mut cmd, "ch32rv", &mut std::io::stdout());
    std::process::ExitCode::SUCCESS
}

fn cmd_version(cli: &Cli) -> std::process::ExitCode {
    let git_rev = env!("CH32RV_GIT_REV");
    let stub_digest = ch32rv_flash::stub::stub_digest();
    // en: Embedded device-DB provenance (source rev + fingerprint) - docs/architecture.ja.md §3.
    // ja: 埋め込み device DB の来歴(source rev + 指紋)。architecture.ja.md §3 の再現性契約。
    let db = ch32rv_target::provenance();
    let mut env = ResultEnvelope::success("version");
    env.result = Some(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_rev": git_rev,
        "target_db": {
            "source": "ch32-device-data",
            "source_rev": db.source_rev,
            "digest": db.digest,
        },
        "flash_stub_digest": stub_digest,
        "build": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
    }));
    if cli.json {
        print_envelope(&env)
    } else {
        println!("ch32rv {} ({git_rev})", env!("CARGO_PKG_VERSION"));
        println!("contract:   {}", contract::CONTRACT_VERSION);
        println!(
            "target db:  ch32-device-data@{} ({})",
            db.source_rev.as_deref().unwrap_or("unknown"),
            db.digest
        );
        println!("flash stub: {stub_digest}");
        println!(
            "build:      {}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        std::process::ExitCode::SUCCESS
    }
}

pub(crate) fn unimplemented_cmd(cli: &Cli, cmd: &str) -> std::process::ExitCode {
    if cli.json {
        let mut env = ResultEnvelope::failure(
            cmd,
            ErrorKind::Unimplemented,
            "not implemented yet (scaffold)",
        );
        if let Some(e) = env.error.as_mut() {
            e.hint = Some(
                "see docs/cli.ja.md for the specified behavior and the milestone plan".to_owned(),
            );
        }
        let code = ErrorKind::Unimplemented.exit_code();
        let _ = print_envelope(&env);
        code.into()
    } else {
        eprintln!("ch32rv: `{cmd}` is not implemented yet (scaffold; see docs/cli.ja.md)");
        contract::ExitCode::Internal.into()
    }
}

pub(crate) fn print_envelope(env: &ResultEnvelope) -> std::process::ExitCode {
    match serde_json::to_string(env) {
        Ok(s) => {
            println!("{s}");
            if env.ok {
                std::process::ExitCode::SUCCESS
            } else {
                env.error
                    .as_ref()
                    .map(|e| std::process::ExitCode::from(e.code))
                    .unwrap_or(contract::ExitCode::Internal.into())
            }
        }
        Err(e) => {
            eprintln!("ch32rv: internal error: failed to serialize result: {e}");
            contract::ExitCode::Internal.into()
        }
    }
}

/// en: Canonical command name (`cmd` in the JSON envelope; docs/contract/result.schema.json).
/// ja: command の正規名(JSON の `cmd`。docs/contract/result.schema.json)。
fn canonical_name(cmd: &Command) -> &'static str {
    match cmd {
        Command::Flash(_) => "flash",
        Command::Verify(_) => "verify",
        Command::Read(_) => "read",
        Command::Write(_) => "write",
        Command::Erase(_) => "erase",
        Command::Reset(_) => "reset",
        Command::Run(_) => "run",
        Command::Recover(_) => "recover",
        Command::Probe(p) => match p {
            ProbeCmd::List { .. } => "probe.list",
            ProbeCmd::Info => "probe.info",
            ProbeCmd::Power(_) => "probe.power",
            ProbeCmd::Mode(ModeCmd::Get) => "probe.mode.get",
            ProbeCmd::Mode(ModeCmd::Set { .. }) => "probe.mode.set",
            ProbeCmd::Firmware(FirmwareCmd::Info) => "probe.firmware.info",
            ProbeCmd::Firmware(FirmwareCmd::Check { .. }) => "probe.firmware.check",
            ProbeCmd::Firmware(FirmwareCmd::ExitIap) => "probe.firmware.exit-iap",
            ProbeCmd::Firmware(FirmwareCmd::Update { .. }) => "probe.firmware.update",
            ProbeCmd::Vendor { .. } => "probe.vendor",
        },
        Command::Target(t) => match t {
            TargetCmd::Info => "target.info",
            TargetCmd::Opt(OptionCmd::Get) => "target.option.get",
            TargetCmd::Opt(OptionCmd::Set { .. }) => "target.option.set",
            TargetCmd::Opt(OptionCmd::Reset) => "target.option.reset",
            TargetCmd::Opt(OptionCmd::WriteRaw { .. }) => "target.option.write-raw",
            TargetCmd::Protect { .. } => "target.protect",
        },
        Command::Dbg(d) => match d {
            DbgCmd::Halt { .. } => "dbg.halt",
            DbgCmd::Resume => "dbg.resume",
            DbgCmd::Step { .. } => "dbg.step",
            DbgCmd::Regs => "dbg.regs",
            DbgCmd::Reg(_) => "dbg.reg",
            DbgCmd::Dmi(_) => "dbg.dmi",
        },
        Command::Monitor(m) => match &m.cmd {
            Some(MonitorCmd::List) => "monitor.list",
            Some(MonitorCmd::Sdi { .. }) => "monitor.sdi",
            None => "monitor",
        },
        Command::Gdb(_) => "gdb",
        Command::Dap(_) => "dap",
        Command::Isp(i) => match &i.cmd {
            IspCmd::List => "isp.list",
            IspCmd::Info => "isp.info",
            IspCmd::Enter { .. } => "isp.enter",
            IspCmd::Flash { .. } => "isp.flash",
            IspCmd::Verify { .. } => "isp.verify",
            IspCmd::Erase => "isp.erase",
            IspCmd::Eeprom(EepromCmd::Read { .. }) => "isp.eeprom.read",
            IspCmd::Eeprom(EepromCmd::Write { .. }) => "isp.eeprom.write",
            IspCmd::Eeprom(EepromCmd::Erase) => "isp.eeprom.erase",
            IspCmd::Config(IspConfigCmd::Get) => "isp.config.get",
            IspCmd::Config(IspConfigCmd::Set { .. }) => "isp.config.set",
            IspCmd::Config(IspConfigCmd::Reset) => "isp.config.reset",
            IspCmd::Reset => "isp.reset",
        },
        Command::Boot(b) => match b {
            BootCmd::Enter { .. } => "boot.enter",
            BootCmd::Dfu(DfuCmd::Flash { .. }) => "boot.dfu.flash",
            BootCmd::Dfu(DfuCmd::Info) => "boot.dfu.info",
            BootCmd::Uf2(Uf2Cmd::Flash { .. }) => "boot.uf2.flash",
            BootCmd::Uart(UartBootCmd::Flash { .. }) => "boot.uart.flash",
            BootCmd::Uart(UartBootCmd::Info { .. }) => "boot.uart.info",
            BootCmd::Hid(HidBootCmd::Flash { .. }) => "boot.hid.flash",
        },
        Command::Db(d) => match d {
            DbCmd::List { .. } => "db.list",
            DbCmd::Info { .. } => "db.info",
        },
        Command::Capabilities => "capabilities",
        Command::Doctor(_) => "doctor",
        Command::Version => "version",
        Command::Complete(_) => "complete",
        Command::Arduino(a) => match a {
            ArduinoCmd::Discovery => "arduino.discovery",
            ArduinoCmd::Monitor => "arduino.monitor",
        },
    }
}
