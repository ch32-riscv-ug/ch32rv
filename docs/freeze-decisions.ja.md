# v1 の契約の決まり(ch32rv)

- この文書は、ch32rv の外の道具(ArduinoCore-CH32RV、pytest プラグイン、bench)が頼る契約の決まりを項目ごとに置く。コードのコメントは、この文書の節の番号で契約の元を指す。
- v1 の凍結までは破壊的変更をし、関係するツールがまとめて一度に追従する。過去との互換は持たない。
- 契約の版は `ch32rv-contract` の `CONTRACT_VERSION`(JSON の封筒の `contract`)。コマンドの詳細は [cli.ja.md](cli.ja.md)。

## 1. `--chip` の語彙と照合

- **語彙**: device-data の family / SKU / series 名。v1 の凍結の時に device-data の名前も一緒に凍結する。凍結後に device-data が名前を変えたら、ch32rv に別名の表を足して吸収する。
- **照合**:
  - 完全一致だけ(大小は無視)。前方一致はしない。
  - family / series を指定したら family で照合する。SKU を指定したら、その SKU の chip id と照合する(DB に chip id が無い SKU は family で照合し、warning を出す)。
  - CLI の `--chip` と monitor の `chip` 設定は同じ照合を使う(monitor でも大小無視、series を受ける)。
  - 空の `--chip` は usage の誤り(exit 2)。DB に無い名前は exit 20(`target-not-in-db`)で、probe を開く前に止まる(bench に触らない)。
- **`--chip auto`**(大小無視): 「`--chip` 無し」と同じ値。recipe は固定の 1 行で、値が空でも引数を消せないためにある。`auto` はつながっている chip を見つけてそこに書くので、**upload の recipe には使わない**(別の板の image が、つながっている chip に書けて exit 0 になる)。自動判定での失敗の形: pin に何も居ない → `target-no-response`(20)、読んだ chip id が DB に無い → `target-not-in-db`(20)、見つけた family を書く手段が無い → `capability-unsupported`(24)。
- **platform の渡し方**: 空の値を渡さない。板ごとに、ch32rv が知っている SKU、無ければ family、ch32rv に名前の無い series の板にはその series 名を渡す(DB に無い series は exit 20 で書く前に止まる)。`auto` は monitor の `chip` 設定の既定(板を決めずに開く)で使う。

## 2. port の scheme と ID

- `wchlink://<serial>`。serial の無い Link は `wchlink://usb-<bus>-<ports>`(位置)で出す。`unknown` は出さない。
- topology は `<bus>-<ports>` だけ。port chain の取れない OS では位置が device address(`<bus>-addrN`、挿し直しで変わる)になるので、discovery と住所には出さない(その device は `--probe` で指定する)。
- `oep://<probe>[/<port>[/<target>]]`: `<probe>` は describe の unit_id で、USB の probe は USB の serial number = unit_id。serial の無い device(UART bridge)だけ位置で出す。`<port>` は probe の label `S.N` の `S`(英字で始まる英字・数字・`-` の 1〜16 文字)。文字は仕様で絞られているので encode はしない。書き方の全体は cli.ja.md §3.4。
- OEP の probe の見分けは、プロジェクトの USB ID 1209:4F45(pid.codes で取得。iProduct では判定しない)。vendor bulk の interface は class 0xFF・subclass 0x4F・protocol 0x45、HID は usage page 0xFF4F・usage 0x45 で見分ける。
- 待つポート: `ch32rv-bootloader://WAIT_FOR_BOOT`(protocol `ch32rv-bootloader`、label "USB bootloader (waits for BOOT)")。discovery は、何もつながっていなくてもいつも出す。書き込むときにだけ現れるボードのブートローダーを、書き込みを押してから待つための住所で、`://` の後ろは個体でなく `WAIT_FOR_BOOT`(大小を問わない。ほかの語は exit 2)。どのブートローダー(工場の USB ISP か HID)かは platform の道具で決まる(既定 `isp flash`、HID のボードは `boot hid flash`)。`isp flash` は、USB の周辺を持たないチップ(工場の USB ISP が無い)を待つ前に exit 24。ソフトウェア USB の HID ブートローダー(V003 の UIAPduino の rv003usb など)は `boot hid flash` の側で、チップを確かめない。debug probe は候補にしない。ArduinoCore-CH32RV と合意(2026-11-10)。名前は、「auto」(1 台しか無いとそこに書くと読める)、「any」(住所はいつも全部を書く、の決まり)、経路ごとに 2 つ(一般の利用者は ISP と HID の違いを知らない)を避けて決めた。
- monitor の `--port` はシリアルの口のパスだけで、どの Link かは `--probe` で選ぶ。

## 3. monitor の `source` と既定値

| protocol | source(先頭が既定) |
|---|---|
| `serial` | `uart`、`sdi`、`dmdata`、`dmseq`、`rtt`、`fixture-uart` |
| `wchlink` | `dmdata`、`dmseq`、`sdi`、`rtt` |
| `oep` | `dmseq`、`dmdata`、`sdi`、`fixture-uart`、`rtt` |

- `wchlink` に `uart` と `fixture-uart` は無い(uart は Link の CDC を `serial` で開く)。
- `arduino monitor` の `baudrate` の既定は 9600(組み込みの serial-monitor に合わせる)。CLI の `monitor --baud` の既定は 115200(別のもの)。
- `--protocol` は `serial` / `wchlink` / `oep` / `ch32rv-app` / `ch32rv-isp` / `ch32rv-hid` / `ch32rv-bootloader` を受ける。ほかは usage の誤り(exit 2)。後ろの 4 つの source の並びは `serial` と同じ(`ch32rv-app` はアプリの CDC を開き、`ch32rv-isp`・`ch32rv-hid`・`ch32rv-bootloader` は OPEN を断る)。

## 4. ブローカー

- key は probe の同一性。OEP の probe は `oep-<unit_id の小文字>`(USB の probe は serial = unit_id)。unit を名指さずに渡した、OEP の USB device を持たない serial port(UART bridge)は `oep-port-<正規化した path>`。WCH-Link は `wch-<serial>`(無ければ `wch-usb-<topology>`)。同じ probe は 1 つのブローカーになる。
- `broker endpoint --json` は結果の封筒(envelope)の `result` に `{endpoint, pid, transport, version, implementation, file}` を返す。`endpoint` は `"127.0.0.1:<port>"` か null。利用者は `result.endpoint` を見る。
- ブローカーの TCP は素の OEP(`length(u16) message`、oep-transports §1)で、127.0.0.1 だけで待つ。
- 詳しくは [oep-host.ja.md](oep-host.ja.md) §7.2。

## 5. OEP host の版

- ch32rv が合わせている oep-spec の版は、`crates/oep/src/registry.rs` の先頭の `source:` の 1 行だけを正とする。文書と CHANGELOG は版を書き写さず、registry.rs を見るよう書く。

## 6. `run` の終了コード

process の exit は tool のコードだけ。target の exit code を process の exit にしない。

- target が 0 で終わる → exit 0(`result.exit = 0`)。
- target が 0 以外で終わる → `target-exit`(**exit 60**)。`result.exit` と誤りの文面に target のコードを入れる。
- `--duration` の上限に達した → `run-timeout`(**exit 61**)。`--exit-on timeout` のときは成功(exit 0、`reason: "timeout"`)。
- semihosting でない halt(breakpoint、例外)→ `target-halted`(**exit 62**)。dpc などは `result` に入れる。
- `--json` でも同じ exit を返す。

## 7. JSON の契約

- 契約の版は `CONTRACT_VERSION`。code・両 schema・README・cli.ja.md の例はこの値に揃える。
- `flash` の結果は経路によらず `result.flash` の 1 つの形: `bytes`、`family`、`programmer`(`stub` / `controller` / `loader` / `oep-loader` / `hid` / `isp` / `dfu`)、`verified`(bool。検証しなかったら null)、`running`(bool / null)、`skipped`、`scope`、経路が持つもの(`pages`、`rewritten`、`chip_id`、`seconds` など)。形は `docs/contract/commands.schema.json` の `flash_result`。
- 検証の結果は、どのコマンドでも `verified`(bool)。
- 再試行は `retry` の event で出す(結果の field には持たない)。
- firmware の版は、どこでも `"<major>.<minor>"` を 0 埋めしない形(例 "2.9"、"2.10")。
- probe は `variant`(安定した ID: `linke`、`link-ch549`、`linkw`、`links`、`daplink`、知らない型番は `unknown-<16 進>`)と `model`(表示用の名前)を分ける。照合には `variant` を使う。
- 実際の出力は試験(`cli/tests/contract.rs` など)で schema と照合する。

## 8. 終了コードの文書と予約

- 終了コードの表は cli.ja.md §3.6 だけ。20 は 1 つ(`target-no-response` と `target-not-in-db` は kind で分ける)。21(target-protected)は HID の読み出し保護と loader の書き込み保護で出す。24 は、各コマンドがその操作に要るものが無いと分かった所で出す。
- 71 は欠番で、使い回さない。
- 予約だけのコマンドは持たない。知らないコマンドは usage の誤り(exit 2)。
- crates.io の `ch32rv-contract` の `ExitCode` は `#[non_exhaustive]`(番号を足しても利用者の match が壊れない)。

## 9. 受け付けて無視する global は持たない

効かない経路で受けた global は、無視せずに断る。

- `--dry-run`: 対応していないコマンドは exit 2 で断る。
- `--core`: 直接 WCH-Link 接続の CH32H417 の `--core 1` の `dbg`(`halt --reset` を除く)と `read` だけ。その他の非 0 は capability-unsupported(exit 24)で断る。halt / resume / step の DMCONTROL は選択を保持する。根拠は [H417 コア選択実測](data-requests/0009-h417-core-selection.ja.md)。
- `--connect-under-reset`: WCH-Link と OEP の probe で使える(cli.ja.md §3.1)。OEP の probe の `flash` では exit 24。
- `--pins` は OEP の probe へ行かないコマンドで、`--release-dm` は OEP の probe へ行くコマンドで exit 24(`arduino monitor` はアドレスを IDE から受けるので断らない)。
- config の `[defaults] chip`: `--chip` も `CH32RV_CHIP` も無いときに使う。

## 10. config の置き場と環境変数

- config の置き場: `./ch32rv.toml`、次に OS ごとの場所(Linux `$XDG_CONFIG_HOME/ch32rv/config.toml`、無ければ `~/.config/ch32rv/config.toml`。macOS `~/Library/Application Support/ch32rv/config.toml`。Windows `%APPDATA%\ch32rv\config.toml`)。
- 環境変数は cli.ja.md §3.2 の表。切り分け用の変数(`CH32RV_PORT_SPEED`、`CH32RV_USB_TRACE`、`CH32RV_OEP_TRACE`)は契約の外。

## 11. udev の規則

- `doctor --emit-udev` が出す `60-ch32rv.rules` には、WCH-Link、WCH の IAP / ISP(`4348:55e0`、`1a86:55e0`)、HID bootloader(`1209:b803`、`1209:b003`。USB と hidraw)、OEP の probe(`1209:4F45`。USB と hidraw)が入る。
- ArduinoCore-CH32RV の CI は `doctor --emit-udev` との byte 一致を見るので、規則を変えるときは core と同時に出す。

## 12. firmware の既知の不良

- 表は ch32-device-data の `index/link_firmware_issues.csv` から生成し、`(variant, mode, 版)` で照合する。probe が mode を返さないときは mode を問わず当てる。
- hash では照合しない。

## 13. 文書の状態と運用

- contract の README と cli.ja.md は「凍結候補(v1 の凍結で確定)」。
- 「2 minor の deprecation」と「release ごとの schema 固定」は凍結後の規則で、凍結までは適用しない(破壊的変更をまとめて入れる)。
