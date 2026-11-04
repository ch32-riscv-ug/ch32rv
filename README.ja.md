# ch32rv

[English](README.md) | 日本語

[![crates.io](https://img.shields.io/crates/v/ch32rv.svg)](https://crates.io/crates/ch32rv)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**WCH CH32 RISC-V マイコン**を WCH-Link / WCH-LinkE、OEP(Open Embedded Probe)の probe、チップ自身の bootloader 経由で**書き込み・デバッグ**する単一の CLI(と、再利用可能な Rust crate 群)。
probe-rs / wlink / minichlink / WCH OpenOCD / WCH-LinkUtility / wchisp などに分散していた機能を 1 つに統合する:
書き込み、verify/read/write、erase、復旧、option byte、run 制御 + GDB server、runtime monitor、probe 管理、
内蔵デバイス DB、Arduino IDE 統合プロトコル。

> **β版。** `0.x` は下流プロジェクト(例: ArduinoCore-CH32RV)が統合するためのβで、`1.0` の正式リリースまでに CLI/ライブラリ API は変わりうる。
>
> **検証範囲。** ベンチの CH32V003 / V006 / V103 / V203 / V205 / V307 / X035 / X315 / L103 / H417 で end-to-end 検証済み。
> WCH の flash loader stub が無い family は、ch32rv 自前の RAM loader で書く(OEP の probe でも同じ loader で書く)。
> Linux / macOS / Windows のバイナリを配布。**Linux x86_64 と Windows x86_64 = verified**、
> macOS と arm は **experimental**(実機未検証)。Windows は WCH-LinkUtility が入れる **WCH 純正ドライバのまま動く**
> (**Zadig / WinUSB 置換は不要**。[Windows](#windows-usb-ドライバ) 参照)。

## インストール

### crates.io から

```sh
cargo install ch32rv
```

### ビルド済みバイナリ

[Releases] から各プラットフォーム向けアーカイブを取得し、`ch32rv` を `PATH` に置く。各アーカイブには `.sha256` を同梱。

### Linux: USB 権限(udev)

probe と bootloader(WCH-Link、OEP の probe、ISP と HID の bootloader)への非 root アクセスには udev ルールが要る。配布 tar には `60-ch32rv.rules` を同梱:

```sh
sudo cp 60-ch32rv.rules /etc/udev/rules.d/            # 展開した tar 内から
sudo udevadm control --reload-rules && sudo udevadm trigger
```

`cargo install`(tar 無し)なら、同じルールをバイナリから出力:

```sh
ch32rv doctor --emit-udev | sudo tee /etc/udev/rules.d/60-ch32rv.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

`ch32rv doctor` は列挙・権限・firmware・probe mode を診断し、次の一手を提案する。

### Windows: USB ドライバ

ドライバの入れ替えは不要。`ch32rv` は次のどちらでも動く:

- **WCH 純正ドライバ**(WCH-LinkUtility が入れるもの)— `ch32rv` がそのドライバ越しに直接 probe を叩くので、
  WCH-LinkUtility と共存でき **Zadig 不要**。この経路は Windows x86_64 限定で、WinUSB よりやや遅い。
- **WinUSB** — probe が既に WinUSB デバイスとして見えていれば(クリーン機では自動 install されることが多い)それを使う。
  使えるドライバが全く無い時だけ Zadig を検討。

`ch32rv` は WinUSB を先に試し、ダメなら WCH 純正ドライバへ自動フォールバックする。probe が見つからなければ `ch32rv doctor` を実行。

## クイックスタート

```sh
ch32rv probe list                       # 接続中の debug probe(WCH-Link、OEP)を探す
ch32rv target info                      # target を識別(SKU / family / 配線 / 容量)

ch32rv flash firmware.elf               # 書き込み(format 自動、erase + verify + reset + run)
ch32rv flash app.bin --offset 0x08000000
ch32rv verify firmware.elf              # 書かずに比較(不一致: exit 30)

ch32rv read --range 0x08000000+256 --format hex-dump
ch32rv erase --all
ch32rv reset

ch32rv gdb                              # GDB server を 127.0.0.1:3333 で(HW + flash BP)
ch32rv monitor --source dmdata          # runtime I/O(uart / sdi / dmdata / dmseq / rtt)
ch32rv capabilities                     # この probe + target の組で何ができるか
```

主なグローバルオプション(`ch32rv --help` 参照): probe 選択の `--probe <selector>`、target を固定する
`--chip <SKU|family>`(省略時は自動検出・曖昧なら fail-closed)、機械可読出力の `--json`、破壊的操作の確認を省く
`--yes`、デバイスを開かず計画だけの `--dry-run`。exit code と JSON envelope は `ch32rv-contract` crate が定義する。

## コマンド

| コマンド | 用途 |
|---|---|
| `flash` | erase / verify / reset / confirm-run 方針付きの書き込み(`--preverify`・`--restore-unwritten`・`--repeat`・`--sdi`・`--monitor`) |
| `verify` / `read` / `write` | image と比較 · dump / blank-check · raw メモリ・flash 書き込み |
| `erase` / `reset` | 消去(`--all` / `--region` / `--range`)· reset して run |
| `run` | HIL ランナー: 書込 → reset → runtime 出力を流す(dmdata / rtt、stdin は target へ)→ semihosting の exit code を伝搬 |
| `recover` | 復旧: power-off、NRST、unprotect(読み出し保護部品の mass-erase unbrick) |
| `probe` | probe 管理: `list`、`info`、firmware `info` / `check` / `update`(IAP 経由で probe 自身の firmware を書換)/ `exit-iap`、`mode get` |
| `target` | `info`、構造化 `option` byte(`get` / `set` / `write-raw` / `reset`)、`protect` |
| `dbg` / `gdb` | ワンショット制御(halt / resume / step / regs / reg / dmi)· GDB server |
| `monitor` | runtime I/O: uart / sdi / dmdata / dmseq / rtt(uart / dmdata / dmseq / rtt は stdin を target へ) |
| `db` / `capabilities` | 内蔵デバイス DB の閲覧 · probe×firmware×target の可否マトリクス |
| `doctor` / `version` / `complete` | 環境診断 · バージョン · shell 補完 |
| `arduino` | Arduino IDE 統合(`discovery` / `monitor` Pluggable プロトコル) |

`isp` はチップの factory bootloader(USB または UART)で書く。`isp enter --via probe` は CH32X03x / CH32H417 を debug 線から bootloader へ入れる。`boot` は独自 bootloader(DFU、HID)で書く。`broker` は 1 つの OEP の probe を開いたまま、複数の ch32rv のコマンドに使わせる。

## ライブラリ crate

`ch32rv` は crates.io に publish した crate からなる。どれも CLI の部品で、crate ごとの API や semver の約束は無い:

| Crate | 提供内容 |
|---|---|
| [`ch32rv-contract`](https://crates.io/crates/ch32rv-contract) | exit code、JSON result envelope、NDJSON progress event、operation policy |
| [`ch32rv-usb`](https://crates.io/crates/ch32rv-usb) | USB 列挙、probe selector、デバイス単位 lock、transaction capture(nusb) |
| [`ch32rv-usb-wch-win`](https://crates.io/crates/ch32rv-usb-wch-win) | Windows: WCH 純正ドライバの device を使う(WinUSB 置換なし) |
| [`ch32rv-wchlink`](https://crates.io/crates/ch32rv-wchlink) | WCH-Link USB protocol(bulk protocol + IAP) |
| [`ch32rv-oep`](https://crates.io/crates/ch32rv-oep) | OEP v1 の host: wire の符号、transport、session |
| [`ch32rv-dmi`](https://crates.io/crates/ch32rv-dmi) | RISC-V Debug Module Interface + 直接 FLASH controller アクセス |
| [`ch32rv-target`](https://crates.io/crates/ch32rv-target) | 生成 CH32 device DB: chip 検出、flash geometry、option byte 配置 |
| [`ch32rv-flash`](https://crates.io/crates/ch32rv-flash) | erase / program / verify / confirm-run オーケストレーション |
| [`ch32rv-debug`](https://crates.io/crates/ch32rv-debug) | run 制御、breakpoint、GDB server |
| [`ch32rv-isp`](https://crates.io/crates/ch32rv-isp) | WCH の factory ISP bootloader の client(USB と UART) |
| [`ch32rv-boot`](https://crates.io/crates/ch32rv-boot) | 独自 bootloader の client(DFU、UF2、UART / RS-485、HID) |

## Arduino IDE

Arduino 統合はプロトコルレベル: `ch32rv arduino discovery` と `ch32rv arduino monitor` が Pluggable
Discovery / Monitor プロトコルを実装する。upload 自体は通常の `ch32rv flash`。

## ドキュメント

仕様の索引は [docs/README.ja.md](docs/README.ja.md)(日本語)。
リリース計画は [docs/release-plan.ja.md](docs/release-plan.ja.md)、変更履歴は [CHANGELOG.md](CHANGELOG.md)(英日併記)。

## ソースからビルド

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

Linux ではビルドに `libudev-dev` と `pkg-config` が要る(serial monitor で使用):

```sh
sudo apt-get install -y libudev-dev pkg-config
```

## ライセンス

[MIT](LICENSE)。

[Releases]: https://github.com/ch32-riscv-ug/ch32rv/releases
