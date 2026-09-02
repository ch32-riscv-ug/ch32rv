# ch32rv CLI 仕様: コマンド体系

- 作成日: 2026-09-01
- 状態: 提案。**全機能を最終的に実装する前提の完成形**を先に固定する。優先度(P0/P1/P2)は実装順であって体系の一部ではない
- 根拠: [requirements.ja.md](requirements.ja.md) の吸収マップ

## 1. 設計原則

1. **経路(route)を隠さない**。target への到達経路は debug probe / factory ISP / custom bootloader の 3 つで、可能な操作・device 発見方法・失敗モードが根本的に違う。probe 経路を top-level 動詞とし、他は `isp` / `boot` 名前空間にする。同じ `flash` 動詞を経路間で overload しない(capability 差がフラグ仕様に漏れて契約が壊れるため)。
2. **noun-verb 階層 + 高頻度動詞のみ top-level**。日常の書き込みループ(flash/verify/erase/reset/run/monitor)と緊急動詞(recover/doctor)だけを最上位に置く。
3. **fail-closed**。probe が一意に解決されない、target が曖昧、DB に無い、capability が無い——いずれも実行せず、候補と根拠を出して固有の exit code で止まる。「最初に見つかった 1 台」に暗黙 fallback しない。
4. **全 command に `--json`**。進捗・再試行・警告は NDJSON の別 stream。text 出力を機械に解析させない。
5. **破壊操作は明示**。erase 範囲・unprotect(全消去を伴う)・firmware update・mode 切替は確認プロンプトを持ち、`--yes` で省略できる。`--non-interactive` 時は `--yes` が無ければ拒否。
6. **追加はするが変更しない**。command・flag・JSON field・exit code は追加のみ(§7 互換性ポリシー)。

## 2. コマンドツリー(完成形)

```text
ch32rv
├─ flash <file>                書き込み(erase/verify/reset/confirm-run policy)     P0  [probe-rs download, wlink flash, LinkUtility]
├─ verify <file>               照合のみ                                            P0  [probe-rs verify]
├─ read                        メモリ/flash 読み出し・dump・blank check            P1  [wlink dump, minichlink -r, LinkUtility]
├─ write                       生メモリ/領域書き込み(上級)                        P1  [minichlink -w, wlink write-mem]
├─ erase                       消去(chip/region/range)                            P0  [各 tool]
├─ reset                       リセット(run/halt/dm) + --confirm-run              P0  [wlink reset]
├─ run <elf>                   書き込み+実行+出力監視+exit code 伝搬               P1  [probe-rs run/attach]
├─ recover                     復旧(power-off/nrst/unprotect/unbrick)             P0  [wlink erase --method, minichlink -u, LinkUtility]
│
├─ probe                       probe 本体の管理
│  ├─ list                     一覧(型番/FW版/mode/serial/使用中/driver)          P0  [wlink list, probe-rs list]
│  ├─ info                     詳細 + 既知不良 FW 判定 + IAP 滞留検出              P0  [wlink status]
│  ├─ power <3v3|5v> <on|off>  電源出力                                            P0  [wlink set-power, minichlink -3/-5/-t/-f]
│  ├─ power cycle              電源再投入                                          P0
│  ├─ mode <get|set>           RISC-V / DAP 切替                                   P1  [wlink mode-switch]
│  ├─ firmware <info|check|update>  版の解釈・不良版検出・IAP 書込                 P0/P0/P1  [wlink-iap, LinkUtility]
│  └─ vendor <hex>             backend 固有 escape(隠し)                          P2  [minichlink -X]
│
├─ target                      target の識別と不揮発設定
│  ├─ info                     chip ID/SKU 候補/UID/容量/保護/option 要約          P0  [minichlink -i, LinkUtility]
│  ├─ option <get|set|reset|write-raw>  構造化 option bytes                        P1  [minichlink -d/-D/-S/-N/-n, LinkUtility]
│  └─ protect <on|off>         読み出し保護                                        P0  [wlink protect/unprotect, minichlink -p/-P]
│
├─ dbg                         実行制御ワンショット
│  ├─ halt [--reset] / resume / step [N]                                           P1  [wlink halt/resume, minichlink -a/-A/-e]
│  ├─ regs / reg <read|write> <name>                                               P1  [wlink regs]
│  └─ dmi <read|write>         DM レジスタ直接操作(expert)                        P2  [minichlink -s/-m, wlink write-reg]
│
├─ monitor [--source uart|sdi|dmdata|rtt]   実行時 I/O                             P0(uart)/P1(sdi,dmdata)/P2(rtt)
│  ├─ list                     monitor 候補 port の列挙                            P1
│  └─ sdi <on|off>             SDI print の有効/無効                               P1  [wlink sdi-print]
│
├─ gdb                         GDB server(attach 時 flash 非改変)                P1  [WCH OpenOCD, minichlink -G, probe-rs gdb]
├─ dap                         DAP server                                          P2  [probe-rs dap-server]
│
├─ isp                         factory ISP 経路(USB/UART)                        P2  [wchisp, WCHISPTool_CMD]
│  ├─ list / info / enter / reset
│  ├─ flash <file> / verify <file> / erase
│  ├─ eeprom <read|write|erase>
│  └─ config <get|set|reset>
│
├─ boot                        custom bootloader 経路                              P2  [dfu-util, UF2 copy, tinyboot, rv003usb]
│  ├─ enter [--method touch1200|double-reset|magic|pin]
│  ├─ dfu <flash|info>
│  ├─ uf2 flash <file>
│  ├─ uart <flash|info> [--node <id>]
│  └─ hid flash <file>
│
├─ db                          target DB の閲覧
│  ├─ list                     SKU 一覧(verified 区別)                           P1  [probe-rs chip list]
│  └─ info <sku>               geometry/領域/option layout/provenance              P1  [probe-rs chip info]
│
├─ capabilities                probe×FW×target×operation の可否 matrix             P0
├─ doctor                      環境診断と次の一手(--emit-udev)                   P0
├─ version                     tool/contract/DB/stub の版(--json)                 P0
├─ complete <shell>            shell 補完                                          P2
│
└─ arduino                     Arduino IDE 統合(machine 向け)
   ├─ discovery                Pluggable Discovery(stdio JSON)                   P1
   └─ monitor                  Pluggable Monitor(stdio JSON)                     P1
```

## 3. 共通契約

### 3.1 グローバルオプション

| flag | 値 | 既定 | 説明 |
|---|---|---|---|
| `--probe <selector>` | §3.4 | (一意なら自動) | probe の選択。複数一致は exit 14 |
| `--chip <SKU\|family>` | 例 `CH32V203C8T6` | 自動検出 | 検出と矛盾したら exit 23(fail-closed)。**実装済(2026-09-02)**: `Session::attach` が chip_id と `--chip` 名を DB family へ解決し、要求名が DB にあり検出 family と不一致なら `target-ambiguous`(23)。SKU/family/series/型番 prefix 一致は通過、DB 外の未知名は検証不能で受理 |
| `--core <n>` | 0.. | 0 | dual-core(H41x)の core 選択 |
| `--speed <low\|medium\|high\|kHz>` | | high | kHz 指定は近い段階に丸めて warn |
| `--connect-under-reset` | | off | NRST を assert して attach |
| `--json` | | off | 結果を JSON で stdout へ(§3.5) |
| `--progress <bar\|ndjson\|none>` | | bar(tty)/none | 進捗の出力形式。ndjson は stderr へ |
| `--non-interactive` | | off | 対話プロンプトを全て拒否に変える |
| `--yes` | | off | 破壊操作の確認を省略 |
| `--lock-timeout <s>` | | 10 | device lock の待ち時間(§3.7) |
| `--timeout <s>` | | 操作ごと | transport timeout の上書き |
| `--db <path>` | | 内蔵 | target DB の overlay(新 SKU の試行用) |
| `--log-file <path>` | | - | 詳細 log の保存 |
| `--capture <path>` | | - | USB/serial transaction の記録(replay fixture 用)P1 |
| `--dry-run` | | off | device を開かず計画のみ表示 P2 |
| `-v` / `-q` | 重ね掛け | | 冗長度 |

### 3.2 環境変数

| 変数 | 対応 flag |
|---|---|
| `CH32RV_PROBE` | `--probe` |
| `CH32RV_CHIP` | `--chip` |
| `CH32RV_DB` | `--db` |
| `CH32RV_NON_INTERACTIVE` | `--non-interactive` |

flag > 環境変数 > 設定ファイル > 既定値。

### 3.3 設定ファイル

`./ch32rv.toml`(プロジェクト)→ `~/.config/ch32rv/config.toml`(ユーザ)の順で探索。probe の別名(HIL fixture の `CH32_PROBE_<名前>` 慣行の一般化)と既定値のみを持つ。挙動を変える隠し設定は置かない。

```toml
[probes]
bench-01 = "1a86:8010:434A124C5596"
bench-02 = "usb:3-1.4.2"

[defaults]
chip = "CH32V203C8T6"
```

### 3.4 probe selector 書式

```text
--probe <selector>
  VID:PID[:SERIAL]      canonical(probe-rs 互換)。例 1a86:8010:434A124C5596
  serial:<sn>           serial だけで指定
  name:<alias>          設定ファイルの別名
  usb:<bus>-<ports>     USB topology(固定 hub の物理 port。HIL lane 用)
  index:<n>             列挙順(非推奨)。--non-interactive では拒否
```

- serial を持たない device(ISP mode 等)は `VID:PID:` (空 serial)と topology で選ぶ。
- 解決結果が 0 台なら exit 10、2 台以上なら exit 14 で候補一覧を出す。
- `probe list --json` が selector に使える全 key を出力する。

### 3.5 出力契約

- **stdout**: 結果。`--json` 時は単一の JSON object のみ。human 出力時も結果のみ。
- **stderr**: log・進捗・警告。`--progress ndjson` 時は 1 行 1 event の NDJSON。
- JSON には必ず `contract`(契約版)と `ok` を含む。schema は `docs/contract/` に置き、CLI の版とは独立に versioning する。

```json
{"contract":"1","ok":true,"cmd":"flash",
 "probe":{"model":"WCH-LinkE","firmware":{"raw":"020c","norm":"2.12","wch":"v32"},"serial":"434A124C5596"},
 "target":{"sku":"CH32V203C8T6","chip_id":"0x30330504","verified":true},
 "flash":{"written":16700,"erase":"sector","verify":"readback","retries":1},
 "run":{"confirmed":true,"pc":"0x08000156"}}
```

NDJSON event(stderr)の例。**再試行は必ず event として可視化する**(「16.7 KB で固まる」問題の運用要件):

```json
{"ev":"phase","name":"erase","total":12}
{"ev":"progress","phase":"program","done":8192,"total":16700}
{"ev":"retry","phase":"program","attempt":2,"cause":"transport-timeout"}
{"ev":"warn","code":"fw-known-bad","msg":"LinkE firmware 2.11 has a known reset defect"}
```

### 3.6 exit code

| code | 意味 |
|---|---|
| 0 | 成功 |
| 2 | 引数・使い方(clap) |
| 10 | 入口 device が見つからない(probe / ISP device / DFU / port) |
| 11 | device を開けない(権限、driver binding) |
| 12 | device firmware が要求を満たさない(既知不良版を含む) |
| 13 | device 使用中(lock 取得失敗) |
| 14 | device が一意に解決されない(fail-closed) |
| 20 | target を特定できない(応答なし / DB に無い。両者は JSON で区別) |
| 21 | target が protected(明示 unprotect が必要) |
| 22 | attach 失敗(配線、電源、BOOT) |
| 23 | target 曖昧(複数候補 / `--chip` と検出の矛盾) |
| 24 | capability 不足(probe×FW×target×operation で不可) |
| 30 | verify 不一致 / blank check 失敗 |
| 40 | transport timeout・転送中断 |
| 41 | probe が固まっており USB 再接続が必要(検出時) |
| 50 | 書けたが target が走っていない(`--confirm-run` 失敗) |
| 70 | 内部エラー(bug。report 情報を出す) |

10-14 は「経路の入口 device」、20-24 は「target」、30-41 は「転送・検証」、50 は「実行確認」。将来の追加は各帯の空き番号のみ使う。

### 3.7 排他制御と再試行

- **lock**: USB serial(無ければ topology)単位の advisory lock を OS の runtime dir に置く。`--lock-timeout` 待って取れなければ exit 13。異常終了した保持者の stale lock は起動時に回収する。
- **open 再試行**: 挿抜直後は CDC interface が vendor interface より先に見える(実測)。open 失敗は 1 秒間隔で計 3 回まで再試行してから exit する。
- **転送再試行**: chunk 単位の timeout→再試行(既定 3 回)。再試行が起きた事実は NDJSON event と JSON 結果(`retries`)に必ず出す。
- **固まり検出**: 再試行が尽きて probe が応答しなくなったら、`USBDEVFS_RESET` は使わず、再接続手順(usbipd / 物理挿抜)を提示して exit 41。

## 4. コマンド詳細

### 4.1 書き込み系(top-level、probe 経路)

#### flash

```text
ch32rv flash <FILE>
  --format auto|elf|hex|bin|uf2      既定 auto(magic 判別)
  --offset <addr>                    bin 用ロード先(既定: code 領域先頭)
  --region code|system               書込先領域(既定 code)。system は対応 family のみ、unlock 手順込み
  --erase auto|sector|chip|none      既定 auto。chip=全 chip 消去(1発・高速)。sector=image が覆う page のみ消去(image 外を消さない)。auto=flash 先頭開始の image は chip・部分/offset image は sector。none=消去しない
  --verify readback|crc|none         既定 readback。crc は capability 依存。none は明示選択
  --preverify                        既に一致していれば erase/program を省略(flash 摩耗回避)
  --reset run|halt|none              既定 run
  --confirm-run[=status|pc]          reset run 後の走行確認(既定 pc)。失敗で exit 50
  --sdi on|off                       書込後に SDI print を設定(capability 判定込み)
  --monitor uart|sdi|dmdata|rtt      書込後そのまま monitor へ移行
  --restore-unwritten                sector 消去で image が埋めない byte を保存(page 単位 erase 必須)
  --repeat                           target の再接続を検知して連続書込(量産)P2
```

- ELF/ihex はセグメントの物理アドレスを使い、`--offset` は無視して warn(bin のみ有効)。セグメントが DB 上の書込可能領域に収まらなければ実行前に exit 2。
- `--confirm-run=status` は DM の running 状態のみ確認。`=pc` はさらに瞬間 halt→dpc 読取→resume で PC を採取し、flash 領域内かを判定する。SRAM 先頭(`0x2000_0018` 型)で止まっていれば「BOOT ピン疑い」を hint に出す。
- 対応 SKU でも **DB 上 verified でない場合は warn を出して続行**する(「実装済み」と「実機確認済み」の区別)。
- **`--erase` のモード別挙動(2026-09-01 修正)**: `chip` は WCH-Link stub の全 chip 消去(`erase_flash`。1発で速い)。`sector` は **image が実際に覆う flash page だけ**を §4.2.1 の直接 FLASH controller(`flash_page_erase`)で消してから stub で program する — image 外の flash(高位の bootloader・校正データ等)を消さない。`auto` は **image の最小番地が flash 先頭(`code_flash_start`)なら chip、そうでなければ(部分/offset 書込)sector** を選ぶ。`none` は消去なし。**選ばれた scope は必ず出力する**(通常出力の `erase:` 行 / JSON の `erase` フィールド)ので auto の挙動が不透明にならない。**修正の背景**: 修正前は `sector`/`auto` とも無条件で全 chip 消去していた(=部分 image を焼くと chip 全体が飛ぶ silent な data-loss footgun)。auto を「全 image=chip・部分=sector」に賢くすることで footgun を潰しつつ、フル書込(=blink 等の常道)の速度も維持する。**sector が全 image 既定でない理由(実測)**: page 単位消去は ~100ms/page(直接 FLASH controller の DMI 往復)で、chip 消去 0.15s に対し 32KB(128 page)= 12.8s、64KB= ~25s、V307 の 288KB なら数分。フル書込を毎回 sector にすると Arduino ビルドループ等が致命的に遅くなるため、フル image は chip を選ぶ。sector は検証済み FLASH-controller profile を持つ family のみ(無ければ fail-closed。`--erase chip` を案内)。同一 page を共有するセグメントは 1 回に畳んで program 前に一括消去。実機検証(L103): フル image + `--erase auto` → `erase: chip`(0.78s で 64KB 書込+program)、`flash <256B> --offset 0x0800FF00 --erase auto/sector` → `erase: sector` で 0x08000000 の firmware・未対象域を無傷のまま対象 page だけ書換え(page 計算は単体テスト `covered_pages`)。program 後に stub が page-erase 済み(chip-erase でない)flash へ書けることも実機確認済み。
- **`--restore-unwritten`(2026-09-01 実装)**: sector 消去は覆う page 全体を消すため、image が page の一部しか埋めない場合その page の残りは blank になる。このフラグを付けると、**消去前に対象 page を read → image を上書き合成 → page 全体を program** することで、image が触れない byte の元値を保つ。**page 単位 erase 必須**なので `--erase chip`/`none` と併用は fail-closed(usage error)、`--erase auto` は自動で sector に倒す。**family 制限**: 消去済みセルが debug read で本来の `0xff` を返す family(profile の `erased_reads_ff`=Buffered/V103 系)のみ。V20x/V30x は placeholder `0xe339e339` を返し blank と実データを区別できず placeholder を焼き込むため fail-closed(capability-unsupported、消去前に判定=非破壊)。実機検証(L103): page に pattern を置き先頭16Bだけ 0xAA を `--restore-unwritten` で書くと 16–255B目の pattern が保存、verify OK。V307 では消去せずに拒否・firmware 無傷を確認。
- **`--preverify`(2026-09-01 実装)**: 破壊操作の前に image 領域を read して現在値と比較し、**全一致なら erase/program をまるごと省略**(flash cycle と摩耗を節約)。一致時は `preverify: target already matches - skipped`(JSON `skipped:true`・`erase:"none"`・`verify:true`)を出し、**reset 方針だけは適用**(reset run 済みの target をちゃんと走らせる)。不一致なら link 状態を戻して通常の flash に流れる。reset+finish の末尾は `reset_and_finish` に切り出して通常経路と skip 経路で共有。実機検証(L103): 同一 image を再 flash → skip、別 image → 通常書込に流れて反映を確認。
- **`--sdi` / `--monitor` / `--repeat`(2026-09-01 実装)**: いずれも scaffold だったものを実装。**`--sdi on|off`**: 書込+reset 後に probe の SDI print forwarding を設定(`set_sdi_print_enabled`)。programming は成功済みなので失敗しても flash は失敗させず warning 止まり。**`--monitor uart|sdi|dmdata|rtt`**: 書込+reset 後にそのまま monitor session へ移行(Ctrl-C まで、`--timeout` でバウンド可)。実装は reset+結果出力の末尾を `finish_flash`(session を値で受けて drop→USB 解放してから monitor が probe を開き直す)に集約し、通常経路・preverify skip 経路の両方から `--sdi`/`--monitor` が効く。**`--repeat`**: `flash` を dispatcher にして `flash_once` をループ。1 台焼く→operator が外す(AttachChip 失敗を poll)→次を挿す(AttachChip 成功を poll)→焼く、を Ctrl-C まで。失敗 board は報告して次へ。実機検証(L103): `--sdi on` が program+設定を完了、`--preverify --monitor dmdata --timeout 4` が skip→dmdata monitor 移行→bound 終了、`--repeat` が #1 を焼いて removal 待ちに入る(remove→insert→再焼きの完全周回は物理 target 交換が要るため未検証)。

#### verify / read / write / erase

```text
ch32rv verify <FILE> [--format ...] [--offset ...] [--region ...]     不一致は exit 30
ch32rv read  (--range <addr>[+len|..end] | --region <r>[+off][+len])
             [-o <file>|-] [--format bin|hex|ihex] [--blank-check]
ch32rv write (<FILE> | hex:<bytes> | word:<u32>) --at <addr|region[+off]>
             [--erase auto|none]                                       上級。flash 先で erase none は warn
ch32rv erase (--all | --region <r> | --range <a>..<b>)                範囲指定は必須(暗黙の全消去をしない)。--all の名は global --chip <SKU> との衝突回避
```

領域名は `code` / `system`(bootloader)/ `option` / `eeprom` / `ram`。minichlink の `flash` / `bootloader` 名は別名として受ける。

**`erase --range` / `--region` の実装状況(2026-09-01)**: `--all`(chip 全消去)に加え、**page 単位の部分消去**を実装。WCH-Link stub の write 経路は部分書き込みを受け付けない(probe が reason 0x55 で拒否)ため、**FLASH controller(0x4002_2000)を DMI 経由で直接叩く**新経路を追加(`DebugModule::flash_page_erase` / `flash_program_page`。KEYR/MODEKEYR unlock → FTER/FTPG + STRT/PGSTART → STATR busy 待ち)。消去は page 粒度なので **start と length を page 境界に揃えることを必須**とし(fail-closed。ズレは Usage error + page サイズを提示)、隣接 page を巻き込まない。`--region code[+off[+len]]` は probe 報告の flash サイズから解決。program は family 別に 3 方式(PgStart=V20x/V30x、Buffered=V003/X035/L103、V103=標準 16bit halfword+commit。§4.2.1)。**対応 family: V20x/V30x・V003/CH641(64B)・X035/CH643・L103・V103(128B)**。実機検証: V307 で page1 だけ消去し page0(reset vector)/page2 無傷、V003 で 64B page、V103 で 128B page 消去(隣接無傷)、**L103 で 256B page を surgical に消去(先頭 firmware・中間 blank 域とも無傷、program/verify 往復 OK)**。**注意: 消去済みセルの read 値は family 差あり(V20x/V30x=`0xe339e339`、X035/V003=`0xff`)**ので、erase 完了判定は read でなく controller の STATR(BUSY クリア + WPRERR 無し)で行う。この直接 FLASH controller 経路は今後 flash SW breakpoint(trigger 無し core)と option byte 書き込みの土台にもなる。

#### reset / run / recover

```text
ch32rv reset [--halt] [--dm] [--confirm-run]      既定: reset して実行、detach
ch32rv run <ELF> [--no-flash] [--source dmdata|rtt|uart|sdi]
             [--exit-on semihosting|timeout=<s>]  target の exit code を伝搬(HIL 用)
ch32rv recover --method power-off|nrst|unprotect|unbrick
             [--chip <family>]                    特殊消去(power-off/nrst)は --chip 必須
```

- `recover` の method は別 operation として固定する: `power-off`(給電断 erase)、`nrst`(RST ピン erase。配線要件を事前表示)、`unprotect`(RDP 解除 = 全消去。確認プロンプト)、`unbrick`(電源サイクル + DM 連打 + option 工場値 + 全消去。minichlink 手順の移植)。
- attach 手段としての connect-under-reset は recover ではなく global `--connect-under-reset`。

### 4.2 probe

```text
ch32rv probe list [--watch]                       --json に selector 全 key、Windows は interface ごとの bound driver 名
ch32rv probe info [--probe <sel>]                 型番/HW/FW 版(raw・正規・WCH 表記)/mode/serial/interface 構成/使用中
ch32rv probe power <3v3|5v> <on|off>
ch32rv probe power cycle [--off-ms 300]
ch32rv probe mode get
ch32rv probe mode set <riscv|dap> [--yes]
ch32rv probe firmware info                        版と hash。既知不良版 DB と照合して判定を出す
ch32rv probe firmware check [--min <ver>]         CI 用。不良版・版不足なら exit 12
ch32rv probe firmware update --image <FILE> [--yes]
ch32rv probe vendor <hex...>                      隠し。backend 固有 command の escape hatch
```

- firmware 版は **raw byte・正規化表記(2.12)・WCH 表記(v32)を常に併記**し、比較は正規化値で行う(表記系の混同と probe-rs の版比較バグを構造的に避ける)。
- `firmware update` は IAP mode(`4348:55e0`)への遷移・書込・再 enumeration 待ち・版確認までを 1 操作にする。既に IAP に滞留した個体を検出したら update の続行か脱出(exit IAP)を提示する。image は同梱しない(user-supplied)。
- 対応 probe: WCH-LinkE / LinkW / LinkS / 旧 Link(CH549)を型番として区別し、非対応 operation は capability で事前に弾く。互換 probe(funprog HID / NHC-Link042 / ardulink / rv003usb 系)は P2 の backend として同じ体系に入る。

### 4.3 target

```text
ch32rv target info                                chip ID/family/SKU 候補(根拠付き)/UID/flash size/保護状態/option 要約/verified
ch32rv target option get
ch32rv target option set <key>=<value>...         例: rdp=off nrst=gpio split=160/32 debug=off
ch32rv target option reset                        工場出荷値
ch32rv target option write-raw <hex> [--yes]      生値(expert)
ch32rv target protect <on|off> [--yes]            off は全消去を伴う旨を明示
```

- 構造化 key は family ごとに DB から導出(`db info <sku>` で一覧可能)。set は read-modify-write-verify。
- **`option set` 実装(2026-09-02)**: `target option set <key=value ...>` を実装。key は (a) family の USER bit 名(DB の `option_fields` 由来。例 L103 は `CFGCANM/STANDYRST/STOPRST/IWDGSW`、値 0|1)、(b) `rdp=on|off`、(c) `data0=/data1=<byte>`。現在値を read→該当 bit/byte のみ変更→**全補数を再計算**(0xFF^value)→§4.2.1 の直接 controller で erase+program→read-back verify。反映は system reset 後。**`rdp=off` は全消去、`rdp=on` は読めなくなる**旨を明示し確認必須。未知 key は既知フィールド一覧付き usage error(fail-closed)。実機検証(L103): `STOPRST=0` で USER `0xff→0xfd`(補数 `00→02`)、read-back で反映、`STOPRST=1` で復元。**構造化エイリアス(`nrst=gpio`・`split=160/32` 等)や multi-bit フィールドは後続**(RM 名の単一 bit のみ現状対応)。
- 「未対応 SKU」と「DB に無い SKU」は JSON で区別する(exit 20 の detail)。
- 実装状況(2026-09-01): **`option get` を実装**(読み取り専用)。option bytes(`0x1FFF_F800`、16 byte)を DMI で読み、共通フィールドを復号: read protection(RDPR==`0xA5`=off)、USER の IWDG/nRST_STOP/nRST_STDBY、Data0/Data1、WRP(write-protect mask)。生バイトを常に表示。**family 固有の USER ビット(RAM split・nRST pin 機能等)は target DB(依頼 0003)生成後**なので構造化復号は暫定とし warning を出す。実機検証: V203/V307/V003/X035 で RDPR=`0xA5`(unprotected)・補数バイト整合を確認。
- **option 書込 `write-raw` / `reset` / `protect` 実装(2026-09-01)**: option-byte programming を `DebugModule::flash_program_option_bytes`(minichlink の option 経路から転記)で実装。手順: FLASH_KEYR + OBKEYR(`0x4002_2008`=STM32F1 系 OPTKEYR)+ MODEKEYR unlock → CTLR の OPTER+OPTWRE+STRT で **全 option byte 消去** → 8 halfword を OPTPG+OPTWRE+STRT で書込(value+complement をそのまま。complement は呼び出し側責任)。**RDPR(halfword0)を最初に書く**ことで消去〜再書込の read-protect 窓を最小化。反映は system reset 後。`option get` と同じ 16 byte レイアウト。**`write-raw <hex>`**(expert、16 byte)は RDPR≠`0xA5` なら「読み出し保護 ON になる」警告して `--yes` 必須。**`reset`** は工場デフォルト(RDPR=`0xA5`・USER/Data/WRP=`0xff`)。**`protect on|off`** は現在値を読んで RDPR のみ変更(on=`0xFF`、off=`0xA5`)、他 byte 保持。**`off` は全 flash mass erase を伴う**旨・**`on` は読めなくなる**旨を明示し `--yes`/確認必須。既に目的状態なら no-op。**family 制限なし**(全 CH32 が STM32F1 系 option 配置)。**構造化 `set`(kv: rdp=/nrst=/split= 等)は family 固有 USER bit=DB(依頼 0003)依存のため後続**。実機検証(L103): `write-raw` に現在値を round-trip 書込 → 不変・RDPR=`0xA5` 維持・flash 無傷・走行復帰、`reset` も同様、fail-closed(bad length / RDP-danger without --yes / protect off 既off の no-op / protect on without --yes 拒否)。`protect on/off` の実トグルは破壊的(on=保護・off=全消去)のため実機未トグル(primitive は round-trip で検証済み)。programming 失敗時は option が消去済み=read-protect の可能性を error に明示し `recover` を案内。

### 4.4 dbg

```text
ch32rv dbg halt [--reset]     ch32rv dbg resume     ch32rv dbg step [N]
ch32rv dbg regs                                     GPR + pc(dpc)一括
ch32rv dbg reg read|write <x1..x31|pc|csr:<addr>> [<value>]
ch32rv dbg dmi read|write <addr> [<value>]          DM レジスタ直接(expert)
```

### 4.5 monitor

```text
ch32rv monitor [--source uart|sdi|dmdata|rtt]
  --port <path:/dev/ttyACM0 | usb:VID:PID[:SERIAL][:IFACE]>   省略時は --probe の CDC から導出
  --baud 115200      (uart のみ)
  --timestamps / --log <file> / --raw
  --reconnect        再 enumeration 追従(既定 on。upload 直後の配送停止は再 open で直る実測に基づく)
ch32rv monitor list [--json]                        候補 port と役割(uart/sdi)の対応
ch32rv monitor sdi <on|off>
```

4 つの `--source` は「4 本の並列 port」ではなく、**2 種類の host 機構**に分かれる(ArduinoCore-CH32 の Serial / SerialSDI / SerialDMDATA / SerialRTT ライブラリが target 側の一次仕様)。

| source | target lib | host が受ける機構 | 対応 probe | 方向 | 備考 |
|---|---|---|---|---|---|
| `uart` | `Serial`(HW UART) | probe の **CDC port を開く**(serialport) | LinkE/LinkW の UART bridge | RX(TX 可) | 物理 TX/RX 配線が要る |
| `sdi` | `SerialSDI` | **LinkE に forward を有効化する probe command を送ってから、同じ CDC port を開く** | **LinkE のみ** | RX のみ | uart と**同じ 1 本の CDC に混ざって出る**(分離不可)。core は halt しない |
| `dmdata` | `SerialDMDATA` | **host が DMI で DM data0/data1 を polling**(minichlink `-T` の framing、7 byte out / 3 byte in) | 任意(CH549 も可) | 双方向 | CDC を使わない。core は halt しない |
| `rtt` | `SerialRTT` | **host が DMI で RAM の RTT ring buffer を read/write**(`_SEGGER_RTT` を symbol/scan で発見) | 任意 | 双方向 | CDC を使わない。core は halt しない |

要点(ユーザー指摘の反映、2026-09-01):

- **`uart` と `sdi` は LinkE の同じ 1 本の CDC port に出る**。`sdi` は「LinkE に SDI forward を有効化させる」probe 側の**設定変更**であって別 port ではない。両方使うと 1 つの monitor 窓に**混在**して届き分離できない。SDI は LinkE 専用(CH549/LinkW 不可)、firmware 2.10+ を capability で判定。
- **`sdi` と `dmdata` は同じ DM data0/data1 レジスタを使うが framing が違う**。SerialSDI は LinkE が forward する framing、SerialDMDATA は minichlink framing。**一方を読むツールにはもう一方は noise に見える**ので、target sketch は SerialSDI か SerialDMDATA のどちらか一方のみ(排他)。
- **`dmdata` / `rtt` は CDC を一切使わず host が debug transport(DMI)で読む**。LinkE forward 不要でどの probe でも動き、双方向。これが「UART/SDI 以外の形式」。`dmdata` は SerialSDI 出力も(LinkE forward せず)DMI 直読みできる利点がある。
- port は VID/PID/serial/interface から決め、COM 番号・`/dev/tty*` の番号に依存しない。`--probe name:bench-01` から同一物理 device の CDC を引けることが HIL の要件。
- 実装は 2 backend に割れる: **CDC serial backend**(`uart`/`sdi`。`sdi` は先に enable command)と **DMI backend**(`dmdata`/`rtt`。core を halt せず running 中に DMI read/write)。

### 4.6 gdb / dap

```text
ch32rv gdb [--listen 127.0.0.1:3333] [--reset-halt] [--no-flash]
ch32rv dap [--port <n>|--stdio]                     P2
```

- **attach 時に target flash を書き換えない**(WCH OpenOCD の挙動を再現しない)。`load`(vFlash)には対応するが必須にしない。
- HW breakpoint(RISC-V trigger module)は **core が実際に trigger slot を持つ場合のみ GDB に広告する**(minichlink の `hwbreak+` 偽装をしない)。**有無は misa や core 世代と一致しない**ので、attach 時に tselect/tdata1 を実際に叩いて**動的に**数える(下表)。`support_hw_breakpoint` は slot>0 のときだけ `Some`、attach ログにも実 slot 数を出す。

  | 実測(2026-09-01) | core | family | trigger slot |
  |---|---|---|---|
  | CH32V307 | QingKe V4F | 0x06 | **4**(実発火確認) |
  | CH32X035 | QingKe V4C | 0x0d | **4**(実発火確認) |
  | CH32L103 | QingKe V4C | 0x0e | **4**(実発火確認。step + HW bp 発火 @0x300/0x2ea、attach でレジスタ非破壊) |
  | CH32V203 | QingKe V4B | 0x05 | **0** |
  | CH32V003 | QingKe V2A | 0x09 | **0** |
  | CH32V103 | QingKe V3 | 0x01 | **0** |

  ※ V203 と X035 は misa 完全一致(`0x40901105`)なのに trigger 有無は逆 → **misa では判別不可、動的検出が必須**。
- GDB が SW(Z0)で要求した `break` は、**動く中で最も安い手段の順**で張る: **(1) RAM の `ebreak` memory patch → (2) 空き HW trigger〔摩耗なし〕→ (3) flash page 書き換えの flash SW breakpoint〔trigger 無し core 用、flash 摩耗あり〕**。attach 時に dcsr の ebreakm/ebreaks/ebreaku を立て `ebreak` を Debug Mode 突入(halt)にする(無いと暴走)。patch 後に read-back で着弾を確認。
  - **HW trigger フォールバック**: RAM patch が着弾しない flash 番地で、空き trigger があれば透過的に使う → V307/X035 は通常 `break` が flash で発火(摩耗なし)。
  - **flash SW breakpoint フォールバック(2026-09-01 追加)**: trigger の無い core でも、検証済み FLASH-controller profile(256byte family)なら **§4.2.1 の直接 FLASH controller で page を read-modify-write** して `ebreak` を焼く。code は低位 alias(0x0000_0000)で走るが FLASH controller には物理 flash 番地(0x0800_0000+off)を渡す。同一 page 内の複数 breakpoint に対応し、page 内容が変わらない set/clear は書き込みを省く。detach 時に全 page を pristine へ復元(途中終了でも `ebreak` を焼き残さない)。**注意: set/clear ごとに flash を書くため摩耗する**(gdb の step-over は remove+再 insert で 2 回書く)。attach ログで警告し `set breakpoint always-inserted on` を勧める。program 方式は family 別(PgStart=V20x/V30x、Buffered=V003/X035/L103。§4.2.1)。**対応: V20x/V30x・V003/CH641・X035/CH643・L103・V103**。**V103 は attach quirk の対処が前提**: WCH-Link の AttachChip が生きた GPR s1/x9 を chip id で上書きし(元値は保存されない)、resume 後に program が s1 を使う瞬間 fault する(mcause=4 load-misaligned)。→ attach 後に soft-reset して program にレジスタを再構築させてから halt する(profile の `attach_corrupts_regs`。target は再起動する)。実機検証: V103 で flash 上コードへの通常 `break` が複数 `continue` で発火。
- RV32E core(CH32V003、misa.E=bit4)対応: GPR は x0-x15 のみ存在し、x16-x31 を abstract command で読むと cmderr が出て session が落ちる。gdb server と `dbg regs` は misa.E を見て x0-x15 だけを扱う。
- 実装状況(2026-09-01): gdbstub ベースの GDB server が **register/memory R/W・halt/continue/step・Ctrl-C・SW(RAM+flash)/HW breakpoint** を実機で end-to-end 動作(riscv-none-embed-gdb)。検証: V003/V203 で RAM SW breakpoint が `continue` を停止、V307/X035/**L103** で flash 上の通常 `break` が HW trigger で発火(L103 は step も毎命令前進・attach でレジスタ非破壊を確認)、**V203(trigger 無し)で flash 上の通常 `break` が flash-patch で発火(複数 `continue`・detach で flash が pristine 復元)**。RV32 arch は x0-x31+pc の整数のみ(V4F FPU は後続)。**V003/V103 の flash SW breakpoint は controller profile 未検証で後続**。`load`(vFlash)は未実装。

### 4.7 isp(factory ISP 経路)

```text
ch32rv isp [--transport usb|uart] [--port <serial-port>] [--baud <n>] [--device <usb:sel|index:n>]
ch32rv isp list                                     ISP mode device の列挙。LinkE IAP(同 VID:PID)は区別して表示
ch32rv isp info                                     chip/BTVER/UID/保護状態
ch32rv isp enter [--via touch1200 --port <p>]       app 協調での ISP 突入(X03x/X315/H417)。他は BOOT 手順を提示
ch32rv isp flash <FILE> [--erase auto|none] [--verify on|none] [--reset run|none]
ch32rv isp verify <FILE>
ch32rv isp erase
ch32rv isp eeprom read|write|erase [<FILE>]         dataflash
ch32rv isp config get|set <key>=<v>|reset           config bytes(debug 有効/無効、保護解除を含む)
ch32rv isp reset
```

- ISP device は USB serial を持たないため、既定は「1 台のみ」の fail-closed。複数台は `usb:<bus>-<ports>` で選ぶ。
- protocol は自前実装(wchisp は GPL-2.0 のため取り込まない)。wchisp/minichlink に無い 0xa6 VERIFY・DATA 系・UART transport も protocol.md に記録した上で実装する。

### 4.8 boot(custom bootloader 経路)

```text
ch32rv boot enter [--method touch1200|double-reset|magic|pin] [--port <p>]
ch32rv boot dfu flash <FILE> [--alt <n>] [--address <a>] [--usb-id <VID:PID>]   dfu-util 相当
ch32rv boot dfu info
ch32rv boot uf2 flash <FILE>                                   volume 検出→(必要なら変換)→copy→完了監視
ch32rv boot uart flash|info <FILE> [--node <id>]               tinyboot 系(RS-485 multi-drop 含む)
ch32rv boot hid flash <FILE> [--usb-id <VID:PID>]              rv003usb / b003fun 系(UIAPduino 等)
```

UF2 family ID・DFU の VID:PID・HID の magic packet は target DB / 設定で管理する。すべて P2。

**任意 VID/PID 指定(逃げ道。TODO 2026-09-01、ユーザー依頼)**: bootloader 系は vendor/build ごとに USB VID:PID が異なる(rv003usb/b003fun 系は既定 `0x1209:0xb003` だが、**UIAPduino は `0x1209:0xb803`** = ボードごとに変わる)。minichlink は PID をソースに **ハードコード**しており、UIAPduino に書くには PID を書き換えて **リビルドが必要**だった(参照: <https://qiita.com/tomorrow56/items/6cae8ddc7470cb64ad7d>)。ch32rv は同じ轍を踏まない:

- **known table**: UIAPduino(CH32V003・16KB・b003fun HID bootloader・`0x1209:0xb803`・bootloader 入りは「reset を押しながら USB 接続」)を含む既知 bootloader device を target DB / 設定で持ち、既定で発見できるようにする。
- **`--usb-id <VID:PID>` 上書き**: known table に無い未知 PID でも、`--usb-id 1209:b803` のように CLI から VID:PID を直接指定して書けるようにする(**リビルド不要の逃げ道**)。`boot dfu` / `boot hid` / `boot uf2`(volume 検出の補助)に共通で効かせる。bootloader protocol は device class から自動判別しつつ、必要なら `--protocol dfu|hid|uf2` で明示指定も許す。
- 設計原則: **単一 PID をコードに焼き込まない**。発見は「known table + ユーザー指定 VID:PID」の二段で、知らない PID にも到達できる状態を既定にする。

### 4.9 db

```text
ch32rv db list [--family <f>] [--verified-only]
ch32rv db info <SKU>            geometry(page/fast/block)、領域、option layout、chip ID、生成元 revision、verified 根拠
```

- **実装状況(2026-09-02)**: `db list` / `db info` を実装(cmd_db.rs、デバイス不要)。生成済み DB(`crates/target/generated/skus.csv` を `include_str!` で埋め込み)を列挙/表示。`db list --family <f>` は family/SKU prefix で絞り込み、`--verified-only` は実機確認済み(実測6台)のみ。`db info <SKU>` は family/device_id/flash/sram/verified を表示。JSON 可(arduino B-3=`db list --json` 対応)。DB は `cargo xtask db-gen` が ch32-device-data(pinned)から生成し commit する(§4.3 target info の chip_id→SKU 解決、§4.3 option get の family-aware USER decode もこの DB を使う)。geometry は flash/sram に加え **page/fast erase・fast program・block erase**(`flash_geometry.csv`、`db info` で表示)。cli テストが DB の `fast_erase_bytes` と `flash_controller_profile.page_size`(手写し)を6 family で突き合わせて乖離をガードする。option layout は register 系 CSV 取り込みで後続。

DB は ch32-device-data / ch32-data からの生成物で手書き YAML を持たない(architecture.ja.md §4)。`--db <path>` overlay で新 SKU を再ビルドなしに試せる。

### 4.10 capabilities / doctor / version / complete

```text
ch32rv capabilities [--probe <sel>] [--chip <sku>]   probe 型番 × probe FW × target family × operation の可否と理由
ch32rv doctor [--emit-udev]                          権限/udev、Windows driver binding、既知不良 FW、IAP 滞留、
                                                     target 電源/BOOT/配線の切り分けと次の一手。--fix は持たない(暗黙の sudo をしない)
ch32rv version [--json]                              tool 版 / git rev / contract 版 / target DB rev+hash / flash stub hash / build target
ch32rv complete <bash|zsh|fish|powershell>
```

すべての操作 command は実行前に capabilities と同じ判定を通り、不可なら exit 24 で同じ構造の理由を返す。`tool supports LinkE` の boolean は存在しない。

- **`capabilities` 実装(2026-09-02)**: cmd_capabilities.rs。attach して **probe variant(LinkE/CH549/…)+ FW + target family/SKU/配線(DB)** を読み、operation ごとに可否+理由を出す(human 表 + JSON)。判定(`build_matrix`、単体テストあり): `connect`(**1線 target × CH549 = 不可**。CH549 は 1線 SWIO 非対応、`Variant` + debug_wiring から)、`flash`(family stub 有無)、`erase --range / flash-bp`(FLASH-controller profile 有無。page サイズ+方式を理由に表示)、`gdb HW breakpoints`(attach 時に動的検出=V4C/V4F は 4)、`monitor sdi`(**LinkE 限定**)、`monitor dmdata`(任意 probe)、`recover power-off`(**LinkE/LinkW 限定**=target 電源制御)。実機検証: L103(LinkE)全 yes、**V103(CH549 probe)は sdi/power-off が NO**、V003(LinkE,1線)は connect yes。今は attach 前提(target 未接続の静的 `--chip` のみ判定は後続)。各操作 command 側の統一 gating(exit 24)への配線も後続。

### 4.11 arduino

```text
ch32rv arduino discovery       Pluggable Discovery protocol(stdio JSON)。probe を wchlink://<serial> の port として公開し、
                               ISP device・CDC monitor port(uart/sdi の役割判定付き)も列挙する
ch32rv arduino monitor         Pluggable Monitor protocol(stdio JSON)。--source uart|sdi|dmdata|rtt を wrap する
```

Arduino 専用の書き込みロジックは持たない。recipe は §5 の通常 command を呼ぶ。

## 5. 呼び出し例

Arduino recipe(platform.txt)。probe selector は空にできる 1 変数に畳む(現行 probe-rs recipe と同じ制約):

```text
"{path}/ch32rv" flash "{build.path}/{build.project_name}.elf" --format elf --chip {build.ch32rv_chip} --reset run --confirm-run --non-interactive --progress none {upload.probe_args}
```

CI / HIL:

```sh
ch32rv probe firmware check --min 2.12 --probe name:bench-01        # 既知不良版なら exit 12
ch32rv flash app.elf --probe name:bench-01 --json > result.json     # retries も JSON に残る
ch32rv run tests.elf --probe name:bench-01 --source dmdata --exit-on semihosting
```

日常:

```sh
ch32rv flash blink.elf --monitor sdi      # 書いて、走行確認して、そのまま SDI print を見る
ch32rv doctor                             # 動かない時の一手目
```

## 6. 互換性ポリシー

- **contract 版**(JSON schema・NDJSON event・exit code)は CLI 版と独立に管理し、破壊変更でのみ major を上げる。field 追加は随時。
- command と flag は追加のみ。廃止する場合は 2 minor 版の deprecation 警告を挟む。
- exit code は追加のみ(§3.6 の帯を守る)。
- `--json` の schema は `docs/contract/` に置き、release ごとに固定する。

## 7. 参照

- [requirements.ja.md](requirements.ja.md)(吸収マップと根拠)
- [原設計案 §4・§6](../../note/research/new-programming-tool-design.ja.md)
- `../../ArduinoCore-CH32/platform.txt`(recipe 制約の現物)
- [Arduino Pluggable Discovery / Monitor specification](https://arduino.github.io/arduino-cli/latest/pluggable-discovery-specification/)
