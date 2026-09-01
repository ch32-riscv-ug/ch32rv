//! en: Factory ISP route (docs/cli.ja.md §4.7).
//! The protocol is implemented in-house (wchisp is GPL-2.0 and is not vendored; references
//! are minichlink's `pgm-wch-isp.c` (MIT) and our own captures). ISP devices
//! (`4348:55e0` / `1a86:55e0`) carry no USB serial, so multiple devices are handled by the
//! topology selector or fail-closed. WCH-Link IAP mode shares the same VID:PID; devices are
//! disambiguated by BTVER / chip type and LinkE units are redirected to `probe firmware`
//! (docs/requirements.ja.md §3.7).
//! Currently a skeleton only.
//!
//! ja: factory ISP 経路。protocol は自前実装(wchisp は GPL-2.0 のため取り込まない)。
//! ISP device は USB serial を持たないため topology selector か fail-closed で扱う。
//! WCH-Link IAP と同 VID:PID なので BTVER / chip 種別で判別する。現状は骨組みのみ。
