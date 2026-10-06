# OEP host(ch32rv が OEP の probe で書き込み・モニタ・ブローカーをする)

- 作成日: 2026-09-29
- 状態: 設計(実装前)。検討中なので日本語のみ([development.ja.md](development.ja.md) §1)
- 出発点:
  - ArduinoCore-CH32RV `docs/oep-workflow.ja.md`(最終の形。2026-09-29 の決定。以下 WF)の依頼 7〜14
  - oep-spec と `registry/oep-v1.toml`(書き始めは e9c8e1f。**ch32rv が今どの版に合わせているかは `crates/oep/src/registry.rs` の先頭の `source:` の行だけを正とする**。この文書や CHANGELOG に出てくる版は、その時点の記録)
  - 参考実装 `oep-client-python` v1(`ch32_flash.py` / `riscv.py` / `link.py`)と `oep-probe-arduino`(`OepV1Target.cpp` / `OepCh32Dm.cpp`)
- 範囲: WF §8。書き込み・人が使うモニタ・discovery・ブローカー。**gdb を OEP の probe で扱うのは今回の範囲外**だが、§8 で余地を決めておく。

## 1. 方針

1. **target の知識は ch32rv だけが持つ**。OEP の probe は target を知らない。DB、flash の手順、RAM loader は host の側にある(OEP の前提)。client の `ch32_flash` は、ch32rv の OEP の書き込みが入ったら消す。
2. **OEP は WCH-Link と同じ境界に載せる**。
   - `DtmAccess`(dmi_read / dmi_write / dmi_nop)を実装し、既存の `DebugModule` をそのまま使う。
   - これに加えて、ブロックの読み書きと run を持つ、transport に依らない trait を足す(§4.2)。
   - gdb server はこの 2 つだけで動く形にしておく(§8)。
3. **family byte に頼らない**。WCH-Link の AttachChip の family byte で引いていた解決の連鎖(`params_for_family` / `flash_controller_profile` / `db_family`)は、OEP では使えない。chip_id(DM 0x7F。attach の target_id scheme 1)と ESIG から DB の family を引き、その family の `flash_program_method` / `flash_geometry` で手順を決める。
4. **仕様に無いことは黙って埋めない**。WF の決定のうち spec にまだ無いものは、決定の形で仮置きする。仮置きした所は §10 に一覧し、oep-spec への依頼に使う。
5. 暫定の形は作らない(WF §9)。利用者はいないので、途中の破壊的変更は許す。

## 2. crate 構成

新しい crate を 1 つ足す。

| crate | 責務 | 依存 |
|---|---|---|
| `ch32rv-oep` | OEP v1 の host 側。codec(message、TLV、COBS + CRC-16、長さ見出し)、transport(serial / vendor bulk / HID / TCP)、link(confirm、pipeline、resync、再送)、session(open / end / keepalive / lock_state、lock の奪い方)、interface(`Wire` / `RiscvDm` / `Console` / `ProbeConfig`)、`OepDtm: DtmAccess` | usb、dmi、contract、serialport、hidapi |

- **番号の台帳**は `registry/oep-v1.toml` から `cargo xtask oep-gen <oep-spec の dir>` で Rust の定数を生成し、commit する。
  - 生成物には台帳の hash(LF に正規化した sha256 の先頭 16 桁。台帳の規則どおり)を埋め込む。
  - `cargo xtask oep-check` でずれを検出する。device DB の `db-gen` / `db-check` と同じ方式。
  - release workflow にも db-check と並べて入れる。
- `ch32rv-oep` は target を知らない(CH32 の手順は持たない)。CH32 の手順は `ch32rv-flash`(loader と編成)と CLI にある。
- 依存方向は architecture §2.1 に `oep` を `wchlink` と並べて置く形。`dmi` の `DtmAccess` を実装するのは `wchlink` と `oep` の 2 つになる。

## 3. transport と link

### 3.1 framing(WF §4.2 の決定を採る)

| transport | frame | resync |
|---|---|---|
| serial に見えるもの(USB CDC、USB-Serial/JTAG、UART bridge) | `COBS(message ‖ crc16_le) 0x00`。CRC-16/CCITT-FALSE。送るときは前にも 0x00 を置く | 要らない。CRC の合わないフレームは捨てる。フレームの外のバイトは雑音として捨てる。欠落は時間切れで判断する |
| vendor bulk、TCP | `length(u16) message` | core §5.1(50 ms 静かになるまで捨てる → confirm 0..255) |
| HID | spec の report の詰め方(report ID + count(u16) + 長さ見出しの stream + 詰め物)。WF の「length(u16)+message」は、この stream の中身のことと読む | 長さ見出しと同じ |

- decoder は、0xFF block で終わる data の後に空 block を付ける形(参照実装の癖)と、付けない形の両方を受ける。空のフレーム(0x00 の連続)は無視する。
- serial port の COBS の受け方は core §3.1 のとおり。開いた直後から最初の 0x00 までもフレームの候補にし、role か corr の合わないフレームは雑音として捨てる。
- 長さ見出しの CDC / USJ の firmware と話す手段は持たない(移行の間だけの形は作らない。spec も CDC / USJ の長さ見出しを残さない)。

### 3.2 serial の開き方

- 必ず排他で開く(serialport crate は unix で TIOCEXCL、Windows は元から)。115200 固定(WF §4.5)。DTR / RTS は立てたまま(参照 client と同じ。下げるなら RTS が先)。1200 baud では開かない。
- serial port は target の console と共用なので(WF §4.1)、応答を待つ間に来るフレームの外のバイトは捨てる。

### 3.3 USB の口の見つけ方

- **OEP の probe の見分け方(oep-core §3.3)**: **プロジェクトの USB ID 1209:4F45 の device** だけを自動で OEP の probe とみなす(2026-10-06、pid.codes で取得)。iProduct は表示の文字で、判定には使わない。ほかの device は、利用者が `oep://<unit_id>` で名指す(USB の serial と大小を区別せず比べる)か、口を選んだときだけ使い、開いたら confirm だけを送り、答えが無ければ閉じる。vendor bulk の interface は class 0xFF・subclass 0x4F・protocol 0x45、HID の interface は usage page 0xFF4F・usage 0x45。
  - 判定は 1 つの関数(`is_oep_device`)にまとめ、後で PID の判定に差し替えられるようにする。discovery(`oep://`)も、serial port を選んだときの「OEP の probe なら raw の serial port の upload は断る」(§6)も、この関数を使う。
  - 名前は device を開かずに読む(nusb の列挙が持つ product の文字列)。
  - fn 0 の describe の `discoverable`(0x4A)= 1 は「この形で列挙している device がある」という意味で、USJ などから開いたときにも分かる。
- 口は interface の種類で選ぶ(core §3.3): CDC(ACM)はすべて OEP を受ける serial port、vendor class の bulk の組は vendor bulk、vendor 定義の HID は HID。DFU や Mass Storage は OEP の外。vendor bulk、HID、serial port の順に試す(2026-09-29 実装、`oep::connect_upstream`)。ブローカーと discovery は、`port:<path>` / `oep://` の serial port を持つ OEP の device の vendor bulk → vendor HID → その serial port の順に開き、confirm が通った最初の経路を使う(開けない・答えないものは次へ。Linux で udev の規則が無く USB のノードに書けないときも serial port に落ちる)。`CH32RV_OEP_TRANSPORT=vendor-bulk|hid|serial` でその経路から始める(比較と切り分け用)。ブローカーの key は serial port の path のまま。
- vendor bulk は nusb で扱う(`ch32rv_usb::BulkPipe`)。alt 0 の vendor class(0xFF)の interface で、bulk の OUT と IN を持つ最初のもの。
  - IN の転送を 2 本出したままにする(read の timeout で cancel しない。cancel すると、その瞬間に届いた分を落とす)。汲み続けるので OUT も詰まらない(参照 client の E160)。
  - wMaxPacketSize の倍数の書き込みの後には ZLP を送る。
  - **probe 側の不具合の記録(2026-09-30)**: X035 治具の P4(oep-probe-arduino 1334b7f まで)は、ちょうど wMaxPacketSize の倍数の frame(1024 byte = 251 語の write_block)+ ZLP を、次の OUT が来るまで処理しなかった(host からは 3 秒の無応答、送り直しも同じ。ブローカーが上流の無応答で終わり、flash が「connection closed」)。`CH32RV_USB_TRACE` の記録で特定し、firmware 6964010(`CFG_TUD_VENDOR_RX_NEED_ZLP=0`、OUT を packet ごとに受ける)で直った。確認: 「monitor 1 秒 → すぐ flash」100 回で 0 回、1024 byte ちょうどの write_block 単独 70 回で 0 回(最遅 7.9 ms)。host は仕様どおり ZLP を送り続ける。
- HID は hidapi で扱う。
  - 同じ VID:PID と serial で、usage page 0xFF00 以上の HID を開き、report 記述子から vendor の report(input と output を持つもの)の ID と大きさを読む(`ch32rv_oep::hid`)。P4 は ID 6、511 byte。
  - report ID があれば、output にも ID を付ける。input は ID を確かめてから count の分を取る。
- 実機確認(2026-09-29、X035 治具の ESP32-P4 HS、oep-probe-arduino 71800ae): vendor bulk(interface 1)、HID(report ID 6)、serial port のどれでも、target info、TickBoth(4.9 KB)の flash が約 0.20〜0.22 秒で書けた。dmseq の monitor、monitor を開いたままの flash(reset の後も流れ続ける)も通った。既定では vendor bulk が選ばれる(`broker endpoint --json` の `transport`)。
- Linux の権限: `60-ch32rv.rules`(`doctor --emit-udev`)に、1209:4F45 の device の USB のノードと hidraw を足した。

### 3.4 link の規則(core §5、§9 の MUST をそのまま)

- **corr**: session ごとに 1 から数え、要求ごとに +1。65535 の次は 1 で、0 は使わない。同じ corr を使うのは、下の再送のときだけ。
- **1 フレームは 1 回の write で送る**。pipeline の一まとまりも 1 回の write にまとめる。
- **pipeline**: `max_inflight` と `window`(未応答の message 長の合計)の中で送り、応答は順に受ける。
- **応答の振り分け**: role で分ける。corr を照合するのは role 0x02 だけ(0x05 / 0x06 の bytes 1-2 は fn)。
- **再送**: 応答が失われたか壊れたら、同じ corr で 1 回だけ送り直す。
  - `result_lost`(72 byte を超える応答は probe が覚えていない)が返ったら、状態を読み直してから新しい corr で出し直す。
  - blind の unsubscribe / end を送った後は、元の要求を送り直さない。
- **応答の読み方**:
  - 失敗の応答が `status(u8)` だけの 1 byte でも受ける(run で hart を止められない、attach / scan の失敗)。参照 client はここで落ちる。
  - 未知の status・reason と accepted(0x02)は失敗として扱う。
  - 固定部分より短い応答や、TLV の途中で切れた応答は壊れているとみなす。
- **1 回の要求の大きさ**: read_block / write_block の語数は、max_frame から出る値、describe の `max_length`(共通 tag 0x03)、1024 byte の小さいほうに絞る(参照 probe は 256 語を超えると malformed。参照 client は max_length を見ていない)。

## 4. session と target の操作

### 4.1 session

- **session_id**: 開くたびに新しい乱数の u32。
  - one-shot のコマンドが同じ session を続けて使うときは、id を probe の `unit_id`(fn 0 describe の tag 0x42)ごとに、利用者ごとの runtime ディレクトリ(DeviceLock と同じ場所)に保存する。USB の serial では保存しない。
  - `no_session` が返ったら、`boot_id` を比べて「別の人が使った」か「probe が再起動した」かを見分ける。
- **lease**:
  - 対話的なコマンド(flash、read、target info)は 3 秒。ブローカーは 10 秒(上げた速さの立て直しが収まる長さ)で、keepalive を 1 秒ごとに送る。
  - pytest の `oep_host` は 10 秒(WF §4.3。client 側の話)。
  - run の timeout は lease と応答待ちより十分短くする(run の間、probe は他の要求に答えない)。run の timeout と dmi の待ちは fn 0 describe の `max_op_ms`(0x4D、参照 firmware は 10000)以下に詰める(2026-10-01、`Probe::max_op_ms`)。
  - lease が切れた後の要求は `rejected no_session` になる(再開は無い、2026-10-06)。ブローカーは no_session を受けたら新しい session_id で open し直し、台帳と client の session を捨てる。client はそれぞれ no_session を受けて open からやり直す。
- **lock の奪い方(WF §4.3)**:
  - `open` が `locked` で返ったとき、transport が serial 1 本だけの probe なら、その場で force する(排他で開けた時点で、前の持ち主は死んでいる)。
  - それ以外の probe では、`lock_state` の残り時間を待つ(上限 5 秒)。持ち主が lease を延ばし続けていれば、exit 13(`device-busy`)にする。force は `--force-lock` を明示したときだけ。
  - 持ち主が同じ PC の ch32rv なら、host 側の DeviceLock のファイルに書いた pid とコマンドで持ち主を名指しできる。OEP 自体は持ち主を返さない(§10 の 7)。
  - 「serial 1 本だけか」は、fn 0 describe の経路の一覧の tag(未採番、§10 の 4)で判断する。tag が無い probe は「複数」として扱う(安全側)。
- **正常終了**: detach(自分の分)→ end。エラーで終わるときも同じ。end が無いのは kill と Ctrl-C だけ(oep-spec の第三者レビューの案への返答どおり)。

### 4.2 target の操作の trait

`ch32rv-dmi` に、transport に依らない操作の trait を足す。

```rust
/// Word-block access and loader runs a probe can do faster than DMI one by one.
pub trait TargetAccess: DtmAccess {
    fn read_words(&mut self, addr: u32, count: usize) -> Result<Vec<u32>, DmiError>;
    fn write_words(&mut self, addr: u32, words: &[u32]) -> Result<(), DmiError>;
    /// Start at `pc` with `regs` set, wait for the halt (ebreak), return dpc + `outs`.
    fn run_until_halt(&mut self, pc: u32, regs: &[(u16, u32)], outs: &[u16], timeout: Duration)
        -> Result<RunResult, DmiError>;
    fn halt(&mut self) -> Result<(), DmiError>;
    fn resume(&mut self) -> Result<(), DmiError>;
    fn reset(&mut self, mode: ResetMode) -> Result<ResetResult, DmiError>;
    fn max_block_words(&self) -> usize;
}
```

- **OEP**: riscv-dm の read_block / write_block / run / halt / resume / reset / step に 1 対 1 で写す。`DtmAccess` の dmi_read / dmi_write は、dmi 要求の 1 手順ずつ。
  - 抽象コマンドの一連(data の書き込み、command、data0 の読み出し)は、1 つの dmi 要求にまとめる手段 `dmi_batch(steps)` を `OepDtm` に持たせる。probe は要求の間で console を poll することがあるため(spec の要求)。
  - `DebugModule` の register の読み書きがこれを使うように直す。
- **WCH-Link**:
  - read_words は既存の高速 read。write_words は DMI 経由。
  - run_until_halt は DebugModule で組む(dpc、regs、resumereq、halt の poll)。
  - WCH-Link の書き込みは今までどおり WCH の stub(`Programmer::Stub`)で行い、この trait は使わない。
- **CH32 の resume 規則は host が持つ**。V006 は resumereq を取りこぼし、L103 は allresumeack を立てない。`state` が返ったら dpc を読み、変わっていなければ出し直す(最大 8 回)。`DebugModule` の resume と同じ規則を共通の関数にする。

### 4.3 target の識別

- attach の応答の target_id(scheme 1 = DMI 0x7F)を chip_id として DB の `resolve_by_chip_id` に渡す。
  - rev の nibble [7:4] は無視する。0 と全 1 は「無い」。
  - 0x7F が確かめられているのは L103 / V203 / V003 / X035。他の family は ESIG を block read で読んで補う。
- `--chip` の照合は WCH-Link と同じ `session::check_chip` で行う(fail-closed)。

## 5. 書き込み(依頼 7)

### 5.1 RAM loader(`crates/flash/loader/`、source から build)

**RV32EC の範囲だけで書く**(x0〜x15、M / A なし、C はあり)。1 本の binary が V003 / V00x(RV32EC)でも IMAC の family でも動くので、「書込方式 × ISA」ごとに loader を分けなくて済む。参照の X035 loader は t3〜t6 を使うため、RV32E では illegal instruction になる。

| loader | 対象(DB の `flash_program_method`) | やること |
|---|---|---|
| `buffered` | L103 / X035 / V003 / V006 / V103 / V205 / M030(buffered、BUFLOAD の幅は 32 / 64 / 128) | fast page erase → BUFRST → 1 語ずつ BUFLOAD → STRT。page の大きさと BUFLOAD の幅は register で受ける |
| `direct` | V20x / V30x / V407 / X315 / H417(direct、PG_STRT) | fast page erase → FTPG で page に書く → PG_STRT |

呼び方:

- 入力: a0 = page の番地、a1 = RAM の buffer、a2 = page の byte 数、a3 = BUFLOAD の幅と flag、a4 = FLASH の base(0x40022000)。
- 成功: `ebreak` を +固定位置に置き、a0 = 0 で止まる。
- 失敗: 別の固定位置の `ebreak` で、a0 = 0x80000000 | STATR で止まる。
- 置き場所は SRAM 0x20000000。buffer は loader の後ろで、page の分。

binary の扱い:

- binary は commit する。`cargo xtask loader-gen` が ArduinoCore-CH32RV の vendor toolchain(xpack riscv-none-elf-gcc)で build し直し、hash を照合する(architecture §3 の「stub は in-repo source から build」を、この loader で先に満たす)。
- V003 の page は 64 byte で fast page erase も 64、V103 は 128 と 128 で、どちらも DB の `flash_geometry` の値を使う。
- wlink の V003 loader(500 byte)は使わない(ライセンスは MIT / Apache で問題ないが、1 本にまとめる)。**buffered で V003 が動くことは実機で確かめる**(§9)。

### 5.2 手順(参照 client の `ch32_flash.program` + 直す所)

1. open(lease 3 秒)→ attach(halt、max_speed は critical、pins は serial port を選んだときのスロットから。§6)。
2. target の識別と `--chip` の照合(§4.3)。RDPR を読み、読み出し保護なら断る(host guide §4.5)。
3. **reset(mode 2 = halt_at_reset)**。走っている IWDG を止めたまま書くため。
4. FLASH の unlock(KEYR、MODEKEYR)と、その確認。
5. loader を置き、読み戻して照合する(最大 3 回)。壊れた loader でも ebreak まで着いてしまい、全 page を壊すため(L103 の事例)。
6. page ごとに write_words(buffer)→ run_until_halt(a0..a4、mstatus = 0、timeout 200 ms、outs = a0)。
   - 16 page ずつ pipeline に乗せる(write、run を 16 回で 1 回の write)。
   - 成功の条件は、ok かつ stopped かつ dpc が成功の位置かつ a0 = 0。
   - dpc が loader の先頭のまま = 走らなかった。その page はやり直す(loader は page 単位で冪等)。
7. **verify**: 全体を read_words で読み戻す。消去値は DB の `erased_word`(V20x / V30x は 0xe339e339)。
8. 違った page と失敗した page を書き直す(最大 2 周。周ごとに loader を置き直す)。
9. `--reset run`: reset(mode 1 = run+確認)を最大 3 回。`--confirm-run` は既存の規則。
10. detach → end。JSON は WCH-Link と同じ形。`flash.programmer = "oep-loader"` と `rewritten` / `retries` を足す。

**`--erase`**:
- `sector`(既定)は、書く page だけを loader の中で消す。
- `chip` は全 page の消去を loader で回す(OEP には mass erase の部品が無い。loader に mass erase の flag を足すかは実測の速さで決める)。
- `auto` は、書く範囲が flash の半分を超えたら chip、それ以外は sector。

## 6. serial port を選んだときのスロット(依頼 14、monitor と共通)

- serial port の path が WCH-Link のものでなければ、その port を開いて confirm を送る(利用者がその port を選んだので、開いてよい)。
  - `OEP!` が返らなければ「OEP の probe ではない」として exit 10。
  - OEP の probe(§3.3 の `is_oep_device`。iProduct の名前)の port なら、upload は常に断る(`oep://` を選ぶよう案内する)。
- **スロットの選び方**(WF §3.4):
  - 板の家系(`--chip`、monitor では `chip` の設定)に合うスロットが 1 つなら、そこを使う。
  - 接続済みのスロットの chip は、describe のスロットの状態から読む。未接続のスロットは、止めない attach で読む。
  - 0 個か 2 個以上なら止めて、スロットの一覧と理由を出す(exit 14 / 23)。
- **スロットは spec にまだ無い**(§10 の 5)。spec に入るまでは、今の spec にあるものに落とす: 各 wire interface(rvswd / swio)の「許されたピンの組」を 1 スロットとみなし、`scan` で chip を読む。

## 7. monitor とブローカー(依頼 11、12)

### 7.1 OEP の probe の monitor

- `arduino monitor` の address が OEP の probe(serial port か `oep://`)で source が sdi / dmdata / dmseq なら、ブローカーの client として `oep.target.console` を開く(実装済み)。止めずに attach する。`rtt` は OEP の console に無いので断る。
- source `uart` は、OEP の probe の port でも素通しのまま。どの device か分からない port に OEP の confirm を送らないため(実装済み)。
- **source `fixture-uart`**(2026-09-29 の決定、実装済み): ブローカー経由の session で `oep.fixture.uart` を開き、monitor の `baudrate` を configure で送る(OPEN のときと、開いたまま変わったとき)。ストリームを読み、入力は書き戻す。「IDE の baud は ch32rv が fixture.uart の configure で届ける」(spec で line coding の写しを消した代わり)はこの source で満たす。fixture の UART にピンの plan が無ければ、OPEN が理由つきの error。
- **`oep://<probe>/<slot>` の port**(protocol `oep`): source は dmseq(既定)/ dmdata / sdi / fixture-uart。console はそのスロットのピンの組で attach する(実装済み)。
- rev 1 は push が無いので、read を poll する。読み出しは lock-free。
- 読み始める位置は、最後の reset mark から(書き込みの直後に開いたとき、最初の行を落とさない)。
- IDE の入力は console write で送る(dmseq は 1 回に 2 byte まで。残りは送り直す)。
- fixture.uart のストリームは、source `fixture-uart` で選べるようにする(板の UART を probe が受けている場合)。
  - UART の速さは、monitor の `baudrate`(と、足すなら `format`)を `oep.fixture.uart` の configure で probe に送って決める。OPEN のときと、開いたまま CONFIGURE で変わったときに送る。probe の CDC の line coding を target の UART に写す機能は spec から消え、probe.config にも保存しない(2026-09-29、dev-oep-07 の決定)。
- 接続を失う(link-lost の mark、boot_id が変わる、transport が消える)と、`[ch32rv monitor] stopped: …` を流して終わる(§1 の規則のまま)。

### 7.2 ブローカー(2026-09-29 の決定、ArduinoCore-CH32RV oep-workflow §7.2)

**probe ごとに、誰の子でもないブローカーを 1 つ置く。ch32rv の各コマンド(flash、monitor、gdb、1 回だけの read / reset)と pytest の `oep_host` は、どれもブローカーの client になる。** 経路は 1 つで、立ち上がる順で形は変わらない。

- **serial の速さ(port_speed、oep-core §3.5、2026-10-01 から既定で使う)**: probe を serial(UART bridge)で開いたブローカーは、endpoint を書いた後、client に答え始める前に、921600 → 750000 → 500000 の順に試す(`CH32RV_PORT_SPEED` で `off` か並びを指定できる)。手順は参照 client の `raise_speed` と同じ: 試す → host も切り替えて 20 ms 待ち confirm → link_source / link_sink で両方向を 1 秒か 32 KiB 確かめる(max_inflight で並べ、壊れたら 1 つずつで確かめ直し、通った上限で使う) → 壊れたフレームが無ければ決める、あれば戻す。全体の上限は 6 秒。試した結果(速さごとの通った / 壊れた、in / out の KB/s、並べた数、かかった時間)はブローカーの log に 1 行で出す。session が口を持っている間は、壊れたフレームを待たずに同じ corr ですぐ送り直す。上げた速さで答えが来なければ、起動時の速さ(115200)に戻って confirm し、送り直す。決めるときの idle_ms は仕様の上限の 3000 ms(`port_speed_idle_max_ms`): ブローカーが落ちても probe は 3 秒の黙りで戻り、ブローカーは 1 秒ごとの keepalive で保つ。serial を開いたときの confirm は、前の host が上げた速さの残りを待つために約 4 秒まで繰り返す(oep-core §3.5)。上げている間はフレームの様子を見て、悪ければ port_speed(戻す)を送って probe と揃えて起動時の速さに戻り、その session では上げない。判定は probe の firmware(fn 0 describe の firmware)で分ける: oep-probe-arduino 0.0.27 以降(壊れ 3 つが続いて初めて自分で戻る)は直近 3 秒の(壊れ + 失われ)の割合が 10 % を超えたら(50 フレーム未満は判定しない。host 開発ガイド §7.4、基準 0)、それより前(1 秒に 3 つで戻る)は 5 秒に 2 つで(probe より先に下げる)。上げた速さで答えが無ければ、まず起動時の速さで約 4 秒 confirm し(開く側と同じ。通らなければリンクの失敗、oep-core §3.5 の義務 5)、probe がそこにいれば(自分で戻っていた)残りをそこで送り直す。この立て直しが lease の中に収まるよう、ブローカーの lease は 10 秒。ブローカーは最後の client が抜けた後 3 秒残り、その間も keepalive で session と速さを保つ(IDE がモニターを閉じてからアップロードを始めるまでの間)。CLI の 1 回だけのコマンドはブローカーの client なので、別に速さを触らない。

- **起動**:
  - client は、まず `<runtime>/<key>.oep`(待ち受けの場所)を見てつなぐ。key は probe の同一性(2026-10-01、docs/freeze-decisions.ja.md §4): OEP の USB の probe は `oep-<USB serial>`(serial = unit_id。無ければ `oep-usb-<位置>`)、OEP の USB device を持たない serial port(UART bridge)は `oep-port-<正規化した path>`、WCH-Link は `wch-<serial>`(無ければ `wch-usb-<位置>`)。同じ probe へのどの道(`oep://`、どちらの CDC の `port:`)も同じブローカーに着く。runtime は DeviceLock と同じ利用者ごとのディレクトリ。
  - 無ければ `ch32rv broker serve --probe <sel>`(利用者向けではない subcommand)を切り離して起動する。Linux / macOS は double fork + setsid、Windows は DETACHED_PROCESS と job object からの breakaway。stdio は捨てる。
  - 起動の取り合いは `<runtime>/<key>.broker.lock` の flock で 1 つにする。取れなかったほうは起動をやめ、`<key>.oep` が現れるのを待ってつなぐ。
- **待ち受け**: 127.0.0.1 の TCP を 1 つ(port 0 で選ぶ)。`<key>.oep` には port、pid、起動時刻を書き、file は 0600 にする。`ch32rv broker endpoint --probe <sel> --json` がこれを返す(結果の封筒の `result` が `{endpoint, pid, transport}`。`endpoint` は `"127.0.0.1:<port>"`、ブローカーが無ければ null)。認証は無い(OEP の TCP の注意どおり)。
- **終わり方**: **client が 0 になったらすぐ終わる**(待ち時間なし)。transport を閉じ、`<key>.oep` を消す。probe を失ったとき(抜かれた、boot_id が変わった)は、client に切断で知らせてから終わる。
- **client から見ると OEP そのもの**(spec の TCP の形 `length(u16) message`)。ブローカーは次のことをする。
  - client ごとに corr を付け替え、probe への pipeline に混ぜる。応答は元の corr に戻して、その client にだけ返す。
  - client の `open` / `end` / `keepalive` / `lock_state` は受け止めて、自分で答える。open は lease_ms と本物の boot_id を返し、client の session id を覚える(それ以外の id の要求には no_session。end でその client の接続と plan を外す)。client の owner の名前は台帳に持ち、`broker endpoint --json` で一覧できるようにする。
  - confirm / list / describe は、ブローカーが持っている写しで答える(boot_id が変わったら取り直す)。
  - 1 つの client の要求の並びは崩さない(probe は順に処理する)。client をまたいだ並びは到着順。
- **client ごとの資源の台帳**:
  - 接続: attach の応答の connection。参照は client ごとに数え、利用者が他にいなくなったときだけ detach する。
  - plan(plan_apply の fn)、console の stream(open したもの)。
  - hart の状態: halt したか、run の途中か、置いた trigger(gdb 用、§8)。
  - client が落ちたら、その client の分だけ外す。plan は plan_release、halt したままなら resume、trigger は外す。昇格や台帳の申告し直しは無い。
- **LinkE もブローカーの裏に置く**。ブローカーが LinkE の debug の口(vendor)を持ち、client には OEP を見せて、interface を WCH-Link の操作に写す。
  - `oep.wire.rvswd` / `swio` の attach は AttachChip(target_id は chip_id)。
  - `oep.target.riscv-dm` の dmi / halt / resume / reset / block / run は DmiOp と DebugModule(`DmTarget`)。
  - `oep.target.console` はブローカーの中で dmdata / dmseq / rtt の mailbox を poll して、位置付きのストリームにする(mechanism は OEP の番号)。
  - WCH の stub での書き込みは、ch32rv 独自の interface(例 `io.github.ch32-riscv-ug.wchlink`)に置く。今の `flash` の速さを保つため。
  - これで gdb と dmseq などの monitor が同じ LinkE を同時に使える。
  - **実装済み(2026-09-29)**: `broker serve --probe serial:<sn>|usb:<topology>|port:<Link の CDC>` で LinkE を裏に置く。`arduino monitor` の dmdata / dmseq は LinkE でもブローカー経由。1 回だけのコマンド(verify / read / reset / target info)は、その LinkE のブローカーが動いていればブローカーを通し、動いていなければ今までどおり直接開く。**flash はブローカーから Link を借りる**: ch32rv 独自の interface `io.github.ch32-riscv-ug.wchlink` の lend でブローカーが session を手放し(console の read は手元の出力で答え続ける)、flash は直接の経路(WCH の stub)で書き、reclaim で返す(flash のプロセスが落ちてもブローカーが取り返す)。V003 で 2.6 KB に 1.0 秒(ブローカーの loader の経路では約 20 秒だった)。rtt / sdi / gdb / erase / dbg などは、まだブローカーを通らない(LinkE のブローカーが動いている間は lock で断られる)。
  - **LinkE の高速の read(`read_mem`)は memory 専用で、書いた直後は古い値を返す**(CH32V003 で実測): 周辺の register(FLASH_CTLR)では直前に書いた鍵の値を返し、DMI で書いた直後の SRAM と、loader で書いた直後の flash も古い値を返した。ブローカーは、code flash の範囲で、しかもその attach の間に何も書いていないときだけ高速の read を使い、それ以外は Debug Module で読む。
  - **uart の source の monitor は CDC だけを使い、ブローカーにも probe の lock にも触れない**(実装済み、e66f7cf)。
- **時間の制約**: `arduino monitor` の OPEN は、ブローカーの起動・transport の open・attach まで含めて、arduino-cli の待ちの内に返す。実測では、OPEN の返事が 6 秒遅れても通り、9 秒遅れるとエラー無しで閉じる。目標は 3 秒以内。
- **確かめること**:
  - Windows / macOS で、arduino-cli が止まったときに切り離した子が一緒に消えないか(WF §10)。
  - 1 回だけのコマンドがブローカーを通る分の遅れ(起動を含む)。
  - flash の最中も monitor が console を読み続けられること(probe は riscv-dm の要求を実行している間だけ console の poll を止める)。reset の mark から続きを読めること。

## 8. デバッグの余地(今回は実装しない)

- (a) `OepDtm` が `DtmAccess` と §4.2 の `TargetAccess` を実装するので、`ch32rv-debug` の `Ch32Target<T: DtmAccess>` は、そのまま OEP の上で動く。ブローカーの client として話す `DtmAccess` も同じ形(TCP の上の `OepDtm`)。
- (b) ブローカーの台帳に、client ごとの「hart を止めている」「breakpoint を置いた trigger」を持たせる。gdb の client が切れたら、trigger を外して resume する。
- (c) **gdb も monitor も、同じブローカーの client**(§7.2)。どちらが先でも同じで、IDE 2.3.10 は debug の開始で monitor に触れない(同梱の bundle で確認済み、b2)。3 つ以上でも同じ。
  - 注意: gdb が hart を止めている間、monitor には何も流れない(spec の規則)。
- UART bridge の probe で DMI の往復が実用になるかは、実装のときに測る(参照の実測は 1 往復 5.7 ms)。

## 9. 実機で確かめること

- RV32EC の loader: V003(SWIO)、V006、X035、L103、V203。
- 使う probe: P4 の X035 治具(USJ)、V003 治具(UART bridge、COBS)、L103 治具(RP2350)。どれも b2 の bench にある。
- 速さの比較: 参照 client の X035 62 KB(1.89 秒)と LinkE + ch32rv(3.28 秒)。
- ブローカー越しの flash の最中に monitor が行を落とさないこと。reset の mark から続くこと。
- lock の奪い方: serial 1 本の probe で、前のプロセスを kill した直後に force で入れること。

## 10. spec との対応

- 設計のときに仮置きした 13 項目は、すべて oep-spec 89879bc に入った。台帳は `cargo xtask oep-gen` で取り直してある。
  - serial port の COBS と前後の 0x00(core §3.1)、serial port の共用(§3.4)、経路の一覧(describe 0x49、enum transport_kind)、discovery に出る形の宣言(0x4A `discoverable`)
  - スロット・接続の数・接続の一覧(wire の describe `max_connections`、op `connections`)
  - bind の mode(probe.config の item slot / bind)
  - 持ち主の名前(open の TLV `owner`、lock_state と locked の payload に返る)
  - lease の範囲(1000〜60000 ms、0 は probe の既定、応答の lease_ms が正)
  - reset の応答の flags
  - console の mechanism の宣言(describe 0x40)
  - `min_max_frame` = 64
  - riscv-dm の `max_length` は byte 数。read_block はバスを通して読む(probe は写しを持たない)
  - ブローカーは host の実装で spec の外

- **ゼロベース見直しへの追従(2026-10-01、oep-spec 37278b6、fake = oep-client-python 7b56c15)**: TLV の長い形(`tag 0xFF len(u16)`)、confirm の boot_id、read の応答の len(console / fixture UART)、マークの time_ns(22 byte)、dmi の nvals(u16)、run の nvals(u8)と stopped 2、attach の一本化(method 0 / 1、max_speed は必須なので ch32rv は常に送る(呼び出し側の値、無ければ線の describe の max_clock_hz、無ければ 1 MHz)、応答の flags は `attach_flags`、bit3 で TLV 0x11 dpc)、scan の skip は TLV 0x02、probe.config のスロットは retry_ms(u32)と max_speed_hz(固定部 19 byte)、スロットの状態は `state` op(0x06)、`discoverable`(0x4A)、資源番号は 1 つの空間(WCH のブローカーは connection 1、ストリームは 2 から)。probe は op の外に target の状態を持ち越さないので、ch32rv の OEP の resume は dpc を読むたびに DATA0 / DATA1 を戻す。
- **簡素化への追従(2026-10-06、oep-spec 3c96daf、fake = oep-client-python 305852a、probe = oep-probe-arduino 0.0.29 以降)**: 要求の見出しは常に 10 byte で session_id を持つ(0 = session なし。role 0x81 は無い)。TLV は `tag len(u16) value` の 1 つの形。並び(list、scan、marks、probe.config の state)は要素の長さを持たない(要素の形は revision で決まる)。スロットの固定部は boot_reset を足して 20 byte、slot_state は reset_at_ns を足す。再開は無い: end・lease 切れ・force で session の資源はすべて外れ、その id は no_session(ブローカーは `no_session` で新しい session を開く。`expired` は無い)。任意の op は各 fn の describe の `ops`(base + bitmap)で知る(features では知らない)。port_speed と線の試験は `oep.link`(source は len(u16) data、最大 max_frame − 26 byte、sink は count(u16) data)。fn 0 の `restart`(op 0x14、任意)と `restart_max_ms`(describe 0x4F): ブローカーは client の restart を自分の session で中継し、答えを返してから起動時の速さで confirm を restart_max_ms まで繰り返し、新しい session を開く(client の session は終わる)。旧 wire との互換は持たない(凍結前の方針、2026-10-06 のユーザーの判断。USB の probe は 1209:4F45 で見分ける)。
- **構成の見直しへの追従(2026-10-06、oep-spec 498ae95、fake = oep-client-python 6326df9、probe = 0.0.29-dev+3c0cd99 以降)**: core は名前の無い fn 0 で、list に出ない(版は confirm の revision だけ)。plan は `oep.probe.plan`、restart は `oep.probe.restart`(op 0x01、describe 0x40 restart_max_ms)、`oep.link` は `oep.probe.link`。ブローカーは plan と restart を名前で探した fn で見分ける。購読は通知を送るインターフェースの 0x30 / 0x32(どのインターフェースの op の空間でも予約)で、ブローカーはどの fn でも断る(通知を中継しない)。heartbeat は無い。fn 0 の `clock`(op 0x04、ロックも session も要らない、答えは boot_id と uptime_ns)はブローカーが中継する(ブローカーが自分で答えるのは confirm / open / end / keepalive / lock_state の 5 つ)。describe の ops は core §7.4 の唯一の符号だけを受ける(`session::decode_ops`、試験は spec の ops_encoding.json)。**restart の中継**: ブローカーは client の restart を自分の session で送り、答えを返したら probe への経路を閉じて終わる(transports §1)。client は restart_max_ms まで待ってやり直す(新しいブローカーが起動する)。上の項の「その場で立て直す」は、ブローカーが中継しなかった再起動(電源、watchdog、他の host)のときだけ。

## 11. 決まったこと

1. 長さ見出しの CDC / USJ の firmware と話す手段は持たない。USJ の X035 治具は、probe の firmware が COBS になってから使う。それまでは偽の probe(fake_serve の pty)と V003 治具(UART bridge、今も COBS)で試す。
2. RAM loader は RV32EC の 1 本(buffered / direct)。wlink の V003 loader と参照の X035 loader は使わない。速さは「LinkE + ch32rv と大きく離れていなければよい、詰めるのは後」。実機で比べた結果は b2 に報告する(実機の試験は b2 の bench で、声をかけてから)。
3. gdb と monitor が同じ probe を使うときの形(§8(c))は確認中。
4. 結合試験は oep-client-python の偽の probe を使う(Python は試験のときだけ uv で動かす)。Rust で同じものは作らない。fake は出ている probe と同じ wire の commit に固定する(`crates/oep/tests/fake/uv.rs` の `FAKE_REV`、隣の checkout から `git archive` で `target/` に取り出す。`$OEP_CLIENT_PYTHON` を与えればその checkout をそのまま使う)。2026-10-06 時点は f91da22(0300973 から fake は簡素化後の wire で、probe 0.0.28 はまだ旧 wire)。ch32rv が新しい wire に移るときに上げる。上流に fake_serve(pty と TCP)が入ったら、`crates/oep/tests/fake/serve.py` は消してそちらに乗り換える。

## 12. 実装の順

1. `ch32rv-oep`: codec + 台帳の生成 + serial(COBS)+ link + session。偽の probe で試験する。
2. `OepDtm` + `TargetAccess`。`target info` と `read` を OEP で動かす(`--probe port:<path>` で OEP の probe を引く)。
3. RV32EC の loader + `flash` / `verify` / `reset`。
4. `arduino monitor` の OEP の console。
5. ブローカー + `broker endpoint`。他のコマンドがブローカーを通るようにする。
6. vendor bulk / HID、`oep://` の discovery、スロットの選び方(spec が追いつき次第)。
