//! en: Debug layer: run control, breakpoints, GDB server (docs/cli.ja.md §4.4, §4.6).
//!
//! Contract:
//! - Never modify target flash on attach (do not reproduce WCH OpenOCD's behavior).
//! - Families without HW breakpoints (V003, ...) get flash-patch SW breakpoints, and GDB is
//!   told the truth (no `hwbreak+` pretense like minichlink).
//! - V4F parts (V307/V317/H41x) have FPU registers missing from gdbstub_arch; we define our
//!   own Arch (docs/architecture.ja.md §1.3).
//!
//! Currently a skeleton only.
//!
//! ja: debug 層。実行制御・breakpoint・GDB server。契約: attach 時に flash を書き換えない、
//! SW breakpoint は実態どおり申告する、V4F の FPU レジスタは自前 Arch 定義で足す。
//! 現状は骨組みのみ。
