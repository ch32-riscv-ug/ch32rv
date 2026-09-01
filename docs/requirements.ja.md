# ch32rv 要件: 巻き取り対象ツールの全機能インベントリ

- 調査日: 2026-09-01
- 状態: 提案。原設計案 §1・§4 を、各ツールの一次資料(ソース・バイナリ help・公式 README)で再調査して確定させたもの
- 用途: [cli.ja.md](cli.ja.md) のコマンド体系と [architecture.ja.md](architecture.ja.md) の crate 分割の根拠

## 1. 再調査の方法

「必要とされていそうな書き込み周りの全ツールを最終的に巻き取る」ために、推測や二次資料ではなく次の一次資料で各ツールの表面を洗い直した。

| 対象 | 一次資料 | 方法 |
|---|---|---|
| wlink 0.1.2 | `../tools/wlink/0.1.2` release binary | 全 subcommand の `--help` 収集 + not stripped バイナリのシンボル解析 |
| minichlink | `../ch32fun/minichlink`(HEAD `618bba5`) | `minichlink.c` ほか全ソース読解 |
| probe-rs 0.32.0 | `../probe-rs`(HEAD `2b4a2cd3`) | CLI 定義・wlink driver・target YAML の読解 |
| WCH OpenOCD | `../tools/MRS_Toolchain_Linux_X64_V240` binary | `wch-riscv.cfg` と埋め込み文字列から vendor command 抽出 |
| wchisp | GitHub main(2026-04 時点最終 push) | README + `src/main.rs` の clap 定義 |
| WCH-LinkUtility | WCH-Link manual V2.4(既存調査の要約) | [host app 調査 §2](../../note/research/wch-linke-host-apps.ja.md) |
| Arduino 統合要件 | `../ArduinoCore-CH32`(platform.txt、ADR-0008/0014、upload-and-fixture) | 読解 |
| target DB 源泉 | `../ch32-device-data`、`../ch32-data` | 構造・カバレッジの照合 |
| Rust エコシステム | crates.io API・GitHub(2026-09-01) | 版・保守状況・名前空き確認 |

## 2. ゴール

**最終状態**: 日常の書き込み・検証・復旧・monitor・実行制御・probe 管理・factory ISP・custom bootloader 書き込みが `ch32rv` 一本(+汎用 GDB クライアント)で完結し、下記 9 系統のツールが不要になる。

1. probe-rs(CH32 向け flash/run/gdb 運用部分)
2. wlink
3. minichlink(LinkE backend、互換 probe backend、terminal、GDB stub)
4. WCH OpenOCD(GDB debug、vendor command)
5. WCH-LinkUtility(Windows GUI の全 headless 機能)
6. wchisp / WCHISPTool_CMD(factory ISP)
7. wlink-iap(LinkE firmware 更新)
8. dfu-util / UF2 file copy / tinyboot CLI / rv003usb host 側(custom bootloader client)
9. rvprog.py

「巻き取る」の意味は**機能の等価以上 + 契約(JSON・exit code・selector)の上位互換**であり、CLI 文法の互換ではない(移行表を提供する)。

## 3. ツール別吸収マップ

新コマンドの詳細仕様は [cli.ja.md](cli.ja.md)。優先度 P0/P1/P2 は原設計案と同じ意味(P0=第1版)。

### 3.1 wlink 0.1.2(全 17 subcommand)

| wlink | ch32rv | 備考・優先度 |
|---|---|---|
| `list` | `probe list` | P0。serial を canonical ID に昇格 |
| `status` | `probe info` + `target info` | P0。probe 型番と firmware 版の machine-readable 化 |
| `flash -a -e -R --enable-sdi-print --watch-serial` | `flash --offset --erase --reset --sdi on --monitor uart` | P0。verify 既定 on(wlink には verify が無い) |
| `erase` | `erase --chip` | P0 |
| `erase --method power-off\|pin-rst` | `recover --method power-off\|nrst` | P0。特殊消去は復旧系に分離 |
| `dump <addr> <len>` | `read --range` | P1 |
| `regs` | `dbg regs` | P1 |
| `write-reg` / `write-mem` | `dbg dmi write` / `write` | P1/P2 |
| `halt` / `resume` / `reset [mode]` | `dbg halt` / `dbg resume` / `reset` | P0-P1 |
| `protect` / `unprotect` | `target protect on\|off` | P0 |
| `sdi-print enable\|disable` | `monitor sdi on\|off` | P1。`monitor --source sdi` は自動 enable |
| `mode-switch --rv\|--dap` | `probe mode set riscv\|dap` | P1 |
| `set-power enable3v3` 等 | `probe power 3v3\|5v on\|off` | P0 |
| `--device INDEX` | `--probe`(serial/alias/topology) | P0。index は対話時のみ許可 |
| `--chip` / `--speed` | 同名 global | P0 |
| `dev`(隠し) | 対象外 | 開発用 |

wlink の契約上の欠陥で、ch32rv が同じ轍を踏まないもの: verify command が無い、probe 選択が index のみ、数値引数のパース失敗が clap エラーでなく panic になる、`--no-detach` の名前と説明が逆。

### 3.2 minichlink(全オプション)

| minichlink | ch32rv | 備考・優先度 |
|---|---|---|
| `-3 -5 -t -f` | `probe power 3v3\|5v on\|off` | P0 |
| `-l <serial>` / `MINICHLINK_LINKE_SERIAL` | `--probe` / `CH32RV_PROBE` | P0。全 backend で有効にする(minichlink は LinkE 限定) |
| `-C linke\|esp32s2chfun\|funprog\|nchlink\|b003boot\|ardulink` | 互換 probe backend | P2。capability を実行前に返す |
| `-C isp` | `isp` 名前空間 | P2 |
| `-w file addr` / `-r file addr size` | `flash` / `write --at` / `read` | P0/P1。named region(`flash` `bootloader` `option` `ram` +offset)は `--region code\|system\|option\|eeprom\|ram[+off]` として継承 |
| `-E` | `erase --chip` | P0 |
| `-u`(unbrick) | `recover --method unbrick` | P0。電源サイクル+DM 連打+option 工場値+全消去 |
| `-U`(bootloader unlock) | `flash --region system` に内蔵 | P1。unlock 手順(FLASH_BOOT_MODEKEYR)は自動化 |
| `-a -A -b -e` | `dbg halt --reset` / `dbg halt` / `reset` / `dbg resume` | P1 |
| `-B`(bootloader へ再起動) | `boot enter` / `isp enter` | P2 |
| `-d -D`(NRST/GPIO) | `target option set nrst=nrst\|gpio` | P1 |
| `-p -P`(読み出し保護) | `target protect off\|on` | P0 |
| `-N -n`(debug module 有効/無効) | `target option set debug=on\|off` | P1。二段階確認を継承 |
| `-S flashKB ramKB` | `target option set split=<code>/<ram>` | P1。許可組合せは DB 由来 |
| `-s reg val` / `-m reg` | `dbg dmi write\|read` | P2(expert) |
| `-i` | `target info` | P0 |
| `-T`(terminal) | `monitor --source dmdata` | P1。**SDI print とは別経路**(DMDATA0/1 メールボックス、双方向)。§5(4) 参照 |
| `-G`(GDB server) | `gdb` | P1。port 固定 3333→`--listen` で可変 |
| `-X ECLK:...` | `probe vendor`(隠し) | P2。backend 固有 escape hatch |
| `-k`(init skip) | 不要(復旧系が probe 単独で動く設計) | - |
| `-Y` `-y`(隠し) | 対象外 / `dbg reg read csr:0x300` | - |
| TCP 4444 cmdserver | 対象外(gdb + monitor の共存で代替) | - |

minichlink の構造的欠陥で、設計として再現しないもの: 固定長 buffer と length 未検証(GDB stub の overflow)、`hwbreak+` を申告してソフトブレークしか無い、単文字オプションの順序依存。

### 3.3 probe-rs 0.32.0(CLI 21 subcommand 中、CH32 運用に関わるもの)

| probe-rs | ch32rv | 備考・優先度 |
|---|---|---|
| `list` / `info` | `probe list` / `probe info` + `target info` | P0 |
| `download --binary-format --base-address --skip --verify --preverify --chip-erase --restore-unwritten` | `flash --format --offset --verify --preverify --erase chip --restore-unwritten` | P0(restore-unwritten は P2) |
| `verify` | `verify` | P0 |
| `erase` | `erase --chip` | P0 |
| `read` / `write` | `read` / `write` | P1 |
| `reset` | `reset` | P0。**`--confirm-run` を追加**(probe-rs に無い最重要差分) |
| `run`(RTT/semihosting/exit code/embedded-test) | `run` | P1(RTT/exit code)。embedded-test runner は P2 以降の検討 |
| `attach` | `run --no-flash` | P1 |
| `gdb` | `gdb` | P1 |
| `dap-server` | `dap` | P2(probe-rs 委譲の選択肢を残す) |
| `chip list\|info` | `db list\|info` | P1。verified 区別と provenance を追加 |
| `complete` | `complete` | P2 |
| `mi meta\|info` | 全 command の `--json` + `version --json` | P0 |
| `--probe VID:PID[:SERIAL]` / env | `--probe`(上位互換書式) | P0 |
| `--connect-under-reset` / `--speed` / `--non-interactive` / `--dry-run` / `--chip-description-path` | 同等 global(`--db` overlay 含む) | P0-P1 |
| `benchmark` / `profile` / `trace` / `itm` / `serve`(remote) / `debug`(REPL) | **巻き取らない**(§4) | - |

probe-rs 0.32.0 は ch32-metapac 生成 target と AttachChip 自動検出で CH32 対応が大幅に強化されており、**flash 単体の競合としては最有力**。ch32rv の差別化は「LinkE 固有機能 + 経路統合(ISP/bootloader)+ 契約 + confirm-run + target DB の gap(7 family)」にあることを常に説明できる状態を保つ(原設計案 §11)。

### 3.4 WCH OpenOCD(MRS 2.40 同梱)

| WCH OpenOCD | ch32rv | 備考・優先度 |
|---|---|---|
| GDB server(flash/step/breakpoint) | `gdb` | P1。**attach 時に flash を書き換えない**ことを契約にする(WCH 版は先頭 48B を nop+ebreak 化して戻さない) |
| `load` 約 1 KB/s | `gdb` の vFlash | P1。速度は明確に上を狙う |
| `wlink_erase` / `wlink_code_erase` | `erase` / `recover` | P0 |
| `wlink_disabledebug` | `target option set debug=off` | P1 |
| `wlink_flash_protect` | `target protect` | P0 |
| `wlink_sdi` | `monitor sdi` | P1 |
| `wlink_rstout` / `wlink_chip_reset` | `reset` / `probe power cycle` | P0 |
| `wlink_getromram` | `target option get`(split) | P1 |
| `wlink_set_index` | `--probe` | P0 |
| dual-core config(H41x) | `--core <n>`(dbg/gdb/reset) | P1 |

### 3.5 WCH-LinkUtility(manual V2.4 の全公表機能)

| LinkUtility | ch32rv | 備考・優先度 |
|---|---|---|
| Erase/Program/Verify/Reset-run | `erase` / `flash` / `verify` / `reset` | P0 |
| UID・flash size・保護状態・Link firmware 版の取得 | `target info` / `probe info` | P0 |
| 読み出し保護の設定・解除、flash 読み出し | `target protect` / `read` | P0/P1 |
| power-off erase / NRST erase | `recover --method power-off\|nrst` | P0 |
| 3.3V/5V 出力制御 | `probe power` | P0 |
| 自動連続 download(量産) | `flash --repeat` | P2 |
| 複数 Link の選択 | `--probe` + `probe list` | P0 |
| 二線 debug interface の無効化 | `target option set debug=off` | P1 |
| user option byte 設定 | `target option set/get` | P1 |
| program/system flash の書込先選択(V003/V00X/CH641/M007 等) | `flash --region code\|system` | P1 |
| SDI printf 有効化 + COM 受信 | `monitor sdi on` + `monitor --source sdi` | P1 |
| Link firmware online update | `probe firmware update --image <file>`(image は user-supplied) | P1 |
| 別 LinkE を使った offline recovery update | 対象外(§4)。手順文書のみ | - |
| RISC-V/ARM mode 切替 | `probe mode set` | P1 |

### 3.6 wchisp(main、GPL-2.0)/ WCHISPTool_CMD

| wchisp | ch32rv | 備考・優先度 |
|---|---|---|
| `probe` | `isp list` | P2 |
| `info` | `isp info` | P2。BTVER・UID・保護状態 |
| `flash -E -V -R` | `isp flash --erase --verify --reset` | P2。policy 語彙は probe 経路と共通 |
| `verify` | `isp verify` | P2 |
| `erase` | `isp erase` | P2 |
| `reset` | `isp reset` | P2 |
| `eeprom dump\|erase\|write` | `isp eeprom read\|erase\|write` | P2 |
| `config info\|set\|reset\|enable-debug\|disable-debug\|unprotect` | `isp config get\|set\|reset`(debug/protect は構造化 key) | P2 |
| `-u/-s -p -b -d` | `isp --transport usb\|uart --port --baud --device` | P2。**ISP device は USB serial を持たない**ため fail-closed + topology selector 必須 |
| (1200bps touch 相当) | `isp enter --via touch1200` | P2。X03x/X315/H417 |
| WCHISPTool_CMD の INI workflow | 対象外(§4) | - |

wchisp は GPL-2.0 のため**コードは取り込まない**。protocol(0xa1 IDENTIFY 〜 0xa8 WRITE_CONFIG、鍵生成、56B chunk XOR)は minichlink `pgm-wch-isp.c`(MIT)と実機 capture を仕様として自前実装する。minichlink 側の未実装(0xa6 VERIFY、DATA_*、OTP、UART transport)は ch32rv で埋める。

### 3.7 wlink-iap

| wlink-iap | ch32rv | 備考・優先度 |
|---|---|---|
| IAP entry / exit | `probe firmware update` に内蔵 | P1 |
| image 書込 | `probe firmware update --image <file>` | P1。firmware blob は同梱しない |
| (無し) | `probe firmware info\|check` | P0。**既知不良版(2.11 の reset 問題)の検出**。hash 照合は `ch32-device-data/evidence/link_firmware.csv` を源泉にできる |

注意: LinkE の IAP mode は `4348:55e0` で enumerate され、**WCH factory ISP device と同じ VID:PID**(LinkE の IAP は搭載 CH32V305 の ISP そのもの)。`isp list` は BTVER/chip 種別で区別し、LinkE と判定したら `probe firmware` へ誘導する。IAP に滞留した個体の検出・脱出(`0x83`)も `probe firmware` / `doctor` が持つ。

### 3.8 custom bootloader client 群(P2)

| 既存 | ch32rv | 備考 |
|---|---|---|
| dfu-util(Swindle BL、PlumBL 等) | `boot dfu flash\|info` | alt setting・address 指定 |
| UF2 file copy(wch-uf2) | `boot uf2 flash` | volume 検出→変換→copy→完了監視。Arduino Upload ボタン統合の要 |
| tinyboot CLI(UART/RS-485) | `boot uart flash\|info` | `--node` で multi-drop |
| minichlink `-C b003boot`(rv003usb HID) | `boot hid flash` | magic packet での BL 突入含む |
| 1200bps touch / double reset / RAM magic | `boot enter --method ...` | 経路共通の entry 統合 |

### 3.9 rvprog.py

flash/erase/unbrick/NRST option の参考実装。`flash` / `erase` / `recover` / `target option set nrst=` で全機能を包含する。固有の吸収項目なし。

## 4. 巻き取らないもの

原設計案 §2(non-goals)に加えて、次を明示的に対象外とする。

| 対象外 | 理由 |
|---|---|
| probe-rs の `benchmark` / `profile` / `itm` / `trace` / `serve`(remote probe)/ `debug`(対話 REPL) | ARM/汎用 probe 向け機能または別製品領域。probe-rs 併用で足りる。REPL は将来 `dbg repl` として追加可能な位置だけ確保 |
| cargo-flash / cargo-embed 互換 shim | Rust 開発 workflow 専用。probe-rs が既に良い |
| WCH-MCU-DL(offline writer)の設定・互換 | ハードウェア製品の管理領域 |
| WCHISPTool_CMD の INI 設定ファイル互換 | Windows GUI 生成物の互換より、自前の contract を優先 |
| LinkE firmware の同梱・自動 download | license。user-supplied image のみ |
| 別 LinkE を probe にした offline recovery update | LinkE を SWD target として書く特殊系。手順文書に留める |
| CH5xx(CH57x/58x/59x/CH570 等) | 名前と射程の決定(naming.ja.md)。ただし probe/DMI 層は family 非依存に保ち、将来の追加でコマンド体系が壊れない形にする |
| GUI | 作らないが、crate 分割で別プロジェクトから作れることを要件にする(architecture.ja.md) |

## 5. 原設計案からの差分(再調査で判明した新事実)

1. **target gap は 5 family ではなく 7 family**。boards.txt の `[compile only]` 実態は V205/V407/V467/X315/M030 に加えて **CH32M103・CH32X305**。probe-rs 0.32.0 の対応は 8 系統(F1/H4/L1/V003+641/V00X/V1/V2+V3/X0+643)。
2. **probe-rs 0.32.0 が CH32 自動検出を実装済み**(AttachChip 応答 + mask 変換 + option byte `0x1ffff802` の CODE/RAM split 解決)。ch32rv の target 検出は最低でも同等が出発点。
3. **probe-rs の firmware 版チェックにはバグがある**(`v_major != 2 && v_minor < 7` のため major=2 なら素通り)。「2.7 未満拒否」は実質機能していない。版比較を契約 + test で固定する根拠。
4. **minichlink `-T` は SDI print ではなく DMDATA0/1 メールボックス**(ch32fun debugprintf、双方向)。monitor の source は uart/sdi/rtt に **dmdata を加えた 4 経路**が正しい。
5. **SDI 有効化を upload に織り込む手段が既存構成に無い**(upload は probe-rs、SDI enable は wlink で、recipe に入らない)。`flash --sdi on` が直接の解になる。
6. **ISP device は USB serial を持たない**(`4348:55e0`)。ISP 経路の複数台識別は topology(bus/port)か「1台のみ」fail-closed になる。probe 経路と同じ selector 文法に topology を含める理由。
7. **LinkE IAP mode と factory ISP device が同じ VID:PID**。`isp list` / `doctor` に区別ロジックが要る(§3.7)。
8. **LinkE firmware の hash→版対応表が手元にある**(`ch32-device-data/evidence/link_firmware.csv`)。既知不良版検出(P0)の源泉。
9. **target DB の源泉分担が確定**: flash geometry・memory map・option 分割・DM レジスタ番地は ch32-device-data(stable 表 + provenance)、**chip ID(device_id)値は ch32-data のみが持つ**が、ちょうど gap の 7 series が欠落 → 実機実測での evidence 表新設が M2 の必須作業。作成は ch32-device-data 側へ依頼し、納品まで ch32rv 内の暫定 overlay で進める(方針は architecture.ja.md §3。実測手順は `ch32-data/docs/device-ids.md` に既存)。
10. **CDC が vendor interface より先に見える attach レース**が実測されている(1 秒間隔 3 回の再試行が必要)。probe open の retry を契約に含める。
11. **Arduino recipe の制約**: `{serial.port}` を使わず、probe selector は「空にできる 1 変数」に畳む必要がある。`--non-interactive` と進捗抑止も必須。現行 platform.txt の probe-rs 呼び出しがそのまま移行先の形を規定する。
12. **エコシステムの現状**(architecture.ja.md §1 の根拠): nusb 0.2.7(活発、hotplug、WinUSB/IOKit/usbfs/WebUSB)、gdbstub 0.7.10 + gdbstub_arch 0.3.3(RISC-V は整数レジスタのみ → V4F の FPU レジスタは自前 Arch 定義)、serialport 4.10.0、cargo-dist は上流(axodotdev)復活済みで 0.32.0、ihex crate は 2020 年から更新なし。
13. **crates.io の `ch32rv` / `wch-link` / `ch32rv-cli` は空き**(2026-09-01 API 確認)。2026 年に入って新規の競合 OSS は確認できず、動きは probe-rs/wlink/wchisp の強化が中心。

## 6. 参照

- [原設計案](../../note/research/new-programming-tool-design.ja.md)
- [WCH-LinkE host application 調査](../../note/research/wch-linke-host-apps.ja.md)
- [全経路調査](../../note/research/programming-tools-and-probes.ja.md)
- [probe・USB 経路調査](../../note/research/programming-probes-and-usb-paths.ja.md)
- `../../ArduinoCore-CH32/docs/upload-and-fixture.ja.md`、`docs/adr/0008`、`docs/adr/0014`
- `../../ch32-device-data/index/README.md`(consumer contract)、`../../ch32-data/docs/device-ids.md`
- [wlink](https://github.com/ch32-rs/wlink)、[wchisp](https://github.com/ch32-rs/wchisp)、[ch32fun/minichlink](https://github.com/cnlohr/ch32fun/tree/master/minichlink)、[probe-rs](https://github.com/probe-rs/probe-rs)、[wlink-iap](https://github.com/cjacker/wlink-iap)
