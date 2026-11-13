# ch32rv CLI 仕様: コマンド体系

- 状態: 凍結候補(v1 の凍結で確定。契約は docs/freeze-decisions.ja.md が決める)。ch32rv のコマンド体系、共通の契約(global、出力、exit)、コマンドごとの動きを書く。ここに書いたコマンドと flag は、すべて今の CLI にある
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
├─ flash <file>                書き込み(erase/verify/reset/confirm-run policy)     [probe-rs download, wlink flash, LinkUtility]
├─ verify <file>               照合のみ                                            [probe-rs verify]
├─ read                        メモリ/flash 読み出し・dump・blank check            [wlink dump, minichlink -r, LinkUtility]
├─ write                       生メモリ/領域書き込み(上級)                        [minichlink -w, wlink write-mem]
├─ erase                       消去(chip/region/range)                            [各 tool]
├─ reset                       リセット(run/halt/dm) + --confirm-run              [wlink reset]
├─ run <elf>                   書き込み+実行+出力監視+target の結果(exit 0 / 60)  [probe-rs run/attach]
├─ recover                     診断+復旧(auto/power-off/nrst/unprotect/unbrick) [wlink erase --method, minichlink -u, LinkUtility]
│
├─ probe                       probe 本体の管理
│  ├─ list                     一覧(型番/FW版/mode/serial/使用中/driver)          [wlink list, probe-rs list]
│  ├─ info                     詳細 + 既知不良 FW 判定 + IAP 滞留検出              [wlink status]
│  ├─ power <3v3|5v> <on|off>  電源出力                                            [wlink set-power, minichlink -3/-5/-t/-f]
│  ├─ power cycle              電源再投入
│  ├─ mode <get|set>           RISC-V / DAP 切替                                   [wlink mode-switch]
│  └─ firmware <info|check|update|exit-iap>  版の解釈・不良版検出・IAP 書込・IAP から出る  [wlink-iap, LinkUtility]
│
├─ target                      target の識別と不揮発設定
│  ├─ list                     target に届く場所の一覧(既定では attach しない)
│  ├─ info                     chip ID/SKU 候補/UID/容量/保護/option 要約          [minichlink -i, LinkUtility]
│  ├─ option <get|set|reset|write-raw>  構造化 option bytes                        [minichlink -d/-D/-S/-N/-n, LinkUtility]
│  └─ protect <on|off>         読み出し保護                                        [wlink protect/unprotect, minichlink -p/-P]
│
├─ dbg                         実行制御ワンショット
│  ├─ halt [--reset] / resume / step [N]                                           [wlink halt/resume, minichlink -a/-A/-e]
│  ├─ regs / reg <read|write> <name>                                               [wlink regs]
│  └─ dmi <read|write>         DM レジスタ直接操作(expert)                        [minichlink -s/-m, wlink write-reg]
│
├─ monitor [--source uart|sdi|dmdata|dmseq|rtt|fixture-uart]   実行時 I/O(sdi 以外は双方向)
│  ├─ list                     monitor 候補 port の列挙
│  └─ sdi <on|off>             SDI print の有効/無効                               [wlink sdi-print]
│
├─ gdb                         GDB server(attach 時 flash 非改変)                [WCH OpenOCD, minichlink -G, probe-rs gdb]
│
├─ isp                         factory ISP 経路                                    [wchisp, WCHISPTool_CMD]
│  ├─ list / info / enter / reset
│  ├─ flash <file> / verify <file> / erase
│  └─ config <get|set>
│
├─ boot                        custom bootloader 経路                              [dfu-util, rv003usb]
│  ├─ dfu <flash|info>
│  └─ hid flash <file>
│
├─ db                          target DB の閲覧
│  ├─ list                     SKU 一覧(verified 区別)                           [probe-rs chip list]
│  └─ info <sku>               geometry/領域/option layout/provenance              [probe-rs chip info]
│
├─ capabilities                probe×FW×target×operation の可否 matrix
├─ doctor                      環境診断と次の一手(--emit-udev)
├─ version                     tool/contract/DB/stub の版(--json)
├─ complete <shell>            shell 補完
│
├─ arduino                     Arduino IDE 統合(machine 向け)
│  ├─ discovery                Pluggable Discovery(stdio JSON)
│  └─ monitor                  Pluggable Monitor(stdio JSON)
│
└─ broker                      probe ごとのブローカー(§4.12)
   └─ endpoint                 ブローカーの待ち受けの場所(--json)
```

## 3. 共通契約

### 3.1 グローバルオプション

| flag | 値 | 既定 | 説明 |
|---|---|---|---|
| `--probe <selector>` | §3.4 | (一意なら自動) | probe の選択。複数一致は exit 14 |
| `--chip <SKU\|family>` | 例 `CH32V203C8T6` | 自動検出 | 検出と矛盾したら exit 23(fail-closed)。`Session::attach` が chip_id と `--chip` 名を DB family へ解決し、要求名が DB にあり検出 family と不一致なら `target-ambiguous`(23)。**名前は SKU / family / series の完全一致(大小無視)だけ**(docs/freeze-decisions.ja.md §1。型番の前方一致はしない)。family / series は family で、SKU はその SKU の device id で照合する(DB に device id の無い SKU は family で)。**空の `--chip` は usage(exit 2)**。**`--chip auto`(大小無視)は「`--chip` 無し」と同じ**(常に `--chip {build.ch32rv_chip}` を渡す recipe が、DB に無い板で自動判定を値として頼めるように。`CH32RV_CHIP` と config の `[defaults] chip` でも同じ)。自動判定での失敗: debug の pin に何も居ない → `target-no-response`(20)、読んだ chip id が DB に無い → `target-not-in-db`(20)、見つけた family を書く手段が無い → `capability-unsupported`(24)。monitor の `chip` 設定も同じ照合(`auto` が既定)。**DB に無い名前は `target-not-in-db`(exit 20)で停止する**(検証できない名前を受理すると、`--chip` を付けたのに刺さっている別チップへ黙って書くことになるため)。未発売の gap series はここに落ちる。省略すれば自動検出 |
| `--db <FILE>` | `CH32RV_DB` | 内蔵 DB のみ | 内蔵 device DB に CSV overlay を重ねる(列は `generated/skus.csv` と同じ)。**同名 SKU / 同一マスク device_id は overlay が勝つ**(in-tree `provisional/skus.csv` は穴埋め専用で逆)。overlay 行は `provisional: true` + `sku-provisional` warning。読めない/有効行なしは usage(2)。出荷テーブルに無い部品を試す逃げ道 |
| `--core <n>` | 0.. | 0 | H417 の core 選択。直接 WCH-Link 接続で `--core 1` は `dbg`（`halt --reset` 除く）と `read` に対応。その他の操作・OEP・index 2 以上は exit 24。実測根拠: [H417 コア選択](data-requests/0009-h417-core-selection.ja.md) |
| `--speed <low\|medium\|high\|kHz>` | | high | kHz 指定は近い段階に丸めて warn(段階の公称値 6000 / 4000 / 400 kHz で判定)。**公称値は線上の速さではない**: WCH-LinkE fw 2.22 + CH32L103 の実測(wch-protocols E163)では high = flash の Program 経路だけ速く(L103 約 2.5 MHz、V203 約 10 MHz)それ以外約 0.89 MHz、medium = 約 0.89 MHz、low = 約 0.47 MHz(V203 も medium / low は同じ)。high と medium の差は flash 書込みの速さだけ(pattern-4k の全体で L103 0.48 s / 0.63 s / 0.87 s、V203 high 0.40 s)。debug の線を使わない `isp`(`--entry probe` を除く)と `boot` には当てはまらない |
| `--connect-under-reset` | | off | NRST を押さえて attach する。WCH-Link: LinkE の RstOut で RST の線を low にし、押さえたまま AttachChip して haltreq を立て、離してすぐ AttachChip し直す(reset の後は AttachChip が要る)。起動直後にデバッグのピンを取る firmware が走る前に止めるためのもの。RST を target の NRST に配線していなければ、通常の attach と同じになる。OEP の probe: ポートの NRST の線(label `<port>.nrst`)を attach の reset TLV で押さえる。OEP の probe の `flash` では exit 24 |
| `--attach-wait <秒>` | | なし | その時間まで attach を繰り返し、答えたらすぐ hart を止める。その間に target の電源を入れ直すか RESET を押す。起動待ち(`setup` の delay、`CH32RV_BOOT_HOLD_MS`)の後に眠る・デバッグのピンを取る firmware を、probe が NRST を引けないボードで止めるためのもの。止めたまま仕事をするコマンドに付ける(`--attach-wait 10 recover --method unbrick --yes`)。WCH-Link と OEP(OEP の probe の `flash` では exit 24) |
| `--release-dm` | | off | 終わるときに DMCONTROL を 0(dmactive = 0)にする。走らせる reset(`reset`、`flash` などの `--reset run`)をしたときは、通常どおり LinkE の soft reset と resume をして(`--confirm-run` もその後)、終わるときに、選ばなかった hart を resume → DMCONTROL = 0 → NRST(LinkE の RstOut)でもう一度 reset し、その後は DetachChip も送らない。RST を NRST に配線していなければ 2 度目の reset は起きない。H417 では、LinkE の attach が残した core 1 のデバッグモード(CSTAR1 bit12)が reset をまたいで残り、core 0 の WAKEIP で起きなくなる。DM を止めてから system reset すると、深い眠り(CSTAR1 = 0xC000)に戻る(ArduinoCore-CH32RV の実測)。DM を止めてから LinkE の soft reset をかける順では、DM が戻り core 0 が止まっていた。WCH-Link のみ。OEP の probe へ行くコマンドでは exit 24 |
| `--pins <ROLE=PIN,...>` | | なし | OEP の probe のピンを役目ごとに名指す(`swdio=<pin>,swclk=<pin>`、1 線は `swio=<pin>`、`nrst` / `rx` / `tx`)。口の label の代わりに使う。OEP の probe へ行かないコマンドでは exit 24(`arduino monitor` はアドレスを IDE から受けるので断らない) |
| `--json` | | off | 結果を JSON で stdout へ(§3.5) |
| `--progress <bar\|ndjson\|none>` | | bar(tty)/none | 進捗の出力形式。ndjson は stderr へ |
| `--non-interactive` | | off | 対話プロンプトを全て拒否に変える |
| `--yes` | | off | 破壊操作の確認を省略 |
| `--lock-timeout <s>` | | 10 | probe が使用中のときの待ち時間(§3.7)。WCH-Link は device lock、OEP の probe は他の host の session の lock(lease)を待つ。取れなければ exit 13 |
| `--timeout <s>` | | 3秒 | transport timeout(probe との転送 1 回の上限)の上書き。WCH-Link、OEP の probe(コマンドとブローカーの間、そのコマンドが起動したブローカーと probe の間)、`isp`。`boot` には当てはまらない |
| `--duration <s>` | | Ctrl-C まで | streaming コマンド(`monitor` / `run`、`flash --monitor`)の実行時間。ほかのコマンドには当てはまらない |
| `--capture <path>` | | - | USB transaction（bulkおよびHID feature report）をNDJSONで記録(replay fixture用) |
| `--replay <path>` | | - | 記録した capture を HW 無しで再生(`enumerate`〜転送を fixture から供給)。`--capture` と排他。CI/バグ再現用 |
| `--dry-run` | | off | device を開かず計画のみ表示。対応は `probe firmware update`(image の版・USB id・frame 数を出して終了)と `boot hid flash` だけ。**ほかのコマンドは device に触れる前に exit 2(usage)で断る** |
| `-v` / `-q` | 重ね掛け | | 冗長度 |

### 3.2 環境変数

| 変数 | 対応 flag |
|---|---|
| `CH32RV_PROBE` | `--probe` |
| `CH32RV_CHIP` | `--chip` |
| `CH32RV_DB` | `--db` |
| `CH32RV_NON_INTERACTIVE` | `--non-interactive` |

flag > 環境変数 > 設定ファイル > 既定値。

切り分け用の環境変数(契約の外。値も意味も予告なく変わりうる):

| 変数 | 働き |
|---|---|
| `CH32RV_PORT_SPEED` | `off`、または速さの並び(`921600,500000`): ブローカーが UART bridge の probe との serial を上げるとき(oep-if-link §3 port_speed)に試す速さ。未設定は 500000 だけ(利用者が明示しなければ 500000 より上を試さない。候補は host が決める、oep-if-link §3)。500000 より上を並べたときは、各方向 1 秒の確かめを通ったときだけ使う。切り分け用で、契約の外 |
| `CH32RV_USB_TRACE` | file の path: OEP の vendor bulk の書き込みと IN の完了を 1 行ずつ記録する |
| `CH32RV_OEP_TRACE` | file の path: OEP の要求と答えを 1 行ずつ記録する(時刻、pid、向き、corr、fn / op / session_id / payload の長さ、payload の先頭)。ブローカーの上流の分は、ブローカーを起動したプロセスの環境に置く(切り分け用、契約の外) |

### 3.3 設定ファイル

`./ch32rv.toml`(プロジェクト)→ OS の設定の場所の `ch32rv/config.toml`(ユーザ。Linux は `$XDG_CONFIG_HOME`、無ければ `~/.config`。macOS は `~/Library/Application Support`。Windows は `%APPDATA%`)の順で探索。`[defaults] chip` は `--chip` も `CH32RV_CHIP` も無いときに使う。probe の別名(`--probe name:<別名>`)と既定値のみを持つ。挙動を変える隠し設定は置かない。

```toml
[probes]
bench-01 = "1a86:8010:434A124C5596"
bench-02 = "topology:3-1.4.2"
bench-03 = "oep://50029191fe34/v003?via=tcp:192.168.1.40:7450"

[defaults]
chip = "CH32V203C8T6"
```

### 3.4 probe selector 書式

```text
--probe <selector>
  VID:PID[:SERIAL]      canonical(probe-rs 互換)。例 1a86:8010:434A124C5596
  serial:<sn>           serial だけで指定
  name:<alias>          設定ファイルの別名(値は下のどの形でもよい。?via= 付きの住所も書ける)
  topology:<bus>-<ports>  USB の位置(固定 hub の物理 port。HIL lane 用)
  <IDE の住所>          IDE の port の住所をそのまま: probe の serial port(/dev/ttyACM0、COM3。symlink 可)、
                        wchlink://<serial>、oep://<probe>[/<port>[/<target>]][?via=<経路>]
```

`--probe` は debug probe(WCH-Link、OEP の probe)だけを名指す。書き込み先の住所(`ch32rv-hid://`、`ch32rv-app://`、`ch32rv-isp://`)は probe ではないので、`--probe` では exit 2 で断り、`isp` / `boot dfu` / `boot hid` の `--device` に渡す(§4.7、§4.8)。

`oep://` の住所(oep-spec host ガイド §5.3)は、場所(probe の unit_id、口、target)と、そこへ届く経路 `via` からなる。`via` は場所の一部ではない。

| `via` | 経路 |
|---|---|
| 無し | ch32rv が選ぶ。USB にその unit_id(serial)の OEP の device があれば USB(vendor bulk → HID → CDC の順)、無ければ DNS-SD で unit_id を探して TCP |
| `usb` | USB の経路のどれか(vendor bulk → HID → CDC の順)。USB に無ければ止まる |
| `bulk`、`hid`、`cdc` | USB の、その種類の経路だけ |
| `net` | DNS-SD で見つけた TCP だけ(IP が変わっても unit_id で届く) |
| `tcp:<host>:<port>` | そのアドレスと port の TCP。IPv6 は `[...]` で囲む。mDNS の届かない所(NAT の裏、WSL 2、VM)はこれで書く |
| `tty:<path>` | OS の serial port(`/dev/ttyACM0`、`/dev/serial/by-id/...`、`COM3`)。USB-UART の変換のように unit_id で見つけられない経路 |

- どの経路でも、開いたら confirm と fn 0 の describe の unit_id が `<probe>` と同じか(大小は区別しない)を確かめ、違えば使わない(書込みなどの前に止まる)。
- 住所は `?` から後を先に外してから、場所を `/` で割る(`tty:` の値は `/` を含む)。`via` の値の `?`、`#`、`&`、`%`、空白は `%XX` で書く。query は `via` の 1 つだけで、知らない名前の query を持つ住所は exit 2 で断る。
- ブローカーの key(この PC での probe の持ち主)は unit_id で決まり、`via` によらない(docs/oep-host.ja.md §7.2)。すでに動いているブローカーが別の経路で probe を持っているとき、`via` を付けた住所はその経路を守れないので exit 13 で断る(`via` 無しならそのブローカーを使う)。
- `probe list` / `target list` / discovery は、`via` の無い形を 1 つの場所につき 1 行出す。
- unit_id を知らないときは、serial port の path をそのまま `--probe` に渡してよい(道具の便利機能)。WCH-Link のものでない serial port は OEP の probe として開く(unit_id は確かめない)。
- 次の形は受けず、代わりの書き方を添えて exit 2 で断る: `port:<住所>` → 住所をそのまま、`tcp:<host>:<port>` / `tcp://<host>:<port>` → `oep://<unit_id>?via=tcp:<host>:<port>`、`tcp:<unit_id>` → `oep://<unit_id>?via=net`、`oep-net://<probe>[/<port>]` → `oep://<probe>[/<port>]?via=net`、`usb:<bus>-<ports>` → `topology:<bus>-<ports>`、`hid://<bus>-<ports>` → `ch32rv-hid://<bus>-<ports>`(`--device` でも)。環境変数 `CH32RV_OEP_TRANSPORT` も無い(`via=bulk|hid|cdc` で選ぶ)。

- **`index:` は無い**: 一覧の順番で選ぶ書き方は、機器の増減で別の probe を指しうるので持たない。`probe list` / `target list` の ADDRESS をそのまま渡す。
- **OEP の復旧**: `--connect-under-reset` は、リセットを押さえたままの attach を最大 4 回やり直し、族が分からない rvswd では SWCLK の休ませ方を probe の自動と low の両方で試す(CH32L103 は high で休ませると DM がリセットされる。離した直後に止められるかは firmware との競争で、1 回では通らないことがあった)。`recover --method nrst [--image <FILE>]`(OEP)は、NRST を押さえて止めたあと、リセットベクタのページを先に DMI で消し(壊れた firmware がもう起動しない)、残りを RAM loader で書く: `--image` を渡せばそのイメージ、無ければ消去済みの値(DB の erased_word)で code flash 全体。読み出し保護があれば、外して全消去する。実機: L103(RP2350、`--pins swdio=0,swclk=1,nrst=2`)で `--connect-under-reset target info` 3 回とも通り、`recover --method nrst --image` も 1 回で書けた。
- **OEP の口と `--pins`**: 口は `channel.{}.label` の `S.N` だけが名付ける(`S` は英字で始まる英字・数字・`-` の 1〜16 文字、`N` は英字・数字・`-`・`_`)。ラベルは書いたとおりの大小文字で表示し、比べるときは ASCII の大小文字を区別しない。名前の無い口と `any` は無い(住所はいつも全部を書く)。名前つきの口が無い probe は `target list` と discovery に出ない。住所でないシリアルポートの path を渡したときは、名前つきの口がちょうど 1 つならそれを使う(道具の便利機能)。ラベルの無い probe には `--pins swdio=<pin>,swclk=<pin>[,nrst=<pin>]`(1 線は `swio=<pin>`、UART は `rx` / `tx`)でピンを直接指定する。ピンは probe の channel の名前(fn 0 の describe の `channel_name`、`GPIO0` など)か番号。`--pins` を付けるとラベルは読まず、`oep://<probe>`(probe 自身)とも組み合わせられる。
- **OEP の一覧**: `probe list` / `target list` / discovery は、USB に無い OEP の probe も `oep://<unit id>[/<port>]` で出し、TOPOLOGY は `network`、MODEL は describe の model(`OEP probe (esp32)`)。両方から届く probe は `oep://` と USB の位置の 1 行で、JSON の `tcp` にネットワークの経路も出す。
- **IDE の住所はそのまま**: IDE の port の住所は、形で見分けられる(`://` を含む URL、`/` で始まる path、`COM<n>`、`\\.\…`)ので、前置き無しでそのまま `--probe`(debug probe)/ `--device`(書き込み先)に渡す(recipe は `--probe "{upload.port.address}"` か `--device "{upload.port.address}"`)。`wchlink://<serial>` は `serial:`、`wchlink://usb-<bus>-<ports>` は `topology:` と同じ。serial port の path は `probe list` の `ports` から逆引きする(Linux は sysfs で USB の位置まで辿るので serial 番号の無い Link でも決まる。Windows / macOS は OS が port の裏の USB を VID/PID/serial でしか教えないので、serial 番号の無い同 VID:PID が 2 台あると曖昧 = exit 14)。比較は unix で symlink を解き(`/dev/serial/by-id/…` 可)、Windows で `\\.\` を外して大文字小文字を畳む。`wchlink://` は serial で device を直接指す。どの probe の port でもなければ exit 10。

- serial を持たない device(ISP mode 等)は `VID:PID:` (空 serial)と topology で選ぶ。
- 解決結果が 0 台なら exit 10、2 台以上なら exit 14 で候補一覧を出す。
- `probe list --json` が selector に使える全 key を出力する。

### 3.5 出力契約

- **stdout**: 結果。`--json` 時は単一の JSON object のみ。human 出力時も結果のみ。
- **stderr**: log・進捗・警告。`--progress ndjson` 時は 1 行 1 event の NDJSON。
- **streaming command(`monitor` / `run` / `flash --monitor`)の target 出力**: human 時は stdout へ生 byte。`--json` 時は stdout を envelope に譲り、stderr へ `{"ev":"output","source":"<uart|sdi|dmdata|rtt|semihosting>","data":"<text>"}` を 1 chunk 1 行で流す(`data` は UTF-8 文字境界で分割、不正列は U+FFFD)。envelope は stream の終わり(`--duration` 満了 / semihosting exit)に 1 つ。
- JSON には必ず `contract`(契約版。`ch32rv-contract` の `CONTRACT_VERSION`。下の例の値はその一例)と `ok` を含む。schema は `docs/contract/` に置き、CLI の版とは独立に versioning する。`result` を出すコマンドは、すべて `docs/contract/commands.schema.json` に形がある(`cmd` で引く)。
- `flash` の結果は経路(`isp flash`、`boot dfu flash`、`boot hid flash` を含む)によらず `result.flash` の 1 つの形。検証の結果はどのコマンドでも `verified`(bool。検証しなかったら null)。

```json
{"contract":"5","ok":true,"cmd":"flash",
 "result":{"flash":{"bytes":16700,"family":"CH32V20x","programmer":"stub","skipped":false,
                    "scope":"sector","verified":true,"running":true}}}
```

- **語彙の約束**:
  - `error.code` と NDJSON の `ev` は開いている。値を足すことがあり、知らない `ev` の行は読み飛ばす。知らない `error.code` は §3.6 の帯の意味で扱う。
  - `error.kind` と `warnings[].code` は開いた語彙として名前を約束する。名前は凍結し、別の意味で使い回さない。足すことはある。知らない `kind` は同じ `error.code` の帯の意味で扱う。
  - warning は `ok` を変えない。`--json` では warning はすべて `warnings[]` に出し、stderr には出さない。
  - `msg` と `hint` は人向けで、文面は約束しない。

NDJSON event(stderr)の例。**再試行は必ず event として可視化する**(「16.7 KB で固まる」問題の運用要件):

```json
{"ev":"phase","name":"erase","total":12}
{"ev":"progress","phase":"program","done":8192,"total":16700}
{"ev":"retry","phase":"program","attempt":2,"cause":"transport-timeout"}
{"ev":"warn","code":"fw-known-bad","msg":"WCH-Link (CH549) RISC-V firmware 2.11 (v31) has a known defect (measured with probe-rs on CH32V103R8T6): download --reset writes the flash and reports success, but the core stays halted (...); a wlink reset starts it; 2.12 works"}
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
| 20 | target を特定できない。detail で `target-no-response`(無応答)と `target-not-in-db`(DB に無い `--chip` / 未収載 SKU)を区別する |
| 21 | target が protected(明示 unprotect が必要)。HID bootloader の読み出し保護、RAM loader の書き込み保護で出す |
| 22 | attach 失敗(配線、電源、BOOT)。無応答は 20(`target-no-response`)で区別 |
| 23 | target 曖昧(複数候補 / `--chip` と検出の矛盾) |
| 24 | capability 不足(probe×FW×target×operation で不可)。各コマンドが、その操作に要るものが無いと分かった所で出す |
| 30 | verify 不一致 / blank check 失敗 |
| 40 | 転送/DMI 操作の失敗、または真の transport timeout。JSON `kind` で `transfer-failed` と `transport-timeout` を区別 |
| 41 | probe が固まっており USB 再接続が必要**(予約。発行しない)** |
| 50 | 書けたが target が走っていない(`--confirm-run` 失敗) |
| 60 | `run`: target が 0 以外で終わった(`target-exit`。target のコードは `result.exit`) |
| 61 | `run`: `--duration` の間に終わりも止まりもしなかった(`run-timeout`) |
| 62 | `run`: semihosting でない理由で止まった(`target-halted`。breakpoint、例外。`result.dpc`) |
| 70 | 内部エラー(bug。report 情報を出す) |
| 71 | 欠番。別の意味では使わない |

10-14 は「経路の入口 device」、20-24 は「target」、30-41 は「転送・検証」、50 は「実行確認」、60-69 は「`run` での target 自身の結果」、70-79 は「tool 自身」。追加は各帯の空き番号のみ使い、欠番(71)は使い回さない。

### 3.7 排他制御と再試行

- **lock**: USB serial(無ければ topology)単位の advisory lock を OS の runtime dir に置く。`--lock-timeout` 待って取れなければ exit 13。異常終了した保持者の stale lock は起動時に回収する。
  - **実装**: `ch32rv-usb::DeviceLock`。`$XDG_RUNTIME_DIR/ch32rv/<key>.lock`(`XDG_RUNTIME_DIR` が無ければ unix では `<temp>/ch32rv-<uid>/`、0700 で利用者ごと。自分の dir でなければ使わない。Windows は `<temp>/ch32rv-locks/`、temp がもともと利用者ごと)に `flock` を取り、probe を使う間だけ保持する。key は probe serial、無ければ bus topology。取れなければ `--lock-timeout` 待って exit 13(`device-busy`)。**stale は flock の性質で自動回収**(保持者が終了すれば OS が解放するので「起動時回収」の別処理は不要)。対象: attach 経路(`Session::attach` = flash/target/dbg/write/monitor dmdata/capabilities/arduino)、`gdb` server、`monitor --source sdi`(`cmd_probe::lock_probe`)。同一プロセスで二重取得しないよう、直接 open 経路(gdb/sdi)は `Session::attach` を経由しない。**`uart`(`monitor --source uart`、`arduino monitor` の uart)は lock を取らない**: LinkE の UART は CDC の別の口で debug の口に触れず、tty は排他で開く。uart を開いたまま同じ probe で flash / gdb を使える。recover・probe list/info・`--repeat` の poll は非対象(read-only か短時間)。
- **OEP の probe**: 使用中は probe の lock(lease)で決まる。ブローカーが probe を開くとき、他の host の session が lock を持っていれば `--lock-timeout` まで待ち、取れなければ、ブローカーを起動したコマンドは exit 13(`device-busy`)。ブローカーはそのコマンドの `--lock-timeout` と `--timeout` で起動する。
- **別の版の ch32rv のブローカー**: probe のブローカーが別の版の ch32rv のもの(IDE 同梱と PATH のものなど)なら、使用中と同じに扱う。client はそのブローカーと話さず、`--lock-timeout` まで待つ(ブローカーは最後の client が抜けると終わるので、そのあと自分の版のブローカーを起動して続ける)。待っても終わらなければ exit 13(`device-busy`)で、msg に相手の版と PID を出す(`the probe's broker is ch32rv 0.24.0 (pid 1234), not ch32rv 0.25.0, …`)。版を持たない古いブローカーも別の版として扱う。
- **open 再試行**: 挿抜直後は CDC interface が vendor interface より先に見える(実測)。open 失敗は 1 秒間隔で計 3 回まで再試行してから exit する。
- **転送再試行**: chunk 単位の timeout→再試行(既定 3 回)。再試行が起きた事実は NDJSON event(`retry`)に必ず出す。
- **固まり**: 再試行が尽きて probe が応答しないときは、exit 40(`transfer-failed`)で終わる。`USBDEVFS_RESET` は使わない。固まりを見分けて再接続手順(usbipd / 物理挿抜)を示す exit 41(`probe-wedged`)は予約で、出さない。
- **capture(`--capture <file>`)**: `ch32rv-usb::capture`。`main` が `--capture` 時に一度 sink を設定し、`UsbInterface` の全 bulk 転送(cmd EP 0x01/0x81・data EP 0x02/0x82)とbootloaderのHID feature reportをNDJSON 1行ずつ追記する。bulk行は`{seq,t_us,ep,chan(cmd|data),dir(out|in),len,ok,data(hex)}`、HID行は`ep:"feature",chan:"hid"`。先頭に `{"_meta":{"format":1,"unit":"us"}}` と `{"_device":{…}}` を持つ。**bulkの`ep`は生のendpoint番地**(`"0x01"`等)で、こちらが正: 複数の列挙をまたぐsession(`probe firmware update`のIAP modeなど)では`chan`だけで役割が決まらない。行ごとflush(途中クラッシュでもそこまで残る)。flag無しはno-op。HIDを含め`--replay`可能。実機検証: `target info --capture`のGetProbeInfo/SetSpeed/AttachChip、および`boot hid flash --capture`のB803 erase/program/verify/runを記録・再生済み。

## 4. コマンド詳細

### 4.1 書き込み系(top-level、probe 経路)

#### flash

```text
ch32rv flash <FILE>
  --format auto|elf|hex|bin|uf2      既定 auto(magic 判別)
  --at <addr>                        bin 用ロード先(既定: code 領域先頭)
  --erase chip|sector|keep-store|none 既定 keep-store。chip=code flash 全体を先に消す。sector=image が覆う page / sector だけ(image 外を残す)。keep-store=STORE を残し、経路ごとに速い方法を選ぶ(STORE が無ければ sector と同じ)。none=消さない(書くものを必ず消す経路では断る)
  --verify readback|none             既定 readback。none は明示選択
  --preverify                        既に一致していれば erase/program を省略(flash 摩耗回避)
  --reset run|halt|none              既定 run
  --confirm-run[=status|pc]          reset run 後の走行確認(既定 pc)。失敗で exit 50
  --sdi on|off                       書込後に SDI print を設定(capability 判定込み)
  --monitor uart|sdi|dmdata|rtt      書込後そのまま monitor へ移行
  --restore-unwritten                image が覆う page の中の image の外の byte を残す(--erase sector|keep-store)
  --repeat                           target の再接続を検知して連続書込(量産)
```

- **経路ごとに守れないフラグは断る**(exit 24 `capability-unsupported`。何も書く前): ch32rv の RAM loader で書く経路(OEP の probe と、WCH の stub が無い WCH-Link の family: CH32H417 / V407 / X315 など)は、image が触れる page を消し、その page の image の外の byte を残し、全 page を読み戻す。そのため `--erase sector|keep-store`、`--verify readback`、`--restore-unwritten` は受け、`--erase chip` は loader の前に DM で全消去する(下の表)。`--erase none`(loader は書く page を必ず消す)、`--verify none`、`--sdi`、`--monitor` は断る。WCH-Link の loader の経路は `--preverify` も断る(OEP は守る)。OEP の probe はさらに `--reset halt`、`--repeat`(次の target を待つのは WCH-Link 越しだけ)、`--connect-under-reset`、`--attach-wait`(flash は自分の止める attach を使う)を、つなぐ前に断る。WCH-Link の loader の経路の `--confirm-run` は確かめないが断らない(Arduino の recipe が常に渡す。止めて確かめると、debug の pin を USB に取る H417 の sketch で失敗する)。結果の `running` は null。
- 書き込み先は code flash だけ。system 領域(bootloader)への書き込みと、target の上の CRC での照合は持たない。
- ELF/ihex はセグメントの物理アドレスを使い、`--at` は無視して warn(bin のみ有効)。セグメントが DB 上の書込可能領域に収まらなければ実行前に exit 2。
- `--confirm-run=status` は DM の running 状態のみ確認。`=pc` はさらに瞬間 halt→dpc 読取→resume で PC を採取し、flash 領域内かを判定する。SRAM 先頭(`0x2000_0018` 型)で止まっていれば「BOOT ピン疑い」を hint に出す。
- 対応 SKU でも **DB 上 verified でない場合は warn を出して続行**する(「実装済み」と「実機確認済み」の区別)。
- **`--erase` の経路ごとの動き**(`result.flash.scope` は実際の範囲 chip / sector / keep-store / none。keep-store では `result.flash.store` に `address` / `bytes` / `method` を出す):

  | 経路 | chip | sector | keep-store(既定) | none |
  |---|---|---|---|---|
  | WCH-Link + WCH の stub(V003 / V103 / V20x / V30x / X035 / L103) | Link の全消去(`Program 0x01`。LinkE が MER を叩く) | stub が書く page を自分で消す family(V003、V20x、V30x、L103。実測)では ch32rv は消さない。ほかの family(V103 の stub は消さない。測っていない X035 なども同じ扱い)では、書く page を ch32rv が DMI で先に消してから stub で書く。stub は 1 つの連続した範囲を書くので、image の先頭の page から最後の page までの隙間も書かれる | stub が自分で消す family は下の規則で (a) sector と同じ、か (b) 退避 → 全消去 → 書き戻し。ほかの family は sector と同じ(STORE には触れない) | stub が自分で消す family は断る(exit 24。stub は書く page を必ず消す)。ほかの family は消さずに書く |
  | WCH-Link + FLASH controller(stub も loader も無い family) | Link の全消去 | 覆う page を DMI で消してから書く | sector と同じ | 消さずに書く |
  | WCH-Link + ch32rv の loader(V006 / V205 / X315 / V407 / H417、`--programmer loader`) | DM で FLASH_CTLR の MER + STRT(`flash_mass_erase`)の後、loader が書く。MER の無い H417 は Link の全消去(`Program 0x01`。960 KiB で 0.3 s、実測) | loader が書く page を消す | sector と同じ(STORE は image の外なので触れない) | 断る(loader は書く page を必ず消す) |
  | OEP の probe(loader) | DM の MER。MER の無い H417 は code flash 全体の page(8 KiB)を DM で順に消す(大きさは chip id から DB で引く。分からなければ exit 24) | 同上 | 同上 | 断る |
  | 工場 ISP(`isp flash`) | `isp erase` と同じ全 sector の消去の後に書く(flash の大きさが分からない chip は exit 24) | image の長さ分の 1 KiB sector(最小 8) | sector と同じ。ただし最小の 8 sector が STORE に届くときは image の分だけ消す | 消さない |
  | HID(`boot hid flash`)/ DFU(`boot dfu flash`) | 断る(exit 24。全体を消す手段が無い) | bootloader / device が書く page / block だけを消す | sector と同じ | 断る(必ず消す) |

- **STORE**: ArduinoCore-CH32RV が flash の末尾に置く、リセットをまたいで残す領域。ELF の section `.ch32rv_store`(NOLOAD なので image には入らない)、無ければ記号 `_ch32rv_store_start` / `_ch32rv_store_end` で範囲を知る。**bin / hex / UF2、section も記号も無い ELF、大きさ 0 の STORE は「STORE 無し」**で、keep-store は sector とまったく同じに動く(image の外はすべて残る。`store.method` は `none-present`)。
- **WCH の stub の経路の keep-store の選び方**(stub が自分で消す family): stub は書く page を自分で消し、全消去の直後の flash でだけ速く書く(実測: V307 の 97 KB で全消去 4.0 s、ch32rv が消さないと 14.4 s。V003 の 16 KB で 2.2 s と 3.0 s)。STORE があれば次の 2 つの見積もりを比べ、(a) が大きければ (b) を選ぶ。`--restore-unwritten` のとき、STORE の範囲が書く範囲に重なるときは (a)。
  - (a) ch32rv は消さない(sector と同じ。`method` は `untouched`)。見積もり = 書く page 数 × page の自己消去の時間。
  - (b) STORE を(stub の page に広げた範囲で)高速読み出しで退避 → 全消去 → image を stub で書く → reset-halt して STORE を ch32rv の RAM loader(DMI。`--programmer loader` と同じもの。page ごとに消して書いて読み戻す)で書き戻す → image と STORE の両方を読み戻して照合する(`--verify none` でも STORE は照合する)。退避した中身がすべて消去済みの値(0xff、または family の消去済み word。V20x / V30x は 0xe339)なら書き戻しは省き、`method` は `blank`。書き戻したときは `restored`。2 回目の stub で書き戻すと効かなかった(V307、LinkE 2.22: STORE が消えたまま読めた)ので loader を使う。family に loader の plan が無ければ (a)。見積もり = 1.8 s + STORE 1 KiB あたり 50 ms。
  - page の自己消去の時間: V003 64 B で 3.1 ms、V30x 256 B で 27 ms、V20x 256 B で 19.5 ms、L103 256 B で 13 ms(stub で実測。LinkE 2.22、(sector − chip) / 書いた page 数)。表は `ch32rv_flash::erase_plan`。V003 の 16 KB は (a)(0.8 s)、V307 の 97 KB は (b)(10 s 超)になる。
  - (b) の途中(全消去から STORE の書き戻しまで)で失敗したときに備え、全消去の前に退避した中身を `$XDG_RUNTIME_DIR/ch32rv/store-<chip の UID、無ければ probe の serial>.bin` に置き、エラーの hint に置き場所と書き戻し方(`ch32rv flash --erase sector --at <番地> <ファイル>`)を出す。STORE を書き戻して照合できたら消す。
- stub の経路の sector は、stub が自分で消す family では DMI で先に消さない(先に消すのは時間の無駄)。ほかの family では DMI で消し、Link に attach し直してから stub で書く(V103 では、Link が attach で program buffer に入れた命令を Link の消去と stub が使うので、DMI で使った後は attach し直す)。`--restore-unwritten` は、書く範囲の page を DMI で読み、image を重ねて page 全体を stub で書く。
- **`--restore-unwritten`**: 書く範囲の page を先に読み、image を上書きして page 全体を書くので、image が触れない byte の元の値が残る。page 単位の消去が要るので `--erase chip|none` とは使えない(usage error)。消去済みのセルが `0xff` を返す family(profile の `erased_reads_ff`)だけ。V20x / V30x は消去済みが `0xe339e339` で、空きと実データを区別できないので、消す前に断る(capability-unsupported)。
- **`--reset none` の注意**: WCH-LinkE は書込の前後で RCC の周辺クロック enable と SysTick を 0 にし、**戻さない**(線上で確認。protocol §7a)。`--reset none` で終えた chip はその状態のままなので、アプリを reset せずに再開すると周辺が動かない。`--reset run`(既定)と `halt` は reset を通るので影響しない。
- **`--preverify`**: 破壊操作の前に image 領域を read して現在値と比較し、**全一致なら erase/program をまるごと省略**(flash cycle と摩耗を節約)。一致時は `preverify: target already matches - skipped`(JSON `skipped:true`・`scope:"none"`・`verified:true`)を出し、**reset 方針だけは適用**(reset run 済みの target をちゃんと走らせる)。不一致なら link 状態を戻して通常の flash に流れる。reset+finish の末尾は `reset_and_finish` に切り出して通常経路と skip 経路で共有。実機検証(L103): 同一 image を再 flash → skip、別 image → 通常書込に流れて反映を確認。
- **`--sdi` / `--monitor` / `--repeat`**: **`--sdi on|off`**: 書込+reset 後に probe の SDI print forwarding を設定(`set_sdi_print_enabled`)。programming は成功済みなので失敗しても flash は失敗させず warning 止まり。**`--monitor uart|sdi|dmdata|rtt`**: 書込+reset 後にそのまま monitor session へ移行(Ctrl-C まで、`--duration` でバウンド可)。実装は reset+結果出力の末尾を `finish_flash`(session を値で受けて drop→USB 解放してから monitor が probe を開き直す)に集約し、通常経路・preverify skip 経路の両方から `--sdi`/`--monitor` が効く。**`--repeat`**: `flash` を dispatcher にして `flash_once` をループ。1 台焼く→operator が外す(AttachChip 失敗を poll)→次を挿す(AttachChip 成功を poll)→焼く、を Ctrl-C まで。失敗 board は報告して次へ。実機検証(L103): `--sdi on` が program+設定を完了、`--preverify --monitor dmdata --timeout 4` が skip→dmdata monitor 移行→bound 終了、`--repeat` が #1 を焼いて removal 待ちに入る(remove→insert→再焼きの完全周回は物理 target 交換が要るため未検証)。

#### verify / read / write / erase

```text
ch32rv verify <FILE> [--format ...] [--at ...]                    不一致は exit 30
ch32rv read  (--range <addr>[+len|..end] | --region <r>[+off][+len])
             [-o <file>|-] [--format bin|hex-dump|ihex] [--blank-check]
ch32rv write (<FILE> | hex:<bytes> | word:<u32>) --at <addr|region[+off]>
             [--erase auto|none]                                       上級。RAM/peripheral=DM 直書き、flash=controller の page RMW(--erase auto で消去)。書込後 readback verify(不一致 exit30)。region: code/flash/ram/option。低位 flash alias は物理番地を案内して拒否
ch32rv erase (--all | --region <r> | --range <a>..<b>)                範囲指定は必須(暗黙の全消去をしない)。--all の名は global --chip <SKU> との衝突回避
```

領域名は `code` / `system`(bootloader)/ `option` / `eeprom` / `ram`。minichlink の `flash` / `bootloader` 名は別名として受ける。**`option` は family 依存**(CH32M030 = `0x1FFF_F300`)なので、`read --region option` と `write --at option` は**attach 後に DB の base で解決し直す**(`erase` は option 領域を拒否するので影響なし)。

**WCH-Link での `flash` の書込経路**: 既定は probe へ WCH の flash loader stub を載せて image を流す経路。**stub が無い family では、DB に loader の plan があれば ch32rv の RAM loader で書き**([oep-host.ja.md](oep-host.ja.md) §5。DMI で 1 語ずつ書くより数倍速い。H417 の 64 KiB で 8 秒と 61 秒)、**無ければ検証済み FLASH-controller profile で、下の直接 FLASH controller 経路で page 単位に書く**。page の埋まっていない部分は `0xff` を書く(消去直後の page の値なので stub 経路と同じ結果)。`--restore-unwritten` は program 前に page を合成するので、どの経路でも意味が変わらない。直接 FLASH controller 経路は **stub 経路より遅い**(word ごとに write + BUFLOAD + busy 待ちの DMI 往復が要る)。

**`erase --range` / `--region`**: `--all`(chip 全消去)に加え、**page 単位の部分消去**を持つ。WCH-Link stub の write 経路は部分書き込みを受け付けない(probe が reason 0x55 で拒否)ため、**FLASH controller(0x4002_2000)を DMI 経由で直接叩く**経路を使う(`DebugModule::flash_page_erase` / `flash_program_page`。KEYR/MODEKEYR unlock → FTER/FTPG + STRT/PGSTART → STATR busy 待ち)。消去は page 粒度なので **start と length を page 境界に揃えることを必須**とし(fail-closed。ズレは Usage error + page サイズを提示)、隣接 page を巻き込まない。`--region code[+off[+len]]` は probe 報告の flash サイズから解決。program は family 別に 3 方式(PgStart=V20x/V30x、Buffered=V003/V00x/X035/L103、V103=標準 16bit halfword+commit)。**対応 family: V20x/V30x・V003/CH641(64B)・V00x(256B)・X035/CH643・L103・V103(128B)**。実機検証: V307 で page1 だけ消去し page0(reset vector)/page2 無傷、V003 で 64B page、V103 で 128B page 消去(隣接無傷)、**L103 で 256B page を surgical に消去(先頭 firmware・中間 blank 域とも無傷、program/verify 往復 OK)**。**注意: 消去済みセルの read 値は family 差あり(RM の系統 B=V20x/V30x=`0xe339e339`、系統 A=X035/V003=`0xff`)**ので、erase 完了判定は read でなく controller の STATR(BUSY クリア + WPRERR 無し)で行う。この直接 FLASH controller 経路は flash SW breakpoint(trigger 無し core)と option byte 書き込みの土台にもなっている。

#### reset / run / recover

```text
ch32rv reset [--halt] [--dm] [--confirm-run]      既定: reset して実行、detach。**soft reset 後に `ackhavereset` → DMSTATUS → halt なら resume**(probe の reset 単体では attach 由来の halt 要求で hart がリセットベクタに止まる。CH32V00x の DM は `havereset` を ack するまで halt/running bit が固着するので ack が先。V006 実測)
ch32rv run <ELF> [--no-flash] [--source dmdata|rtt]
             [--exit-on semihosting|timeout] [--duration <s>]  target の結果を tool の exit で返す(HIL 用。target が 0 なら 0、それ以外は 60、target のコードは result.exit)。
                                                              出力は DMI source のみ(uart/sdi は monitor)。stdin は target へ
ch32rv recover                                    診断のみ(何も書かない): 状態と推奨を表示
ch32rv recover --method auto                      診断して推奨を適用(各処理の確認プロンプトあり)
ch32rv recover --method power-off|nrst|unprotect|unbrick
             [--chip <family>]                    特殊消去(power-off/nrst)は --chip 必須
             [--image <FILE>]                     OEP の probe の --method nrst だけ。消去の後にこの image を書く。ほかでは exit 24
```

- **`recover` の診断(method 省略)**: 指定 `--speed` で attach し、応答が無ければ low で再試行 → halt → option bytes を 2 回読む → resume。書込は一切しない。状態は `healthy`(保護 off、DB が定義する USER bit が復位値)/ `read-protected`(RDPR≠`0xA5`)/ `option-nonstandard`(DB 定義の USER bit が復位値と違う。違う field 名を表示)/ `option-unreadable`(2 回の読みが不一致、または RDPR・USER の補数が合わない)/ `unreachable`(どの speed でも応答なし)。推奨はそれぞれ なし / `recover --method unprotect` / `target option reset` / `recover --method unprotect`(一律 image へ fallback)/ `recover --method power-off --chip <family>`(LinkE/LinkW のみ。それ以外の probe では手動の電源サイクル)。low でしか応答しなかった場合はその旨を note に出し、推奨コマンドに `--speed low` を付ける。JSON は `result.state` / `result.recommendation.{action,command}` / `notes` / `caveats`。exit は診断ができれば状態によらず 0。
- **`--method auto`**: 診断の推奨をそのまま実行する(`unprotect` / `option reset` は診断で応答した speed で attach、power-off は `--chip` 必須で確認プロンプト付き)。`healthy` と手動しかない場合は診断結果を出して何もしない。`option reset` は Data0/1 と WRP も工場値(`0xff`)に戻す点に注意(DB が知らない多 bit の USER field は保持)。
- **power-off の注意(診断・auto が表示)**: 名前に反し、WCH-LinkE fw 2.22 はこの消去の間も 3V3 を切らず、RST を pulse していた(target なしの実測、wch-protocols E167)。RST を target の NRST に配線すると確実になる可能性がある。NRST 未配線でも電源は落ちない(RAM の目印が残る。wch-protocols E168)。正常な target なら約 20 ms で halt を取って全消去する。attach で止まった target では halt が取れず `0x00` になるが、その間に電源が落ちないまま reset がかかり(出どころは未確定)、アプリが起動し直してから、次の回で halt を取って消える(推定、E168)。ch32rv が `0x00` のときに繰り返すのはこのため。reset を起こしているのは 0f のコマンドそのもので、待っても AttachChip を繰り返しても target は戻らない(E169)。1 回目の `0x00` の直後は flash を消さないまま AttachChip が通り、hart は halt している(E169)。ch32rv はこれを使って消さずに直す経路を持たない。attach で止まった CH32X035 ではこれで戻ったが、通るかは target の状態による。結果は `read --blank-check` か通常の attach で確かめる。
- **特殊消去(`power-off` / `nrst`)の確認**: probe の応答 status を見て、`0x00`(消えていない)なら最大 3 回まで繰り返す。`0x0f`(CH32X035 で消去を確認した値)が返れば完了、返らなければ warning `special-erase-unconfirmed` を出して `read --blank-check` を案内する(他 family の応答は未記録なので exit は 0)。JSON は `attempts` / `answers` / `confirmed`。
- **止まった書き込みの後始末**: WCH-Link で `flash` / `erase` / `write` を始める前に FLASH_CTLR を読み、前の書き込みが残した消去・書き込みのモードのビット(PG、PER、MER、OBPG、OBER、STRT、FTPG、FTER、BUFLOAD、BUFRST、PGSTART)を下ろす。見つけたら warning `flash-controller-reset`(`flash` と `erase --all`)。理由: DMI でページを消している途中でプロセスが止まると FTER が立ったまま残り、WCH の stub はチップが reset されるまで、どの image も 1 ページ目しか書けない(CH32V003、chip erase でも同じ)。走っていた hart は確かめた後に走らせ直す。
- **CH32X035 の attach 停止**: HPRE の bit 3(CFGR0 bit 7: HCLK /2 や /4)を立てて走る X035 は WCH-LinkE の attach で止まり、以後 no target になる(option bytes は変わらない。wch-protocols `protocols/pc-to-link.ja.md#x035-hpre-stop`)。診断は `--chip` 無しか X035 のとき、`unreachable` の note でこれを挙げる。復旧は power-off。
- 診断が見ない brick: `RST_MODE` 等の**多 bit USER field は DB に無い**ため `option-nonstandard` として検出できない(CH32X035 の `RST_MODE=00` で PA21 を外部 reset にして保持され attach 不能になった例は `unreachable` 扱い)。reset 直後/電源投入直後に attach して消去せずに option を直す経路(第 2 段)は、**host が自分で窓を作ることは WCH-LinkE(fw 2.22)ではできない**(wch-protocols E166 / E167): 応答しない target に halt を取るまで食い下がるのは特殊消去の中だけで(その中で flash は消える)、`probe power 3v3 off` は target の電源を切らず、AttachChip が成功していないと DmiOp は target に届かない(`0xffffffff` / status 3)。なお特殊消去は「電源を入れ直した直後の起動の窓」を使っているのではない。電源は一度も落ちず(E168 の RAM の目印)、正常な target は reset もされない。ただし E169 で、止まった X035 では 1 回目の特殊消去(`0x00`、消えない)が target を reset し、その直後の AttachChip が flash を消さないまま通る。ch32rv はこれを使う経路を持たない。それ以外で消さずに直すには、NRST の配線か、host が target の電源を切れる配線が要る(OEP probe など)。

- **`recover` の option 書込**: `unprotect` と `unbrick` の保護解除は、**現在の option bytes を読んで RDPR だけ `0xA5` に差し替える**(他の byte は一切変えない)。読めない/補数が整合しない場合のみ一律 image(RDPR off + `0xff`)へ fallback し、警告を出す。保護解除が SRAM 分割や NRST の機能を書き換えないための措置。
- `recover` の method は別 operation として固定する: `power-off`(給電断 erase)、`nrst`(RST ピン erase。配線要件を事前表示)、`unprotect`(RDP 解除 = 全消去。確認プロンプト)、`unbrick`(電源サイクル + DM 連打 + option 工場値 + 全消去。minichlink 手順の移植)。
- attach 手段としての connect-under-reset は recover ではなく global `--connect-under-reset`。

### 4.2 probe

```text
ch32rv probe list [--watch]                       --json に selector 全 key、Windows は interface ごとの bound driver 名。
                                                  各行に kind(wchlink / oep)。OEP の USB の probe は USB の記述子だけから出し、開かない
                                                  (serial = unit id)。USB の同一性の無い UART bridge の OEP の probe は出さない。
                                                  表の列は中身の幅に合わせる。
                                                  先頭の列 ADDRESS(JSON `address`)はそのまま --probe に渡せる住所: WCH-Link は
                                                  wchlink://<serial>(serial が無ければ wchlink://usb-<bus>-<ports>)、OEP は oep://<unit id>。
                                                  並びは種類(WCH-Link、USB の OEP、ネットワークの OEP)→ USB の位置を数として比べた順
                                                  (ネットワークは unit id 順)。OEP の FIRMWARE は fn 0 の describe を lock 無しで短く読む
                                                  (ブローカーがあればその link、無ければ serial port を短く開く。読めなければ -)
ch32rv probe info [--probe <sel>]                 型番/HW/FW 版(raw・正規・WCH 表記)/mode/serial/interface 構成/使用中。
                                                  OEP の probe(`oep://<probe>`、OEP の serial port)は、lock 無しで fn 0 の describe
                                                  (firmware、model、unit id、chip、max_op_ms)、interface の一覧、ラベルの口を出す
ch32rv probe power <3v3|5v> <on|off>
ch32rv probe power cycle [--off-ms 300]
ch32rv probe mode get
ch32rv probe mode set <riscv|dap> [--yes]
ch32rv probe firmware info                        版(0 埋めしない "2.22"、WCH 表記 v42、raw 0216)、variant、mode、既知不良の判定(ch32-device-data の `index/link_firmware_issues.csv` から生成した表を variant・mode・版で照合。hash では照合しない。probe が mode を返さないときは mode を問わず当てる。文面(warning `fw-known-bad` の msg、`known_bad_reason`)は表の symptom と works_in から作り、約束しない)
ch32rv probe firmware check [--min <ver>]         CI 用。不良版・版不足なら exit 12(実機: LinkE 2.22 は --min 2.20 通過/2.30 で exit12、CH549 2.12 は --min 2.20 で exit12)
ch32rv probe firmware update --image <FILE> [--yes]  IAP 経由で probe 自身の firmware を書換(実機: LinkE 2.22 ⇔ 2.13 を相互に更新し版を確認)
ch32rv probe firmware exit-iap                     IAP mode の probe を、何も書かずに今入っている app で起動させる
```

- **`probe power` は target の電源を切れないことがある**: WCH-LinkE fw 2.22 は `3v3 off` / `5v off` を受理する(rc 0)が、LinkE から給電している CH32L103 / V203 / V003 / X035 で、off の間も target は走り続けた(UART が途切れず、X035 は loop counter も連続。wch-protocols E166)。**回り込みではなく、出力がそもそも切れていない**: target をつながない LinkE の 3V3 を ADC で測ると、`3v3 off`(`0a`)の後も `5v off`(`0c`)の後も 3.24 V のままだった(最小 3.1 V、E167)。`probe power` は off / cycle のたびに warning `power-off-unverified` を出す。特殊消去の中でも 3V3 は切れず、RST が pulse する(E167。§4.3)。RST 未配線のとき、正常な target は特殊消去で reset されない(havereset なし、RAM の目印が残る。E168)。attach で止まった target では、電源が落ちないまま reset がかかる(出どころは未確定)。
- firmware 版は **raw byte・正規化表記(2.12)・WCH 表記(v32)を常に併記**し、比較は正規化値で行う(表記系の混同と probe-rs の版比較バグを構造的に避ける)。
- `firmware update` は IAP mode(`4348:55e0`)への遷移・書込・再 enumeration 待ち・版確認までを 1 操作にする(protocol は wch-protocols `protocols/pc-to-link.ja.md#iap`)。**既に IAP に滞留した個体はそのまま書ける**(entry 不要)ので、中断した更新の復旧経路が同じコマンドになる。image は同梱しない(user-supplied)。
- **image は同梱しない**。WCH の [WCH-LinkUtility](https://www.wch.cn/downloads/WCH-LinkUtility_ZIP.html)(**中文サイトのみ**。英語サイトの同名ページは開いても内容が出ない)を展開した `Firmware_Link/` に平文で入っている(WCH-LinkE = `FIRMWARE_CH32V305.bin`)。MounRiver Studio 同梱版でも同じ。`--help`・エラー hint からもこの入手先を案内する。
- **IAP entry は不揮発**: probe を IAP に入れると、app が無傷でも電源再投入では戻らない(protocol §6.1)。抜けるのは終了 frame だけなので、`firmware exit-iap` を脱出口として持つ。中断した更新は `firmware update` の再実行でそのまま復旧する(どの時点で切れても IAP に残る)。
- image は渡された時点で検査する: **BL 付き `*_APP_IAP.bin` は拒否**(外部書込機用。IAP に流すと app 領域に BL を書く)、非 RISC-V image は拒否、版は image 内の `bcdDevice` から読む。
- **image の型番は sha256 で照合する**。WCH-LinkUtility が配る image を ch32-device-data の `index/link_firmware.csv` から `cargo xtask db-gen` が `crates/target/generated/link_firmware.csv` に作り、image の sha256 で引く。表の語彙:
  - `device`: probe の製品名(`WCH-LinkE` / `WCH-Link`(CH549)/ `WCH-LinkW` / `WCH-DAPLink`)。probe の variant と照らす(`linke` = `WCH-LinkE` など。対応は `crates/target/src/link_variant.rs` の 1 か所)。
  - `mode`: CH549 の WCH-Link の image だけが持つ `RISC-V` / `ARM`。ほかの型番は空(1 本で両方)。CH549 はこのコマンドで更新しないので照合には使わない。
  - `role`: `application` = `FIRMWARE_*.bin`(application だけ。IAP 経路で書くのはこれ)、`with-bootloader` = `*APP_IAP*.bin`(IAP bootloader の後に application。外部書込機用)。
  - `version`: その image で probe が返す版。

  | image | 結果 |
  |---|---|
  | 表にあり `role` = `with-bootloader` | 拒否(exit 2)。hint に同じ型番の `FIRMWARE_*.bin`。`--dry-run` でも拒否 |
  | 表にあり `application`、`device` が probe と一致 | 警告なしで書く |
  | 表にあり `application`、`device` が probe と違う | 拒否(exit 2)。hint にその probe の `FIRMWARE_*.bin` |
  | 表にない(版違い・手製の image) | warning `image-model-unverified` を出して書く |
  | probe が IAP mode に居る(型番を返さない) | 型番は照合できないので warning `image-model-unverified`(表にあれば、どの型番の image かを書く) |

  `--dry-run` と `--json` の `result.image` は `sha256` と `known`(表の行。無ければ `null`)を出す。上書きの flag は無い。
- 対応 probe: WCH-LinkE / LinkW / LinkS / CH549 の WCH-Link を型番として区別し、非対応 operation は capability で事前に弾く。互換 probe(funprog HID / NHC-Link042 / ardulink)は扱わない(rv003usb 系の HID bootloader は `boot hid`)。

### 4.3 target

```text
ch32rv target list [--identify [oep|all]]         target に届く場所の一覧。列と並びは probe list と同じ先頭(ADDRESS、MODE、
                                                  TOPOLOGY、MODEL。種類 → USB の位置)に、チップの列(CHIP、CHIP ID、UID、
                                                  FLASH、SRAM)が続く。WCH-Link、USB の OEP の口(ラベルを lock 無しで短く読む)、
                                                  ネットワークの OEP、CH32RV のアプリ、ISP / HID の bootloader。既定では何も
                                                  attach しない。`--identify` は OEP の口に止めずに attach し、子 process の
                                                  `target info` でチップの列を埋める(列は変わらない。読めなければ短い理由、
                                                  全文は JSON)。`--identify all` は WCH-Link も attach する(target を止め、
                                                  clock を変える)。CH32RV のアプリ(USB の serial がチップの UID)は、同じ UID を読んだデバッグの場所があれば、そのチップの列を出す(JSON は `runs_on` にその場所)。ADDRESS はそのまま --probe に渡せる
ch32rv target info                                chip ID/family/SKU 候補(根拠付き)/UID/flash size/保護状態/option 要約/verified

ch32rv target option get                          option bytes を key ごとに値の語と生の値で表示
ch32rv target option set <key>=<value>...         例: stop-reset=on reset-pin=off can-bus-off=mode2
ch32rv target option reset                        工場出荷値
ch32rv target option write-raw <hex> [--yes]      生値(expert)
ch32rv target protect <on|off> [--yes]            off は全消去を伴う旨を明示
```

WCH-Link の `target info` は未登録チップでも接続成功とDB照合結果を分けて表示する。
`connected (unregistered)` は、応答を取得したがDBにIDが無い状態。JSONの `result` に
`connection` (`connected`)、`identification` (`identified` / `ambiguous` / `unregistered`)、
生の `family_byte`、`silicon_revision`、DB由来の `sram_bytes` (未解決時null) を出す。
既存の `target.chip_id` / `uid` / `flash_bytes` も取得できたものは未登録時に残す。
収載前のデータは暫定overlayに隔離し、`target.provisional` と警告で明示する。
容量欄が0または消去済みパターン (`0xffff` / `0xe339`) なら、一意に解決したSKUのDB容量を使い、
未解決なら容量はnullとする。`result.flash_capacity_raw` は生のKiB欄をhexで残し、
`flash_capacity_source` (`probe` / `db` / `unavailable`) で出所を示す。X315のDB容量192 KiBは
ゼロウェイト領域で、総Flash480 KiB (192 + 288)とは区別する。

- **option bytes の語彙(`target option set` と `isp config set` で同じ)**。USER byte の option は WCH の EVT の `FLASH_UserOptionByteConfig` の定数の名前で書き、極性は値の語が持つ(bit の値を書かない)。family が持たない key は usage error(exit 2)で、hint にその family の key を全部出す。**USER の意味名・値の語・EVT の定数は手書きせず、ch32-device-data の `index/option_byte_fields` の `option` / `evt_argument` / `evt_values` 列から `cargo xtask db-gen` が作る**(`crates/target/generated/option_fields.csv`。`option` の無い field だけが `user.<field>`)。

  | key | 値 | EVT の定数(USER の bit) | 極性 |
  |---|---|---|---|
  | `read-protect` | `on` / `off` | (RDPR。`off` = `0xA5`) | `on` は RDPR を `0xA5` 以外にする(debug 線は `0xFF`、ISP は `0x00`)。状態が変わるときだけ書く |
  | `write-protect` | `<mask>`(u32) | `FLASH_EnableWriteProtection` の `FLASH_WRProt_Pages*`(WRPR0–3) | 立った bit の sector(群)を保護(DB の WRPR の行の `evt_values` = `1=protected`。違えば db-gen が止まる)。WRPR はその反転で、`get` の `raw` は WRPR の値 |
  | `data0` / `data1` | `<byte>`(10 進か `0x`) | (Data0 / Data1) | — |
  | `iwdg` | `sw` / `hw` | `OB_IWDG_SW` / `OB_IWDG_HW`(bit 0) | `sw` = 1 |
  | `stop-reset` | `on` / `off` | `OB_STOP_RST` / `OB_STOP_NoRST`(bit 1) | `on`(STOP で reset)= 0 |
  | `standby-reset` | `on` / `off` | `OB_STDBY_RST` / `OB_STDBY_NoRST`(bit 2) | `on`(STANDBY で reset)= 0 |
  | `reset-pin` | `off` / `12ms` / `1ms` / `128us` | `OB_RST_NoEN` / `OB_RST_EN_DT12ms` / `_DT1ms` / `_DT128us`(bit 4:3) | `off`(NRST を使わない)= 11b、`12ms` = 10b、`1ms` = 01b、`128us` = 00b |
  | `reset-pin`(CH32M030) | `off` / `1ms` / `512us` / `128us` | `OB_RST_NoEN` / `OB_RST_EN_DT1ms` / `_DT512us` / `_DT128us`(bit 4:3) | 11b / 10b / 01b / 00b |
  | `reset-pin`(CH32X315) | `off` / `on` | `OB_RST_NoEN` / `OB_RST_EN`(bit 3) | `off` = 1 |
  | `reset-pin-select`(CH32M030) | `pa15` / `pc0` | `OB_RST_PIN_PA15` / `OB_RST_PIN_PC0`(bit 5、RST_PIN_SEL) | `pa15` = 1 |
  | `over-voltage`(CH32M030) | `on` / `off` | `OB_OV_CFG_EN` / `OB_OV_CFG_NoEN`(bit 7、OV_CFG) | `off` = 1 |
  | `start-mode`(V003 / V006 / X315) | `boot` / `user` | `OB_PowerON_Start_Mode_BOOT` / `_USER`(V003 / V006 は bit 5 START_MODE、X315 は bit 4 START_MD) | `boot` = 1 |
  | `can-bus-off`(L103 / V205) | `mode1` / `mode2` | `OB_CAN_BusOff_MODE1` / `_MODE2`(bit 5、CFGCANM) | `mode1` = 1 |
  | `ram-split`(V407) | `mod1` / `mod0` | `OB_RAM_CODE_MOD1` / `_MOD0`(bit 6、RAM_CODE_MOD) | `mod1` = 1 |
  | `usbfs-download`(H417) | `on` / `off` | `OB_USBFSDL_EN` / `_NoEN`(bit 6、USBFSDLEN) | `on` = 1 |
  | `usbhs-download`(X315) | `on` / `off` | `OB_USBHSDL_EN` / `_NoEN`(bit 6、USBHSDLEN) | `on` = 1 |
  | `usart-download`(H417 / X315) | `on` / `off` | `OB_USARTDL_EN` / `_NoEN`(bit 7、USARTDLEN) | `on` = 1 |
  | `user.<RM の field 名>` | bit の値(`0`〜) | DB に `option` の無い USER の field(DB の `option_fields`。いまは V103 の `PORCTR` だけ) | 変換しない(生の bit の値) |

- **値の語の規則**(db-gen が `evt_values` の定数から作る。手で付けない): その option の全部の定数に共通する、`_` で終わるいちばん長い頭(ふつうは引数名 + `_`。`OB_RST_`、`OB_PowerON_Start_Mode_`、`OB_CAN_BusOff_`、`OB_RAM_CODE_`)を除き、残りが `RST` / `EN` なら `on`、`NoRST` / `NoEN` なら `off`、`EN_DT<時間>` なら `<時間>`、それ以外は小文字にして `_` を `-` にする(`OB_IWDG_SW` → `sw`、`OB_RST_EN_DT512us` → `512us`、`OB_RST_PIN_PA15` → `pa15`、`OB_RAM_CODE_MOD1` → `mod1`)。field の値ごとにちょうど 1 つの語が付かない・語が重なる・その形にならない定数があれば db-gen が止まる。
- family ごとの意味名(DB の `option` 列。EVT の各 `*_flash.c` の `FLASH_UserOptionByteConfig` の引数): V103 / V20x / V307 = `iwdg` `stop-reset` `standby-reset`。V407 = その 3 つ + `ram-split`。L103 / V205 = その 3 つ + `can-bus-off`。X035 = その 3 つ + `reset-pin`。V003 / V006 = `iwdg` `standby-reset` `reset-pin` `start-mode`(`stop-reset` は無い)。M030 = `stop-reset` `standby-reset` `reset-pin` `reset-pin-select` `over-voltage`(`iwdg` は無い)。X315 = `iwdg` `reset-pin` `start-mode` `usbhs-download` `usart-download`。H417 = `iwdg` `usbfs-download` `usart-download`。
- `user.<field>` は `option` の無い field だけに使う(いまは V103 の `user.PORCTR`)。意味名のある bit を `user.STOPRST` / `user.CFGCANM` や素の `STOPRST` で書くと、意味名での書き方(値の語と EVT の定数)を示して usage error。DB が復位値を持たない意味名の field(V006 の RST_MODE、V407 の RAM_CODE_MOD)も同じ。field 名の大小文字は区別しない。
- 資料の食い違いは ch32-device-data の `index/conflicts.csv` で見る(`table` = `option_byte_fields` の行。`field` が争点の列)。**`field` が `bits` か `default`(書く値そのもの)、または空(争点の列を 1 つに決められない)の行があれば db-gen が止まり**、family・subject・残した値・異議の値を出す(ch32-device-data に解決を頼む)。`field` が `field`(field 名の綴りだけ。いまは M030 の RST_MODE、V003 / V205 の IWDG で、RM の中国語版と英語版の綴り違い。bit の位置と `option` / `evt_*` 列は一致)の行では止まらず、その行はほかの行と同じに使う。db-gen は `option_byte_fields` の `confidence` を読まない。
- `get` は 2 つのコマンドで同じ形(`docs/contract/commands.schema.json` の `option_bytes`): `family`、`db_family`、`raw`(読んだ byte の hex。debug 線は補数込み 16 byte、ISP は config 12 byte)、`read_protected`、`write_protected`、`options`。`options` は `set` が受ける key ごとに `{"value": "<値の語>", "raw": <生の値>}`(例 `"stop-reset": {"value": "on", "raw": 0}`、`"user.PORCTR": {"value": "1", "raw": 1}`)。`value` はそのまま `set` に渡せる。文字の出力も `key=value` を 1 行ずつ出す。USER の配置を知らない family は `read-protect` / `write-protect` / `data0` / `data1` だけを出し、`warning[option-decode-interim]`。
- `target option set` は現在値を読み → 指定の field だけ変え → **補数を全部計算し直し** → 直接 FLASH controller 経路(§4.1 の `erase --range`)で erase+program → reset してから read-back で照合(違えば exit 30)。反映は system reset 後。変更は必ず確認する(`--yes`)。**保護中の `read-protect=off` は全消去、`read-protect=on` は読めなくなる**旨を確認文に出す。実機(L103、意味名にする前の RM 名の経路): STOPRST の bit を 0 にして USER `0xff→0xfd`(補数 `00→02`)、read-back で反映、1 に戻して復元。
- 「未対応 SKU」と「DB に無い SKU」は JSON で区別する(exit 20 の detail)。
- `option get` は option bytes(DB の base から 16 byte)を DMI で読む(読み取り専用)。実機: V203 / V307 / V003 / X035 で RDPR=`0xA5`(保護なし)と補数の整合を確認。
- **option 書込 `write-raw` / `reset` / `protect`**: option-byte programming を `DebugModule::flash_program_option_bytes`(minichlink の option 経路から転記)で実装。手順: FLASH_KEYR + OBKEYR(`0x4002_2008`=STM32F1 系 OPTKEYR)+ MODEKEYR unlock → CTLR の OPTER+OPTWRE+STRT で **全 option byte 消去** → 8 halfword を OPTPG+OPTWRE+STRT で書込(value+complement をそのまま。complement は呼び出し側責任)。**RDPR(halfword0)を最初に書く**ことで消去〜再書込の read-protect 窓を最小化。反映は system reset 後。**verify は soft reset を挟んでから read-back する**: V103 は option 領域を reset まで書込前の像で読み続けるため、直後に読むと**書けているのに `verify-mismatch`(exit 30)**を返す(CH549 Link + V103 で実測)。`option get` と同じ 16 byte レイアウト。**`write-raw <hex>`**(expert、16 byte)は RDPR≠`0xA5` なら「読み出し保護 ON になる」警告して `--yes` 必須。**`reset`** は「工場デフォルト」を**一律 `0xff` で作らない**: 現在値を読み、**DB が定義する USER bit だけ RM の復位値へ戻し、DB が知らない bit は現在値を保つ**。Data/WRP は `0xff` にクリア。理由は実測([measured/option-bytes-2026-09-06.md](data-requests/measured/option-bytes-2026-09-06.md)): 出荷値は V003=`0xf7`(`RST_MODE`=`10b`)・V20x=`0x3f`・V307=`0xbf`(`RAM_CODE_MOD` は個体差)で、**`0xff` を書くと NRST の機能や SRAM/flash 分割が変わる**。**`protect on|off`** は現在値を読んで RDPR のみ変更(on=`0xFF`、off=`0xA5`)、他 byte 保持。**`off` は全 flash mass erase を伴う**旨・**`on` は読めなくなる**旨を明示し `--yes`/確認必須。既に目的状態なら no-op。**OB base は DB 由来**: `option_bytes.csv` の family 別 base を生成 DB 経由で引く。**共通番地ではない**(CH32M030 = `0x1FFF_F300`)ため決め打ちせず、**DB に無い family は fail-closed**(capability-unsupported。推測して別番地を読み書きしない)。読み(`option get`)・書き(`write-raw`/`reset`/`set`/`protect`)・`recover` の RDPR 判定と工場値書込のすべてがこの base を使う。なお DB は L103/M030/V006/V205/X035 の option 書込を fast page(FTPG)手順に分類しているが、**本実装の half-word(OPTPG)経路は L103 実機で動作確認済み**なので分類では gate せず、未検証の FTPG 分類 family でのみ警告を出す。実機検証(L103): `write-raw` に現在値を round-trip 書込 → 不変・RDPR=`0xA5` 維持・flash 無傷・走行復帰、`reset` も同様、fail-closed(bad length / RDP-danger without --yes / protect off 既off の no-op / protect on without --yes 拒否)。`protect on/off` の実トグルは破壊的(on=保護・off=全消去)のため実機未トグル(primitive は round-trip で検証済み)。programming 失敗時は option が消去済み=read-protect の可能性を error に明示し `recover` を案内。

### 4.4 dbg

```text
ch32rv dbg halt [--reset]     ch32rv dbg resume [--all]     ch32rv dbg step [N]
ch32rv dbg regs                                     GPR + pc(dpc)一括
ch32rv dbg reg read|write <x1..x31|pc|csr:<addr>> [<value>]
ch32rv dbg dmi read|write <addr> [<value>]          DM レジスタ直接(expert)
```

- **`dbg reg` / `dbg dmi`**: `dbg reg read|write` / `dbg dmi read|write`(cmd_dbg.rs)。`reg` 名は `x0..x31`/`pc`/`dpc`/`csr:<addr>`/CSR 別名(misa/mstatus/mcause/mepc/mtval/dcsr/mvendorid/marchid)、アクセス時に halt。`dmi` は DtmAccess の生 DM レジスタ read/write(例 0x11 DMSTATUS、0x10 DMCONTROL)。実機(L103): `dbg reg read misa`=0x40901105(`csr:0x301` と一致)、`dbg dmi read 0x11`=0x00000382/`0x10`=0x80000001。write は expert(走行中プログラムを乱す)。パースは単体テスト。

### 4.5 monitor

```text
ch32rv monitor [--source uart|sdi|dmdata|dmseq|rtt|fixture-uart]
  --source 省略時: WCH-Link では uart、OEP の probe では target の console(dmseq)
  --port <serial port の path>   省略時は --probe の probe の CDC(どの probe かは --probe で選ぶ)
  --baud 115200      uart と fixture-uart の速さ。ほかの source には当てはまらない
ch32rv monitor list [--json]                        候補 port と役割(uart/sdi)の対応
ch32rv monitor sdi <on|off>
```

- **入力**: `uart` / `dmdata` / `dmseq` / `rtt` は stdin を target へ流す(端末なら行入力、pipe なら EOF で送信終了)。`sdi` は受信のみ。
- **出力**: stdout へ生 byte。`--json` 時は §3.5 の `output` event(stderr)。
- **終了**: Ctrl-C か `--duration`。device 喪失(CDC の EOF / DMI 失敗)は exit 0 ではなく 10 / 40 で終わる(wrapper が再起動を判断できるように)。再 enumeration の追従は持たない。
- **attach は target のクロックを変える**: WCH-Link の AttachChip は、**CH32L103 / V20x / V30x / X035 では**クロックを系統ごとの決まった設定に組み直し、**元に戻さない**(L103 / V20x / V30x は PLL の設定へ、X035 は AHB の分周を外して flash の wait を 2 に。L103 / V203 / X035 は線上で、V307 は target 自身に UART で報告させた試験で確認し、L103 / V203 / V307 ではアプリの UART が実際に化けた。**V003 は RCC に触れない**。その他の family は未観測。protocol §7a)。`dmdata` / `dmseq` / `rtt` / `sdi` はどれも attach を通るので、観測中の firmware は **次の reset まで probe のクロックで走る**(UART の baud・タイマ・PWM がずれる。V20x / V30x で HSE + PREDIV / PLL2 を使う firmware は PLL の入力も変わる)。元の値は AttachChip の中で上書きされるので ch32rv は戻せない。monitor は開始時に、**全 target で** warning `attach-reclocks-target` を「クロックが変わっている可能性がある」という形で出す(family ごとの全体像はまだ埋まっておらず、断定すると外れた family で誤りになるため)。**firmware 自身のクロックのまま見たいときは attach のあとに reset する**: `ch32rv run <elf> --no-flash --source dmdata|dmseq|rtt`(接続 → DMI reset-halt → 流す。firmware は再起動する)か、ボードの reset。attach しないのは `uart` だけ。同じことは `target info` / `read` / `dbg` / `arduino monitor` にも当てはまる(警告は monitor だけが出す)。
- **CH32V103 は attach で program を起動し直す**: WCH-Link の AttachChip は V103 の生きた GPR s1(x9)を上書きし(WCH-LinkE では chip id、WCH-Link(CH549)2.12 では `0x0297df3f`。元の値はどこにも残らない)、hart を止める。Link は DetachChip で(`dbg halt` の後でも)hart を走らせるので、そのままでは program は次に s1 を使ったところで落ちる(mcause = 4)。そのため WCH-Link 越しに V103 に attach するコマンドは、reset していなければ離す前に soft reset で program を起動し直す。target を走らせたまま持つ `monitor`(dmdata / dmseq / rtt / sdi)、`arduino monitor` の rtt / sdi、WCH-Link のブローカーは、attach の直後に起動し直す。V103 での Link の高速読み出しは attach で Link が用意したものの上で動き、program が走った後は 0 を返して次の DMI 書き込みを失敗させる(WCH-Link(CH549)2.12 で実測)ので、V103 の RTT とブローカーのメモリ読み出しは Debug Module で読む。warning `attach-corrupts-register` を出す。gdb は前から attach の後に起動し直している(§gdb)。OEP の probe の attach は AttachChip を使わないので当てはまらない。
- **CH32V205 も同じく起動し直す**: WCH-LinkE(2.22)の AttachChip は、V205 の family(DB では CH32V205 と CH32V203CCT6)の `s0`(x8)と `dscratch0` を `0xE000D004` にしたまま戻さない(接続の前に送るチップの種類の値を変えても同じ。元の値はどこにも残らない)。`s0` にポインタを持って走っているプログラムは、次にそれを使ったところで例外になる(`mtval` `0xE000D008`)。V103 と同じく、WCH-Link 越しに attach するコマンドは離す前に program を起動し直し、`monitor`、`arduino monitor`、WCH-Link のブローカー、gdb は attach の直後に起動し直す。warning `attach-corrupts-register` はどのレジスタかを名指す。ArduinoCore-CH32RV の DMSeq のコンソールの sketch で、0.30.4 は接続の後に READY を返さず、今は monitor でも pluggable monitor でも、ブローカー越しの `read` を挟んでも返ることを確かめた(V205RCT6、LinkE 2.22)。
- **CH32H417 は sketch の hart を相手にする**: ArduinoCore-CH32RV の既定のビルド(CPU Core = V5F)では、reset の後に core 0(V3F)が core 1(V5F)を起こし、自分は割り込みを切って wfi で待つ。sketch と DMSeq / SDI / RTT のコンソールは V5F で動く。WCH-LinkE の AttachChip は、コマンドが選ばなかった hart(既定では core 1)を止める。単発のコマンドは離す前に走らせ直す(前から)。target を走らせたまま持つ `monitor`、`arduino monitor`、WCH-Link のブローカー、SDI の転送は、attach の直後に、attach の前に走っていた hart をすべて走らせ直す。さらに monitor・Arduino のモニタ・ブローカーでは、core 1 が走っていたなら、session の相手(止める・メモリを読む・走らせる)を core 1 にする。メモリの読み出しは DATA0 を通り、走っている V5F の DMSeq がそこを書くので、core 0 越しの読み出しは flash ではなくコンソールの語を返したため。H417 のブローカーのメモリ読み出しは Link の高速読み出しを使わず、Debug Module で読む(同じ条件で flash に無い語を返した)。ArduinoCore の selfcheck(V5F のビルド)で、0.30.4 は monitor でも pluggable monitor でも READY を返さず、今はどちらも返る。ブローカー越しの `read` は flash の中身と一致し、`read`、`target info`、`monitor sdi off`、`flash`、`reset` の後も V5F は走り続ける(LinkE 2.22)。V3F だけのビルドでは core 1 は眠ったままなので、これまでどおり core 0 を相手にする。
- **CH32H417 の V3F だけのビルドも起動し直す**: WCH-LinkE の AttachChip は、H417 の core 0(V3F)の `a0`(x10)を、およそ 2 回に 1 回 0 にする(2.22 で 9 回中 5 回、2.23 で 15 回中 7 回。core 1 の GPR は変わらない)。attach の前は DMI がすべて 0 を返すので、元の値を先に読んでおくこともできない。core 0 で走っているプログラムは、`a0` を使っている命令で止められると壊れる。ArduinoCore-CH32RV の V3F のビルドでは、`SerialDMSeq.available()` の入口(`a0` = `this`)で止められると、sketch が無い 1 byte を読み、次の行の頭に `\xff` が入った(`UNKNOWN 2 [\xffready …]`)。そのため **core 0 だけが走っていた**(core 1 は走っていなかった)H417 は、V103 / V205 と同じく、離す前、あるいは `monitor`、`arduino monitor`、WCH-Link のブローカー、SDI の転送、gdb では attach の直後に、program を起動し直す(warning `attach-corrupts-register`。レジスタ名は `a0 (core 0)`)。**両方の core が走っていた**とき(既定の V5F のビルドでは core 0 は wfi で仕事を待つだけで、`a0` は仕事を受け取ってから使う)は起動し直さず、warning `attach-may-corrupt-register` だけを出す。`dbg` のコマンドは hart を見ないので、H417 では起動し直さない(調べたい状態を消さないため。`dbg` の後に走らせた program は `a0` が壊れているかもしれないので、`reset` で起動し直す)。
- **`sdi` は終わるときに LinkE の SDI 転送を止める**: CDC を誰も読まないまま SDI の転送を有効にしておくと、LinkE は target の SDI 出力を、UART bridge が host へ送る buffer に溜め続ける。そしてあとで、UART が受けた byte の代わりに、同じ量の古い SDI 出力を送る。UART の試験で、何分も前に書き換えた sketch の `SDI <build> <n>` の行が、開くたびに少しずつ届いた(折り返しは届かない)。WCH-LinkE 2.22 + CH32V203 / V307 で実測し、probe の電源の入れ直しでしか直らなかった。そのため `monitor --source sdi`(`--duration`、Ctrl-C、SIGTERM)と `arduino monitor` の sdi(CLOSE、QUIT、stdin の EOF、source の切り替え)は、CDC を開いたまま転送を止めてから終わる。そのあと line rate を一度別の値にして戻し、LinkE の buffer を空にする(rate を変えると LinkE は持っている分を捨てる。同じ rate の設定では捨てない)。**`monitor sdi off` も転送を止めたあと、同じ方法で buffer を空にする**。そのため、ほかのツールや `flash --sdi on` / `monitor sdi on` で有効のまま残った状態も、これ 1 つで直る。CDC を他が開いているときは空にできないので、warning `cdc-not-cleared` を出す。Windows の `sdi` の monitor は終わるときに転送を止めるだけで、buffer は空にしない(`monitor sdi off` は空にする)。
- **WCH-Link(CH549)は SRAM を Debug Module で読む**: CH549 の Link(2.12)の高速読み出しで SRAM の上の方を読むと、止めた hart の `s1`(x9)が読み出しの終わりの番地に、`a0`(x10)が `0xE0000000` に書き換わる(CH32V203RBT6 で `0x2000F000` から 4 KiB を読んで実測。flash の読み出し、SRAM の下側の読み出し、WCH-LinkE では、どの GPR も変わらない)。`monitor --source rtt` は control block を探すために SRAM を全部読むので、走っている program の `s1` が壊れた。ArduinoCore-CH32RV の console-rtt は `if (s1) …` の先の自分に飛び続けるループに入り、入力も出力も通らなかった。そのため CH549 の Link では、SRAM にかかる読み出し(RTT と `read`)は Debug Module で読む。flash の読み出しは高速読み出しのまま。
- **rtt の channel**: control block の up[0] / down[0] のみ流す。複数 channel を持つ block は warning `rtt-channels` を出す(channel を選ぶ機能は無い)。

5 つの `--source` は「5 本の並列 port」ではなく、**2 種類の host 機構**に分かれる(ArduinoCore-CH32RV の Serial / SerialSDI / SerialDMDATA / SerialRTT ライブラリが target 側の一次仕様)。

| source | target lib | host が受ける機構 | 対応 probe | 方向 | 備考 |
|---|---|---|---|---|---|
| `uart` | `Serial`(HW UART) | probe の **CDC port を開く**(serialport) | LinkE/LinkW の UART bridge | RX(TX 可) | 物理 TX/RX 配線が要る |
| `sdi` | `SerialSDI` | **LinkE に forward を有効化する probe command を送ってから、同じ CDC port を開く** | **LinkE のみ** | RX のみ | uart と**同じ 1 本の CDC に混ざって出る**(分離不可)。core は halt しない |
| `dmdata` | `SerialDMDATA` | **host が DMI で DM data0/data1 を polling**(minichlink `-T` の framing、7 byte out / 3 byte in) | 任意(CH549 も可) | 双方向 | CDC を使わない。core は halt しない |
| `dmseq` | `SerialDMSeq` | **host が DMI で DM data0/data1 を polling**(通番 + CRC-8 の framing。6 byte out / 2 byte in) | 任意(CH549 も可) | 双方向 | `dmdata` と同じ mailbox の**別 framing**。CDC を使わない。core は halt しない |
| `rtt` | `SerialRTT` | **host が DMI で RAM の RTT ring buffer を read/write**(`_SEGGER_RTT` を symbol/scan で発見) | 任意 | 双方向 | CDC を使わない。core は halt しない |

要点:

- **`uart` と `sdi` は LinkE の同じ 1 本の CDC port に出る**。`sdi` は「LinkE に SDI forward を有効化させる」probe 側の**設定変更**であって別 port ではない。両方使うと 1 つの monitor 窓に**混在**して届き分離できない。SDI は LinkE 専用(CH549/LinkW 不可)、firmware 2.10+ を capability で判定。
- **`sdi` と `dmdata` は同じ DM data0/data1 レジスタを使うが framing が違う**。SerialSDI は LinkE が forward する framing、SerialDMDATA は minichlink framing。**一方を読むツールにはもう一方は noise に見える**ので、target sketch は SerialSDI か SerialDMDATA のどちらか一方のみ(排他)。
- **`dmdata` / `rtt` は CDC を一切使わず host が debug transport(DMI)で読む**。LinkE forward 不要でどの probe でも動き、双方向。これが「UART/SDI 以外の形式」。`dmdata` は SerialSDI 出力も(LinkE forward せず)DMI 直読みできる利点がある。
- **`dmseq` は `dmdata` と同じ DM data0/data1 を使う別 framing**(OEP `target.console` framing 2。仕様は oep-spec の `docs/target-console-dmseq.ja.md`)。両方向に 1 bit の通し番号と全 word の CRC-8 があり、**答えの書込みが落ちたときの重複と、それを直そうとしたときの欠落を区別できる**のが `dmdata` との違い。CRC は必須で、実際 attach が DATA0 に残す `0xffffffff` や CH32V30x の flash が残す `0xe339e339` を弾くのにも効く(後者は bit7 が 0 なので、CRC が無いと target が有効な答えとして受け取る)。target sketch は `SerialDMDATA` と `SerialDMSeq` のどちらか一方のみ(排他)。host 側の追加通知: target が答えを待つのをやめると warning `dmseq-target-timeout`(その間の出力は捨てられている)、3 秒フレームが来なければ warning `dmseq-no-console`。
- port は VID/PID/serial/interface から決め、COM 番号・`/dev/tty*` の番号に依存しない。`--probe name:bench-01` から同一物理 device の CDC を引けることが HIL の要件。
- 実装は 2 backend に割れる: **CDC serial backend**(`uart`/`sdi`。`sdi` は先に enable command)と **DMI backend**(`dmdata`/`dmseq`/`rtt`。core を halt せず running 中に DMI read/write)。

### 4.6 gdb

```text
ch32rv gdb [--listen 127.0.0.1:3333] [--reset-halt] [--no-flash]
```

- `--reset-halt`: GDB がつながったら target を reset し、リセットベクタで止めてから相手をする(probe-rs の `gdb --reset-halt` と同じ意味)。WCH-Link は DMI の reset-halt、OEP の probe とブローカー経由の WCH-Link は riscv-dm の reset(halt)。
- `--no-flash`: `load`(vFlash)を受けない。gdb server は vFlash を持たないので、付けても付けなくても同じ。

- **attach 時に target flash を書き換えない**(WCH OpenOCD の挙動を再現しない)。`load`(vFlash)は持たない(書き込みは `flash`)。
- HW breakpoint(RISC-V trigger module)は **core が実際に trigger slot を持つ場合のみ GDB に広告する**(minichlink の `hwbreak+` 偽装をしない)。**有無は misa や core 世代と一致しない**ので、attach 時に tselect/tdata1 を実際に叩いて**動的に**数える(下表)。`support_hw_breakpoint` は slot>0 のときだけ `Some`、attach ログにも実 slot 数を出す。

  | 実測 | core | family | trigger slot |
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
  - **flash SW breakpoint フォールバック**: trigger の無い core でも、検証済み FLASH-controller profile(256byte family)なら **直接 FLASH controller 経路(§4.1 の `erase --range`)で page を read-modify-write** して `ebreak` を焼く。code は低位 alias(0x0000_0000)で走るが FLASH controller には物理 flash 番地(0x0800_0000+off)を渡す。同一 page 内の複数 breakpoint に対応し、page 内容が変わらない set/clear は書き込みを省く。detach 時に全 page を pristine へ復元(途中終了でも `ebreak` を焼き残さない)。**注意: set/clear ごとに flash を書くため摩耗する**(gdb の step-over は remove+再 insert で 2 回書く)。attach ログで警告し `set breakpoint always-inserted on` を勧める。program 方式は family 別(PgStart=V20x/V30x、Buffered=V003/V00x/X035/L103。§4.1 の `erase --range`)。**対応: V20x/V30x・V003/CH641・V00x・X035/CH643・L103・V103**。**V103 は attach quirk の対処が前提**: WCH-Link の AttachChip が生きた GPR s1/x9 を chip id で上書きし(元値は保存されない)、resume 後に program が s1 を使う瞬間 fault する(mcause=4 load-misaligned)。→ attach 後に soft-reset して program にレジスタを再構築させてから halt する(profile の `attach_corrupts_regs`。target は再起動する)。実機検証: V103 で flash 上コードへの通常 `break` が複数 `continue` で発火。
- RV32E core(CH32V003、misa.E=bit4)対応: GPR は x0-x15 のみ存在し、x16-x31 を abstract command で読むと cmderr が出て session が落ちる。gdb server と `dbg regs` は misa.E を見て x0-x15 だけを扱う。**GDB にも 16 本 + pc で見せる**: RV32E の ELF を読んだ GDB は 68 byte を待ち、`g` に 33 本(132 byte)を返すと `Remote 'g' packet reply is too long` で止まる。RV32E の hart では x0-x15 と pc だけを返し、target description(`org.gnu.gdb.riscv.cpu`、pc は番号 32)も 16 本 + pc を出すので、ELF が無くても同じ形になる。実機確認: V006(LinkE、ブローカー経由、dmseq の monitor を開いたまま)で ELF ありの break / step / x / detach、ELF 無しで `info registers`。
- gdbstub ベースの GDB server が **register/memory R/W・halt/continue/step・Ctrl-C・SW(RAM+flash)/HW breakpoint** を実機で end-to-end 動作(riscv-none-embed-gdb)。検証: V003/V203 で RAM SW breakpoint が `continue` を停止、V307/X035/**L103** で flash 上の通常 `break` が HW trigger で発火(L103 は step も毎命令前進・attach でレジスタ非破壊を確認)、**V203(trigger 無し)で flash 上の通常 `break` が flash-patch で発火(複数 `continue`・detach で flash が pristine 復元)**。RV32 arch は x0-x31+pc の整数のみ(V4F の FPU の register は見せない)。

### 4.7 isp(factory ISP 経路)

```text
ch32rv isp [--transport usb|uart] [--device <住所>] [--port <serial port>] [--baud <bps>] [--wait <SECS>]
           [--entry none|auto|probe|dfu-detach|touch1200]
           --device: ch32rv-isp://<bus>-<ports> | topology:<bus>-<ports> | <serial port> | ch32rv-app://<id> | VID:PID[:SN]
                     | ch32rv-bootloader://WAIT_FOR_BOOT
ch32rv isp list                                     ISP mode device の列挙。Identify に答えない device(LinkE IAP 等)は区別して表示
ch32rv isp info                                     chip/BTVER/UID/保護状態/config 12 B
ch32rv isp enter [--via probe]                      --probe の debug 線で boot mode を立てて reset(CH32X03x、CH32H417)
ch32rv isp flash <FILE> [--erase chip|sector|keep-store|none] [--verify on|none] [--reset run|none]
ch32rv isp verify <FILE>                            bootloader が照合(不一致 exit 30)
ch32rv isp erase                                    code flash 全消去(確認あり、--yes で省略)
ch32rv isp reset                                    bootloader を抜けて app を起動
ch32rv isp config get                               option bytes を解いて表示
ch32rv isp config set <key>=<v>...                  option bytes を変更して読み直しで照合
```

- **USB transport**: CH32X035F8U6(BTVER 2.60、`1a86:55e0`)で info / flash / verify / erase / reset を実機確認し、書いた内容は RVSWD の読み出しで照合した。byte の記録は wch-protocols `protocols/pc-to-device-isp.ja.md#x035-bytes`、replay fixture は `tests/fixtures/replay/isp-*-x035.ndjson`。
- **UART transport(実機未確認)**: `--transport uart --port <serial port>`(`--baud` 既定 115200、8N1)。frame は USB と同じで、request を `57 ab` + frame + byte 和、reply を `55 aa` + frame + byte 和で包む(先行実装からの転記)。reply の前の余計な byte は読み飛ばし、和が合わなければ transfer-failed。UART には「現れる」が無いので、`--wait`(entry をしたなら最低 5 s)の間 Identify を繰り返し、答えなければ exit 10。`--entry` は USB と同じ入り方をしてから port を開く。出力の位置は `uart:<port>`(JSON は `at`、`usb` / `topology` は null)。auto-baud と `0xC5` SetBaud は使わない(`--baud` は port の速さだけ)。疑似 bootloader(pty)で info を通した。
- ISP device は USB serial を持たないため、既定は「1 台のみ」の fail-closed(2 台以上は exit 14 で位置の候補を出す)。複数台は `--device` で選ぶ。
- `--device <住所>`: 書く device を 1 つの option で名指す。`--probe` は `--entry probe` の debug probe だけ。
  - `ch32rv-isp://<bus>-<ports>`(discovery の bootloader の住所): その位置の ISP device。アプリには頼まない。
  - `topology:<bus>-<ports>`: その位置の ISP device。居なくて `--entry` があれば、その位置の device をアプリとして頼む(bootloader はアプリと同じ位置に現れる)。`auto` でその位置に何も無ければ頼まない。
  - アプリ: serial port の path(IDE が渡す serial port。裏の USB device を引く。Linux では上流の hub も同じ tty を持つので、他の一致の下に居ないものを本体とする)、`ch32rv-app://<id>`、`VID:PID[:SERIAL]`。先にその device を引き、**その USB の位置の ISP device だけ** を使う。居なければ `--entry` で頼む。アプリが居ない、または複数に当たるとき(チップがもう bootloader に居ると、IDE が選んだままのアプリの port はこう見える)は、`--device` 無しと同じに唯一の ISP device を使う(2 台以上は exit 14 で `ch32rv-isp://` を選ぶよう hint)。ISP device も居なければ、`--entry` がアプリを引けずに exit 10(hint に、今の板の port を選び直すよう出す)。形の誤りと USB の列挙の失敗は止める(exit 2 / 11)。
  - 別の経路の bootloader(`ch32rv-hid://`)と無くなった形(`usb:`、`hid://`、`port:`)は exit 2。`--app-device` は受けない(exit 2、hint に `--device <その値>`)。
- `--wait <SECS>`(既定 0): device が無ければその秒数だけ待つ。最初に、入り方(Boot キーを押したまま USB を接続、または `--entry probe`)を stderr に 1 回出す。期限切れは exit 10。
- `--entry probe`: ISP device が無ければ、`--probe` の debug 線で hart を reset vector で止め、`FLASH_KEYR` と `FLASH_BOOT_MODEKEYR` を外し、`FLASH_STATR.BOOT_MODE` を立てて reset してから、最大 5 s 待つ。flash の中身を先に走らせないので、壊れた image が入っていても入れる。OEP の probe と WCH-Link の両方で使える(WCH-Link は DMI の reset-halt で捕まえ、最後は probe の system reset)。family は CH32X033 / X035 / H417 だけ(他は exit 24。H417 は RM 46.4 の同じ register と鍵による。H417 の ROM が USBFS(PA11 / PA12)で download を受けるのは、USER の USBFSDLEN が立っているときだけ)。WCH-LinkE(fw 2.22)+ CH32X035C8T6 でも、reset 後に `FLASH_STATR` = `0xF000`・PC が BOOT 領域になることを確認した(この板の USB は PC に未接続)。入れたのに ISP device が現れないときは、チップの USB の接続を疑うよう hint を出す。読み出し保護中のチップ(`FLASH_OBR.RDPRT`)には入れず exit 21(bootloader が走らないため。`recover --method unprotect` を hint に出す)。`isp enter` は `--entry probe` で開いて Identify / ReadConfig までして終わる。
- `--entry` は ISP device が居ないときの入り方(既定 `none`)。どれも、入れた後は最大 5 s(`--wait` が長ければそれ)待つ。
  - **DFU について**: ここで使う DFU は、DFU 1.1 の **run-time interface と DFU_DETACH 要求だけ**(アプリに「bootloader へ再起動して」と頼む標準の合図)。その後に現れるのは WCH の factory ISP(`1a86:55e0`、独自 protocol)で、DFU mode の device ではない。**DFU の download(DNLOAD)は使わず、dfu-util では書けない**。DFU mode で書く DFU の device は別の経路(`boot dfu`)。
  - `dfu-detach`: アプリの DFU 1.1 run-time interface(class `0xfe`/`0x01`/`0x01`)へ DFU_DETACH(`21 00`、`wValue` 1000、`wIndex` = interface)。`--device` が無ければ、run-time interface を持つ device がちょうど 1 台のときだけそれを使う(0 台 exit 10、2 台以上 exit 14)。アプリは status stage の前に切れることがあり(Linux では stall)、それでも detach するので、エラーは `warning[dfu-detach]` にとどめて待つ。
  - `touch1200`: アプリの serial port を 1200 bps で開き、DTR を落として閉じる(Arduino の約束)。**`--device` で名指しした device にだけ送る**(無関係な板を reset しないため。無ければ exit 2)。
  - `auto`: 名指しされたアプリが DFU run-time interface を持てば `dfu-detach`、無くて serial port があれば `touch1200`。アプリの指定が無く `--probe` があれば `probe`。どれも無ければ何もせず、`--wait` の間、入り方を出して待つ。Arduino の upload recipe はこれを使う(`--entry auto --device "{upload.port.address}"`)。
  - アプリ経由で入れたときは **アプリと同じ USB の位置に現れた ISP device だけ** を使う(X035 で一致を確認)。
  - 実機確認(CH32X035F8U6、ArduinoCore-CH32RV の CH32RVUsbDevice で CDC + DFU run-time interface + HID のスケッチ): `dfu-detach`(アプリの指定無し / VID:PID / serial port / USB の位置)、`touch1200`、`auto`、アプリ指定無しの `auto` + `--probe` のどれも、書込・照合・アプリ再起動まで通った。DFU_DETACH はほぼ毎回 stall で返った。
- **discovery(ArduinoCore-CH32RV と合意)**: アプリの VID:PID はスケッチ次第(既定は pid.codes のテスト用 PID)で serial も自由なので、外から CH32RV のアプリだと分かる印は **DFU run-time interface の iInterface `CH32RV`**(ArduinoCore-CH32RV の `CH32RV_USB_DFU_RUNTIME_NAME` の既定)だけ。名前を変えたスケッチは discovery に出ない(`--device` で名指せば DFU_DETACH は動く)。
  - `ch32rv-app://<id>`: 名前 `CH32RV` の DFU run-time を持つ device。id は USB serial(ArduinoCore-CH32RV の既定はチップの UID 由来なので、別の USB port に挿し直しても同じ名前になる)。serial が無いか、つながっている他のアプリと重なるとき(スケッチは serial を自由に決められる)だけ `usb-<bus>-<ports>`。`wchlink://` と同じ規則。CDC があっても出す(serial port だけでは CH32RV と分からないので、書込はこちらを選ぶ)。label は `<product 文字列> (CH32RV, USB)`、properties は `vid` / `pid` / `serialNumber` / `topology`、hardwareId は serial(無ければ位置)。monitor はその device の CDC port を開く(CDC の無いアプリは「monitor する CDC が無い」で断る)。
  - `ch32rv-isp://<bus>-<ports>`: `1a86:55e0`(Boot キーなどで入った bootloader。約 10 s で app へ戻るので一時的)。`4348:55e0` は WCH-Link の IAP と同じなので出さない。monitor は無い(断る)。
  - `--device` は `ch32rv-app://<id>`(その id のアプリを今の位置で探す。serial が 2 台で重なれば exit 14)と `ch32rv-isp://…`(頼まずにその位置の ISP へ書く)を受ける。upload recipe は `--entry auto --device "{upload.port.address}"` で、tty / ch32rv-app / ch32rv-isp のどれを選んでも動く。
  - `--entry dfu-detach` で `--device` が無いときも、名前 `CH32RV` の DFU run-time を持つ device だけを候補にする(他社の本物の DFU 機器を再起動させないため)。
  - 実機確認(CH32X035F8U6、CDC の無い DFU run-time のみのスケッチ): discovery の LIST に `ch32rv-app://3-2.4.3` が出て、その port から書込・照合・起動まで通った。ISP に入れた後は `ch32rv-isp://3-2.4.3` が出て、その port からも書けた。CDC のあるアプリでは、`ch32rv-app://<serial>` の monitor がその CDC の出力を流すことも確認した。
- `isp flash` は ELF / HEX / bin を code flash 先頭からの 1 塊にし(隙間は `0xff`)、`--erase` は `flash` と同じ値(§4.1 の表)。既定の keep-store と sector は image の長さ分の 1 KiB sector(最小 8)を消し、chip は全 sector を消す。書いた後の `IspEnd` は応答を読んでから終える(読まないと bootloader に残る。wch-protocols `protocols/pc-to-device-isp.ja.md#x035-entry`)。
- `isp config set` / `get` の語彙と `get` の形は `target option set` / `get` と同じ(§4.3)。bootloader の series から family を引く(CH32V10x / F10x → V103、V30x → V307、V20x / F20x → V20x、X03x → X035、L10x → L103)。CH32V00x は部品で DB の field が違うので `user.<field>` は使えず(`db_family` は null)、意味名は V003 / V006 共通のもの。補数 byte は計算し直し、知らない USER の bit は読んだ値のまま。書いた後に ReadConfig で読み直し、違えば exit 30。`read-protect` を変えるときは確認する(`--non-interactive` では `--yes` が要る。off にするとチップが code flash を全消去する)。CH32X035F8U6 で全キーを書いて戻し、option bytes(`0x1FFFF800`)を RVSWD で照合した。`read-protect=on` は `00 ff` を書き、reset 後は RVSWD の flash 読み出しが fault になる。**保護中は factory bootloader が走らない**(X035 BTVER 2.60 の実測。debug 線から入れても、debugger 無しの電源投入後にアプリから DFU_DETACH で入れても、PC 0 で止まり ISP device は現れない)。したがって ISP の `read-protect=off` は使えず、解除は debug 線の `recover --method unprotect`(code flash 全消去)だけ。`read-protect=on` の確認文にもそう書く。BOOT キー(電源投入時の PC17)で入った場合は未確認。
- bootloader は最後の通信から約 60 s で app へ抜ける。一度も話しかけられなければ約 9.3〜9.7 s で抜ける(ArduinoCore-CH32RV の観測)ので、ch32rv は現れ次第 Identify を送る。
- protocol は自前実装(wchisp は GPL-2.0 のため取り込まない)。byte の事実は wch-protocols `protocols/pc-to-device-isp.ja.md#commands`。

### 4.8 boot(custom bootloader 経路)

```text
ch32rv boot dfu flash <FILE> [--format auto|elf|hex|bin] [--at <addr>] [--device <住所>] [--erase sector|keep-store]   DFU 1.1 の download
ch32rv boot dfu info [--device <住所>]
ch32rv boot hid flash <FILE> [--usb-id <VID:PID>] [--wait <SECS>] [--device <住所>] [--erase sector|keep-store]  rv003usb / b003fun 系(UIAPduino 等)
```

書く device は `--device` で名指す。boot は debug probe を通らないので、`--probe` を付けると exit 2 で断り、hint に `--device <その値>` を出す(環境変数 `CH32RV_PROBE` は他のコマンドの probe なので、boot では見ない)。

- `boot dfu` の `--device`: serial port の path、`ch32rv-app://<id>`、`topology:<bus>-<ports>`、`VID:PID[:SERIAL]`(`isp --device` のアプリの書き方と同じ)。無ければ DFU の interface を持つ唯一の device(0 台 exit 10、2 台以上 exit 14)。bootloader の住所(`ch32rv-isp://`、`ch32rv-hid://`)は exit 2。
- `boot hid flash` の `--device`: 位置だけ。`ch32rv-hid://<bus>-<ports>`(discovery の住所)か `topology:<bus>-<ports>`。無ければ最初に見つかった 1 台。`hid://<bus>-<ports>` は `ch32rv-hid://<bus>-<ports>` を添えて exit 2。`ch32rv-bootloader://WAIT_FOR_BOOT`(待つポート)は、位置を決めずに、少なくとも 60 s 待つ(入り方と、10 s ごとに残り時間を stderr に出す)。
- `boot dfu` の `--device` に待つポート(`ch32rv-bootloader://WAIT_FOR_BOOT`)を渡すと exit 2(hint に `isp flash`)。
- **待つポート `ch32rv-bootloader://WAIT_FOR_BOOT`**(IDE のための住所。[freeze-decisions](freeze-decisions.ja.md) §2): 書き込みを押してから BOOT で入れる、ボードのブートローダーに書く。どのブートローダー(工場の USB ISP か、HID)かはボードで決まる(ArduinoCore-CH32RV の boards.txt が道具を選ぶ。既定は `isp flash`、HID のボードは `boot hid flash`)。
  - `isp flash --device ch32rv-bootloader://WAIT_FOR_BOOT`: 位置を決めず、走っているアプリにも probe にも頼まず(`--entry` は使わない)、ISP device を少なくとも 60 s(`--wait` がそれより長ければその長さ)待つ。最初に入り方を、10 s ごとに残り時間を stderr に出す。現れた 1 台に書く(2 台以上は exit 14、時間切れは exit 10)。`--chip` が USB の周辺を持たないチップ(DB の capabilities で USB の行が無い部品だけの名前。V002 / V003 / V004 / V005 / V006 / V007 / M007)なら、工場の USB ISP が無いので、待つ前に exit 24(hint に、ソフトウェア USB の HID ブートローダー(rv003usb、UIAPduino)のボードは `boot hid flash` で書くことを出す。HID の経路はチップを確かめない)。`--transport uart` とは組めない(exit 2)。
  - `WAIT_FOR_BOOT` は大小を問わない。`ch32rv-bootloader://` の後ろがほかの語なら exit 2。debug probe は待つポートの候補にならない(probe は自分の住所で一覧に出るので、待つポートで probe に書くことはない)。

HID flashは、既定で`1209:b803`（UIAPduino）→`1209:b003`（rv003usb）の順に探索し、
64 byte page単位でpreverify・erase・program・read-back verifyを行ってuser appを起動する。
`--capture` / `--replay`もHID feature reportを含めて対応する。

bootloaderが見つからないときは、`--wait`の秒数(既定30、`0`で即exit 10)だけ200 ms間隔で探し直す。
最初に見つからなかった時点で、待つことと入り方(RESETを押したままUSBを接続し、すぐ離す。押し続けると
hostが列挙をあきらめる)をstderrへ1回出す。IDEの出力欄にはこれしか出ないので、`--progress none`でも出す。
待っている間に現れた直後のopen失敗(OSがノードの権限を付ける前)も再試行する。期限切れはexit 10で、
同じ入り方をhintに出す。`--replay`中は待たない。

**任意 VID/PID 指定**: bootloader 系は vendor/build ごとに USB VID:PID が異なる(rv003usb/b003fun 系は既定 `0x1209:0xb003` だが、**UIAPduino は `0x1209:0xb803`** = ボードごとに変わる)。minichlink は PID をソースに **ハードコード**しており、UIAPduino に書くには PID を書き換えて **リビルドが必要**だった(参照: <https://qiita.com/tomorrow56/items/6cae8ddc7470cb64ad7d>)。ch32rv は同じ轍を踏まない:

- **known table**: UIAPduino(CH32V003・16KB・b003fun HID bootloader・`0x1209:0xb803`)とrv003usb (`0x1209:0xb003`)を既定で発見する。
- **`--usb-id <VID:PID>` 上書き**: known table に無い未知 PID でも、`--usb-id 1209:b803` のようにCLIからVID:PIDを直接指定して書ける(**リビルド不要の逃げ道**)。`boot hid flash` が受ける。
- 設計原則: **単一 PID をコードに焼き込まない**。発見は「known table + ユーザー指定 VID:PID」の二段で、知らない PID にも到達できる状態を既定にする。

### 4.9 db

```text
ch32rv db list [--family <f>] [--verified-only]
ch32rv db info <SKU>            geometry(page/fast/block)、領域、option layout、chip ID、生成元 revision、verified 根拠
```

- **`db list` / `db info`**(cmd_db.rs、デバイス不要)。生成済み DB(`crates/target/generated/skus.csv` を `include_str!` で埋め込み)を列挙/表示。`db list --family <f>` は family/SKU prefix で絞り込み、`--verified-only` は実機確認済みのみ。`db info <SKU>` は family/device_id/flash/sram/verified を表示。JSON 可。DB は `cargo xtask db-gen` が ch32-device-data(pinned)から生成し commit する(§4.3 target info の chip_id→SKU 解決、§4.3 option get の family-aware USER decode もこの DB を使う)。geometry は flash/sram に加え **page/fast erase・fast program・block erase**(`flash_geometry.csv`、`db info` で表示)。`db info` は **消去済み word の読み出し値**(`erased:` 行。系統 A `0xffffffff` / B `0xe339e339`)と **FLASH controller の書込方式**(`flash prog:` 行。`buffered`/`direct` + commit bit + buffer load 幅 + DB の confidence)も出す。`flash_controller_profile`(§4.2.1 の直接 controller 経路)は手写しを持たず**この DB から導出**する。cli テストは、実機で駆動している family について **DB 側に page 消去粒度・消去後の値・`conflict` でない書込手順が揃っている**ことを検査する(納品でどれかが欠けると `erase --range` / flash breakpoint が黙って無効化されるため)。方式・粒度が実機検証値と一致することは `ch32rv-flash` の単体テストで固定。

DB は ch32-device-data からの生成物で手書き YAML を持たない(architecture.ja.md §3)。device_id は WCH EVT の ChipID 一覧と実測だけから作られ(第三者 DB の ch32-data は使わない)、1 つに決まらない型番(例: EVT が 2 つ書く CH32V203F6P6)は ID 無しで載る。ID 無しの型番を `--chip` で指定すると family だけを照合する(書く前の ID 照合はできない)。`--db <path>` overlay で新 SKU を再ビルドなしに試せる。

### 4.10 capabilities / doctor / version / complete

```text
ch32rv capabilities [--probe <sel>] [--chip <sku>]   probe 型番 × probe FW × target family × operation の可否と理由
ch32rv doctor [--emit-udev]                          権限/udev、Windows driver binding、既知不良 FW、IAP 滞留、
                                                     target 電源/BOOT/配線の切り分けと次の一手。--fix は持たない(暗黙の sudo をしない)
ch32rv version [--json]                              tool 版 / git rev / contract 版 / target DB rev+hash / flash stub hash / build target
ch32rv complete <bash|zsh|fish|powershell>          補完スクリプトを stdout へ(clap_complete で clap tree から生成=CLI と常に一致)
```

操作 command は、その操作に要るものが無いと分かった所で exit 24 を返す(`capabilities` の判定とは別に、各コマンドが持つ)。`tool supports LinkE` の boolean は存在しない。

- **`capabilities`**: cmd_capabilities.rs。attach して **probe variant(LinkE/CH549/…)+ FW + target family/SKU/配線(DB)** を読み、operation ごとに可否+理由を出す(human 表 + JSON)。判定(`build_matrix`、単体テストあり): `connect`(**1線 target × CH549 = 不可**。CH549 は 1線 SWIO 非対応、`Variant` + debug_wiring から)、`flash`(family stub 有無)、`erase --range / flash-bp`(FLASH-controller profile 有無。page サイズ+方式を理由に表示)、`gdb HW breakpoints`(attach 時に動的検出=V4C/V4F は 4)、`monitor sdi`(**LinkE 限定**)、`monitor dmdata`(任意 probe)、`recover power-off`(**LinkE/LinkW 限定**=target 電源制御)。**2 モード**: `--chip` 無し=attach して live target を判定、`--chip <SKU/family>` 有り=**probe info のみで静的判定(target 未接続でも可=pre-flight プランナ)**。static は probe を attach せず `probe_info()` で variant/FW を取り、`--chip` を family_byte(`family_byte_from_name`)+ series/配線(DB)へ解決。実機検証: L103(LinkE)全 yes、**V103(CH549 probe)は sdi/power-off が NO**、V003(LinkE,1線)connect yes、**静的 `--probe <CH549> --chip CH32V003` が target 未接続で `connect NO`(1線×CH549)を予測**、未知 `--chip` は usage error。

### 4.11 arduino

```text
ch32rv arduino discovery       Pluggable Discovery protocol(stdio JSON)。WCH-Link を wchlink://<serial>、
                               OEP の probe の口を oep://<probe>/<port>(名前つきの口が無い probe は oep://<probe>)、
                               rv003usb / UIAPduino の HID bootloader を ch32rv-hid://<bus>-<ports>、
                               CH32RV のアプリを ch32rv-app://<serial>(無い・重なるなら usb-<bus>-<ports>)、
                               factory ISP bootloader(1a86:55e0)を ch32rv-isp://<bus>-<ports> で出す。
                               加えて、何もつながっていなくても、待つポート ch32rv-bootloader://WAIT_FOR_BOOT
                               (protocol ch32rv-bootloader、label "USB bootloader (waits for BOOT)")をいつも出す
ch32rv arduino monitor [--protocol <p>]
                               Pluggable Monitor protocol(stdio JSON)。--protocol は serial / wchlink / oep /
                               ch32rv-app / ch32rv-isp / ch32rv-hid / ch32rv-bootloader。serial port・wchlink://・oep://・ch32rv-app:// の port を受け、
                               source を双方向に wrap する
```

Arduino 専用の書き込みロジックは持たない。recipe は §5 の通常 command を呼ぶ。

- **discovery**: HELLO/START/LIST/START_SYNC/STOP/QUIT。**USB descriptor だけから列挙**し AttachChip しない(同一 probe への upload/monitor 中でも乱さない。LinkE の attach は target のクロックを組み替える)。**普通の serial port は開かない**(組み込みの serial discovery が出す)。START_SYNC は現在の port を `add` で一度出す(hotplug の監視は持たず、IDE の再 LIST に委ねる)。
  - `wchlink://<serial>`: protocol `wchlink`、properties に serial / vid / pid / mode。Windows で device に ch32rv が開けない driver が付いている Link(usbipd の stub など)は、label を `WCH-Link <serial> (cannot open: driver <name>)` にし、properties に `driver` を足す(一覧からは消さない)。実機確認(Windows): usbipd で WSL に attach 中の LinkE 7 台がすべて `WCH-Link <sn> (cannot open: driver VBoxUSB)`(usbipd-win の stub は `VBoxUSB` の名で見える)。
  - `oep://<probe>/<port>`(oep-spec host ガイド §5.3): OEP の probe(`is_oep_device` = USB ID 1209:4F45 の device。oep-transports §3)の口ごとに 1 つ。口は probe に保存した label `S.N`(`S.swdio` / `S.swclk` / `S.swio` / `S.nrst` / `S.rx` / `S.tx`)の `S` で、名前つきの口が無い probe は probe 自身の `oep://<probe>` で出す(線は `--pins` で指す)。`<probe>` は USB の serial(= OEP の unit_id。serial の無い device だけ位置)。attach しないので target は出さない。label は lock 無しで読む(その port のブローカーが動いていればブローカー経由、ここでは起動しない。無ければ HID、vendor bulk、CDC の順に短く開く(HID は他の道具と取り合わない)。応答待ちは 0.3 秒)。読めなければ前回の一覧(runtime dir の `<key>.ports.json`)。properties に vid / pid / port_name(`oep://<probe>` では null)/ port。普通の serial port は開かない。
  - `ch32rv-hid://<topology>`(protocol `ch32rv-hid`): `1209:b803` / `1209:b003`。bootloader は serial 番号を持たないので位置で出し、`boot hid flash --device ch32rv-hid://<topology>` も位置で引く(Linux は hidraw ノードで照合、他 OS は位置が取れないので同じ ID が 2 台あると曖昧 = exit 14)。properties に vid / pid / topology。monitor は無い(断る)。
- **monitor(ArduinoCore-CH32RV `docs/oep-workflow.ja.md` §6)**: platform.txt の `pluggable_monitor.pattern.<protocol>` に登録し、protocol を `--protocol` で渡す(既定 `serial`)。arduino-cli 1.3.1 は DESCRIBE の `protocol` を port の protocol と照合し、違うと OPEN の前に止まる(ArduinoCore-CH32RV の実測)。
  - **DESCRIBE**: キーは `port_description`、列挙の値は `value`(arduino-cli が読むキー)。設定は `source` / `baudrate` / `dtr` / `rts` / `chip`。
    - `source`(凍結の表、docs/freeze-decisions.ja.md §3。先頭が既定):

      | protocol | source |
      |---|---|
      | `serial` | `uart`、`sdi`、`dmdata`、`dmseq`、`rtt`、`fixture-uart` |
      | `wchlink` | `dmdata`、`dmseq`、`sdi`、`rtt` |
      | `oep` | `dmseq`、`dmdata`、`sdi`、`fixture-uart`、`rtt` |

      `wchlink` に `uart` と `fixture-uart` は無い(uart は Link の CDC を `serial` で開く)。`ch32rv-app` / `ch32rv-isp` / `ch32rv-hid` の source は `serial` と同じ並び。`--protocol` はこの 6 つだけを受け、ほかは usage(exit 2)。
    - `baudrate` / `dtr` / `rts`: `uart` 用。値と既定は組み込みの serial-monitor と同じ(9600、on、on)。他の source でも受けて無視する。開いたまま変えるとすぐ効く。
    - `chip`: `--chip` と同じ意味(板の `monitor_port.serial.chip` を IDE が渡す入口)。値は `auto`(既定、= 指定なし)と DB の family / SKU(`--chip`・core の `build.ch32rv_chip` と同じ綴り)。attach する source(sdi / dmdata / dmseq / rtt)で fail-closed に照合し、合わなければ OPEN が error。`uart` は attach しないので照合しない。
  - **OPEN の address**: `wchlink://<serial>`、`oep://<probe>[/<port>[/<target>]][?via=<経路>]`(`--probe` と同じに解く)、または serial port の path(`--probe` と同じ逆引きで WCH probe を引く)。probe の無い素の serial port は `uart` だけが使え、他の source は OPEN が error。解決・source の open・TCP 接続までを OPEN の中で済ませ、失敗は OPEN の error(arduino-cli は exit 1 で message を出す)。
  - **source**: `uart` = serial port の素通し(双方向、serialport crate で排他 open)。`sdi` = LinkE の SDI 転送を有効にして CDC を読む(受信のみ。DTR を立てない素の open + TIOCEXCL + raw termios)。`dmdata` / `dmseq` / `rtt` = attach して DMI で(双方向)。開いたまま `source` を変えると同じ TCP のまま backend を替える。
  - **`fixture-uart`**: OEP の probe の fixture の UART(`oep.fixture.uart`)。`baudrate` を configure で送る(開いたまま変えるとすぐ効く)。`wchlink://` の port には出さない(CONFIGURE が unknown source)。WCH-Link の CDC を `serial` で開いた port では、選べるが OPEN が error。
  - **`oep://<probe>/<port>`**(protocol `oep`): source は上の表のとおり。その口の label が名指すピンで attach する(下の「口の選び方」)。
  - **OEP の probe**: WCH-Link のものでない serial port で debug の source(sdi / dmdata / dmseq)を選ぶと、その port の probe のブローカー(docs/oep-host.ja.md §7.2、無ければ起動)の client として `oep.target.console` を開く(止めずに attach し、最後の reset の mark から読む。入力は console write)。**`rtt` は ch32rv が host 側で行う**: OEP の console の方式に RTT は無いので、ch32rv が probe の riscv-dm の上で、WCH-Link の時と同じく RAM の control block を探し、poll のたびに hart を一瞬止めて ring を読み書きする(止める / 走らせるは probe の op、RAM は block 読み。poll は 50 ms ごと)。CLI の `monitor --source rtt` も同じ。`uart` は OEP の probe の source ではない(CLI の monitor は exit 24 で、`--source` を省く(target の console)か dmseq / dmdata / sdi / rtt / fixture-uart を案内する)。CLI の monitor は `--source` を省くと、OEP の probe では dmseq の console を読む(mechanism は host が選ぶ)。`chip` は attach で読む chip id と照合する。WCH-Link ではないので attach のクロックの注意は出さない。`uart` は OEP の probe でも port の素通しのまま(知らない port に confirm を送らない。OEP の UART bridge の probe は、session の無い間は bind したストリームを port に生で流す)。
  - **終わり方**: **stdin の EOF で必ず終わる**(arduino-cli は CLOSE / QUIT を送らずに死ぬことがあり、tool は別のプロセスグループなので killpg も届かない)。開いた後に続けられなくなったとき(読み書きの失敗、切り替え先の source が開けない)は `\r\n[ch32rv monitor] stopped: <理由>\r\n` を data として送ってからプロセスごと終わる(arduino-cli は tool の stderr を見せず、落ちても exit 0 で終わるため)。
  - **attach のクロックの注意**: attach する source(uart 以外)を開いたとき(OPEN と、開いたままの切り替え)に、`[ch32rv monitor] attaching may have changed the target clock (UART baud rate, millis, timers); reset the board to run at its own clock` を data の行で流す。`monitor` の warning `attach-reclocks-target` と同じく、全 target に「可能性」として出す(IDE の利用者が読むのはモニタの窓だけのため)。reset して離す設定(on-close)は無い。
  - **lock**: attach する source(sdi / dmdata / dmseq / rtt)は probe 単位の lock を持つ(開いている間の flash は待つか exit 13)。`uart` は lock を取らない(CDC だけを使い、tty は排他)。
  - 実機検証(WCH-LinkE fw 2.22、arduino-cli の代わりに protocol を話す試験 script): V006(K8U6)の SerialEcho を `uart` 115200 で往復、V203 の HelloSDI を `sdi`(serial port 経由・`wchlink://` 経由とも)、L103 の HelloDMDATA を `dmdata` で往復(小文字が大文字で戻る)、開いたままの `dmdata` → `uart` の切り替え、素の serial port(CH343)で `sdi` へ切り替えて理由の行を送って終わる、`chip` の不一致で OPEN が error、stdin の EOF で 0.1 秒以内に exit 0、開いている間は 2 つ目の open が EBUSY。`ch32rv-hid://` は UIAPduino が無く未検証。

### 4.12 broker と OEP の probe

```text
ch32rv broker endpoint --probe <sel> [--json]   その probe のブローカーの待ち受け(封筒の result = {"endpoint":"127.0.0.1:<port>"|null,"pid":…,"transport":"vendor-bulk"|"hid"|"serial"|"wchlink","version":"<版>","implementation":"ch32rv","file":"<oep/brokers/<unit_id>.json の場所>"|null})
ch32rv broker serve --probe <sel>               ブローカー本体(利用者向けではない。client が切り離して起動する)
```

- **ブローカー**(docs/oep-host.ja.md §7.2): probe ごとに 1 つの、誰の子でもないプロセス。probe の transport と session を持ち、ch32rv のコマンドと pytest の `oep_host` は 127.0.0.1 の TCP で OEP を話す client になる。client の session の要求はブローカーが自分で答え、残りは 1 回の pipeline にまとめて probe に送る。接続と plan は client ごとの台帳に持ち、抜けた client の分だけ外す。最後の client が抜けてから 3 秒で終わる(IDE がモニターを閉じてからアップロードを始めるまでの間を保つ)。起動の取り合いは flock で 1 つに(unit_id のある probe は OEP 共通の `oep/brokers/<unit_id>.lock` も)、待ち受けは oep-spec の host ガイド §7.2 の `oep/brokers/<unit_id>.json`(0600。unit_id で名指せない probe は runtime dir の内部の記録)。
  - **公開の口**: `broker endpoint --json` の `result`(`endpoint`、`pid`、`transport`、`version`、`implementation`、`file`。ブローカーが動いていなければ `file` 以外は null)、host ガイド §7.2 の `oep/brokers/<unit_id>.json` と `.lock`(OEP の取り決めで、どの道具も読む)、そのブローカーの TCP(127.0.0.1、素の OEP)。runtime dir のファイル(`.broker.json`、`.status`、`.broker.lock`、`.sid`、`.broker.log`、`.ports.json`、`.unit`、`<key>.lock`)は内部のもので、名前も中身も約束しない。
  - **ロック**: ブローカーのロックは 1 つ。ch32rv の道具は、open に名前つき拡張 `oep.probe.share`(host ガイド §7.1、critical)を付けて相乗りする(monitor、gdb、flash を同時に使える)。ほかの道具も同じ拡張で相乗りできる。拡張の無い OEP の host には、1 つの probe として core のとおりに答える(相乗りの間は locked、force なら相乗りから奪う。詳しくは docs/oep-host.ja.md §7.2)。
  - **版**: ブローカーは記録の `implementation` に `{"name":"ch32rv","version":"<版>"}` を書く。ch32rv の client は、名前と版の両方が自分と同じブローカーとだけ話す(ほかの実装のブローカーも、別の版の ch32rv と同じに扱う)。合わなければ §3.7 の「別の版の ch32rv のブローカー」のとおり待って exit 13。この PC のブローカー自身を `oep://<unit_id>?via=tcp:127.0.0.1:<port>` で名指したときも、その port の記録で決め、別の版ならすぐ exit 13。WCH-Link を借りる直接のコマンド(flash など)も同じ。一覧(`probe list`、`arduino discovery`)は別の版のブローカーにはつながず、ブローカーが無いときと同じに扱う。confirm の答えには版を付けない(confirm の答えは 64 byte に収まる、oep-core §7.1)。ch32rv の外の利用者には、WCH-Link のブローカーは fn 0 の describe の `firmware` に `ch32rv-broker <版> protocol <番号>` と書く(WCH-Link を OEP の probe として見せているのはブローカー自身なので)。OEP の probe の前のブローカーは中継なので、`firmware` は probe のものをそのまま返す。
  - **OEP の probe**: どの経路(`oep://` のどの `via` も、serial port の path も)でも、OEP の probe を使うコマンドはすべてブローカーを通る(無ければ起動する。どの経路でも同じ振る舞い)。`oep://` の probe のブローカーの key は経路によらず unit_id(この PC での持ち主は 1 つ)。ブローカーは住所の `via` を守って probe を開き、開いたら describe の unit_id が住所の `<probe>` と同じかを確かめ、違えば使わない。`via` 無しか `via=net` のブローカーは起動のたびに DNS-SD で引き直す(Wi-Fi に入り直してアドレスが変わっても追う)。この PC の ch32rv のブローカー自身の `127.0.0.1:<port>` を `via=tcp:` で名指したときだけ、ブローカーを重ねずに直接つなぎ、その接続で unit_id を確かめる。WCH-Link のブローカーの unit_id は Link の serial(小文字)なので、`oep://<Link の serial>?via=tcp:127.0.0.1:<port>`(`<port>` は `broker endpoint --json` の `endpoint`)で名指せる。
  - **WCH-Link**: ブローカーの裏に置け、OEP を Link に写す(attach は AttachChip、riscv-dm は DmiOp / Debug Module、console は dmdata / dmseq の mailbox をブローカーが poll する)。`arduino monitor` の dmdata / dmseq は LinkE でもブローカー経由。verify / read / reset / target info は、その Link のブローカーが動いていればブローカーを通り、動いていなければ直接開く。flash は、ブローカーが動いていれば Link を借りて(`io.github.ch32-riscv-ug.wchlink` の lend / reclaim。貸している間ブローカーは console の手元の出力だけを答える)直接の経路(WCH の stub)で書き、終わったら返す。uart は CDC だけを使いブローカーに触れない。それ以外で Link を直接開くコマンド(probe info / mode / power / firmware、target option / protect、dbg、erase、recover、capabilities、write、run、monitor の sdi / rtt)は、ブローカーが動いていれば flash と同じく Link を借りて使い、終わったら返す。**貸している間は誰も console を汲まないので、その間に target が出した出力は落ちる**(dmseq の target は答えが無いと待ってから捨てる)。**gdb はブローカーの client**: ブローカーが動いていれば Link を借りずにブローカー経由で attach し(monitor の接続を共有)、gdb の間も console は流れ続ける(target が止まっている間は出力が無いだけ)。ブローカーが無ければ直接開く。
  - gdb は止めた時の DATA0 / DATA1(DM の console の mailbox)を覚え、走らせる直前に戻す(register の読み書きは DATA0 を通るので、残った値で dmseq が止まるのを防ぐ)。OEP の probe では、走らせるのに probe 自身の resume を使う(probe は host が raw DMI を書いてから自分で resume するまで console の poll を止めるので、DMI で走らせると console が止まったままになった。X035 治具で実測)。
  - 後始末の flash breakpoint の復元は、hart が走っていれば止めてから行い、走らせ直す(continue 中に GDB が切れた session で `ebreak` が flash に残った。V203 で実測)。戻せなかった page は警告する。
  - 実機確認: X035 治具(OEP、vendor bulk)と LinkE→V203 のブローカー経由で、monitor(dmseq)を開いたまま riscv-none-elf-gdb の break / step / メモリの読み出し / detach が通り、monitor は gdb の後も流れ続けた。V203(trigger 無し)の flash breakpoint は 1 回に約 2 秒(ブローカー経由 1.9 秒、直接 1.6 秒)で、gdb の既定の応答待ち 2 秒に近いので `set remotetimeout 10` を勧める(接続時の表示にも出す)。
- **OEP の probe で使えるコマンド**: `flash`(ch32rv の RAM loader)、`verify`、`read`、`reset`、`target info`、`gdb`(§4.6。止めずに attach し、GDB がつながったら止める。終わったら flash の breakpoint を外し、止まっていれば走らせてから detach)、`arduino monitor`(console / fixture-uart)、`arduino discovery`(`oep://`)。`--probe` は `oep://<probe>[/<port>[/<target>]][?via=<経路>]`(§3.4)、または WCH-Link のものでない serial port の path。**TCP の probe の発見(oep-spec transports §3)**: TCP で待ち受ける probe は DNS-SD の `_oep._tcp` を mDNS で名乗る(TXT `unit_id=`、port は SRV)。ch32rv は 1 回の問い合わせ(224.0.0.251:5353 へ、unicast で答えを受ける。IPv4 だけ)を、IPv4 のすべての口からそれぞれ出して引く(Windows では 0.0.0.0 のソケットだと WSL などの仮想アダプタから出て、Wi-Fi の probe に届かない。Windows で ATOM の probe を見つけて確認済み)。USB に無い probe の `oep://<unit_id>/<port>` と `?via=net` はこれで見つけ、開いた後に describe の unit_id が同じときだけ使う。`probe list` は USB にもある probe に `tcp`(`<address>:<port>`)を足し、ネットワークだけの probe を `network` に出す。`_oep._tcp` は IANA に登録しない名前で他のサービスも名乗りうるので(oep-spec host ガイド §4.1)、名乗った unit_id に短い confirm と describe で答えたものだけを出し、ほかは警告 `oep-tcp-unverified` をつけて除く。`arduino discovery` はネットワークだけの probe の口も `oep://<unit_id>/<port>` で出す(properties の `tcp`)。mDNS は同じリンクの中だけなので、NAT の裏(WSL 2 の既定、VM)では見つからない: そのときは `oep://<unit_id>/<port>?via=tcp:<host>:<port>` で名指す。target の family は attach で読む chip id から引く。
  - **口の選び方**(oep-spec host ガイド §5.3 の解決): attach せずに分かること(probe の unit_id、保存した label が名指す口の名前)だけで 1 つに絞り、残りが 1 つでなければ止める。家系(`--chip`)で口を選ぶことはしない(家系は、口の label から wire が決まらないときに wire を決めるのにだけ使う)。
    - `oep://<probe>/<port>`: その口。その名前の口が無ければ exit 10(口の一覧つき)。
    - `oep://<probe>` だけ: probe 自身を指す。「どれでも」を表す書き方は無い。候補が 1 つのときに口を省くのは、この道具の便利さ(host ガイド §5.3 が許す)。
      - 名前つきの口がちょうど 1 つなら、そこを使う。
      - 名前つきの口が無ければ、`--pins` で線を指す(無ければ exit 2)。
      - 2 つ以上なら exit 23(一覧つき)。
    - `oep://<probe>/<port>/<target>`(target は `<family 小文字>-<UID の 16 進>`): その口で作業に使う接続を作り、書き込みなどの前に同じ接続で UID を読んで照合する。違えば何もせず exit 23。
    - アドレスではない serial port の path は、口が 1 つならそこを使い、2 つ以上なら一覧つきで exit 23。
    - wire は口の label で決める(`S.swio` があれば swio、`S.swclk` があれば rvswd)。決まらなければ `--chip` の家系から。ピンはその口の `S.swdio` / `S.swclk` / `S.swio`。probe が組を 1 つだけ許す wire はピンを付けずに attach し、host がピンを選ぶ wire で label が無ければ「ピンを名指して」と止める(scan で組を探すことはしない)。fixture の UART は `S.rx` / `S.tx`、NRST は `S.nrst`(口が 1 つなら素の `nrst` も、最後に firmware の label)。
    - rvswd の線の休ませ方(oep-spec oep-if-debug §3): `--chip`(またはアドレスの target)が CH32L103 なら idle_clock low を明示する。指定が無ければ probe が決め(設定 `channel.{}.rest`、無いか auto なら target_id の系統)、attach の応答の休ませ方を読む。L103 なのに low でない接続(設定で自動を切った probe や古い firmware)は、low を明示して attach し直す。

## 5. 呼び出し例

Arduino recipe(platform.txt、ArduinoCore-CH32RV の upload)。port は discovery の address をそのまま渡し、ch32rv が自分で解く。debug probe の経路(`wchlink://` / `oep://` / serial port)は `--probe`、書き込み先の経路(`ch32rv-app://` / `ch32rv-isp://` / `ch32rv-hid://`)は `isp` / `boot` の `--device`。`{build.ch32rv_chip}` は板ごとに SKU か family、ch32rv に名前の無い series の板は series 名(DB に無ければ exit 20 で書く前に止まる)。`auto` は使わない(つながっている別の chip に書いてしまう。docs/freeze-decisions.ja.md §1):

```text
"{path}/{cmd}" flash "{build.path}/{build.project_name}.elf" --format elf --chip {build.ch32rv_chip} --reset run --confirm-run --non-interactive --progress none {upload.verbose} --probe "{upload.port.address}"
"{path}/{cmd}" --non-interactive --progress none isp flash "{build.path}/{build.project_name}.elf" --chip {build.ch32rv_chip} --entry auto --device "{upload.port.address}" {upload.verbose}
"{path}/{cmd}" --non-interactive --progress none boot dfu flash "{build.path}/{build.project_name}.bin" --format bin --device "{upload.port.address}" {upload.verbose}
"{path}/{cmd}" --non-interactive --progress none boot hid flash "{build.path}/{build.project_name}.bin" --device "{upload.port.address}" {upload.verbose}
```

CI / HIL:

```sh
ch32rv probe firmware check --min 2.12 --probe name:bench-01        # 既知不良版なら exit 12
ch32rv flash app.elf --probe name:bench-01 --json > result.json     # 結果を JSON で残す
ch32rv run tests.elf --probe name:bench-01 --source dmdata --exit-on semihosting
```

日常:

```sh
ch32rv flash blink.elf --monitor sdi      # 書いて、走行確認して、そのまま SDI print を見る
ch32rv doctor                             # 動かない時の一手目
```

## 6. 互換性ポリシー

- **v1 の凍結までは、以下の規則を適用しない**: 破壊的変更をまとめて入れ、関係するツールが一度に追従する。過去との互換は持たない。
- 凍結後の規則:
  - **contract 版**(JSON schema・NDJSON event・exit code)は CLI 版と独立に管理し、破壊変更でのみ版を上げる。field 追加は随時。
  - command と flag は追加のみ。廃止する場合は 2 minor 版の deprecation 警告を挟む。
  - exit code は追加のみ(§3.6 の帯を守る)。
  - `--json` の schema は `docs/contract/` に置き、release ごとに固定する。schema は知らない field を禁じない。実際の出力は試験(cli/tests/contract.rs、ほかの試験の `--json` も)で schema と照合する。
  - `error.kind` と `warnings[].code` の名前は凍結する(§3.5)。

## 7. 参照

- [requirements.ja.md](requirements.ja.md)(吸収マップと根拠)
- `../../ArduinoCore-CH32RV/platform.txt`(recipe 制約の現物)
- [Arduino Pluggable Discovery / Monitor specification](https://arduino.github.io/arduino-cli/latest/pluggable-discovery-specification/)

### H417 のコア選択

`ch32rv --probe serial:49808F06CE30 --core 1 dbg reg read csr:0xf14` は core 1 の mhartid を読む。`dbg regs` / `dbg halt` / `dbg resume` / `dbg step` / `dbg reg` / `dbg dmi` と `read` は、同じ attach セッションで選択した hart を扱う。DMCONTROL の halt / resume / reset-ack 書き込みでも選択を保持する。`--core 0` は通常の経路を使う。

core 1 の `read` は、LinkE の bulk read による暗黙のコア切替を避けるため DMI 経由で行う。RAM は `--range` で実アドレスを指定する(H417 の RAM bank を `--region ram` では指せない)。Flash・option byte の変更、システム reset、GDB・monitor などは core 1 に対応しない(exit 24)。raw `dbg dmi write` は expert 操作で、DMCONTROL の直接変更も transport が保持する hartsel に反映する。

WCH-Link の AttachChip / DetachChip は probe firmware 自身の操作を含む。`--core 1` はその副作用や他コアの状態を完全に隔離する保証ではない。選択していないコアを halt / resume しないことを USB 上の host 発行 DMCONTROL で検証する。

選ばなかったコアの扱い: LinkE の AttachChip は、選ばなかった hart も止め、DetachChip では放さない。そのままでは `--core 0 read` の後に core 1(V5F)が、`--core 1 dbg resume` の後に core 0(V3F)が止まったまま残る。

- `dbg` 以外のコマンド(`read`、`flash`、`reset`、`target info` など)は、接続した直後に各 hart を調べ、終わるときに「接続する前に走っていた hart」だけを resume する。走っていたとみなすのは、走っている hart と、pc が 0 でない所で止まっている hart(AttachChip に止められたもの)。DMSTATUS の `allhavereset` は判定に使わない(reset の後に core 0 の WAKEIP で起きた core 1 は、立てたまま走る。resume のときに ack する)。終わるときにも同じ条件をもう一度確かめる。core 0 を選んだときの core 0 は LinkE に任せる(`flash --reset none` などはそのまま止めておく)。**reset から止まっている hart は resume しない**: H417 の core 1 は reset の後 pc 0(V3F の image)で止まっていて、resume すると両コアが core 0 のスケッチを走らせた(7b42aff の `--release-dm reset` で ArduinoCore-CH32RV が実測)。そのため、別の session の `dbg halt` で止めた hart も、`dbg` 以外のコマンドの後には走り出す。
- `dbg` のコマンドは、選ばなかったコアに触らない(上の約束どおり)。両方を走らせるには `dbg resume --all` を使う。選んだ hart を resume し、終わるときに他方も resume する。
- デバッガの状態を残さずに試すには `--release-dm`(§2 の表)を使う。
