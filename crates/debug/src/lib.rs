//! en: debug layer. Run control, breakpoints, GDB server (docs/cli.ja.md §4.4, §4.6).
//!
//! Contract:
//! - Never modify target flash on attach (do not reproduce WCH OpenOCD's behavior).
//! - Software breakpoints are memory-patched `ebreak` (RAM only for now); flash breakpoints
//!   need the QingKe trigger module and are a follow-up. GDB is told the truth (no `hwbreak+`
//!   pretense like minichlink).
//! - V4F parts (V307/V317/H41x) have FPU registers missing from our RV32 arch; only integer
//!   registers are exposed for now (docs/architecture.ja.md §1.3).
//!
//! ja: debug 層。実行制御・breakpoint・GDB server。契約: attach 時に flash を書き換えない、
//! SW breakpoint は memory patch(当面 RAM のみ)で実態どおり申告する、当面は整数レジスタのみ。

pub mod arch;
pub mod server;

pub use arch::{Rv32, Rv32CoreRegs, Rv32RegId};
pub use server::Ch32Target;
