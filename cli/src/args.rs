//! en: Command-tree definition. Mirrors the finalized tree of docs/cli.ja.md §2 into clap.
//! The shape (commands, flags, defaults) is fixed here regardless of implementation status.
//!
//! ja: コマンドツリー定義。docs/cli.ja.md §2 の完成形をそのまま clap に写す。
//! 実装状況に関わらず、体系(コマンド・フラグ・既定値)はここで固定する。

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use ch32rv_contract::policy::{
    ConfirmRunMode, EraseMode, ImageFormat, MonitorSource, RecoverMethod, Region, ResetPolicy,
    VerifyMode,
};

#[derive(Parser)]
#[command(
    name = "ch32rv",
    version,
    about = "Flashing and debugging tool for WCH CH32 RISC-V microcontrollers",
    propagate_version = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    // en: Global options (docs/cli.ja.md §3.1).
    // ja: グローバルオプション(docs/cli.ja.md §3.1)。
    /// Probe selector: VID:PID[:SERIAL] | serial:<sn> | name:<alias> | usb:<bus>-<ports> | index:<n>
    #[arg(long, global = true, env = "CH32RV_PROBE")]
    pub probe: Option<String>,
    /// Target SKU or family (auto-detected when omitted; fail-closed on ambiguity)
    #[arg(long, global = true, env = "CH32RV_CHIP")]
    pub chip: Option<String>,
    /// Core index for dual-core parts (H41x)
    #[arg(long, global = true, default_value_t = 0)]
    pub core: u32,
    /// Debug speed: low|medium|high|<kHz>
    #[arg(long, global = true, default_value = "high")]
    pub speed: String,
    /// Attach with NRST asserted
    #[arg(long, global = true)]
    pub connect_under_reset: bool,
    /// Print the result as JSON on stdout
    #[arg(long, global = true)]
    pub json: bool,
    /// Progress output (default: bar on a tty, none otherwise)
    #[arg(long, global = true, value_enum)]
    pub progress: Option<ProgressMode>,
    /// Fail instead of prompting
    #[arg(long, global = true, env = "CH32RV_NON_INTERACTIVE")]
    pub non_interactive: bool,
    /// Skip confirmation for destructive operations
    #[arg(long, global = true)]
    pub yes: bool,
    /// Seconds to wait for the device lock
    #[arg(long, global = true, default_value_t = 10)]
    pub lock_timeout: u64,
    /// Override the transport timeout (seconds)
    #[arg(long, global = true)]
    pub timeout: Option<u64>,
    /// Target DB overlay (for trying new SKUs without rebuilding)
    #[arg(long, global = true, env = "CH32RV_DB")]
    pub db: Option<PathBuf>,
    /// Write a detailed log to this file
    #[arg(long, global = true)]
    pub log_file: Option<PathBuf>,
    /// Record USB/serial transactions (replay fixture)
    #[arg(long, global = true)]
    pub capture: Option<PathBuf>,
    /// Plan only; do not open any device
    #[arg(long, global = true)]
    pub dry_run: bool,
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[arg(short = 'q', long, global = true, action = ArgAction::Count)]
    pub quiet: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProgressMode {
    Bar,
    Ndjson,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SwitchState {
    On,
    Off,
}

#[derive(Subcommand)]
pub enum Command {
    /// Program the target (with erase/verify/reset/confirm-run policies)
    Flash(FlashArgs),
    /// Compare target contents against an image (mismatch: exit 30)
    Verify(VerifyArgs),
    /// Read memory/flash: dump and blank check
    Read(ReadArgs),
    /// Raw memory/region write (advanced)
    Write(WriteArgs),
    /// Erase flash (--all / --region / --range)
    Erase(EraseArgs),
    /// Reset the target (default: reset, run, detach)
    Reset(ResetArgs),
    /// Flash, run, monitor output, and propagate the exit code (for HIL)
    Run(RunArgs),
    /// Recovery operations (power-off / nrst / unprotect / unbrick)
    Recover(RecoverArgs),
    /// Manage the probe itself
    #[command(subcommand)]
    Probe(ProbeCmd),
    /// Identify and configure the target
    #[command(subcommand)]
    Target(TargetCmd),
    /// One-shot execution control
    #[command(subcommand)]
    Dbg(DbgCmd),
    /// Runtime I/O (uart / sdi / dmdata / rtt)
    Monitor(MonitorArgs),
    /// GDB server (never modifies flash on attach)
    Gdb(GdbArgs),
    /// DAP server
    Dap(DapArgs),
    /// Factory ISP route (USB/UART)
    Isp(IspArgs),
    /// Custom bootloader route
    #[command(subcommand)]
    Boot(BootCmd),
    /// Inspect the target DB
    #[command(subcommand)]
    Db(DbCmd),
    /// Capability matrix: probe x firmware x target x operation
    Capabilities,
    /// Diagnose the environment and suggest next steps
    Doctor(DoctorArgs),
    /// Show tool/contract/DB/stub versions
    Version,
    /// Generate shell completions
    Complete(CompleteArgs),
    /// Arduino IDE integration protocols (machine-facing)
    #[command(subcommand)]
    Arduino(ArduinoCmd),
}

// en: §4.1 programming commands. / ja: §4.1 書き込み系。

#[derive(Args)]
pub struct FlashArgs {
    /// Input image (ELF / Intel HEX / bin / UF2)
    pub file: PathBuf,
    #[arg(long, value_enum, default_value = "auto")]
    pub format: ImageFormat,
    /// Load address for bin input (default: start of the code region)
    #[arg(long)]
    pub offset: Option<String>,
    /// Destination region (code|system; system only where the family supports it)
    #[arg(long, value_enum, default_value = "code")]
    pub region: Region,
    #[arg(long, value_enum, default_value = "auto")]
    pub erase: EraseMode,
    #[arg(long, value_enum, default_value = "readback")]
    pub verify: VerifyMode,
    /// Skip programming when the contents already match
    #[arg(long)]
    pub preverify: bool,
    #[arg(long, value_enum, default_value = "run")]
    pub reset: ResetPolicy,
    /// Confirm the target is actually running after reset (exit 50 on failure)
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "pc")]
    pub confirm_run: Option<ConfirmRunMode>,
    /// Set the SDI print state after programming
    #[arg(long, value_enum)]
    pub sdi: Option<SwitchState>,
    /// Start a monitor session right after programming
    #[arg(long, value_enum)]
    pub monitor: Option<MonitorSource>,
    /// Preserve unwritten bytes within erased sectors
    #[arg(long)]
    pub restore_unwritten: bool,
    /// Program repeatedly as targets are re-connected (production)
    #[arg(long)]
    pub repeat: bool,
}

#[derive(Args)]
pub struct VerifyArgs {
    pub file: PathBuf,
    #[arg(long, value_enum, default_value = "auto")]
    pub format: ImageFormat,
    #[arg(long)]
    pub offset: Option<String>,
    #[arg(long, value_enum, default_value = "code")]
    pub region: Region,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("src").required(true).args(["range", "region", "blank_check"])))]
pub struct ReadArgs {
    /// Range to read: <addr>[+len|..end]
    #[arg(long)]
    pub range: Option<String>,
    /// Region name: code|system|option|eeprom|ram[+off][+len]
    #[arg(long)]
    pub region: Option<String>,
    /// Output file (- for stdout)
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "bin")]
    pub format: ReadFormat,
    /// Blank check (failure: exit 30)
    #[arg(long)]
    pub blank_check: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReadFormat {
    Bin,
    Hex,
    Ihex,
}

#[derive(Args)]
pub struct WriteArgs {
    /// Input: <FILE> | hex:<bytes> | word:<u32>
    pub source: String,
    /// Destination: <addr> | <region>[+off]
    #[arg(long)]
    pub at: String,
    #[arg(long, value_enum, default_value = "none")]
    pub erase: WriteErase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum WriteErase {
    Auto,
    None,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("scope").required(true).args(["all", "region", "range"])))]
pub struct EraseArgs {
    /// Erase the whole chip
    #[arg(long)]
    pub all: bool,
    /// Erase a named region
    #[arg(long)]
    pub region: Option<String>,
    /// Erase a range: <a>..<b>
    #[arg(long)]
    pub range: Option<String>,
}

#[derive(Args)]
pub struct ResetArgs {
    /// Halt after reset
    #[arg(long)]
    pub halt: bool,
    /// Reset the debug module only
    #[arg(long)]
    pub dm: bool,
    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "pc")]
    pub confirm_run: Option<ConfirmRunMode>,
}

#[derive(Args)]
pub struct RunArgs {
    pub elf: PathBuf,
    /// Attach only; do not flash
    #[arg(long)]
    pub no_flash: bool,
    #[arg(long, value_enum)]
    pub source: Option<MonitorSource>,
    /// Exit condition: semihosting | timeout=<s>
    #[arg(long)]
    pub exit_on: Option<String>,
}

#[derive(Args)]
pub struct RecoverArgs {
    #[arg(long, value_enum)]
    pub method: RecoverMethod,
}

// en: §4.2 probe. / ja: §4.2 probe。

#[derive(Subcommand)]
pub enum ProbeCmd {
    /// List probes (--json includes every selector key)
    List {
        /// Keep watching hotplug events
        #[arg(long)]
        watch: bool,
    },
    /// Model / HW / firmware / mode / serial / interfaces + known-bad firmware check
    Info,
    /// Control power outputs
    #[command(subcommand)]
    Power(PowerCmd),
    /// RISC-V / DAP mode
    #[command(subcommand)]
    Mode(ModeCmd),
    /// Probe firmware: version info, known-bad check, IAP update
    #[command(subcommand)]
    Firmware(FirmwareCmd),
    /// Backend-specific escape hatch
    #[command(hide = true)]
    Vendor {
        #[arg(required = true)]
        hex: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum PowerCmd {
    /// 3.3V output
    #[command(name = "3v3")]
    V3v3 {
        #[arg(value_enum)]
        state: SwitchState,
    },
    /// 5V output
    #[command(name = "5v")]
    V5 {
        #[arg(value_enum)]
        state: SwitchState,
    },
    /// Power cycle
    Cycle {
        #[arg(long, default_value_t = 300)]
        off_ms: u64,
    },
}

#[derive(Subcommand)]
pub enum ModeCmd {
    Get,
    Set {
        #[arg(value_enum)]
        mode: ProbeModeSet,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProbeModeSet {
    Riscv,
    Dap,
}

#[derive(Subcommand)]
pub enum FirmwareCmd {
    /// Version (raw / normalized / WCH notation), hash, known-bad check
    Info,
    /// For CI: exit 12 on a known-bad version or one below --min
    Check {
        #[arg(long)]
        min: Option<String>,
    },
    /// Enter IAP mode and write a user-supplied image
    Update {
        #[arg(long)]
        image: PathBuf,
    },
}

// en: §4.3 target. / ja: §4.3 target。

#[derive(Subcommand)]
pub enum TargetCmd {
    /// Chip ID / SKU candidates / UID / sizes / protection / option summary / verified
    Info,
    /// Structured option bytes
    #[command(subcommand, name = "option")]
    Opt(OptionCmd),
    /// Read protection (turning it off implies a full erase)
    Protect {
        #[arg(value_enum)]
        state: SwitchState,
    },
}

#[derive(Subcommand)]
pub enum OptionCmd {
    Get,
    /// e.g. rdp=off nrst=gpio split=160/32 debug=off
    Set {
        #[arg(required = true)]
        kv: Vec<String>,
    },
    /// Restore factory defaults
    Reset,
    /// Raw value (expert)
    WriteRaw {
        hex: String,
    },
}

// en: §4.4 dbg. / ja: §4.4 dbg。

#[derive(Subcommand)]
pub enum DbgCmd {
    Halt {
        #[arg(long)]
        reset: bool,
    },
    Resume,
    Step {
        n: Option<u32>,
    },
    /// Dump GPRs + pc (dpc)
    Regs,
    #[command(subcommand)]
    Reg(RegCmd),
    /// Direct DM register access (expert)
    #[command(subcommand)]
    Dmi(DmiCmd),
}

#[derive(Subcommand)]
pub enum RegCmd {
    /// x1..x31 | pc | csr:<addr>
    Read {
        name: String,
    },
    Write {
        name: String,
        value: String,
    },
}

#[derive(Subcommand)]
pub enum DmiCmd {
    Read { addr: String },
    Write { addr: String, value: String },
}

// en: §4.5 monitor. / ja: §4.5 monitor。

#[derive(Args)]
pub struct MonitorArgs {
    #[command(subcommand)]
    pub cmd: Option<MonitorCmd>,
    #[arg(long, value_enum, default_value = "uart")]
    pub source: MonitorSource,
    /// Port selector: path:<dev> | usb:VID:PID[:SERIAL][:IFACE] (default: derived from --probe's CDC)
    #[arg(long)]
    pub port: Option<String>,
    #[arg(long, default_value_t = 115200)]
    pub baud: u32,
    #[arg(long)]
    pub timestamps: bool,
    #[arg(long)]
    pub log: Option<PathBuf>,
    #[arg(long)]
    pub raw: bool,
    /// Disable re-enumeration tracking (enabled by default)
    #[arg(long)]
    pub no_reconnect: bool,
}

#[derive(Subcommand)]
pub enum MonitorCmd {
    /// List candidate monitor ports and their roles
    List,
    /// Enable/disable SDI print
    Sdi {
        #[arg(value_enum)]
        state: SwitchState,
    },
}

// en: §4.6 gdb / dap. / ja: §4.6 gdb / dap。

#[derive(Args)]
pub struct GdbArgs {
    #[arg(long, default_value = "127.0.0.1:3333")]
    pub listen: String,
    #[arg(long)]
    pub reset_halt: bool,
    /// Refuse vFlash (load)
    #[arg(long)]
    pub no_flash: bool,
}

#[derive(Args)]
pub struct DapArgs {
    #[arg(long)]
    pub port: Option<u16>,
    #[arg(long)]
    pub stdio: bool,
}

// en: §4.7 isp. / ja: §4.7 isp。

#[derive(Args)]
pub struct IspArgs {
    #[command(subcommand)]
    pub cmd: IspCmd,
    #[arg(long, global = true, value_enum, default_value = "usb")]
    pub transport: IspTransport,
    /// Serial port for the UART transport
    #[arg(long, global = true)]
    pub port: Option<String>,
    #[arg(long, global = true)]
    pub baud: Option<u32>,
    /// ISP device selector: usb:<bus>-<ports> | index:<n> (fail-closed by default; ISP devices carry no serial)
    #[arg(long, global = true)]
    pub device: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IspTransport {
    Usb,
    Uart,
}

#[derive(Subcommand)]
pub enum IspCmd {
    /// List ISP-mode devices (WCH-Link IAP devices shown separately)
    List,
    /// Chip / BTVER / UID / protection state
    Info,
    /// Enter ISP with application cooperation
    Enter {
        #[arg(long)]
        via: Option<String>,
    },
    Flash {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "auto")]
        erase: IspErase,
        #[arg(long, value_enum, default_value = "on")]
        verify: IspVerify,
        #[arg(long, value_enum, default_value = "run")]
        reset: IspReset,
    },
    Verify {
        file: PathBuf,
    },
    Erase,
    /// Dataflash (EEPROM)
    #[command(subcommand)]
    Eeprom(EepromCmd),
    /// Config bytes (incl. debug enable/disable, unprotect)
    #[command(subcommand)]
    Config(IspConfigCmd),
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IspErase {
    Auto,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IspVerify {
    On,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IspReset {
    Run,
    None,
}

#[derive(Subcommand)]
pub enum EepromCmd {
    Read { out: Option<PathBuf> },
    Write { file: PathBuf },
    Erase,
}

#[derive(Subcommand)]
pub enum IspConfigCmd {
    Get,
    Set {
        #[arg(required = true)]
        kv: Vec<String>,
    },
    Reset,
}

// en: §4.8 boot. / ja: §4.8 boot。

#[derive(Subcommand)]
pub enum BootCmd {
    /// Enter the bootloader (touch1200 | double-reset | magic | pin)
    Enter {
        #[arg(long)]
        method: Option<String>,
        #[arg(long)]
        port: Option<String>,
    },
    #[command(subcommand)]
    Dfu(DfuCmd),
    #[command(subcommand)]
    Uf2(Uf2Cmd),
    #[command(subcommand)]
    Uart(UartBootCmd),
    #[command(subcommand)]
    Hid(HidBootCmd),
}

#[derive(Subcommand)]
pub enum DfuCmd {
    Flash {
        file: PathBuf,
        #[arg(long)]
        alt: Option<u8>,
        #[arg(long)]
        address: Option<String>,
    },
    Info,
}

#[derive(Subcommand)]
pub enum Uf2Cmd {
    Flash {
        file: PathBuf,
        #[arg(long)]
        volume: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum UartBootCmd {
    Flash {
        file: PathBuf,
        /// RS-485 multi-drop node id
        #[arg(long)]
        node: Option<String>,
    },
    Info {
        #[arg(long)]
        node: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum HidBootCmd {
    Flash { file: PathBuf },
}

// en: §4.9 db and §4.10 misc. / ja: §4.9 db、§4.10 その他。

#[derive(Subcommand)]
pub enum DbCmd {
    List {
        #[arg(long)]
        family: Option<String>,
        #[arg(long)]
        verified_only: bool,
    },
    Info {
        sku: String,
    },
}

#[derive(Args)]
pub struct DoctorArgs {
    /// Emit udev rules
    #[arg(long)]
    pub emit_udev: bool,
}

#[derive(Args)]
pub struct CompleteArgs {
    #[arg(value_enum)]
    pub shell: Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

#[derive(Subcommand)]
pub enum ArduinoCmd {
    /// Pluggable Discovery protocol (stdio JSON)
    Discovery,
    /// Pluggable Monitor protocol (stdio JSON)
    Monitor,
}
