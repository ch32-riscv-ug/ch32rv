# v1 凍結前の契約の決定(ch32rv)

- 日付: 2026-10-01
- 発端: ArduinoCore-CH32RV の洗い出し(凍結前に破壊的変更が要りそうな所、13 項目)
- 方針: 凍結までは破壊的変更をし、関係するツールがまとめて一度に追従する。過去との互換は持たない(ユーザーの方針、2026-09-30)。
- 決め方: 1・2・6・8 の中の 4 点はユーザーが決めた。ほかは ch32rv の担当として決め、この文書を契約の元にする。
- 実装: この文書の順に入れる。入れたものには commit を書く。契約の版は **"3" → "4"** に上げる(§7)。

各項目は「今」「決定」「追従する相手」の順に書く。

## 1. `--chip` の語彙と照合

- **今**: device-data の family / SKU / series 名を、大小無視・前方一致で受ける(`--chip C` が通る)。SKU を指定しても family だけで照合する。monitor の `chip` 設定は完全一致で、series 名を断る。空の `--chip ""` は全 family に当たって通る。
- **決定(ユーザー)**: 語彙は device-data の family / SKU / series 名のまま。v1 の凍結の時に device-data の名前も一緒に凍結する。凍結後に device-data が名前を変えたら、ch32rv に別名の表を足して吸収する。
- **決定(照合)**:
  - 完全一致だけにする(大小は無視)。前方一致をやめる。
  - family / series を指定したら family で照合する。SKU を指定したら、その SKU の chip id と照合する(DB に chip id が無い SKU は family で照合し、warning を出す)。
  - CLI の `--chip` と monitor の `chip` 設定は同じ照合を使う(monitor でも大小無視、series を受ける)。
  - 空の `--chip` は usage の誤り(exit 2)。DB に無い名前は今までどおり exit 20。2026-10-01 からは probe を開く前に止まる(bench に触らない)。
  - **`--chip auto`**(大小無視、2026-10-01 追加、ArduinoCore-CH32RV の依頼): 「`--chip` 無し」と同じ正規の値。recipe は固定の 1 行で、値が空でも引数を消せないために足した。ただし `auto` はつながっている chip を見つけてそこに書くので、**upload の recipe には使わない**(ArduinoCore-CH32RV が「[compile only]」の板で試し、Generic CH32V205 の image が V203 に書けて exit 0 になった。2026-10-01)。自動判定での失敗の形: pin に何も居ない → `target-no-response`(20)、読んだ chip id が DB に無い → `target-not-in-db`(20)、見つけた family を書く手段が無い → `capability-unsupported`(24)。
- **追従**: platform は空の値を渡さない。板ごとに、ch32rv が知っている SKU、無ければ family、ch32rv に名前の無い series の板にはその series 名を渡す(ArduinoCore-CH32RV f131155)。DB に無い series は `target-not-in-db`(exit 20)で書く前に止まる。`auto` は monitor の `chip` 設定の既定(板を決めずに開く)で使い、upload には使わない。文書の「空は 20」は「空は 2」に直す。

## 2. port の scheme と ID

- **今**: serial の無い Link が `wchlink://unknown` で衝突する。topology は `<bus>-<ports>` か、port chain が取れないと `<bus>-addrN`(挿し直しで変わる)。`oep://` の slot 名を escape しない。OEP の probe は iProduct の `OEP` 接頭辞で見分ける。
- **決定**:
  - `wchlink://<serial>`。serial の無い Link は `wchlink://usb-<bus>-<ports>`(位置)で出す。`unknown` は出さない。
  - topology は `<bus>-<ports>` だけ。port chain の取れない OS の `addrN` は「挿し直しで変わる」と文書に書き、discovery では出さない(その device は `--probe` で指定する)。
  - `oep://<unit_id>/<slot>`(dev_oep の決定、2026-10-01、oep-spec v1-freeze #3 で確定): `<unit_id>` は describe の unit id で、USB の probe は serial number = unit_id(P4 の `-hs` は外す)。ch32rv は USB の serial をそのまま使うので、これで unit_id になる。serial の無い device(UART bridge)だけ位置で出す。1〜32 byte の `a-z 0-9 -`。`<slot>` は 1〜32 byte の `a-z 0-9 - _`(probe-config §1.1)。どちらも仕様で文字を絞るので encode はしない。
  - OEP の probe の見分けは、プロジェクトの USB ID 1209:4F45(2026-10-06、pid.codes で取得。iProduct では判定しない)。vendor bulk の interface は class 0xFF・subclass 0x4F・protocol 0x45、HID は usage page 0xFF4F・usage 0x45 で見分ける。
  - monitor の `--port` の `path:` / `usb:` の文法は削る(ユーザー)。`--port` はシリアルの口のパスだけで、どの Link かは `--probe` で選ぶ。
- **追従**: pytest プラグインと bench は `unknown` を当てにしない。

## 3. monitor の `source` と既定値

- **決定(凍結する表)**:

| protocol | source(先頭が既定) |
|---|---|
| `serial` | `uart`、`sdi`、`dmdata`、`dmseq`、`rtt`、`fixture-uart` |
| `wchlink` | `dmdata`、`dmseq`、`sdi`、`rtt` |
| `oep` | `dmseq`、`dmdata`、`sdi`、`fixture-uart`、`rtt`(2026-10-01 に追加。足すだけなので互換) |

  - `wchlink` に `uart` と `fixture-uart` は無い(uart は Link の CDC を `serial` で開く)。
  - `arduino monitor` の `baudrate` の既定は 9600(組み込みの serial-monitor に合わせる)。CLI の `monitor --baud` の既定は 115200(別のもの)。
  - `--protocol` は `serial` / `wchlink` / `oep` だけを受ける。ほかは usage の誤り(exit 2)。
  - 文書の 3 か所をこの表に揃える。
- **追従**: IDE の保存設定と bench の `port_config: source: dmseq` はこの表のまま使える。

## 4. ブローカー

- **今**: key は `oep-<serial port の path>`。同じ probe に `oep://` と別の CDC の `port:` で 2 つのブローカーができうる。`broker endpoint --json` の help と実際の形が違う。
- **決定**:
  - key は probe の同一性にする。OEP の probe は `oep-<unit_id>`(USB の probe は serial = unit_id。UART bridge も開けば describe の unit id が分かる)。unit id が読めない間だけ `oep-port-<正規化した path>`。WCH-Link は `wch-<serial>`(無ければ `wch-usb-<topology>`)。これで同じ probe は 1 つのブローカーになる。
  - `broker endpoint --json` は結果の封筒(envelope)の `result` に `{endpoint, pid, transport}` を返す。`endpoint` は `"127.0.0.1:<port>"` か null。help と文書をこれに揃える。
  - ブローカーの TCP は素の OEP(`length(u16) message`、oep-core §3.1)で、127.0.0.1 だけで待つ。
- **追従**: pytest プラグインは `result.endpoint` だけを見ればよい(両方を許す分岐は消してよい)。

## 5. OEP host の追従

- スロットの項目は今の仕様の形で読む(固定部 17 byte)。dev_oep の「末尾を読み飛ばす規則」が入ったら、それに従う。
- oep-spec の版の記録は `crates/oep/src/registry.rs` の先頭の 1 行だけを正とする。文書と CHANGELOG は、その時点の版として書き、今の版は registry.rs を見るよう書く。

## 6. `run` の終了コード

- **今**: target の semihosting の exit code を process の exit にする(tool の誤りのコードと衝突、256 は 0 になる)。`--json` では常に 0。上限時間は transport-timeout(40)。semihosting 以外の halt は 50。
- **決定(ユーザー)**: process の exit は tool のコードだけにする。
  - target が 0 で終わる → exit 0(`result.exit = 0`)。
  - target が 0 以外で終わる → 新しい種類 `target-exit`(**exit 60**)。`result.exit` と誤りの文面に target のコードを入れる。
  - `--duration` の上限に達した → 新しい種類 `run-timeout`(**exit 61**)。`--exit-on timeout` のときは今までどおり成功(exit 0、`reason: "timeout"`)。
  - semihosting でない halt(breakpoint、例外)→ 新しい種類 `target-halted`(**exit 62**)。dpc などは `result` に入れる。
  - `--json` でも同じ exit を返す。
- **追従**: HIL の試験(pytest)は exit 60 と `result.exit` を見る。

## 7. JSON の契約(版 "4")

- **今**: code は "3"、events.schema は "1"、README は「1(draft)」、cli.ja.md の例は "1"。`flash` の結果が経路で 4 つの形。`verify`(文字列)と `verified`(bool)が混ざる。`retries` は文書だけ。firmware の版の表記が "2.9" と "2.09"。`probe.model` が自由文。
- **決定**:
  - 契約の版を **"4"** にし、code・両 schema・README・cli.ja.md の例を揃える。
  - `flash` の結果は経路によらず `result.flash` の 1 つの形にする: `bytes`、`family`、`programmer`(`stub` / `controller` / `loader` / `oep-loader` / `hid`)、`verified`(bool。検証しなかったら null)、`running`(bool / null)、`skipped`、`scope`、経路が持つもの(`pages`、`rewritten`、`chip_id`、`seconds` など)。`verify` の文字列はやめる。
  - 検証の結果は、どのコマンドでも `verified`(bool)。
  - `retries` は文書から消す(`retry` の event はそのまま)。
  - firmware の版は、どこでも `"<major>.<minor>"` を 0 埋めしない形(例 "2.9"、"2.10")にする。`firmware info` の "2.09" をやめる。
  - probe は `variant`(安定した ID: `linke`、`link-ch549`、`linkw`、`link` など)と `model`(表示用の名前)を分ける。照合には `variant` を使う。
  - release ごとに schema を固定し、CI で実際の出力を schema と照合する試験を足す。
- **追従**: bench は `probe.model` ではなく `probe.variant` を照合する。`flash` の結果を読む所は `result.flash` を見る。

## 8. 終了コードの文書と予約

- 文書の 20 の重複を 1 つにする。21(target-protected)は発行している(HID の読み出し保護、loader の書き込み保護)と書く。24 の説明を今の実装(コマンドごとに出す)に合わせる。
- `unimplemented` は internal(70)と別の **exit 71** にする。
- 未実装のコマンド(`isp`、`boot enter` / `dfu` / `uf2` / `uart`、`dap`、`probe vendor`)は `--help` から隠し、名前は予約する(ユーザー)。実行したら exit 71。
- crates.io の `ch32rv-contract` の `ExitCode` を `#[non_exhaustive]` にする(番号を足しても利用者の match が壊れないように)。

## 9. 受け付けて無視していた global

- `--dry-run`: 対応していないコマンドは exit 2 で断る(4e7808d、実装済み)。
- `--core`: 0 以外は capability-unsupported(exit 24)で断る(H41x の 2 つ目の core は未実装)。
- `--connect-under-reset`: 未実装なので exit 24 で断る(warning を出して無視するのをやめる)。
- config の `[defaults] chip`: 実装する(`--chip` も `CH32RV_CHIP` も無いときに使う)。

## 10. config の置き場と環境変数

- config の置き場: `./ch32rv.toml`、次に OS ごとの場所(Linux `$XDG_CONFIG_HOME/ch32rv/config.toml`、無ければ `~/.config/ch32rv/config.toml`。macOS `~/Library/Application Support/ch32rv/config.toml`。Windows `%APPDATA%\ch32rv\config.toml`)。
- 環境変数の表(cli.ja.md §3)に `CH32RV_OEP_TRANSPORT` と `CH32RV_USB_TRACE` を足す(どちらも切り分け用で、契約の外と書く)。

## 11. udev の規則

- 足す: WCH の IAP / ISP(`4348:55e0`、`1a86:55e0`)、HID bootloader(`1209:b803`、`1209:b003`、USB と hidraw)。
- OEP の probe は、プロジェクトの USB ID 1209:4F45 で見分ける(2026-10-06、pid.codes で取得。iProduct では判定しない)。
- core の CI は `doctor --emit-udev` と byte 一致を見るので、規則を変えるときは core と同時に出す。

## 12. firmware の既知の不良

- 表の鍵を `(variant, 版)` にする(今は版だけ)。
- hash での照合は実装していないので、文書から消す(版で照合する、と書く)。

## 13. 文書の状態と運用

- contract の README と cli.ja.md の「状態: 提案 / draft」を「凍結候補(v1 の凍結で確定)」にする。
- 「2 minor の deprecation」と「release ごとの schema 固定」は、凍結までは適用しない(破壊的変更をまとめて入れる)と書く。凍結後の規則として残す。
- CI の schema 照合は §7 の試験で実装する。
