# OEP host(ch32rv が OEP の probe で書き込み・モニタ・デバッグ・ブローカーをする)

- 日本語のみ([development.ja.md](development.ja.md) §1)
- **ch32rv が合わせている OEP の版は、`crates/oep/src/registry.rs` の先頭の `source:` の行だけを正とする**。番号の台帳は oep-spec の `registry/oep-v1.toml` から取る(§2)。
- 範囲: 書き込み・人が使うモニタ・discovery・ブローカー・gdb。コマンド、住所、exit の決まりは [cli](cli.ja.md)。
- 文中の「参照 client」は `oep-client-python`(`ch32_flash.py` / `riscv.py` / `link.py`)、「参照 probe」は `oep-probe-arduino`。

## 1. 方針

1. **target の知識は ch32rv だけが持つ**。OEP の probe は target を知らない。DB、flash の手順、RAM loader は host の側にある(OEP の前提)。
2. **OEP は WCH-Link と同じ境界に載せる**。
   - `OepDtm` は `DtmAccess`(dmi_read / dmi_write / dmi_nop / dmi_sequence)を実装し、既存の `DebugModule` をそのまま使う。
   - これに加えて、ブロックの読み書きと run を持つ、transport に依らない trait `TargetAccess` を実装する(§4.2)。
   - gdb server はこの 2 つだけで動く(§8)。
3. **family byte に頼らない**。WCH-Link の AttachChip の family byte で引く解決の連鎖(`params_for_family` / `flash_controller_profile` / `db_family`)は、OEP では使えない。chip_id(DM 0x7F。attach の target_id scheme 1)と ESIG から DB の family を引き、その family の `flash_program_method` / `flash_geometry` で手順を決める。
4. **仕様に無いことは黙って埋めない**。ch32rv が spec の外で要るものは、名前つき拡張 `io.github.ch32-riscv-ug.*` として置く(§7.2)。
5. OEP v1 の形だけを話す。v1 より前の wire や firmware との互換は持たない。

## 2. crate 構成

| crate | 責務 | 依存 |
|---|---|---|
| `ch32rv-oep` | OEP v1 の host 側。registry(番号)、address(`oep://` の文法)、codec(message、TLV、COBS + CRC-16、長さ見出し)、link(confirm、pipeline、resync、再送)、session(open / end / keepalive / lock_state)、config(probe の設定とラベル)、stream(console と fixture UART)、target(`oep.wire.*`、`oep.target.riscv-dm`、`OepDtm`)、hid(report の詰め方)、speed(port_speed) | dmi、serialport |

- vendor bulk と HID の device を開くのは `ch32rv-usb`(`BulkPipe`)と CLI。
- **番号の台帳**は `registry/oep-v1.toml` から `cargo xtask oep-gen <oep-spec の dir>` で Rust の定数を生成し、commit する。
  - 生成物には台帳の hash(LF に正規化した sha256 の先頭 16 桁。台帳の規則どおり)を埋め込む。
  - `cargo xtask oep-check` でずれを検出する。device DB の `db-gen` / `db-check` と同じ方式。
- `ch32rv-oep` は target を知らない(CH32 の手順は持たない)。CH32 の手順は `ch32rv-flash`(loader と編成)と CLI(`cli/src/oep.rs`、ブローカーは `cli/src/broker.rs`)にある。
- `dmi` の `DtmAccess` を実装するのは `wchlink` と `oep` の 2 つ。

## 3. transport と link

### 3.1 framing

| transport | frame | resync |
|---|---|---|
| serial に見えるもの(USB CDC、USB-Serial/JTAG、UART bridge) | `COBS(message ‖ crc16_le) 0x00`。CRC-16/CCITT-FALSE。送るときは前にも 0x00 を置く | 要らない。CRC の合わないフレームは捨てる。フレームの外のバイトは雑音として捨てる。欠落は時間切れで判断する |
| vendor bulk、TCP | `length(u16) message` | core §5.1(50 ms 静かになるまで捨てる → confirm 0..255) |
| HID | spec の report の詰め方(report ID + count(u16) + 長さ見出しの stream + 詰め物) | 長さ見出しと同じ |

- decoder は、0xFF block で終わる data の後に空 block を付ける形(参照実装の癖)と、付けない形の両方を受ける。空のフレーム(0x00 の連続)は無視する。
- serial port の COBS の受け方は oep-transports §1 のとおり。開いた直後から最初の 0x00 までもフレームの候補にし、role か corr の合わないフレームは雑音として捨てる。
- 長さ見出しの CDC / USJ の firmware と話す手段は持たない(spec も CDC / USJ の長さ見出しを持たない)。

### 3.2 serial の開き方

- 必ず排他で開く(serialport crate は unix で TIOCEXCL、Windows は元から)。起動時の速さは 115200。DTR / RTS は立てたまま(参照 client と同じ)。1200 baud では開かない。
- serial port は target の console と共用なので(ArduinoCore-CH32RV oep-workflow §4.1)、応答を待つ間に来るフレームの外のバイトは捨てる。
- UART bridge の probe の速さを上げるのはブローカーだけ(§7.2)。

### 3.3 USB の口の見つけ方

- **OEP の probe の見分け方(oep-transports §3)**: **プロジェクトの USB ID 1209:4F45 の device** だけを自動で OEP の probe とみなす(pid.codes で取得)。iProduct は表示の文字で、判定には使わない。ほかの device は、利用者が `oep://<unit_id>` で名指す(USB の serial と大小を区別せず比べる)か、口を選んだときだけ使い、開いたら confirm だけを送り、答えが無ければ閉じる。vendor bulk の interface は class 0xFF・subclass 0x4F・protocol 0x45、HID の interface は usage page 0xFF4F・usage 0x45。
  - 判定は 1 つの関数(`is_oep_device`)にまとめる。discovery(`oep://`)も、serial port を選んだときの「OEP の probe なら raw の serial port の upload は断る」(§6)も、この関数を使う。
  - 名前は device を開かずに読む(nusb の列挙が持つ product の文字列)。
  - fn 0 の describe の `discoverable`(0x4A)= 1 は「この形で列挙している device がある」という意味で、USJ などから開いたときにも分かる。
- 口は interface の種類で選ぶ(oep-transports §3): CDC(ACM)はすべて OEP を受ける serial port、vendor class の bulk の組は vendor bulk、vendor 定義の HID は HID。DFU や Mass Storage は OEP の外。vendor bulk、HID、serial port の順に試す(`oep::connect_upstream`)。ブローカーと discovery は、`oep://` / serial port の path の serial port を持つ OEP の device の vendor bulk → vendor HID → その serial port の順に開き、confirm が通った最初の経路を使う(開けない・答えないものは次へ。Linux で udev の規則が無く USB のノードに書けないときも serial port に落ちる)。住所の `?via=bulk|hid|cdc` はその種類だけ、`?via=tty:<path>` はその serial port だけを開く(cli §3.4)。どの経路でも、開いたら describe の unit_id が住所の `<probe>` と同じかを確かめる。
- vendor bulk は nusb で扱う(`ch32rv_usb::BulkPipe`)。alt 0 の vendor class(0xFF)の interface で、bulk の OUT と IN を持つ最初のもの。
  - IN の転送を 2 本出したままにする(read の timeout で cancel しない。cancel すると、その瞬間に届いた分を落とす)。汲み続けるので OUT も詰まらない。
  - wMaxPacketSize の倍数の書き込みの後には ZLP を送る(仕様どおり)。oep-probe-arduino 6964010 より前の P4 の firmware は、ちょうど wMaxPacketSize の倍数の frame(1024 byte = 251 語の write_block)+ ZLP を次の OUT が来るまで処理せず、host からは 3 秒の無応答に見える(flash が「connection closed」で止まる)。
- HID は hidapi で扱う。
  - 同じ VID:PID と serial で、usage page 0xFF00 以上の HID を開き、report 記述子から vendor の report(input と output を持つもの)の ID と大きさを読む(`ch32rv_oep::hid`)。P4 は ID 6、511 byte。
  - report ID があれば、output にも ID を付ける。input は ID を確かめてから count の分を取る。
- 実機(X035 治具の ESP32-P4 HS): vendor bulk(interface 1)、HID(report ID 6)、serial port のどれでも、target info、TickBoth(4.9 KB)の flash が約 0.20〜0.22 秒で書けた。dmseq の monitor、monitor を開いたままの flash(reset の後も流れ続ける)も通った。既定では vendor bulk が選ばれる(`broker endpoint --json` の `transport`)。
- Linux の権限: `60-ch32rv.rules`(`doctor --emit-udev`)には、1209:4F45 の device の USB のノードと hidraw も入っている。

### 3.4 link の規則(core §4.4、§5)

- **corr**: session ごとに 1 から数え、要求ごとに +1。65535 の次は 1 で、0 は使わない。同じ corr を使うのは、下の再送のときだけ。
- **1 フレームは 1 回の write で送る**。pipeline の一まとまりも 1 回の write にまとめる。
- **pipeline**: `max_inflight` と `window`(未応答の message 長の合計)の中で送り、応答は順に受ける。
- **応答の振り分け**: role で分ける。corr を照合するのは role 0x02 だけ(0x05 / 0x06 の bytes 1-2 は fn)。
- **再送**: 応答が失われたか壊れたら、同じ corr で 1 回だけ送り直す。
  - `result_lost`(72 byte を超える応答は probe が覚えていない)が返ったら、状態を読み直してから新しい corr で出し直す。
  - blind の unsubscribe / end を送った後は、元の要求を送り直さない。
- **応答の読み方**:
  - 失敗の応答が `status(u8)` だけの 1 byte でも受ける(run で hart を止められない、attach / scan の失敗)。
  - 未知の status・reason と accepted(0x02)は失敗として扱う。
  - 固定部分より短い応答や、TLV の途中で切れた応答は壊れているとみなす。
- **1 回の要求の大きさ**: read_block / write_block の語数は、describe の `max_length`(共通 tag 0x03)と、max_frame から出る値(write の要求の見出し 20 byte を除いた分)の小さいほうに絞る。`max_length` が無ければ 256 語まで。

## 4. session と target の操作

### 4.1 session

- **session_id**: 開くたびに新しい乱数の u32。
  - ブローカーは probe での自分の session_id を、利用者ごとの runtime ディレクトリ(DeviceLock と同じ場所)の `<key>.sid` に保存する。次のブローカーはまずその id で開いて end し、古い lease が切れるまで locked で待たずに済ませる。
  - `no_session` が返ったら、`boot_id` を比べて「別の人が使った」か「probe が再起動した」かを見分ける。
- **lease**:
  - コマンドがブローカーに開く session は 3 秒。ブローカーが probe に開く session は 10 秒(上げた速さの立て直しが収まる長さ)で、keepalive を 1 秒ごとに送る。
  - run の間、probe は他の要求に答えないので、run の timeout は lease と応答待ちより短くする。run の timeout と dmi の待ちは fn 0 describe の `max_op_ms`(0x4D)以下に詰める(`Probe::max_op_ms`)。
  - lease が切れた後の要求は `rejected no_session` になる(再開は無い)。ブローカーは no_session を受けたら新しい session_id で open し直し、台帳と client の session を捨てる。client はそれぞれ no_session を受けて open からやり直す。
- **lock の奪い方(ArduinoCore-CH32RV oep-workflow §4.3)**:
  - `open` が `locked` で返ったとき、transport が serial 1 本だけの probe なら、その場で force する(排他で開けた時点で、前の持ち主は死んでいる)。
  - それ以外の probe では、`lock_state` の残り時間を `--lock-timeout` まで待つ。持ち主が lease を延ばし続けていれば、exit 13(`device-busy`)にする。
  - open の TLV `owner` で名乗った名前が、lock_state と locked の payload に返る。ch32rv は `ch32rv <コマンド> pid <pid>`(ブローカーは `ch32rv broker pid <pid>`)と名乗る。
  - 「serial 1 本だけか」は、fn 0 describe の経路の一覧(`transport` 0x49)で判断する。tag が無い probe は「複数」として扱う(安全側)。
- **正常終了**: detach(自分の分)→ end。エラーで終わるときも同じ。end が無いのは kill と Ctrl-C だけ。

### 4.2 target の操作の trait

`ch32rv-dmi` の、transport に依らない操作の trait。

```rust
/// Word-block access and loader runs a probe can do faster than DMI one by one.
pub trait TargetAccess: DtmAccess {
    fn max_block_words(&self) -> usize;
    fn read_words(&mut self, addr: u32, count: usize) -> Result<Vec<u32>, DmiError>;
    fn write_words(&mut self, addr: u32, words: &[u32]) -> Result<(), DmiError>;
    /// Start at `pc` with `regs` set, wait for the halt (ebreak) up to `timeout`, read `outs`.
    fn run_until_halt(&mut self, pc: u32, regs: &[(u16, u32)], outs: &[u16], timeout: Duration)
        -> Result<RunResult, DmiError>;
    fn halt(&mut self) -> Result<(), DmiError>;
    fn resume_once(&mut self) -> Result<(), DmiError>;
    fn reset(&mut self, mode: ResetMode) -> Result<ResetResult, DmiError>;
}
```

- **OEP**(`OepDtm`): riscv-dm の read_block / write_block / run / halt / resume / reset に 1 対 1 で写す。`DtmAccess` の dmi_read / dmi_write は、dmi 要求の 1 手順ずつ。
  - 抽象コマンドの一連(data の書き込み、command、data0 の読み出し)は、`DtmAccess::dmi_sequence` で 1 つの dmi 要求にまとめる。probe は要求の間で console を poll することがあるため(spec の要求)。`DebugModule` の register の読み書きはこれを使う。
- **WCH-Link**(`DmTarget`):
  - read_words は既存の高速 read。write_words は DMI 経由。
  - run_until_halt は DebugModule で組む(dpc、regs、resumereq、halt の poll)。
  - WCH の stub(`Programmer::Stub`)での書き込みは、この trait を使わない。
- **CH32 の resume 規則は host が持つ**(`resume_ch32`)。V006 は resumereq を取りこぼし、L103 は allresumeack を立てない。dpc を読み、変わっていなければ出し直す(最大 8 回)。

### 4.3 target の識別

- attach の応答の target_id(scheme 1 = DMI 0x7F)を chip_id として DB の `resolve_by_chip_id` に渡す。
  - rev の nibble [7:4] は無視する。0 と全 1 は「無い」。
  - 0x7F が確かめられているのは L103 / V203 / V003 / X035。他の family は ESIG を block read で読んで補う。
- `--chip` の照合は WCH-Link と同じ `session::check_chip` で行う(fail-closed)。

## 5. 書き込み

### 5.1 RAM loader(`crates/flash/loader/`、source から build)

**RV32EC の範囲だけで書く**(x1〜x15、M / A なし、C はあり)。1 本の binary が V003 / V00x(RV32EC)でも IMAC の family でも動き、位置に依らない(相対分岐だけ)。DB の `flash_program_method` / `flash_geometry` から方式(a3)を選ぶ(`ch32rv_flash::loader::plan_for_family`)。

| a3 | 方式 | 対象 | やること |
|---|---|---|---|
| 0 | buffered | V003 / V00x / X035 / L103 / V205 など | fast page erase → BUFRST → 1 語ずつ BUFLOAD → STRT |
| 1 | pgstart | V20x / V30x | fast page erase → FTPG で 1 語ずつ書く(WRBSY を待つ)→ PGSTART |
| 2 | v103 | V103 | PG で halfword ずつ書き、V103 の commit |
| 3 | x315 | X315 / H417 / V407 | PER で 4 KiB を消し、256 byte ずつ 16 回 PG_STRT |
| 4 | m030 | M030 | BUFRST → 2 語ずつ BUFLOAD(64 bit)→ STRT。buffer の load ごとに commit を書く |

呼び方:

- 入力: a0 = 先頭の page の番地(a2 に揃える)、a1 = RAM の buffer(a5 page 分を続けて置く)、a2 = page の byte 数(64 / 128 / 256 / 4096)、a3 = 方式、a5 = page 数(1 以上)。a4 は loader が FLASH の base(0x40022000)にする。
- FLASH の unlock(KEYR と MODEKEYR)と mstatus.MIE を下ろすのは host。
- 終わりは常に先頭から +4 の `ebreak`。a0 = 0 なら成功、0x80000000 | STATR なら書き込み保護の誤り(その前の page は書けている)。BSY が下りない controller では loader が回り続け、host の run の timeout で止める。
- 置き場所は SRAM 0x20000000(H417 は 0x20000000 に RAM が無いので 0x20120000)。buffer は loader の 0x300 後ろ。

binary の扱い:

- binary は commit する。`cargo xtask loader-gen` が vendor toolchain(xpack riscv-none-elf-gcc。`CH32_GCC_BIN`、無ければ ArduinoCore-CH32RV の `.tools/`)で build し直し、`cargo xtask loader-check` が commit した binary との一致を確かめる。
- page の大きさは DB の `flash_geometry` の値を使う(V003 は 64 byte、V103 は 128 byte)。H417 は FLASH_CFGR0.DBMODE が立っていれば、DB の dual の page(8 KiB)を使う。

### 5.2 手順(`ch32rv_flash::loader::program` と `cli/src/oep.rs`)

1. ブローカーの client として open(lease 3 秒)→ attach(halt。max_speed は `--speed` の段階の公称値、pins は口の label から。§6)。
2. target の識別と `--chip` の照合(§4.3)。family に loader の方式が無ければ exit 24。
3. **reset(mode 1 = halt_at_reset)**。走っている IWDG に書く途中で reset させないため。
4. `--preverify` なら image の範囲を読み、すべて同じなら書かずに 9. へ。
5. FLASH の unlock(KEYR、MODEKEYR)と、CTLR の読み直しでの確認。lock のままなら reset-halt して鍵を入れ直す(最大 3 回)。
6. loader を置き、読み戻して照合する(最大 3 回)。壊れた loader でも ebreak まで着いてしまい、全 page を壊すため。
7. image が触れる page を集める。image が一部だけ覆う page は先に読み、image を重ねる(image の外の byte が残る)。連続する page を 1 KiB(最大 8 page)ずつ buffer に書いて 1 回 run する(run の timeout 2.5 秒、outs = a0)。
   - 成功の条件は、止まった、dpc が +4、a0 = 0。
   - dpc が loader の先頭のまま = 走らなかった。その run を出し直す(最大 3 回。loader は page 単位で冪等)。
   - a0 が書き込み保護の誤りなら、exit 21(`target-protected`)。
8. **verify**: 書いた page を読み戻す。違った page と失敗した page を書き直す(最大 2 周。周ごとに loader を置き直す)。それでも違えば exit 30。最後に FLASH を lock し直す(どの経路でも)。
9. `--reset run`: reset(mode 0 = run。`--confirm-run` なら走ったことを probe が確かめ、走っていなければ exit 50)。
10. detach → end。JSON は WCH-Link と同じ `result.flash` の形で、`programmer = "oep-loader"`、`pages`、`rewritten`(書き直した page)、`restarted_runs`(出し直した run)、`chip_id`、`seconds` を足す。

**`--erase`**(値と経路ごとの表は [cli](cli.ja.md) §4.1):
- `keep-store`(既定)と `sector` は、書く page だけを loader の中で消す。STORE は image の外なので触れない。
- `chip` は、loader を置く前に DM で FLASH_CTLR の MER + STRT を回して code flash 全体を消す(`DebugModule::flash_mass_erase`)。MER の無い CH32H417 は断る(exit 24)。
- `none` は断る(loader は書く page を必ず消す)。

## 6. 口の選び方(monitor と共通。oep-spec host ガイド §5.3)

probe は配線を持たず(スロットは無い)、口は probe の設定の label(core §13.8 の `channel.{}.label`)`S.N` の `S` で名付ける。
ch32rv は attach せずに分かること(unit_id、口の名前)だけで口を 1 つに絞り、残りが 1 つでなければ止める。家系で口を選ばない
(attach して探すと、rvswd では wake が target をリセットしうる)。アドレスごとの動きと exit は [cli](cli.ja.md) §4.12 の「口の選び方」。

- serial port の path が WCH-Link のものでなければ、その port を開いて confirm を送る(利用者がその port を選んだので、開いてよい)。
  - `OEP!` が返らなければ「OEP の probe ではない」として exit 10。
  - USB にも OEP の device として出る probe の port なら、upload は常に断る(`oep://<probe>[/<port>]` を案内する)。経路を明示する `oep://<probe>/<port>?via=tty:<path>` は断らない。
- `oep://<probe>` だけは probe 自身を指す。名前つきの口がちょうど 1 つならそこを使い(道具の便利さ、host ガイド §5.3)、2 つ以上なら exit 23、名前つきの口が無ければ `--pins` で線を指す。名前の無い口は住所で指せない。
- wire は口の label(`S.swio` / `S.swclk`)で、決まらなければ `--chip` の家系で決める。ピン・NRST・fixture UART の線も、その口の label から引く。
- label には target のピンを `@P` で添えてよい: `x035.swdio@pc18` は `x035.swdio` として引く。役目の無い線 `x035@pb6` は口 `x035` に数えるが、役目の名前では引かない。`P` は英数字と `-`・`_`(`pc16-pc11` のようにパッケージの中でつながったピン)。
- rvswd の休ませ方は、§9 の「rvswd の休ませ方」。

## 7. monitor とブローカー

### 7.1 OEP の probe の monitor

- `arduino monitor` の address が OEP の probe(serial port か `oep://`)で source が sdi / dmdata / dmseq なら、ブローカーの client として `oep.target.console` を開く。止めずに attach する。
- source `rtt` は ch32rv が host 側で行う。OEP の console に RTT の方式は無いので、probe の riscv-dm の上で RAM の control block を探し、poll のたびに hart を一瞬止めて ring を読み書きする(WCH-Link と同じ)。
- source `uart` は、OEP の probe の port でも素通しのまま。どの device か分からない port に OEP の confirm を送らないため。
- **source `fixture-uart`**: ブローカー経由の session で `oep.fixture.uart` を開き、monitor の `baudrate` を configure で送る(OPEN のときと、開いたまま変わったとき)。ストリームを読み、入力は書き戻す。probe の CDC の line coding を target の UART に写す機能は spec に無いので、UART の速さはこの configure で決める。fixture の UART にピンの plan が無ければ、OPEN が理由つきの error。
- **`oep://<probe>/<port>` の port**(protocol `oep`): source は dmseq(既定)/ dmdata / sdi / fixture-uart / rtt。console はその口の label のピンで attach する(§6)。
- console は read を poll する(ブローカーは通知を中継しない)。読み出しは lock-free。
- 読み始める位置は、最後の reset mark から(書き込みの直後に開いたとき、最初の行を落とさない)。
- IDE の入力は console write で送る(dmseq は 1 回に 2 byte まで。残りは送り直す)。
- 接続を失う(link-lost の mark、boot_id が変わる、transport が消える)と、`[ch32rv monitor] stopped: …` を流して終わる。

### 7.2 ブローカー

**probe ごとに、誰の子でもないブローカーを 1 つ置く。ch32rv の各コマンド(flash、monitor、gdb、1 回だけの read / reset)と pytest の `oep_host` は、どれもブローカーの client になる。** 経路は 1 つで、立ち上がる順で形は変わらない。

- **serial の速さ(port_speed、oep-if-link §3、既定で使う)**: probe を serial(UART bridge)で開いたブローカーは、endpoint を書いた後、client に答え始める前に、500000 を試す(500000 より上は試さない。`CH32RV_PORT_SPEED` で `off` か並びを指定できる)。手順は参照 client の `raise_speed` と同じ: 試す → host も切り替えて 20 ms 待ち confirm → link_source / link_sink で両方向を 1 秒か 32 KiB 確かめる(max_inflight で並べ、壊れたら 1 つずつで確かめ直し、通った上限で使う) → 壊れたフレームが無ければ決める、あれば戻す。全体の上限は 6 秒。試した結果(速さごとの通った / 壊れた、in / out の KB/s、並べた数、かかった時間)はブローカーの log に 1 行で出す。session が口を持っている間は、壊れたフレームを待たずに同じ corr ですぐ送り直す。上げた速さで答えが来なければ、起動時の速さ(115200)に戻って confirm し、送り直す。決めるときの idle_ms は仕様の上限の 3000 ms(`port_speed_idle_max_ms`): ブローカーが落ちても probe は 3 秒の黙りで戻り、ブローカーは 1 秒ごとの keepalive で保つ。serial を開いたときの confirm は、前の host が上げた速さの残りを待つために約 4 秒まで繰り返す(oep-if-link §3)。上げている間はフレームの様子を見て、悪ければ port_speed(戻す)を送って probe と揃えて起動時の速さに戻り、その session では上げない。判定は probe の firmware(fn 0 describe の firmware)で分ける: oep-probe-arduino 0.0.27 以降(壊れ 3 つが続いて初めて自分で戻る)は直近 3 秒の(壊れ + 失われ)の割合が 10 % を超えたら(50 フレーム未満は判定しない。基準 0)、それより前(1 秒に 3 つで戻る)は 5 秒に 2 つで(probe より先に下げる)。上げた速さで答えが無ければ、まず起動時の速さで約 4 秒 confirm し(開く側と同じ。通らなければリンクの失敗。oep-if-link §3)、probe がそこにいれば(自分で戻っていた)残りをそこで送り直す。この立て直しが lease の中に収まるよう、ブローカーの lease は 10 秒。ブローカーは最後の client が抜けた後 3 秒残り、その間も keepalive で session と速さを保つ(IDE がモニターを閉じてからアップロードを始めるまでの間)。CLI の 1 回だけのコマンドはブローカーの client なので、別に速さを触らない。

- **起動**:
  - client は、まず `<runtime>/<key>.oep`(待ち受けの場所)を見てつなぐ。key は probe の同一性(docs/freeze-decisions.ja.md §4): `oep://` の probe は `oep-<unit_id の小文字>`(`via` によらない。USB の probe は serial = unit_id、serial が無ければ `oep-usb-<位置>`)、unit を名指さずに渡した、OEP の USB device を持たない serial port(UART bridge)は `oep-port-<正規化した path>`、WCH-Link は `wch-<serial>`(無ければ `wch-usb-<位置>`)。同じ probe へのどの道(`oep://` のどの `via`、どちらの CDC の path)も同じブローカーに着く。runtime は DeviceLock と同じ利用者ごとのディレクトリ。
  - 無ければ `ch32rv broker serve --probe <sel>`(利用者向けではない subcommand)を切り離して起動する。Linux / macOS は double fork + setsid、Windows は DETACHED_PROCESS と job object からの breakaway。stdio は捨てる。
  - 起動の取り合いは `<runtime>/<key>.broker.lock` の flock で 1 つにする。取れなかったほうは起動をやめ、`<key>.oep` が現れるのを待ってつなぐ。
- **待ち受け**: 127.0.0.1 の TCP を 1 つ(port 0 で選ぶ)。`<key>.oep` には port、pid、起動時刻、transport、ch32rv の版、client との取り決めの番号を書き、file は 0600 にする(ファイルは内部のもの。版の扱いは cli.ja.md §4.12)。`ch32rv broker endpoint --probe <sel> --json` がこれを返す(結果の封筒の `result` が `{endpoint, pid, transport, version}`。`endpoint` は `"127.0.0.1:<port>"`、ブローカーが無ければ null)。認証は無い(OEP の TCP の注意どおり)。
- **終わり方**: **最後の client が抜けてから 3 秒で終わる**(IDE がモニターを閉じてからアップロードを始めるまでの間、ブローカーと上げた速さを保つ)。transport を閉じ、`<key>.oep` を消す。probe を失ったとき(抜かれた、boot_id が変わった)は、client に切断で知らせてから終わる。
- **client から見ると OEP そのもの**(spec の TCP の形 `length(u16) message`)。ブローカーは次のことをする。
  - client ごとに corr を付け替え、probe への pipeline に混ぜる。応答は元の corr に戻して、その client にだけ返す。
  - client の `open` / `end` / `keepalive` / `lock_state` は受け止めて、自分で答える。open は lease_ms と本物の boot_id を返す。client の owner の名前は台帳に持ち、`broker endpoint --json` で一覧できるようにする。
  - **ロックは 1 つ**(oep-spec の host ガイド §7)。ch32rv の道具(flash、monitor、gdb、pytest の `oep_host`)は、open に名前つき拡張 `io.github.ch32-riscv-ug.share` を critical(tag 0xFF、値は name_len と名前だけ)で付けて、1 つのロックに相乗りする。相乗りの session は client ごとで、end か切断でその client の分だけ外す。相乗りしない client(拡張の無い OEP の host)には、ブローカーは 1 つの probe として core のとおりに答える: core §6.2 の表、lease の期限切れ、force、end での解放、そして probe に 1 つの再送の状態(最後の session の最後の corr と、要求と答えの記録 64 件、core §5.1)。相乗りは 1 つの持ち主として扱う: 相乗りの間、拡張の無い client の open は locked、force 付きなら奪う(相乗りの全員の session が終わり、道具は次の要求で locked か no_session を受けて止まる。ch32rv は「別の host が force でロックを取ったか、lease が切れた」と出す)。拡張の無い client がロックを持つ間は、相乗りの open が locked。その client の接続が切れても、session は lease が切れるまで残る(transports §3)。lock_state は、どちらかが持っている間 locked と答える。
  - 拡張を知らない相手(OEP の probe、ほかのブローカー)は、critical の拡張を unsupported(0xFF)で断る。ch32rv の client はそれを見て、拡張の無い open をやり直す。
  - confirm / list / describe は、ブローカーが持っている写しで答える(boot_id が変わったら取り直す)。
  - 1 つの client の要求の並びは崩さない(probe は順に処理する)。client をまたいだ並びは到着順。
- **client ごとの資源の台帳**:
  - 接続: attach の応答の connection。参照は client ごとに数え、使う client がいなくなったときだけ detach する。同じピンへの 2 つ目の attach には、その接続を貸す(名前つき拡張 `io.github.ch32-riscv-ug.shared`。§9 の「接続とストリーム」)。
  - plan(plan_apply の fn)。
  - client が落ちたら、その client の分だけ外す(使う client のいない接続は detach、plan は plan_release)。昇格や台帳の申告し直しは無い。
- **WCH-Link もブローカーの裏に置く**。ブローカーが Link の debug の口(vendor)を持ち、client には OEP を見せて、interface を WCH-Link の操作に写す。
  - `oep.wire.rvswd` / `swio` の attach は AttachChip(target_id は chip_id)。
  - `oep.target.riscv-dm` の dmi / halt / resume / reset / block / run は DmiOp と DebugModule(`DmTarget`)。
  - `oep.target.console` はブローカーの中で dmdata / dmseq の mailbox を poll して、位置付きのストリームにする(mechanism は OEP の番号)。
  - `broker serve --probe serial:<sn>|topology:<bus>-<ports>|<Link の CDC の path>` で Link を裏に置く。`arduino monitor` の dmdata / dmseq は Link でもブローカー経由。1 回だけのコマンド(verify / read / reset / target info)は、その Link のブローカーが動いていればブローカーを通し、動いていなければ直接開く。
  - **flash はブローカーから Link を借りる**: ch32rv 独自の interface `io.github.ch32-riscv-ug.wchlink` の lend でブローカーが Link を手放し(console の read は手元の出力で答え続ける)、flash は直接の経路(WCH の stub)で書き、reclaim で返す(flash のプロセスが落ちてもブローカーが取り返す)。V003 で 2.6 KB に 1.0 秒。Link を自分で開くほかのコマンド(probe info / mode / power / firmware、target option / protect、dbg、erase、recover、capabilities、write、run、monitor の sdi / rtt)も、ブローカーが動いていれば同じように借りる。貸している間は誰も mailbox を汲まないので、その間に target が出した console の出力は落ちる。
  - gdb は Link を借りず、ブローカーの client になる(§8)。
  - **Link の高速の read(`read_mem`)は memory 専用で、書いた直後は古い値を返す**(CH32V003 で実測): 周辺の register(FLASH_CTLR)では直前に書いた鍵の値を返し、DMI で書いた直後の SRAM と、loader で書いた直後の flash も古い値を返した。ブローカーは、code flash の範囲で、しかもその attach の間に何も書いていないときだけ高速の read を使い、それ以外は Debug Module で読む。
  - **uart の source の monitor は CDC だけを使い、ブローカーにも probe の lock にも触れない**。
- **時間の制約**: `arduino monitor` の OPEN は、ブローカーの起動・transport の open・attach まで含めて、arduino-cli の待ちの内に返す(arduino-cli 1.3.1 の実測: OPEN の返事が 6 秒遅れても通り、9 秒遅れるとエラー無しで閉じる)。
- flash の最中も、monitor は console を読み続ける(probe は riscv-dm の要求を実行している間だけ console の poll を止める)。reset の後は reset の mark から続きを読む。

## 8. デバッグ(gdb)

- `OepDtm` が `DtmAccess` と §4.2 の `TargetAccess` を実装するので、`ch32rv-debug` の `Ch32Target<T: DtmAccess>`(gdb server)は、そのまま OEP の上で動く。ブローカーの client として話すときも同じ形(TCP の上の `OepDtm`)。
- **gdb も monitor も、同じブローカーの client**(§7.2)。どちらが先でも同じで、3 つ以上でも同じ。ブローカーの動いている WCH-Link でも、gdb は Link を借りずにブローカー経由で attach し、gdb の間も monitor は流れ続ける。
  - gdb が hart を止めている間、monitor には何も流れない(spec の規則)。
- gdb は止めた時の DATA0 / DATA1(DM の console の mailbox)を覚え、走らせる直前に戻す。OEP の probe では、走らせるのに probe 自身の resume を使う(probe は host が raw DMI を書いてから自分で resume するまで console の poll を止める)。
- flash の breakpoint は gdb が自分で後始末する(終わるときに戻し、hart が走っていれば止めてから戻して走らせ直す)。ブローカーの台帳は hart の状態と trigger を持たない。
- 使い方は [cli](cli.ja.md) §4.6、§4.12。

## 9. spec との対応

ch32rv が仕様の主な決まりにどう合わせているか。

- **message と TLV**: 要求の見出しは常に 10 byte で session_id を持つ(0 = session なし)。TLV は `tag len(u16) value` の 1 つの形で、TLV 番号 0 は壊れたもの。並び(list、scan、marks)は要素の長さを持たない(要素の形は revision で決まる)。confirm は boot_id を返す。corr は open をまたいで続く。どの要求も max_op_ms を待つ。
- **session**: 再開は無い。end・lease 切れ・force で session の資源はすべて外れ、その id は no_session になる(ブローカーは `no_session` で新しい session を開く)。
- **任意の op**: 各 fn の describe の `ops`(base + bitmap)だけで知る(features では知らない)。describe の ops は core §7.4 の符号だけを受ける(`session::decode_ops`、試験は spec の ops_encoding.json)。
- **fn の名前**: core は名前の無い fn 0 で、list に出ない(版は confirm の revision だけ)。plan は `oep.probe.plan`、restart は `oep.probe.restart`(op 0x01、describe 0x40 restart_max_ms)、port_speed と線の試験は `oep.probe.link`(source は len(u16) data、最大 max_frame − 26 byte、sink は count(u16) data)。ブローカーは plan と restart を名前で探した fn で見分ける。購読は通知を送るインターフェースの 0x30 / 0x32 で、ブローカーはどの fn でも断る(通知を中継しない)。fn 0 の `clock`(op 0x04、ロックも session も要らない、答えは boot_id と uptime_ns)はブローカーが中継する(ブローカーが自分で答えるのは confirm / open / end / keepalive / lock_state の 5 つ)。
- **restart**: ブローカーは client の restart を自分の session で送り、答えを返したら probe への経路を閉じて終わる(transports §1)。client は restart_max_ms まで待ってやり直す(新しいブローカーが起動する)。ブローカーが中継しなかった再起動(電源、watchdog、他の host)のときは、ブローカーがその場で立て直す: 起動時の速さで confirm を restart_max_ms まで繰り返し、新しい session を開く(client の session は終わる)。
- **接続とストリーム**: 番号は u32 で、接続とストリームで 1 つの空間(WCH のブローカーは connection 1、ストリームは 2 から)。attach は method 0 / 1 の 1 つの形で、毎回新しい接続を作る(同じピンの 2 つ目は unavailable cause 1)。ブローカーは 1 本を client に貸す(名前つき拡張 `io.github.ch32-riscv-ug.shared`)。max_speed は必須なので ch32rv は常に送る(呼び出し側の値、無ければ線の describe の max_clock_hz、無ければ 1 MHz)。応答の flags は `attach_flags` で、bit3 で TLV 0x11 dpc。scan の skip は TLV 0x02。スロットは無い(§6)。
- **riscv-dm と console**: dmi の nvals は u16、run の nvals は u8 で stopped 2。reset の mode は 0 run / 1 halt_at_reset。write_block は送り直さない。read の応答は len を持つ(console / fixture UART)。マークの time_ns は 22 byte の形。console の mechanism は名前の一覧で、開き直しは新しいストリーム。probe は op の外に target の状態を持ち越さないので、ch32rv の OEP の resume は dpc を読むたびに DATA0 / DATA1 を戻す。
- **設定**: 設定は本体が取りまとめる(core §13)。ch32rv はラベルを fn 0 の get(op 0x20、ロック無し、ページ送り)で読む。値は `fn number key len data`(固定部分 7 byte)。項目の名前は持ち主の fn の describe の setting(0x08、`form key_kind keys(u16) max_len flags name`)で引く。口の名前は本体の項目 `channel.{}.label` の `S.N` だけで、書いたとおりの表記で持ち、ASCII の大小文字を区別せずに比べる。線も口の `S.N` だけで探す(素の `swdio` / `nrst` や firmware の固定の名前では探さない)。`any` は無い。ラベルの無い probe は `--pins` で端子を直接指定する。
- **rvswd の休ませ方**(oep-if-debug §3): idle_clock の無い attach では probe が決める(SWCLK の channel の設定 `channel.{}.rest`(`oep.wire.rvswd`)、無いか auto なら target_id の系統で、`idle_low_family` = CH32L103 は low)。使った休ませ方は attach の応答 TLV 0x03 と connections の TLV 0x02 で返る。ch32rv は `--chip` / アドレスの target が L103 なら low を明示し、それ以外は応答を読む。L103 なのに low でない接続(設定で自動を切った probe、休ませ方を答えない firmware)は low を明示して attach し直す。ブローカーは同じ休ませ方を求める attach にだけ接続を貸し、WCH-Link のブローカーは chip id から規則どおりの値を答える。
- **線の保持**(oep-if-debug §2): 線切れ以外で接続が閉じても、probe はその口を休みの状態で駆動し続ける(CH32L103 は駆動が止まると約 5 ms で halt を失う)。保持中のピンは空きの一種で、次の attach がそのまま取る。保持は必須の op `holds`(0x14、ロック不要、connections とは別。oep-if-debug §2.2)で読め、口ごとの設定 `channel.{}.hold`(key は SWDIO の channel、既定 on)で切れる。ch32rv のコマンドはプロセスをまたいで halt を保てる(`dbg halt` → `dbg resume`)。ch32rv の host 側は `target::holds` で読める。WCH-Link のブローカーは `holds` を宣言して空の一覧を答える(Link の休ませ方は firmware が持ち、見えない)。

## 10. 決まりごと

1. 長さ見出しの CDC / USJ の firmware と話す手段は持たない。
2. RAM loader は RV32EC の 1 本(§5.1)。wlink の V003 loader と参照の X035 loader(t3〜t6 を使い、RV32E では illegal instruction になる)は使わない。
3. 結合試験は oep-client-python の virtual bench(probe と target と治具の配線を模したもの)を使う(Python は試験のときだけ uv で動かす)。Rust で同じものは作らない。virtual bench は `tests/oep-virtual/pyproject.toml` と `uv.lock` で公開 Python 依存の commit を固定し、自前の隔離環境へ取得する。隣接 checkout を検索しない。`OEP_CLIENT_PYTHON` は明示した locked Python project への開発用上書きで、設定不備・準備失敗はテストを失敗させる。起動は `python -m oep_client.virtual_bench_serve`(pty と TCP)。詳細は [OEPテストガイド](../tests/oep-virtual/README.ja.md)。
