# ch32rv アーキテクチャ: 実装言語と crate 構成

要件は [requirements.ja.md](requirements.ja.md)、コマンド体系は [cli.ja.md](cli.ja.md)。

## 1. 実装言語

### 1.1 要件

[cli.ja.md](cli.ja.md) の体系を実装・配布・保守するために言語へ要求されるもの:

| # | 要件 | 由来 |
|---|---|---|
| R1 | 単一の binary を Win/Linux/macOS に配る(Arduino Board Manager 同梱) | 配布(§5) |
| R2 | USB(bulk/HID)と serial に user space から接続。WinUSB / udev / IOKit を吸収 | probe/ISP/monitor 層 |
| R3 | memory safety と protocol decoder の堅牢さ(minichlink の GDB stub の overflow のような欠陥を構造で防ぐ) | 動機そのもの |
| R4 | 機能単位の library crate に分けて組み立てられる | crate 構成(§2) |
| R5 | 型付き error と JSON contract("成功表示なのに失敗"の構造的排除) | 契約(cli §3) |
| R6 | GDB stub 等の実績ある部品が存在する | debug 層 |
| R7 | license inventory が自プロジェクトの依存だけで閉じる(**MIT** で再配布) | 配布 |

### 1.2 候補比較

| 候補 | R1 配布 | R2 USB | R3 安全性 | R4 分割 | R5 契約 | R6 部品 | R7 license | 所見 |
|---|---|---|---|---|---|---|---|---|
| **Rust** | ◎ OS ごとの native build を CI の matrix で | ◎ nusb(pure Rust) | ◎ + cargo-fuzz | ◎ crate / cdylib / wasm | ◎ serde + thiserror | ◎ gdbstub, serialport, clap | ◎ | 採用 |
| C | △ Makefile/CI 自作 | ○ libusb 同梱要 | × 手動 | △ header/so | △ 手書き | ○ | ○ | minichlink の現状が答え |
| C++ | △ | ○ libusb | △ | △ ABI 地獄 | ○ | ◎ OpenOCD 系 | △ 依存追跡が重い | |
| Go | ◎ 単一 binary | × gousb は cgo+libusb(pure Go の USB host stack 不在) | ○ | ○ | ◎ | △ | ○ | R2 で脱落 |
| Zig | ○ | × エコシステム不在 | ○ | △ | △ | × | ○ | 言語自体 0.x |
| Python | × Board Manager 配布不可 | ○ PyUSB | △ | ○ | ◎ | ○ | ○ | rvprog.py の位置 |

**Rust を使う。** 決め手は R2(nusb により libusb の同梱・build 依存が無い)と R1(単一 binary を OS ごとに native build できる)で、他の候補では実質再現できない。

### 1.3 主な部品と扱い

版は `Cargo.toml` の `[workspace.dependencies]` が正。

| 部品 | 扱い |
|---|---|
| nusb | `ch32rv-usb` だけが使い、他の crate に nusb の型を漏らさない。nusb を別の USB 層(rusb/libusb など)へ差し替えても、変わるのは `ch32rv-usb` だけで済む境界にする。**Windows の例外**: WCH 純正ドライバが interface を持っていて WinUSB(nusb)で開けない device は、`ch32rv-usb-wch-win`(CH375 IOCTL)へ device 単位でフォールバックする([windows-wch-driver.ja.md](windows-wch-driver.ja.md)) |
| gdbstub | exact pin(probe-rs が exact pin した前例に倣う)。gdbstub_arch の RISC-V は使わず、`ch32rv-debug` が自前の Arch を持つ: x0-x31 + pc(RV32E は x0-x15 + pc)。FPU / CSR レジスタは GDB に出さない |
| serialport | UART bridge、CDC、ISP の UART 経路 |
| clap / serde / object | CLI、JSON、ELF。ELF は object(goblin は使わない) |
| Intel HEX | crate に依存せず `ch32rv-flash` の自前 parser(仕様が小さい) |
| hidapi | HID の custom bootloader(`boot hid`) |
| 配布 | cargo-dist は使わず、素の GitHub Actions の matrix(`.github/workflows/release.yml`) |

**Rust で自動解決しないもの**: 経験的な protocol 知識、target DB の正しさ、実機での確認、firmware 版ごとの癖。これらは record/replay(§4)、生成 DB(§3)、protocol の記録(wch-protocols。§2.3)という仕組みで担保する。

## 2. crate 構成

workspace は機能単位の library crate 群 + 薄い CLI。crate の一覧は `Cargo.toml` の `[workspace] members`。library crate は CLI の部品として crates.io に publish する(`cargo install ch32rv` が build できるように)。crate ごとの API と semver は約束しない(各 crate の README、[development.ja.md](development.ja.md) §5)。publish しない crate とその順番は [release-plan.ja.md](release-plan.ja.md) §1。

| crate | 責務 | 主な公開 API | 内部依存 |
|---|---|---|---|
| `ch32rv-contract` | exit code・JSON の result envelope・NDJSON event・操作の policy 語彙。**CLI/CI の共通語彙** | `ExitCode`, `ErrorKind`, `ResultEnvelope`, `Event`, `ProgressSink`, `CancelToken`, `policy` | - |
| `ch32rv-usb` | USB 境界層: 列挙(nusb)、selector 解決、advisory lock、transaction の capture/replay、DFU。転送 backend は nusb と、Windows のみ CH375 フォールバック | `enumerate()`, `UsbInterface`, `Selector`, `resolve`, `DeviceLock`, `capture`, `replay`, `dfu` | contract, (Windows) usb-wch-win |
| `ch32rv-usb-wch-win` | **Windows 専用・workspace で唯一 `unsafe` を許す FFI の crate**: WCH 純正ドライバ(CH375 系 IOCTL)経由の bulk 転送。protocol 非依存([windows-wch-driver.ja.md](windows-wch-driver.ja.md) §4.1) | `list_interfaces()`, `DeviceInterface`, `Ch375Device`(`write_pipe` / `read_pipe`), `GUID_CH375` | - |
| `ch32rv-wchlink` | WCH-Link bulk protocol(`0x81 cmd len ...`)+ IAP。送る frame はすべて `Request` が作り、wch-protocols の CSV と test で照合する(§2.3)。`DtmAccess` を実装する | `WchLink`, `Request`, `ProbeInfo`, `IapDevice`, `known_bad_firmware` | contract, usb, dmi, target |
| `ch32rv-oep` | OEP v1 の host 側(wire codec、transport、session)。`DtmAccess` / `TargetAccess` を実装する([oep-host.ja.md](oep-host.ja.md)) | `session`, `link`, `target::OepDtm`, `address` | dmi |
| `ch32rv-dmi` | RISC-V Debug Module 準拠層。**WCH 固有の差分は quirk として分ける** | `DtmAccess` trait, `TargetAccess`, `DebugModule`, `DmSeq` | contract |
| `ch32rv-target` | 生成 device DB、chip 解決、option byte layout、verified / provisional flag | `Db`, `flash_geometry()`, `flash_program_method()`, `option_bytes_layout()`, `provenance()` | contract |
| `ch32rv-flash` | erase/program/verify の編成、image(elf/hex/bin/uf2)、WCH の flash stub、ch32rv の RAM loader | `Image`, `FlashOptions`, `flash_controller_profile`, `loader`, `stub` | contract, dmi, target, wchlink |
| `ch32rv-debug` | 実行制御、breakpoint(RAM patch / HW trigger / flash SW breakpoint)、gdbstub の target | `Ch32Target`, `Rv32` | contract, dmi, target, flash |
| `ch32rv-monitor` | 骨組みだけ(`publish = false`)。monitor の実体は CLI 側(`cli/src/cmd_monitor.rs`、`cli/src/source.rs`) | - | contract, usb, dmi |
| `ch32rv-isp` | factory ISP protocol(USB / UART)の自前実装 | `IspDevice` | contract, usb |
| `ch32rv-boot` | custom bootloader の client(rv003usb / UIAPduino の HID bootloader) | `HidBoot` | contract, usb |
| `ch32rv`(bin) | CLI。上を組み合わせる。arduino discovery/monitor protocol、OEP の broker もここ | - | contract, usb, wchlink, oep, dmi, target, flash, debug, isp, boot |

probe の backend は、`ch32rv-dmi` の `DtmAccess`(と `TargetAccess`)を実装する crate として足す。いまの backend は `ch32rv-wchlink`(WCH-Link)と `ch32rv-oep`(OEP の probe)。

### 2.1 依存方向

```text
ch32rv (bin/CLI)
├─ debug    → flash, dmi, target
├─ flash    → wchlink, dmi, target
├─ wchlink  → usb, dmi, target
├─ oep      → dmi
├─ isp      → usb
├─ boot     → usb
├─ usb      → (Windows のみ) usb-wch-win
└─ dmi, target
contract: 全 crate から参照される語彙(usb-wch-win と oep は使わない)
```

正確な依存は各 crate の `Cargo.toml`(上の表の「内部依存」)。

- `dmi` は transport を知らない(`DtmAccess` trait 越し)。`wchlink` と `oep` がそれを実装する。この境界が「RISC-V Debug Spec 準拠層と WCH quirk の分離」の実体で、新しい family・新しい probe を足すコストを下げる。
- API は同期 + event callback。callback の event 型は `ch32rv-contract` の NDJSON event 型と同じにしてあり、RPC/async の層(GUI・IDE 統合用)を上に足しても語彙が割れない。probe-rs は、同期 Session + 排他借用の API を GUI から使うために worker thread 分離が要り、別の async RPC 層を持つに至った。

### 2.2 library API 規約

詳細は [development.ja.md](development.ja.md) §5。

| 規約 | 内容 |
|---|---|
| unsafe | workspace の lint で `unsafe_code = forbid`(`ch32rv-usb-wch-win` だけ例外。§2 の表) |
| panic | clippy の `unwrap_used` / `expect_used` / `panic` / `todo` / `unimplemented` を deny(テストは局所 allow)。unknown response は黙殺せず typed error |
| error | crate ごとの `#[non_exhaustive]` enum(thiserror)。`ch32rv-contract` の分類(exit code 帯)へ写像できる |
| I/O | library で println/exit/対話をしない。進捗は `&dyn ProgressSink`(= contract の `Event`)、中断は `&CancelToken` |
| capability | 実行できない操作は実行前に断る(CLI は exit 24、`capability-unsupported`) |
| 実行モデル | 同期(nusb は `MaybeFuture::wait()` でブロッキング利用)。GUI から使うなら worker thread に閉じ込める |
| MSRV / 依存 | MSRV は `Cargo.toml` の `rust-version`。`cargo-deny`(license と advisory)を CI で回す。unmaintained crate を入れない |

nusb は WebUSB(wasm)backend も持つので、同じ crate 群でブラウザ版の flasher を作ることも原理上はできる。ch32rv はそれを作っていない。

### 2.3 protocol の事実と実装判断

protocol の事実(byte 列・応答・status・実測)は **wch-protocols** の `protocols/*.ja.md` が一次で、ch32rv はそれを file + anchor で引く(例 `protocols/pc-to-link.ja.md#cmd-rst-out`)。WCH-Link のコマンド表の正本は `protocols/data/wchlink-commands.csv` で、`ch32rv-wchlink` の unit test が `Request::frame()` の byte を CSV の `request_frame` と、`family_name` を `protocols/data/families.csv` と比べる。照合するのは ch32rv に置いた写し(`crates/wchlink/wch-protocols/`、元の commit は `WCH_PROTOCOLS_COMMIT`、`cargo xtask wch-protocols-sync` で更新)で、隣の checkout には頼らない。chip のデータと firmware の指紋は ch32-device-data が正本で、ch32rv は `cargo xtask db-gen` で生成する。ch32rv で分かった事実はそちらへ送る。ch32rv 側に置くのは次の実装判断だけ:

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

**データ調達の原則**: tool が必要とする device データは ch32rv 内部で作らず、**`ch32-device-data` への CSV 追加依頼を第一**とする(依頼文書は `docs/data-requests/`)。依頼は即納されない前提で、納品までの間は `crates/target/provisional/` に暫定 overlay(依頼番号付き)を置いてよい。納品時に暫定側と突き合わせて受け入れ、受け入れたら暫定側の行を消す——2 系統を恒久的に併存させない。差分が出た場合は data repo 側を正として調査する。

| 項目 | 方針 |
|---|---|
| 源泉 | `ch32-device-data` の公開面 `index/*.csv` だけを読む(device_ids、parts、flash_geometry、flash_program_method、option_bytes、option_byte_fields、debug_interfaces、esig、link_firmware、link_firmware_issues、conflicts)。列は名前で引き、固定は data repo の commit と `index/manifest.csv` の sha256。列が消えたり改名されたりすれば、引く所で生成が止まる。chip ID(device_id)は WCH EVT の ChipID 一覧と実測だけから作られ、device_ids に無い型番も parts から ID 無しで載る。ch32-device-data の probe の名前(`device` + `mcu`)から variant ID(`linke` など)への対応は `crates/target/src/link_variant.rs` の 1 か所だけに書き、db-gen も同じファイルを読んで、対応の無い device で生成を止める |
| 暫定 overlay | 依頼中データの一時置き場(`crates/target/provisional/skus.csv`)。生成 DB と一緒に埋め込み、CLI 出力に `provisional` flag を出す(verified と同列の可視性) |
| 生成 | `cargo xtask db-gen [DATA_DIR]`(既定 `$CH32_DEVICE_DATA`、無ければ `../ch32-device-data`)が `crates/target/generated/*.csv` を作り、**commit する**。`ch32rv-target` は `include_str!` で埋め込むので、build はネットワークにも隣の repo にも依存しない |
| 検査 | `cargo xtask db-check` が生成物をメモリ上で作り直し、commit 済みのものとデータを比べる。差があれば exit 1。`# source:` の行(data repo の rev)は無視するので、上流の commit が進んでもデータが同じなら通る。release workflow が版 bump の前に回す |
| 再現性 | 生成物に入力の rev と sha256 を埋め、`version --json` の `target_db`(`source_rev`、`digest`)に出す |
| verified | SKU ごとの「実機で device_id を確認した」flag。対象は xtask の `MEASURED`、根拠は `docs/data-requests/measured/` |
| 手書き禁止 | 例外が要る場合も暫定 overlay として入れ、`ch32-device-data` への依頼と紐付ける |

生成時の fail-closed: device_id の don't-care bits が [7:4] でない行、mask した device_id(`0xFFFFFF0F`)が実際に衝突する行は生成を止める。

DB から引くもの: `flash_geometry.csv` は page / fast erase / fast program / block erase のサイズと `erased_word`(消去済み word の読み出し値。RM の 2 系統 `0xFFFFFFFF` / `0xe339e339`)。`flash_program_method.csv` は FLASH controller の書込方式(`buffered` / `direct`)・commit bit・buffer load 幅・confidence。`ch32rv-flash::flash_controller_profile` は手書きの表を持たず、page サイズ・方式・消去パターン・buffer load 幅をすべて DB から引く(`conflict` 行・page 消去なし・DMI で駆動できない手順は fail-closed)。`option_bytes.csv` は family 別の option byte の base と書込方式で、`option get/set/reset/write-raw/protect` と `recover` が使う。base は共通ではない(CH32M030 = `0x1FFF_F300`)ため、DB に無い family は fail-closed。

**flash の書込コード**: WCH-Link の flash stub(target RAM で走る機械語)は wlink の `src/flash_op.rs`(元は WCH EVT の flash routine)から逐語転記した blob で、`crates/flash/src/stub.rs` にある。手で編集しない。全 stub の digest を `version --json` の `flash_stub_digest` に出す。ch32rv 自前の RAM loader(OEP の probe と、stub の無い WCH-Link の family で使う)は in-repo の source `crates/flash/loader/ch32_loader.S` から `cargo xtask loader-gen` で作った binary を commit し、`cargo xtask loader-check` で source と一致するかを確かめる。

## 4. テストの骨子

- **record/replay**: `--capture` が吐く USB/serial の transaction log(NDJSON)を、そのまま `--replay` の fixture にする(`tests/fixtures/replay/`)。CLI の統合テスト(`cli/tests/`)が実機なしで protocol の regression を検出する。
- unit test(mock の DTM を含む)、replay、OEP の仮想 probe(`tests/oep-virtual/`)は実機なしで回る。CI は fmt / clippy / test / deny(`.github/workflows/ci.yml`)。
- 実機の手順は [testing.ja.md](testing.ja.md)、実機の harness は `tests/hardware/`。

## 5. 配布

| 項目 | 方針 |
|---|---|
| license | **MIT**(単一)。依存 crate の inventory は cargo-deny で管理 |
| binary | release workflow(`.github/workflows/release.yml`。cargo-dist は使わない)で Win x64、Linux x64/arm64、macOS x64/arm64。各 archive に checksum(`.sha256`)を付ける。手順は [release-plan.ja.md](release-plan.ja.md) |
| USB 権限 | Linux は udev rule(`cli/60-ch32rv.rules`)を release の archive に同梱し、`doctor --emit-udev` でも同じものを出す。Windows は WinUSB でも WCH 純正ドライバでも開ける([windows-wch-driver.ja.md](windows-wch-driver.ja.md))。開けない driver が付いた WCH-Link は `arduino discovery` が理由付きで出す |
| Arduino | Board Manager package。ch32rv は自前の release を持つので、ArduinoCore-CH32RV の ADR-0011 の `mirror-` 枠で追従できる(ADR-0014 の `build-` 枠は要らない) |
| vendor blob | 同梱しない(LinkE firmware は user-supplied) |

## 6. 参照

- [requirements.ja.md](requirements.ja.md) / [cli.ja.md](cli.ja.md) / [development.ja.md](development.ja.md) / [release-plan.ja.md](release-plan.ja.md)
- [nusb](https://github.com/kevinmehall/nusb)、[gdbstub](https://github.com/daniel5151/gdbstub)
- ch32-device-data の `index/README.md`(consumer contract)
