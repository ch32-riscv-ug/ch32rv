# ch32rv 要件: 巻き取り対象ツールと吸収マップ

ch32rv が置き換える書き込み周りのツールと、その機能が ch32rv のどのコマンドに当たるか。コマンドの詳細は [cli.ja.md](cli.ja.md)、crate の分け方は [architecture.ja.md](architecture.ja.md)。

## 1. 対象にした版

各ツールの機能は、推測や二次資料ではなく次の一次資料で洗った。

| 対象 | 一次資料 |
|---|---|
| wlink 0.1.2 | release binary の全 subcommand の `--help` とシンボル |
| minichlink | ch32fun の `minichlink/` の全ソース |
| probe-rs 0.32.0 | CLI 定義・wlink driver・target YAML |
| WCH OpenOCD | MounRiver Studio toolchain V240 同梱の binary(`wch-riscv.cfg` と埋め込み文字列の vendor command) |
| wchisp | `src/main.rs` の clap 定義と README |
| WCH-LinkUtility | WCH-Link manual V2.4 の公表機能 |
| Arduino 統合 | ArduinoCore-CH32RV の platform.txt と upload の recipe |

## 2. ゴール

日常の書き込み・検証・復旧・monitor・実行制御・probe 管理・factory ISP・custom bootloader 書き込みを `ch32rv` 一本(+汎用 GDB クライアント)で済ませ、次のツールを要らなくする。

1. probe-rs(CH32 向け flash/run/gdb 運用部分)
2. wlink
3. minichlink(LinkE backend、互換 probe backend、terminal、GDB stub)
4. WCH OpenOCD(GDB debug、vendor command)
5. WCH-LinkUtility(Windows GUI の headless にできる機能)
6. wchisp / WCHISPTool_CMD(factory ISP)
7. wlink-iap(LinkE firmware 更新)
8. dfu-util / UF2 file copy / tinyboot CLI / rv003usb host 側(custom bootloader client)
9. rvprog.py

「巻き取る」の意味は**機能の等価以上 + 契約(JSON・exit code・selector)の上位互換**であり、CLI 文法の互換ではない。

## 3. ツール別吸収マップ

「状態」は CLI の `--help` に当たるコマンドがあるか。**提供** = そのコマンドがある、**一部** = 一部の family・経路だけ、**未提供** = 当たるコマンドが無い。

### 3.1 wlink 0.1.2

| wlink | ch32rv | 状態 | 備考 |
|---|---|---|---|
| `list` | `probe list` | 提供 | serial を canonical ID にする |
| `status` | `probe info` + `target info` | 提供 | probe 型番と firmware 版を machine-readable に |
| `flash -a -e -R --enable-sdi-print --watch-serial` | `flash --at --erase --reset --sdi on --monitor uart` | 提供 | verify は既定で readback(wlink には verify が無い) |
| `erase` | `erase --all` | 提供 | |
| `erase --method power-off\|pin-rst` | `recover --method power-off\|nrst` | 提供 | 特殊消去は復旧系に分ける |
| `dump <addr> <len>` | `read --range` | 提供 | |
| `regs` | `dbg regs` | 提供 | |
| `write-reg` / `write-mem` | `dbg reg write` / `write` | 提供 | |
| `halt` / `resume` / `reset [mode]` | `dbg halt` / `dbg resume` / `reset` | 提供 | |
| `protect` / `unprotect` | `target protect on\|off` | 提供 | |
| `sdi-print enable\|disable` | `monitor sdi on\|off` | 提供 | `monitor --source sdi` |
| `mode-switch --rv\|--dap` | `probe mode set riscv\|dap` | 提供 | |
| `set-power enable3v3` 等 | `probe power 3v3\|5v on\|off` | 提供 | |
| `--device INDEX` | `--probe`(serial/alias/topology/port) | 提供 | 列挙順の index は受けない |
| `--chip` / `--speed` | 同名の global | 提供 | |
| `dev`(隠し) | 対象外 | - | 開発用 |

wlink の契約上の欠陥で、ch32rv が同じ轍を踏まないもの: verify command が無い、probe 選択が index のみ、数値引数のパース失敗が clap エラーでなく panic になる、`--no-detach` の名前と説明が逆。

### 3.2 minichlink

| minichlink | ch32rv | 状態 | 備考 |
|---|---|---|---|
| `-3 -5 -t -f` | `probe power 3v3\|5v on\|off` | 提供 | |
| `-l <serial>` / `MINICHLINK_LINKE_SERIAL` | `--probe` / `CH32RV_PROBE` | 提供 | どの probe でも使える(minichlink は LinkE 限定) |
| `-C linke` | WCH-Link / WCH-LinkE | 提供 | |
| `-C b003boot` | `boot hid flash` | 提供 | |
| `-C esp32s2chfun\|funprog\|nchlink\|ardulink` | 互換 probe backend | 未提供 | ch32rv の WCH-Link 以外の probe は OEP([oep-host.ja.md](oep-host.ja.md)) |
| `-C isp` | `isp` 名前空間 | 提供 | |
| `-w file addr` / `-r file addr size` | `flash` / `write --at` / `read` | 提供 | 領域名は `code`(別名 `flash`)/ `option` / `ram`。`system` / `eeprom` は family 依存なので `--range` で番地を書く |
| `-E` | `erase --all` | 提供 | |
| `-u`(unbrick) | `recover --method unbrick` | 提供 | |
| `-U`(bootloader unlock) | system 領域への書込 | 未提供 | 書き込み先は code flash だけ(cli.ja.md) |
| `-a -A -b -e` | `dbg halt --reset` / `dbg halt` / `reset` / `dbg resume` | 提供 | |
| `-B`(bootloader へ再起動) | `isp enter` | 提供 | `--entry probe` は CH32X033 / X035 のみ |
| `-d -D`(NRST/GPIO) | `target option set reset-pin=off\|12ms\|1ms\|128us` | 一部 | `reset-pin` を持つ family だけ(DB の option field) |
| `-p -P`(読み出し保護) | `target protect off\|on` | 提供 | |
| `-N -n`(debug module 有効/無効) | - | 未提供 | 当たる option のキーが無い |
| `-S flashKB ramKB` | `target option set ram-split=` | 一部 | V407 だけ(DB の option field)。他は `target option write-raw` で生値 |
| `-s reg val` / `-m reg` | `dbg dmi write\|read` | 提供 | expert |
| `-i` | `target info` | 提供 | |
| `-T`(terminal) | `monitor --source dmdata` | 提供 | **SDI print とは別経路**(DMDATA0/1 メールボックス、双方向)。§5-3 |
| `-G`(GDB server) | `gdb` | 提供 | port は `--listen` で変えられる(既定 `127.0.0.1:3333`) |
| `-X ECLK:...` | - | 未提供 | backend 固有の escape hatch |
| `-k`(init skip) | 不要 | - | 復旧系は probe 単独で動く |
| `-Y` `-y`(隠し) | 対象外 / `dbg reg read csr:0x300` | 提供 | |
| TCP 4444 cmdserver | 対象外 | - | gdb + monitor の共存で代わる |

minichlink の構造的欠陥で、設計として再現しないもの: 固定長 buffer と length 未検証(GDB stub の overflow)、`hwbreak+` を申告してソフトブレークしか無い、単文字オプションの順序依存。

### 3.3 probe-rs 0.32.0(CH32 運用に関わる subcommand)

| probe-rs | ch32rv | 状態 | 備考 |
|---|---|---|---|
| `list` / `info` | `probe list` / `probe info` + `target info` | 提供 | |
| `download --binary-format --base-address --verify --preverify --chip-erase --restore-unwritten` | `flash --format --at --verify --preverify --erase chip --restore-unwritten` | 提供 | |
| `verify` | `verify` | 提供 | |
| `erase` | `erase --all` | 提供 | |
| `read` / `write` | `read` / `write` | 提供 | |
| `reset` | `reset` | 提供 | **`--confirm-run`** は probe-rs に無い |
| `run`(RTT/semihosting/exit code) | `run` | 提供 | `--source rtt`、`--exit-on semihosting` で target の exit code を返す |
| `run` の embedded-test runner | - | 未提供 | |
| `attach` | `run --no-flash` | 提供 | |
| `gdb` | `gdb` | 提供 | |
| `dap-server` | - | 未提供 | |
| `chip list\|info` | `db list\|info` | 提供 | verified と provisional を出す |
| `complete` | `complete` | 提供 | |
| `mi meta\|info` | 全 command の `--json` + `version --json` | 提供 | |
| `--probe VID:PID[:SERIAL]` / env | `--probe`(上位互換書式) | 提供 | |
| `--connect-under-reset` / `--speed` / `--non-interactive` / `--dry-run` / `--chip-description-path` | 同等の global(`--db` overlay) | 提供 | |
| `benchmark` / `profile` / `trace` / `itm` / `serve`(remote) / `debug`(REPL) | **巻き取らない**(§4) | - | |

probe-rs は CH32 向けの target と AttachChip による自動検出を持ち、flash 単体では最も近い競合になる。ch32rv の違いは、LinkE 固有の機能(電源・SDI・mode・firmware 更新)、経路の統合(ISP / bootloader)、契約(JSON・exit code)、`--confirm-run`、生成 target DB にある。

### 3.4 WCH OpenOCD(MounRiver Studio 同梱)

| WCH OpenOCD | ch32rv | 状態 | 備考 |
|---|---|---|---|
| GDB server(flash/step/breakpoint) | `gdb` | 提供 | **attach 時に flash を書き換えない**(WCH 版は先頭 48B を nop+ebreak 化して戻さない) |
| `load` | `gdb` の vFlash | 提供 | `--no-flash` で断れる |
| `wlink_erase` / `wlink_code_erase` | `erase` / `recover` | 提供 | |
| `wlink_disabledebug` | - | 未提供 | 当たる option のキーが無い |
| `wlink_flash_protect` | `target protect` | 提供 | |
| `wlink_sdi` | `monitor sdi` | 提供 | |
| `wlink_rstout` / `wlink_chip_reset` | `reset` / `probe power cycle` | 提供 | |
| `wlink_getromram` | `target option get` | 提供 | |
| `wlink_set_index` | `--probe` | 提供 | |
| dual-core config(H41x) | `--core <n>` | 一部 | H417 の core 1 は `dbg`(reset なし)と `read` |

### 3.5 WCH-LinkUtility(manual V2.4 の公表機能)

| LinkUtility | ch32rv | 状態 | 備考 |
|---|---|---|---|
| Erase/Program/Verify/Reset-run | `erase` / `flash` / `verify` / `reset` | 提供 | |
| UID・flash size・保護状態・Link firmware 版の取得 | `target info` / `probe info` | 提供 | |
| 読み出し保護の設定・解除、flash 読み出し | `target protect` / `read` | 提供 | |
| power-off erase / NRST erase | `recover --method power-off\|nrst` | 提供 | |
| 3.3V/5V 出力制御 | `probe power` | 提供 | |
| 自動連続 download(量産) | `flash --repeat` | 提供 | |
| 複数 Link の選択 | `--probe` + `probe list` | 提供 | |
| 二線 debug interface の無効化 | - | 未提供 | |
| user option byte 設定 | `target option get/set` | 提供 | |
| program/system flash の書込先選択(V003/V00X/CH641/M007 等) | - | 未提供 | 書き込み先は code flash だけ |
| SDI printf 有効化 + COM 受信 | `monitor sdi on` + `monitor --source sdi` | 提供 | |
| Link firmware online update | `probe firmware update --image <file>` | 提供 | image は user-supplied |
| 別 LinkE を使った offline recovery update | 対象外(§4) | - | |
| RISC-V/ARM mode 切替 | `probe mode set` | 提供 | |

### 3.6 wchisp(GPL-2.0)/ WCHISPTool_CMD

| wchisp | ch32rv | 状態 | 備考 |
|---|---|---|---|
| `probe` | `isp list` | 提供 | |
| `info` | `isp info` | 提供 | BTVER・UID・保護状態 |
| `flash -E -V -R` | `isp flash --erase --verify --reset` | 提供 | policy の語彙は probe 経路と共通 |
| `verify` | `isp verify` | 提供 | |
| `erase` | `isp erase` | 提供 | |
| `reset` | `isp reset` | 提供 | |
| `eeprom dump\|erase\|write` | - | 未提供 | |
| `config info\|set` | `isp config get\|set` | 提供 | `target option set` と同じ語彙 |
| `config reset\|enable-debug\|disable-debug\|unprotect` | - | 未提供 | 保護の解除は `isp config set read-protect=off` |
| `-u/-s -p -b -d` | `isp --transport usb\|uart --port --baud --device` | 提供 | **ISP device は USB serial を持たない**ので、複数台は USB の位置(topology)で選ぶか断る。UART の framing は先行実装からの転記で、実機ではまだ見ていない |
| (1200bps touch 相当) | `isp --entry touch1200` | 提供 | `--entry auto` / `dfu-detach` / `probe` もある |
| WCHISPTool_CMD の INI workflow | 対象外(§4) | - | |

wchisp は GPL-2.0 のため**コードは取り込まない**。protocol(0xa1 IDENTIFY 〜 0xa8 WRITE_CONFIG、鍵生成、XOR)は minichlink `pgm-wch-isp.c`(MIT)、wch-protocols の `protocols/pc-to-device-isp.ja.md`、実機 capture を仕様として自前で実装する。0xa6 VERIFY は ch32rv にある。

### 3.7 wlink-iap

| wlink-iap | ch32rv | 状態 | 備考 |
|---|---|---|---|
| IAP entry / exit | `probe firmware update` に内蔵 / `probe firmware exit-iap` | 提供 | |
| image 書込 | `probe firmware update --image <file>` | 提供 | firmware blob は同梱しない |
| (無し) | `probe firmware info\|check` | 提供 | **既知不良版の検出**。既知不良は ch32-device-data の `index/link_firmware_issues.csv`(実測で不良の版)を variant・mode・版で照合する。`probe firmware update` の image の型番照合は `index/link_firmware.csv` の sha256 |

注意: LinkE の IAP mode は `4348:55e0` で enumerate され、**WCH factory ISP device と同じ VID:PID**(LinkE の IAP は搭載 CH32V305 の ISP そのもの)。Identify に factory の形で答えないので区別でき、`isp list` は WCH-Link の IAP を分けて出す。IAP に留まった個体は `doctor` が見つけ、`probe firmware exit-iap` が抜けさせる。

### 3.8 custom bootloader client 群

| 既存 | ch32rv | 状態 | 備考 |
|---|---|---|---|
| dfu-util(DFU 1.1) | `boot dfu flash\|info` | 提供 | run-time interface があれば先に DFU_DETACH |
| UF2 file copy(wch-uf2) | - | 未提供 | `flash` / `verify` は UF2 の image を読める |
| tinyboot CLI(UART/RS-485) | - | 未提供 | |
| minichlink `-C b003boot`(rv003usb HID) | `boot hid flash` | 提供 | |
| 1200bps touch / double reset / RAM magic | - | 一部 | ISP 経路の `isp --entry touch1200\|dfu-detach` はある。bootloader 共通の entry コマンドは無い |

### 3.9 rvprog.py

flash/erase/unbrick/NRST option の参考実装。`flash` / `erase` / `recover` / `target option set reset-pin=` が当たる。固有の吸収項目は無い。

## 4. 巻き取らないもの

| 対象外 | 理由 |
|---|---|
| probe-rs の `benchmark` / `profile` / `itm` / `trace` / `serve`(remote probe)/ `debug`(対話 REPL) | ARM/汎用 probe 向け機能または別製品領域。probe-rs 併用で足りる |
| cargo-flash / cargo-embed 互換 shim | Rust 開発 workflow 専用。probe-rs が既に良い |
| WCH-MCU-DL(offline writer)の設定・互換 | ハードウェア製品の管理領域 |
| WCHISPTool_CMD の INI 設定ファイル互換 | Windows GUI 生成物の互換より、自前の contract を優先 |
| LinkE firmware の同梱・自動 download | license。user-supplied image のみ |
| 別 LinkE を probe にした offline recovery update | LinkE を SWD target として書く特殊系 |
| CH5xx(CH57x/58x/59x/CH570 等) | 名前と射程の決定([naming.ja.md](naming.ja.md))。probe/DMI 層は family に依存しない形に保つ |
| GUI | 作らない |

## 5. 設計の前提になった事実

1. **probe-rs 0.32.0 は CH32 の自動検出を持つ**(AttachChip 応答 + mask 変換 + option byte `0x1ffff802` の CODE/RAM split 解決)。ch32rv の target 検出はこれと同等以上が出発点。
2. **probe-rs 0.32.0 の firmware 版チェックにはバグがある**(`v_major != 2 && v_minor < 7` のため major=2 なら素通り)。「2.7 未満拒否」は実質機能していない。ch32rv が版の比較を契約 + test で固定する理由。
3. **minichlink `-T` は SDI print ではなく DMDATA0/1 メールボックス**(ch32fun debugprintf、双方向)。monitor の source は uart / sdi / dmdata を別経路として持つ(他に dmseq / rtt、OEP の fixture-uart)。
4. **SDI の有効化を upload に織り込む手段が他のツールの組み合わせに無い**(upload は probe-rs、SDI enable は wlink で、recipe に入らない)。`flash --sdi on` が直接の解。
5. **ISP device は USB serial を持たない**(`4348:55e0` / `1a86:55e0`)。ISP 経路の複数台識別は topology(bus/port)か「1 台のみ」fail-closed になる。selector の文法に topology を含める理由。
6. **LinkE IAP mode と factory ISP device が同じ VID:PID**。`isp list` / `doctor` に区別が要る(§3.7)。
7. **WCH-Link 系 firmware の hash → 型番・版の対応表がある**(ch32-device-data の `index/link_firmware.csv`)。`probe firmware update` の image 照合の源泉。
8. **target DB の源泉は ch32-device-data**(flash geometry・memory map・option byte・debug 配線・chip ID)。足りないデータは依頼し、納品まで ch32rv 内の暫定 overlay で進める([architecture.ja.md](architecture.ja.md) §3)。
9. **CDC が vendor interface より先に見える attach のレースがある**。probe の open は 1 秒間隔で再試行する(最大 3 回)。
10. **Arduino recipe の制約**: probe selector は空にできる 1 変数に畳む必要がある。`--probe` は IDE の port address(`{upload.port.address}`)をそのまま受ける。`--non-interactive` と進捗の抑止も要る。

## 6. 参照

- [wlink](https://github.com/ch32-rs/wlink)、[wchisp](https://github.com/ch32-rs/wchisp)、[ch32fun/minichlink](https://github.com/cnlohr/ch32fun/tree/master/minichlink)、[probe-rs](https://github.com/probe-rs/probe-rs)、[wlink-iap](https://github.com/cjacker/wlink-iap)
- ch32-device-data の `index/README.md`(consumer contract)
