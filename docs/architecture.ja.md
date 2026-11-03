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
| nusb | 0.2.7(2026-08)。hotplug、backend は usbfs/WinUSB/IOKit/**WebUSB(wasm)**。0.1→0.2 で API 変動あり | `ch32rv-usb` に完全隔離し、他 crate に nusb 型を漏らさない。万一死んだら rusb/libusb へ差し替え可能な境界にする。**Windows のみ例外**: WCH 純正ドライバが interface を所有していて WinUSB(nusb)で開けない場合、`ch32rv-usb-wch-win`(CH375 IOCTL 直叩き)へ device 単位でフォールバックする(docs/windows-wch-driver.ja.md) |
| gdbstub | 0.7.10(2026-03)。no_std、実績多数。**gdbstub_arch の RISC-V は整数レジスタのみ** | V4F(V307/V317/H41x)の FPU レジスタは自前 Arch 定義で足す(作業量小)。probe-rs が `=0.7.8` に pin した前例に倣い exact pin |
| serialport | 4.10.0(2026-08)。活発 | 採用 |
| clap / serde / object | いずれも活発 | 採用。ELF は object を採用(goblin は不採用) |
| ihex | 3.0.0 から **2020 年以降更新なし** | 依存せず自前 parser(仕様が小さい。fuzz 対象にする) |
| cargo-dist | 上流(axodotdev)が 2025-07 に復活、0.32.0(2026-05)。Astral fork はアーカイブ済み | 採用。bus factor に備え、素の GitHub Actions matrix への脱出路を CI 構成で確保 |

**Rust で自動解決しないもの**(原設計案 §7 の再確認): 経験的 protocol 知識、target DB の正しさ、実機 CI、firmware 版ごとの癖。これらは record/replay harness・生成 DB・protocol の記録(wch-protocols。§2.3)という「仕組み」で担保する(原設計案 §7.1 を維持)。

## 2. crate 構成

workspace は機能単位の library crate 群 + 薄い CLI。**すべての library crate は単体で crates.io に publish 可能・他プロジェクトから利用可能**を要件とする(GUI はこの上に別プロジェクトで作る)。

| crate | 責務 | 主要公開 API | 依存 |
|---|---|---|---|
| `ch32rv-contract` | exit code・JSON/NDJSON event 型・capability 語彙・エラー分類。**CLI/GUI/CI の共通語彙** | `ExitCode`, `Event`, `CapabilityReport`, serde 型 | serde |
| `ch32rv-usb` | USB 境界層: 列挙(nusb)、selector 解決、advisory lock、transaction capture/replay。転送 backend は nusb + Windows のみ CH375 フォールバック | `enumerate()`, `Selector`, `DeviceLock`, `Capture` | nusb, (win) usb-wch-win |
| `ch32rv-usb-wch-win` | **Windows 専用・workspace 唯一の unsafe FFI 島**: WCH 純正ドライバ(CH375 系 IOCTL)経由の bulk 転送。protocol 非依存で他ツールからも部品利用可(docs/windows-wch-driver.ja.md §4.1) | `list_interfaces()`, `Ch375Device`(write/read_pipe), `GUID_CH375` | windows-sys |
| `ch32rv-wchlink` | WCH-Link bulk protocol(`0x81 cmd len ...`)+ IAP。送る frame はすべて `Request` が作り、wch-protocols の CSV と test で照合する(§2.3) | `WchLink`(open/attach/dmi/vendor cmd/power/mode/sdi/fw) | usb, contract |
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

### 2.3 protocol の事実と実装判断

protocol の事実(byte 列・応答・status・実測)は **wch-protocols** の `protocols/*.ja.md` が一次で、ch32rv はそれを file + anchor で引く(例 `protocols/pc-to-link.ja.md#cmd-rst-out`)。WCH-Link のコマンド表の正本は `protocols/data/wchlink-commands.csv` で、`ch32rv-wchlink` の unit test が `Request::frame()` の byte を CSV の `request_frame` と比べる(`WCH_PROTOCOLS_DIR`、既定は workspace の隣の `../wch-protocols`。無ければ skip)。ch32rv で分かった事実はそちらへ送る。ch32rv 側に置くのは次の実装判断だけ:

- **転送**: `WchLink` の既定 timeout は 500 ms で、CLI は 3 s(`--timeout`)にする。応答の先頭が `0x81` なら probe 側エラー(byte 1 を reason として返す)。
- **高速 read**(`WchLink::read_mem`): 16 KiB ごとに SetReadMemoryRegion + ReadMemory を出す。probe が領域を弾いたときだけ DMI の word 読みへ落とす。verify の不一致は DMI の読みで確かめ直してから報告する。DB のメモリ領域の外は warning `read-outside-memory-map`。
- **stub 書込**: 1 回の書込に連続 region を 1 つだけ渡す(`Image::program_span`、隙間は `0xff`)。読出し保護は保護中のときだけ外す。
- **直接 FLASH controller 経路**: 方式は `ch32-device-data` の `flash_program_method.csv` から引き、family ごとに手書きしない。buffer load が 32 bit でない family は fail-closed(V103 だけ標準 half-word へ倒す)。`confidence=conflict` の family と page 単位の fast erase を持たない family も fail-closed。option bytes の番地は DB の `option_bytes.csv`、消去済みの値は `flash_geometry.csv` から引き、erase の成否は STATR で決める。
- **reset**: probe の soft reset の後は ack havereset → DMSTATUS → halt なら resume の順で走行を保証する。reset vector で止める操作は DMI の reset-halt で行う。
- **debug**: HW trigger は write-readback で検出する。SW breakpoint は RAM patch → 空き HW trigger → flash SW breakpoint の順で、どれも無ければ未対応を返す(`hwbreak+` を偽らない)。RV32E は x0-x15 だけを扱う。`attach_corrupts_regs` の family(V103)は attach の後に soft reset してから halt する。
- **速度**: attach の前に placeholder family `0x01` で 1 回だけ SetSpeed を送る。実 family での送り直しは `monitor --source sdi` と `gdb` だけ。
- **SDI の CDC**: DTR を立てない開き方で読む。UART bridge は serialport で開く。
- **復旧**: ChipInfo が同じ word の繰り返しなら RedetectChip + detach + 再 attach。`recover --method power-off|nrst` は `0x00` の間は 3 回まで繰り返し、`0x0f` が無ければ warning `special-erase-unconfirmed`。
- **版**: 比較は正規化した `major*10+minor` で行う。既知不良は `ch32-device-data` の `index/link_firmware_issues.csv`(実測で不良の版)から生成した `link_firmware_issues.csv` を variant・mode・版で引き、hash は使わない。probe が mode を返さないときは mode を問わず当てる。
- **IAP 更新**: ack の timeout は 3 s。`00 00` 以外の応答は即中断する。再列挙は消失と再出現を検出して待ち、戻った直後の open 失敗は retry する。
- **factory ISP**: IspKey の seed は全 0。IspEnd は応答を読んでから終える。`--erase` は `flash` と同じ値。keep-store / sector は image の長さ分の sector(最小 8。keep-store は最小が STORE に届くとき image の分だけ)、chip は全 sector。`isp enter --via probe` は reset vector で止めてから 1 回の attach で `BOOT_MODE` を立てる。

## 3. target DB 生成

**データ調達の原則**: tool が必要とする device データは ch32rv 内部で作らず、**`ch32-device-data` への CSV 追加依頼を第一**とする。依頼は即納されない前提で、納品までの間は `ch32rv-target/provisional/` に暫定 overlay(provenance 付き)を置いて開発を進めてよい。納品時に暫定側と突き合わせて(crosscheck)受け入れ、受け入れ後は暫定側を削除する——2 系統の恒久併存を作らない。差分が出た場合は data repo 側を正として調査する。

| 項目 | 方針 |
|---|---|
| 源泉 | flash geometry・memory map・option 分割・DM レジスタ番地 = `ch32-device-data`(stable 表、provenance 付き)。chip ID(device_id)値 = `ch32-device-data`(WCH EVT の ChipID 一覧と実測だけから作る。device_ids に無い型番も parts から ID 無しで生成する)。flash mode 付き memory 定義 = `ch32-data`。**gap の 7 series(V205/V407/V467/X305/X315/M030/M103)の device_id は `ch32-device-data` に evidence 表の新設を依頼する**(実測手順は `ch32-data/docs/device-ids.md`、実測 basis の前例は `curated/debug-data-measured.json`)。probe firmware の hash→型番・版の対応は `index/link_firmware.csv` から `link_firmware.csv` を生成して使う(`probe firmware update` の image 照合)。既知不良の firmware は `index/link_firmware_issues.csv` から `link_firmware_issues.csv` を生成して使う(手書きの表は持たない)。ch32-device-data の probe の名前(`device` + `mcu`)から variant ID(`linke` など)への対応は `crates/target/src/link_variant.rs` の 1 か所だけに書き、db-gen も同じファイルを読んで、対応の無い device で生成を止める |
| 暫定 overlay | 依頼中データの一時置き場(`provisional/`)。生成 pipeline を必ず通し、生成物と CLI 出力に `provisional` flag を出す(verified と同列の可視性) |
| 生成 | `xtask db-gen` が入力 repo の pinned revision から `ch32rv-target/generated/` を生成して **commit する**(hermetic build。build.rs でネットワークや隣接 repo に依存しない)。CI が再生成一致を検査 |
| 再現性 | 入力 rev と sha256 を生成物に埋め、`version --json` に出す |
| verified | SKU ごとに「実機確認済み」flag と根拠(いつ・どの probe・どの操作)を持ち、CLI 出力に出す |
| flash stub | 事前 build blob を持たず、in-repo の source から CI で build して hash を `version --json` に出す(データではなくコードなので ch32rv 内で持つ) |
| 手書き禁止 | 例外が要る場合も暫定 overlay として入れ、`ch32-device-data` への依頼(issue 等)と紐付ける |

**実装**: `cargo xtask db-gen [DATA_DIR]`(既定 `$CH32_DEVICE_DATA` or `../ch32-device-data`)。ch32-device-data の `evidence/device_ids.csv` + `index/parts.csv` を part_number で join して `crates/target/generated/skus.csv`(65 SKU)、`evidence/option_byte_fields.csv` の USER byte から `option_fields.csv`(43 fields)を生成し commit する。`ch32rv-target` は両者を `include_str!` で埋め込むので build は隣接 repo 非依存。生成時に (a) don't-care bits が [7:4] でなければ拒否、(b) masked device_id(0xFFFFFF0F)が実衝突すれば拒否、で fail-closed。`verified` 列は本プロジェクトが実機測定した 6 SKU のみ true(`MEASURED` 定数、docs/data-requests/measured/ 由来)。geometry は skus.csv の flash/sram に加え `flash_geometry.csv`(page/fast erase・fast program・block erase)。**CI 再生成一致検査は `cargo xtask db-check`**: 生成物を in-memory 再生成して committed とデータ比較し、drift があれば exit 1。**provenance ヘッダ(source rev 行)は無視**するので、ch32-device-data の HEAD が無関係な commit で進んでもデータが同一なら通る(実データ変化のみ検出)。生成/検査は `generate()` を共有。`flash_geometry.csv` は `erased_word`(消去済み word の読み出し値。RM の 2 系統 `0xFFFFFFFF` / `0xe339e339`)も持つ。`flash_program_method.csv` は FLASH controller の書込方式(`buffered` / `direct`)・commit bit・buffer load 幅・confidence を持つ。`ch32rv-flash::flash_controller_profile` は手書きの表を持たず、page サイズ・方式・消去パターンをすべて DB から引く(`conflict` 行・page 消去なし・DMI で駆動できない手順は fail-closed)。buffer load 幅(32/64/128)も DB の `program_buffer_load_bits` 由来で、**手書き・暫定値は持たない**。**option byte の位置も DB 化**: `option_bytes.csv`(family 別 base + 書込方式)を生成し、`option get/set/reset/write-raw/protect` と `recover` が使う。base は共通ではない(CH32M030 = `0x1FFF_F300`)ため、DB に無い family は fail-closed。device_id・debug wiring・option byte layout は、実測 6 台と rev [7:4] don't-care で全一致を確認済み。gap 7 series(V205/V407/V467/X305/X315/M030/M103)は未発売でデータ側も未収載。

## 4. テストの骨子(原設計案 §7.1 の確認)

- **USB record/replay harness を最初に作る**。`--capture` が吐く transaction log をそのまま fixture 化し、firmware 2.11/2.12/2.15 の記録で protocol regression を実機なし CI で検出する。
- unit + replay + fuzz(protocol decoder、image parser)は HIL なしで回る構成。実機 CI(LinkE + 各 family)は段階導入。
- 「FW 2.11 では走らない」「16.7 KB で固まる」を **test として固定する**ことが replay harness の受け入れ条件。

## 5. 配布

| 項目 | 方針 |
|---|---|
| license | **MIT**(単一。repo の LICENSE 取得済み)。依存 crate の inventory は cargo-deny で管理 |
| binary | release workflow(`.github/workflows/release.yml`。cargo-dist は使わない)で Win x64、Linux x64/arm64、macOS x64/arm64。各 archive に checksum(`.sha256`)を付ける。手順は [release-plan.ja.md](release-plan.ja.md) |
| USB 権限 | udev rule を同梱し `doctor --emit-udev` でも出す。Windows は WinUSB binding の検出と手順提示を `doctor` が持つ(自動変更しない) |
| Arduino | Board Manager package。ch32rv は自前 release を持つため ADR-0011 の `mirror-` 枠で追従できる(ADR-0014 の `build-` 枠は不要) |
| vendor blob | 同梱しない(LinkE firmware は user-supplied) |

## 6. 参照

- [requirements.ja.md](requirements.ja.md) / [cli.ja.md](cli.ja.md)
- [原設計案 §7](../../note/research/new-programming-tool-design.ja.md)
- [nusb](https://github.com/kevinmehall/nusb)、[gdbstub](https://github.com/daniel5151/gdbstub)、[cargo-dist](https://github.com/axodotdev/cargo-dist)
- `../../ch32-device-data/index/README.md`(consumer contract)、`../../ch32-data/docs/device-ids.md`
- `../../ArduinoCore-CH32RV/docs/adr/0011`、`0014`(配布枠組み)
