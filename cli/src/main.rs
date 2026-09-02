//! en: ch32rv CLI. A thin layer that only composes the library crates (docs/architecture.ja.md §2).
//! Scaffold status: the whole command tree (docs/cli.ja.md) is defined, `version` works,
//! everything else exits 70 (unimplemented).
//!
//! ja: ch32rv CLI。library crate 群を組み合わせるだけの薄い層(docs/architecture.ja.md §2)。
//! 現状は scaffold: コマンド体系(docs/cli.ja.md)は全定義済み、`version` のみ動作し、
//! 他は exit 70(unimplemented)を返す。

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
    match &cli.command {
        Command::Version => cmd_version(&cli),
        Command::Probe(ProbeCmd::List { watch }) => cmd_probe::list(&cli, *watch),
        Command::Probe(ProbeCmd::Info) => cmd_probe::info(&cli),
        Command::Probe(ProbeCmd::Mode(ModeCmd::Get)) => cmd_probe::mode_get(&cli),
        Command::Probe(ProbeCmd::Firmware(FirmwareCmd::Info)) => cmd_probe::firmware_info(&cli),
        Command::Probe(ProbeCmd::Firmware(FirmwareCmd::Check { min })) => {
            cmd_probe::firmware_check(&cli, min.as_deref())
        }
        Command::Target(TargetCmd::Info) => cmd_target::info(&cli),
        Command::Target(TargetCmd::Opt(OptionCmd::Get)) => cmd_target::option_get(&cli),
        Command::Target(TargetCmd::Opt(OptionCmd::WriteRaw { hex })) => {
            cmd_target::option_write_raw(&cli, hex)
        }
        Command::Target(TargetCmd::Opt(OptionCmd::Set { kv })) => cmd_target::option_set(&cli, kv),
        Command::Target(TargetCmd::Opt(OptionCmd::Reset)) => cmd_target::option_reset(&cli),
        Command::Target(TargetCmd::Protect { state }) => cmd_target::protect(&cli, *state),
        Command::Dbg(DbgCmd::Regs) => cmd_dbg::regs(&cli),
        Command::Dbg(DbgCmd::Halt { reset }) => cmd_dbg::halt(&cli, *reset),
        Command::Dbg(DbgCmd::Resume) => cmd_dbg::resume(&cli),
        Command::Dbg(DbgCmd::Step { n }) => cmd_dbg::step(&cli, *n),
        Command::Dbg(DbgCmd::Reg(sub)) => cmd_dbg::reg(&cli, sub),
        Command::Dbg(DbgCmd::Dmi(sub)) => cmd_dbg::dmi(&cli, sub),
        Command::Read(args) => cmd_dbg::read(&cli, args),
        Command::Flash(args) => cmd_flash::flash(&cli, args),
        Command::Verify(args) => cmd_flash::verify(&cli, args),
        Command::Erase(args) => cmd_flash::erase(&cli, args),
        Command::Reset(args) => cmd_flash::reset(&cli, args),
        Command::Recover(args) => cmd_flash::recover(&cli, args),
        Command::Doctor(args) => cmd_doctor::doctor(&cli, args),
        Command::Monitor(args) => cmd_monitor::monitor(&cli, args),
        Command::Gdb(args) => cmd_gdb::gdb(&cli, args),
        Command::Db(DbCmd::List {
            family,
            verified_only,
        }) => cmd_db::list(&cli, family.as_deref(), *verified_only),
        Command::Db(DbCmd::Info { sku }) => cmd_db::info(&cli, sku),
        Command::Capabilities => cmd_capabilities::capabilities(&cli),
        Command::Write(args) => cmd_write::write(&cli, args),
        Command::Arduino(ArduinoCmd::Discovery) => cmd_arduino::discovery(&cli),
        Command::Arduino(ArduinoCmd::Monitor) => cmd_arduino::monitor(&cli),
        Command::Complete(a) => cmd_complete(a.shell),
        other => unimplemented_cmd(&cli, canonical_name(other)),
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
    let mut env = ResultEnvelope::success("version");
    env.result = Some(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_rev": git_rev,
        "contract": contract::CONTRACT_VERSION,
        // en: target DB is not generated yet (data request 0001 pending).
        // ja: target DB は未生成(依頼 0001 待ち)。
        "target_db": serde_json::Value::Null,
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
            ProbeCmd::Power(PowerCmd::V3v3 { .. }) => "probe.power.3v3",
            ProbeCmd::Power(PowerCmd::V5 { .. }) => "probe.power.5v",
            ProbeCmd::Power(PowerCmd::Cycle { .. }) => "probe.power.cycle",
            ProbeCmd::Mode(ModeCmd::Get) => "probe.mode.get",
            ProbeCmd::Mode(ModeCmd::Set { .. }) => "probe.mode.set",
            ProbeCmd::Firmware(FirmwareCmd::Info) => "probe.firmware.info",
            ProbeCmd::Firmware(FirmwareCmd::Check { .. }) => "probe.firmware.check",
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
            DbgCmd::Reg(RegCmd::Read { .. }) => "dbg.reg.read",
            DbgCmd::Reg(RegCmd::Write { .. }) => "dbg.reg.write",
            DbgCmd::Dmi(DmiCmd::Read { .. }) => "dbg.dmi.read",
            DbgCmd::Dmi(DmiCmd::Write { .. }) => "dbg.dmi.write",
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
