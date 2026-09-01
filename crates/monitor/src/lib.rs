//! en: Runtime I/O layer (docs/cli.ja.md §4.5).
//! The four sources are distinct transports even when they appear on the same COM port:
//! `uart` (physical UART bridge) / `sdi` (official WCH SDI print, receive-only) /
//! `dmdata` (ch32fun DMDATA0/1 mailbox, bidirectional) / `rtt`.
//! Ports are resolved from VID/PID/serial/interface, never from COM numbers.
//! Re-enumeration tracking is on by default (measured: the LinkE CDC can stop delivering
//! right after flashing and recovers on re-open).
//! Currently a skeleton only; the source vocabulary is [`ch32rv_contract::policy::MonitorSource`].
//!
//! ja: 実行時 I/O 層。4 経路は同じ COM に見えても別 transport として扱う。port は
//! VID/PID/serial/interface から決め、COM 番号に依存しない。再 enumeration 追従は既定で有効
//! (flash 直後に LinkE の CDC 配送が止まる実測への対応)。現状は骨組みのみ。
