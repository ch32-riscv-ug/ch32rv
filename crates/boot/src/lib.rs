//! en: Custom bootloader route (docs/cli.ja.md §4.8; priority P2).
//! Clients for dfu (dfu-util equivalent) / uf2 (volume detection -> conversion -> copy ->
//! completion watch) / uart (tinyboot-style incl. RS-485 multi-drop) / hid
//! (rv003usb / b003fun), plus unified bootloader entry
//! (`boot enter --method touch1200|double-reset|magic|pin`).
//! Currently a skeleton only.
//!
//! ja: custom bootloader 経路(P2)。dfu / uf2 / uart / hid の client と bootloader への
//! entry 統合を持つ。現状は骨組みのみ。
