# ch32rv アーキテクチャ: 言語検証と crate 構成

- 作成日: 2026-09-01
- 状態: 提案
- 根拠: [requirements.ja.md](requirements.ja.md)、エコシステム調査(2026-09-01)

## 1. 実装言語の検証

### 1.1 要件

[cli.ja.md](cli.ja.md) の体系を実装・配布・保守するために言語へ要求されるもの:

| # | 要件 | 由来 |
|---|---|---|
| R1 | 単一 static binary を Win/Linux/macOS × x64/arm64 の 6 platform に配布(Arduino Board Manager 同梱) | 配布(§5) |
| R2 | USB(bulk/HID)と serial に user space から接続。WinUSB / udev / IOKit を吸収 | probe/ISP/monitor 層 |
| R3 | memory safety と protocol decoder の fuzz(minichlink の overflow への構造的回答) | 動機そのもの |
| R4 | library として分割再利用できる(GUI 別プロジェクト、将来は他言語・ブラウザも視野) | 本要件 |
| R5 | 型付き error と JSON contract("成功表示なのに失敗"の構造的排除) | 契約(cli §3) |
| R6 | GDB stub 等の実績ある部品が存在する | debug 層 |
| R7 | license inventory が自プロジェクトの依存だけで閉じる(**MIT** で再配布) | 配布 |

### 1.2 候補比較

| 候補 | R1 配布 | R2 USB | R3 安全性 | R4 再利用 | R5 契約 | R6 部品 | R7 license | 所見 |
|---|---|---|---|---|---|---|---|---|
| **Rust** | ◎ cargo-dist / cross | ◎ nusb(pure Rust) | ◎ + cargo-fuzz | ◎ crate / cdylib / wasm | ◎ serde + thiserror | ◎ gdbstub, serialport, clap | ◎ | 採用 |
| C | △ Makefile/CI 自作 | ○ libusb 同梱要 | × 手動 | △ header/so | △ 手書き | ○ | ○ | minichlink の現状が答え |
| C++ | △ | ○ libusb | △ | △ ABI 地獄 | ○ | ◎ OpenOCD 系 | △ 依存追跡が重い | |
| Go | ◎ 単一 binary | × gousb は cgo+libusb(pure Go の USB host stack 不在) | ○ | ○ | ◎ | △ | ○ | R2 で脱落 |
| Zig | ○ | × エコシステム不在 | ○ | △ | △ | × | ○ | 言語自体 0.x |
| Python | × Board Manager 配布不可 | ○ PyUSB | △ | ○ | ◎ | ○ | ○ | rvprog.py の位置 |

**結論: Rust を採用する(原設計案の想定を検証の上で確定)**。決め手は R2(nusb により libusb の同梱・build 依存が消える)と R1(6 platform の release engineering が cargo-dist で成立)で、これは他候補では実質再現できない。

### 1.3 部品の現状確認(2026-09-01)と Rust で自動解決しないもの

| 部品 | 版・状況 | 対応方針 |
|---|---|---|
| nusb | 0.2.7(2026-08)。hotplug、backend は usbfs/WinUSB/IOKit/**WebUSB(wasm)**。0.1→0.2 で API 変動あり | `ch32rv-usb` に完全隔離し、他 crate に nusb 型を漏らさない。万一死んだら rusb/libusb へ差し替え可能な境界にする。**Windows のみ例外**: WCH 純正ドライバが interface を所有していて WinUSB(nusb)で開けない場合、`ch32rv-usb-wch-win`(CH375 IOCTL 直叩き)へ device 単位でフォールバックする(2026-09-02 追加、docs/windows-wch-driver.ja.md) |
| gdbstub | 0.7.10(2026-03)。no_std、実績多数。**gdbstub_arch の RISC-V は整数レジスタのみ** | V4F(V307/V317/H41x)の FPU レジスタは自前 Arch 定義で足す(作業量小)。probe-rs が `=0.7.8` に pin した前例に倣い exact pin |
| serialport | 4.10.0(2026-08)。活発 | 採用 |
| clap / serde / object | いずれも活発 | 採用。ELF は object を採用(goblin は不採用) |
| ihex | 3.0.0 から **2020 年以降更新なし** | 依存せず自前 parser(仕様が小さい。fuzz 対象にする) |
| cargo-dist | 上流(axodotdev)が 2025-07 に復活、0.32.0(2026-05)。Astral fork はアーカイブ済み | 採用。bus factor に備え、素の GitHub Actions matrix への脱出路を CI 構成で確保 |

**Rust で自動解決しないもの**(原設計案 §7 の再確認): 経験的 protocol 知識、target DB の正しさ、実機 CI、firmware 版ごとの癖。これらは record/replay harness・生成 DB・protocol.md という「仕組み」で担保する(原設計案 §7.1 を維持)。

## 2. crate 構成

workspace は機能単位の library crate 群 + 薄い CLI。**すべての library crate は単体で crates.io に publish 可能・他プロジェクトから利用可能**を要件とする(GUI はこの上に別プロジェクトで作る)。

| crate | 責務 | 主要公開 API | 依存 |
|---|---|---|---|
| `ch32rv-contract` | exit code・JSON/NDJSON event 型・capability 語彙・エラー分類。**CLI/GUI/CI の共通語彙** | `ExitCode`, `Event`, `CapabilityReport`, serde 型 | serde |
| `ch32rv-usb` | USB 境界層: 列挙(nusb)、selector 解決、advisory lock、transaction capture/replay。転送 backend は nusb + Windows のみ CH375 フォールバック | `enumerate()`, `Selector`, `DeviceLock`, `Capture` | nusb, (win) usb-wch-win |
| `ch32rv-usb-wch-win` | **Windows 専用・workspace 唯一の unsafe FFI 島**: WCH 純正ドライバ(CH375 系 IOCTL)経由の bulk 転送。protocol 非依存で他ツールからも部品利用可(docs/windows-wch-driver.ja.md §4.1) | `list_interfaces()`, `Ch375Device`(write/read_pipe), `GUID_CH375` | windows-sys |
| `ch32rv-wchlink` | WCH-Link bulk protocol(`0x81 cmd len ...`)+ IAP。**protocol.md を repo の一級成果物として併設** | `WchLink`(open/attach/dmi/vendor cmd/power/mode/sdi/fw) | usb, contract |
| `ch32rv-dmi` | RISC-V Debug Module(0.13.2/1.0)準拠層。**WCH 固有差分は quirk 層に隔離** | `DtmAccess` trait, `DebugModule`, `HaltControl` | contract |
| `ch32rv-target` | 生成 device DB、chip 検出、option byte layout、verified flag | `Db`, `detect()`, `Sku`, `OptionLayout` | contract |
| `ch32rv-flash` | erase/program/verify/confirm-run の編成、flash stub(in-repo source から build、hash 管理) | `FlashSession`, `FlashOptions`, `Image`(elf/hex/bin/uf2) | dmi, target, object |
| `ch32rv-debug` | 実行制御、HW/SW breakpoint(V003 は flash patch)、gdbstub server | `RunControl`, `GdbServer` | dmi, target, flash, gdbstub |
| `ch32rv-monitor` | uart/sdi/dmdata/rtt の source、port 発見、再 enumeration 追従 | `Source`, `PortFinder`, `Session` | usb, serialport, dmi |
| `ch32rv-isp` | factory ISP protocol(USB/UART)自前実装 | `IspDevice`(identify/erase/program/verify/config/eeprom) | usb, serialport, contract |
| `ch32rv-boot` | custom bootloader client(dfu/uf2/uart/hid)P2 | `DfuClient`, `Uf2Volume`, `UartBoot`, `HidBoot` | usb, serialport |
| `ch32rv`(bin) | CLI。上を組み合わせるだけ。arduino discovery/monitor protocol もここ | - | 全部, clap |

将来の互換 probe backend(funprog HID / NHC-Link042 / ardulink 等、P2)は `ch32rv-probe-<name>` として **`DtmAccess` + `ProbeService` trait を実装する別 crate** にし、core の依存を増やさない。

### 2.1 依存方向

```text
                 ch32rv (bin/CLI)
   ┌──────────┬──────┴──────┬───────────┬─────────┐
 flash      debug        monitor      isp       boot
   │  └──┐     │  └────┐     │           │         │
 target   └─ dmi ──────┘     │           │         │
   │           │(DtmAccess trait を実装)│         │
   │        wchlink ─────────┤           │         │
   │           │             │           │         │
   └───────── usb ───────────┴───────────┴─────────┘
                │
            contract(全 crate から参照される語彙)
```

- `dmi` は transport を知らない(`DtmAccess` trait 越し)。`wchlink` がそれを実装する。この境界が「RISC-V Debug Spec 準拠層と WCH quirk の分離」(原設計案 §7.1)の実体で、新 family・新 probe の追加コストを下げる。
- probe-rs の調査から得た教訓: 同期 Session + 排他借用の API は GUI から使うと worker thread 分離が前提になり、probe-rs はそのために別の async RPC 層を持つに至った。ch32rv は第 1 版では**同期 API + event callback** に留め、callback の event 型を `ch32rv-contract` の NDJSON event 型と同一にしておく。これにより後から RPC/async 層(GUI・IDE 統合用)を足しても語彙が割れない。

### 2.2 library API 規約(GUI 再利用の担保)

| 規約 | 内容 |
|---|---|
| unsafe | `#![forbid(unsafe_code)]`(`ch32rv-usb` 以外) |
| panic | library crate では禁止。clippy `unwrap_used`/`expect_used` を deny。unknown response は黙殺せず typed error |
| error | crate ごとの `#[non_exhaustive]` enum(thiserror)。`ch32rv-contract` の分類(exit code 帯)へ写像可能 |
| I/O | library で println/exit/対話をしない。進捗は `&dyn ProgressSink`(= contract の `Event`)、中断は `&CancelToken` を全長時間操作が受ける |
| capability | すべての操作 API は実行前判定を通り、不可は `Err(Unsupported(CapabilityReport))` |
| 実行モデル | 同期(nusb は `MaybeFuture::wait()` でブロッキング利用)。GUI は worker thread に閉じ込める。async facade は必要になった時に追加 |
| MSRV / 依存 | MSRV 固定、`cargo-deny`(license と advisory)、unmaintained crate を入れない。protocol decoder は cargo-fuzz 対象 |

利用イメージ(GUI・自動化から):

```rust
let sel: Selector = "name:bench-01".parse()?;
let probe = ch32rv_wchlink::WchLink::open(&ch32rv_usb::resolve(&sel)?)?;
let target = ch32rv_target::detect(&probe, Db::builtin())?;   // 曖昧なら Err(Ambiguous{candidates})
let mut fs = ch32rv_flash::FlashSession::attach(probe, &target)?;
fs.program(&Image::load("app.elf")?, &FlashOptions { verify: Readback, reset: Run, confirm_run: true },
           &progress, &cancel)?;                               // progress は GUI のイベントループへ
```

nusb が WebUSB(wasm)backend を持つため、**ブラウザ版 flasher(wch-web-isp 相当)も同じ crate 群で原理上成立**する。第 1 版では追わないが、`ch32rv-usb` の境界設計時に wasm target を CI の build check にだけ入れておく。非 Rust GUI 向けの C ABI(cdylib + cbindgen)は P2 で判断する。

## 3. target DB 生成

**データ調達の原則**: tool が必要とする device データは ch32rv 内部で作らず、**`ch32-device-data` への CSV 追加依頼を第一**とする。依頼は即納されない前提で、納品までの間は `ch32rv-target/provisional/` に暫定 overlay(provenance 付き)を置いて開発を進めてよい。納品時に暫定側と突き合わせて(crosscheck)受け入れ、受け入れ後は暫定側を削除する——2 系統の恒久併存を作らない。差分が出た場合は data repo 側を正として調査する。

| 項目 | 方針 |
|---|---|
| 源泉 | flash geometry・memory map・option 分割・DM レジスタ番地 = `ch32-device-data`(stable 表、provenance 付き)。chip ID(device_id)値と flash mode 付き memory 定義 = `ch32-data`。**gap の 7 series(V205/V407/V467/X305/X315/M030/M103)の device_id は `ch32-device-data` に evidence 表の新設を依頼する**(実測手順は `ch32-data/docs/device-ids.md`、実測 basis の前例は `curated/debug-data-measured.json`)。probe firmware の hash→版対応も既存の `evidence/link_firmware.csv` を使い、不足分は依頼で埋める |
| 暫定 overlay | 依頼中データの一時置き場(`provisional/`)。生成 pipeline を必ず通し、生成物と CLI 出力に `provisional` flag を出す(verified と同列の可視性) |
| 生成 | `xtask db-gen` が入力 repo の pinned revision から `ch32rv-target/generated/` を生成して **commit する**(hermetic build。build.rs でネットワークや隣接 repo に依存しない)。CI が再生成一致を検査 |

**実装状況(2026-09-02)**: `cargo xtask db-gen [DATA_DIR]`(既定 `$CH32_DEVICE_DATA` or `../ch32-device-data`)を実装。ch32-device-data の `evidence/device_ids.csv` + `index/parts.csv` を part_number で join して `crates/target/generated/skus.csv`(65 SKU)、`evidence/option_byte_fields.csv` の USER byte から `option_fields.csv`(43 fields)を生成し commit。`ch32rv-target` は両者を `include_str!` で埋め込むので build は隣接 repo 非依存。生成時に (a) don't-care bits が [7:4] でなければ拒否、(b) masked device_id(0xFFFFFF0F)が実衝突すれば拒否、で fail-closed。`verified` 列は本プロジェクトが実機測定した 6 SKU のみ true(`MEASURED` 定数、docs/data-requests/measured/ 由来)。geometry は skus.csv の flash/sram に加え `flash_geometry.csv`(page/fast erase・fast program・block erase)。**CI 再生成一致検査 `cargo xtask db-check` を実装**: 生成物を in-memory 再生成して committed とデータ比較し、drift があれば exit 1。**provenance ヘッダ(source rev 行)は無視**するので、ch32-device-data の HEAD が無関係な commit で進んでもデータが同一なら通る(実データ変化のみ検出)。生成/検査は `generate()` を共有。

- 依頼 0001(device_id)/0002(debug wiring)/0003(option byte layout)は 2026-09-02 に納品受け入れ。実測6台と rev [7:4] don't-care で全一致を確認済み。gap 7 series(V205/V407/V467/X305/X315/M030/M103)は未発売でデータ側も未収載。
| 再現性 | 入力 rev と sha256 を生成物に埋め、`version --json` に出す |
| verified | SKU ごとに「実機確認済み」flag と根拠(いつ・どの probe・どの操作)を持ち、CLI 出力に出す |
| flash stub | 事前 build blob を持たず、in-repo の source から CI で build して hash を `version --json` に出す(データではなくコードなので ch32rv 内で持つ) |
| 手書き禁止 | 例外が要る場合も暫定 overlay として入れ、`ch32-device-data` への依頼(issue 等)と紐付ける |

## 4. テストの骨子(原設計案 §7.1 の確認)

- **USB record/replay harness を最初に作る**。`--capture` が吐く transaction log をそのまま fixture 化し、firmware 2.11/2.12/2.15 の記録で protocol regression を実機なし CI で検出する。
- unit + replay + fuzz(protocol decoder、image parser)は HIL なしで回る構成。実機 CI(LinkE + 各 family)は段階導入。
- 「FW 2.11 では走らない」「16.7 KB で固まる」を **test として固定する**ことが replay harness の受け入れ条件。

## 5. 配布

| 項目 | 方針 |
|---|---|
| license | **MIT**(単一。repo の LICENSE 取得済み)。依存 crate の inventory は cargo-deny で管理 |
| binary | cargo-dist で Win x64/arm64、Linux x64/arm64、macOS x64/arm64。checksum + artifact attestation + SBOM |
| USB 権限 | udev rule を同梱し `doctor --emit-udev` でも出す。Windows は WinUSB binding の検出と手順提示を `doctor` が持つ(自動変更しない) |
| Arduino | Board Manager package。ch32rv は自前 release を持つため ADR-0011 の `mirror-` 枠で追従できる(ADR-0014 の `build-` 枠は不要) |
| vendor blob | 同梱しない(LinkE firmware は user-supplied) |

## 6. 参照

- [requirements.ja.md](requirements.ja.md) / [cli.ja.md](cli.ja.md)
- [原設計案 §7](../../note/research/new-programming-tool-design.ja.md)
- [nusb](https://github.com/kevinmehall/nusb)、[gdbstub](https://github.com/daniel5151/gdbstub)、[cargo-dist](https://github.com/axodotdev/cargo-dist)
- `../../ch32-device-data/index/README.md`(consumer contract)、`../../ch32-data/docs/device-ids.md`
- `../../ArduinoCore-CH32/docs/adr/0011`、`0014`(配布枠組み)
