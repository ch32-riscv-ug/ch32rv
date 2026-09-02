# Changelog

## Unreleased

- (EN) Initial release. `ch32rv` flashes and debugs WCH CH32 RISC-V MCUs over WCH-Link / WCH-LinkE, shipped as a CLI (`cargo install ch32rv`) and as reusable library crates. The 0.x line is a beta for downstream projects (e.g. ArduinoCore-CH32) to integrate against; the API and CLI may still change before the 1.0 formal release. Verified on a six-board bench (CH32V003 / V103 / V203 / V307 / X035 / L103). Prebuilt binaries for Linux / macOS / Windows - Linux x86_64 is verified, the others are experimental.
- (JA) 初回リリース。`ch32rv` は WCH-Link / WCH-LinkE 経由で WCH CH32 RISC-V MCU を書き込み・デバッグするツール。CLI(`cargo install ch32rv`)と再利用可能な library crate として配布。0.x は下流プロジェクト(ArduinoCore-CH32 等)が統合するためのβで、1.0 の正式リリースまでに API/CLI は変わりうる。6台ベンチ(CH32V003 / V103 / V203 / V307 / X035 / L103)で実機検証。Linux / macOS / Windows のバイナリを配布 - Linux x86_64 = verified、他は experimental。
- (EN) flash: `flash` (program + verify) with erase modes (auto / sector / chip / none; sector is page-granular), `--restore-unwritten`, `--preverify`, `--repeat`, and a post-flash `--sdi` / `--monitor` handoff; plus `verify`, `read`, `write` (raw memory / flash), `erase` (all / range / region), and `reset`.
- (JA) flash: `flash`(program + verify)は erase モード(auto / sector / chip / none、sector は page 単位)・`--restore-unwritten`・`--preverify`・`--repeat`・書込後の `--sdi` / `--monitor` 移行に対応。ほか `verify` / `read` / `write`(raw メモリ・flash)/ `erase`(all / range / region)/ `reset`。
- (EN) debug: `dbg` halt / resume / step / regs / reg / dmi, and a gdb server with hardware and flash breakpoints.
- (JA) debug: `dbg` halt / resume / step / regs / reg / dmi と、HW / flash ブレークポイント対応の gdb server。
- (EN) target & probe: `target info` with SKU / family / debug-wiring / flash-geometry from an embedded device DB; option bytes read / decode / `set` / `write-raw` / `reset` / `protect`; `recover`; `probe list` / `info` and firmware info / check; `db list` / `info`; `capabilities`; and `monitor` (uart / sdi / dmdata).
- (JA) target・probe: 埋め込み device DB による `target info`(SKU / family / デバッグ配線 / flash 容量)、option byte の read / decode / `set` / `write-raw` / `reset` / `protect`、`recover`。`probe list` / `info` と firmware info / check、`db list` / `info`、`capabilities`、`monitor`(uart / sdi / dmdata)。
- (EN) Arduino & shell: `arduino discovery` and `arduino monitor` (the Pluggable Discovery / Monitor protocols; upload itself is plain `flash`), and `complete` for shell completions.
- (JA) Arduino・shell: `arduino discovery` / `arduino monitor`(Pluggable Discovery / Monitor プロトコル。upload は通常の `flash`)、`complete` で shell 補完。
- (EN) Library crates published to crates.io - contract, usb, dmi, target, wchlink, flash, debug - so other tools can reuse the WCH-Link protocol, the RISC-V Debug Module + FLASH-controller access, and the device DB.
- (JA) crates.io に library crate を配布 - contract / usb / dmi / target / wchlink / flash / debug - 他ツールが WCH-Link protocol・RISC-V Debug Module + FLASH controller アクセス・device DB を再利用できる。
