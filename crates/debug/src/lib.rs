//! en: debug layer. Run control, breakpoints, GDB server (docs/cli.ja.md §4.4, §4.6).
//!
//! Contract:
//! - Never modify target flash on attach (do not reproduce WCH OpenOCD's behavior).
//! - A breakpoint is a RAM `ebreak` patch, a hardware trigger, or a flash page rewrite, in that
//!   order (see [`server`]). GDB is told the truth (no `hwbreak+` pretense like minichlink).
//! - Only the integer registers and the PC are exposed; V4F parts' FPU registers are not.
//!
//! ja: debug 層。実行制御・breakpoint・GDB server。契約: attach 時に flash を書き換えない、
//! breakpoint は RAM patch → HW trigger → flash page 書き換えの順で実態どおり申告する、整数レジスタのみ出す。

pub mod arch;
pub mod server;

pub use arch::{Rv32, Rv32CoreRegs, Rv32RegId};
pub use server::Ch32Target;
