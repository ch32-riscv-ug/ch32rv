# OEP host(ch32rv が OEP の probe で書き込み・モニタ・ブローカーをする)

- 作成日: 2026-09-29
- 状態: 設計(実装前)。検討中なので日本語のみ([development.ja.md](development.ja.md) §1)
- 出発点:
  - ArduinoCore-CH32 `docs/oep-workflow.ja.md`(最終の形。2026-09-29 の決定。以下 WF)の依頼 7〜14
  - oep-spec の HEAD(e9c8e1f)と `registry/oep-v1.toml`
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
- **今の spec(core §3.1)は CDC / USJ を長さ見出しとしている**。WF の決定と食い違うので §10 の 1 に入れる。
  - 移行の間、長さ見出しの firmware と話す手段を ch32rv が持つかは、決めてもらう(§11 の問 1)。
  - 持つ場合は `--oep-framing cobs|length` の明示だけとし、自動判別はしない(serial port に OEP でないバイトが混ざる前提なので、判別が危うい)。

### 3.2 serial の開き方

- 必ず排他で開く(serialport crate は unix で TIOCEXCL、Windows は元から)。115200 固定(WF §4.5)。DTR / RTS は立てたまま(参照 client と同じ。下げるなら RTS が先)。1200 baud では開かない。
- serial port は target の console と共用なので(WF §4.1)、応答を待つ間に来るフレームの外のバイトは捨てる。

### 3.3 USB の口の見つけ方

- **専用 PID の probe**(pid.codes で取る予定。WF §3.3): iInterface が `OEP` で始まる interface を探す。vendor bulk、HID、CDC の順に試す(core §3.3)。
- vendor bulk は nusb で扱う。
  - IN は専用 thread で汲み続ける(汲まないと OUT が詰まる、参照 client の E160)。
  - wMaxPacketSize の倍数の書き込みの後には ZLP を送る。
- HID は hidapi で扱う。
  - vendor の report(usage page 0xFF00 以上)を descriptor から探す。
  - report ID があれば、output にも ID を付ける。

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
  - 対話的なコマンド(flash、read、target info)は 3 秒。monitor とブローカーは 3 秒で、keepalive を 1 秒ごとに送る。
  - pytest の `oep_host` は 10 秒(WF §4.3。client 側の話)。
  - run の timeout は lease と応答待ちより十分短くする(run の間、probe は他の要求に答えない)。
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

- binary は commit する。`cargo xtask loader-gen` が ArduinoCore-CH32 の vendor toolchain(xpack riscv-none-elf-gcc)で build し直し、hash を照合する(architecture §3 の「stub は in-repo source から build」を、この loader で先に満たす)。
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
  - 専用 PID の probe の port なら、upload は常に断る(`oep://` を選ぶよう案内する)。
- **スロットの選び方**(WF §3.4):
  - 板の家系(`--chip`、monitor では `chip` の設定)に合うスロットが 1 つなら、そこを使う。
  - 接続済みのスロットの chip は、describe のスロットの状態から読む。未接続のスロットは、止めない attach で読む。
  - 0 個か 2 個以上なら止めて、スロットの一覧と理由を出す(exit 14 / 23)。
- **スロットは spec にまだ無い**(§10 の 5)。spec に入るまでは、今の spec にあるものに落とす: 各 wire interface(rvswd / swio)の「許されたピンの組」を 1 スロットとみなし、`scan` で chip を読む。

## 7. monitor とブローカー(依頼 11、12)

### 7.1 OEP の probe の monitor

- `arduino monitor` の address が OEP の probe(serial port か `oep://`)なら、session を持ち、`oep.target.console` を開く(mechanism は source の設定: sdi / dmdata / dmseq)。
- rev 1 は push が無いので、read を poll する。読み出しは lock-free。
- 読み始める位置は、最後の reset mark から(書き込みの直後に開いたとき、最初の行を落とさない)。
- IDE の入力は console write で送る(dmseq は 1 回に 2 byte まで。残りは送り直す)。
- fixture.uart のストリームは、source `fixture-uart` で選べるようにする(板の UART を probe が受けている場合)。
  - UART の速さは、monitor の `baudrate`(と、足すなら `format`)を `oep.fixture.uart` の configure で probe に送って決める。OPEN のときと、開いたまま CONFIGURE で変わったときに送る。probe の CDC の line coding を target の UART に写す機能は spec から消え、probe.config にも保存しない(2026-09-29、dev-oep-07 の決定)。
- 接続を失う(link-lost の mark、boot_id が変わる、transport が消える)と、`[ch32rv monitor] stopped: …` を流して終わる(§1 の規則のまま)。

### 7.2 ブローカー

- monitor(と、次の作業では gdb server)は、probe との 1 本の session を持ち、127.0.0.1 の TCP を 1 つ待ち受ける。
  - 待ち受けの場所は `<runtime>/<key>.oep` に書く。key は unit_id。runtime は DeviceLock と同じ利用者ごとのディレクトリ。
  - 中身は port、pid、起動時刻。file は 0600。
  - `ch32rv broker endpoint --probe <sel> --json` がこれを返す。無ければ `{"endpoint":null}` で exit 0。
- **client から見ると OEP そのもの**(spec の TCP の形 `length(u16) message`)。ブローカーは次のことをする。
  - client ごとに corr を付け替え、probe への pipeline に混ぜる。応答は元の corr に戻して、その client にだけ返す。
  - client の `open` / `end` / `keepalive` / `lock_state` は受け止めて、自分で答える。open は `resumed = 0` と本物の boot_id を返し、lease はブローカーの値を返す。
  - confirm / list / describe は、ブローカーが持っている写しで答える(boot_id が変わったら取り直す)。
  - **client ごとの資源の台帳**を持つ。
    - 接続: attach の応答の connection。参照は client ごとに数え、monitor 自身も 1 人の利用者とする。
    - plan: plan_apply の fn。
    - console の stream: open したもの。
    - hart の状態: halt したか、run の途中か(gdb 用。§8)。
  - client が切れたら、その client の分だけ外す。plan は plan_release する。接続は、利用者が他にいなくなったときだけ detach する。halt したままなら resume する。
  - 1 つの client の要求の並びは崩さない(probe は順に処理する)。client をまたいだ並びは到着順。
- **他の ch32rv コマンドはブローカーを通す**。flash / read / reset などは、`<key>.oep` があればそこへつなぎ、transport を開かない。
  - flash の最中も、monitor は console を読み続ける(probe は riscv-dm の要求を実行している間だけ console の poll を止める)。
  - reset の mark で、monitor は続きから読む。
- **ブローカーは monitor と一緒に死ぬ**。単体のデーモンは作らない。
- 認証は無い(OEP の TCP の注意どおり)。127.0.0.1 に限り、endpoint の file を 0600 にする。同じ PC の他の利用者からつながれる余地は残るので、§10 の 8 に入れる。

## 8. デバッグの余地(今回は実装しない)

- (a) `OepDtm` が `DtmAccess` と §4.2 の `TargetAccess` を実装するので、`ch32rv-debug` の `Ch32Target<T: DtmAccess>` は、そのまま OEP の上で動く。
- (b) ブローカーの台帳に、client ごとの「hart を止めている」「breakpoint を置いた trigger」を持たせる。gdb の client が切れたら、trigger を外して resume する。
- (c) **gdb が monitor と同じ probe を使う形: 先に probe を開いた長寿命のプロセスがブローカーになり、後から来たほうは client になる**。
  - monitor が先なら、gdb server はブローカーの client になる。gdb が先なら、gdb server がブローカーを持ち、後から開いた monitor が client になる。
  - どちらが先に死んでも、残ったほうがそのまま続けられるよう、ブローカーの役を引き継ぐ手順が要る。引き継ぎは、end して資源を残し、次の open に渡す(core §9 の「end で残し、次の open に移る」)。これも設計に含める。
  - 注意: gdb が hart を止めている間、monitor には何も流れない(spec の規則)。
- UART bridge の probe で DMI の往復が実用になるかは、実装のときに測る(参照の実測は 1 往復 5.7 ms)。

## 9. 実機で確かめること

- RV32EC の loader: V003(SWIO)、V006、X035、L103、V203。
- 使う probe: P4 の X035 治具(USJ)、V003 治具(UART bridge、COBS)、L103 治具(RP2350)。どれも b2 の bench にある。
- 速さの比較: 参照 client の X035 62 KB(1.89 秒)と LinkE + ch32rv(3.28 秒)。
- ブローカー越しの flash の最中に monitor が行を落とさないこと。reset の mark から続くこと。
- lock の奪い方: serial 1 本の probe で、前のプロセスを kill した直後に force で入れること。

## 10. spec に無いので仮置きした所(oep-spec への依頼の一覧)

| # | 仮置きした形 | spec の今 |
|---|---|---|
| 1 | serial に見える transport(CDC / USJ / UART bridge)は、すべて COBS + CRC-16、0x00 区切り | core §3.1 は CDC / USJ を長さ見出し、COBS は bridge の VID:PID で選ぶ |
| 2 | probe → PC は `0x00 <COBS> 0x00`(前にも区切り)。host は空のフレームを無視する | 区切りの位置は書かれていない(参照 encoder は後ろだけ) |
| 3 | serial port を OEP と console の生のバイトで共用する(フレームの外は console へ、生の転送を止める port、終わったら最後の reset の位置から再開) | 無い。bind の data 口は OEP の口と別 |
| 4 | fn 0 describe に経路の一覧の tag(0x49 と仮定、値は種類の u8 の並び: 1 UART bridge / 2 CDC / 3 USJ / 4 vendor bulk / 5 HID / 6 TCP) | 無い |
| 5 | スロット(wire、pins、name、attach の方針、console の mechanism、target_id)、同時に持てる接続の数、接続の一覧の操作、describe のスロットの状態 | 1 つの wire interface に接続 1 つ。スロット無し |
| 6 | bind の組み直し(複数のストリーム、last-reset / manual / mixed) | bind の port は CDC の data 口の番号 |
| 7 | lock の持ち主の名指し | `lock_state` は locked と残り時間だけ。locked の拒否は持ち主の id を隠す。`locked` の値の意味(自分が持っているか)も書かれていない |
| 8 | ブローカー(corr の付け替え、open / end の吸収、client ごとの資源) | TCP の複数 host は未決 |
| 9 | lease の既定値・下限・上限と、0 の意味(参照 probe は 0 → 3000、上限 600000) | 無い |
| 10 | reset の応答の `flags` / `attempts` の意味(参照 probe: bit0 running、bit1 pc で確認、bit2 やり直し、bit3 確認の halt / resume が失敗) | 定義が無い |
| 11 | console の対応 mechanism、riscv-dm の block の方式を describe で宣言する | 無い |
| 12 | confirm の前の frame の大きさの上限(最初の要求は 64 byte に収める) | 無い |
| 13 | 専用 PID と、HID の口の推奨の作り | 未決(WF §10) |

## 11. b2 に決めてほしいこと

1. 移行の間、CDC / USJ で長さ見出しの firmware と話す手段(`--oep-framing length`)を ch32rv が持つか。持たないなら、probe の firmware が COBS になるまで、USJ の X035 治具では ch32rv を試せない。
2. RV32EC の loader 1 本にまとめ、wlink の V003 loader と参照の X035 loader は使わない方針でよいか(こちらは実機で速さを比べてから決めたい)。
3. §8(c) の「先に開いた長寿命のプロセスがブローカー」でよいか。WF §7.2 の「ブローカーは monitor と一緒に死ぬ」を「ブローカーを持つプロセスと一緒に死ぬ」に広げることになる。
4. 試験: `oep-client-python` の `fake.py`(偽の probe)を ch32rv の結合試験に使ってよいか(Python に依存するのは試験だけ)。使えないなら、Rust で同じものを作る。

## 12. 実装の順

1. `ch32rv-oep`: codec + 台帳の生成 + serial(COBS)+ link + session。偽の probe で試験する。
2. `OepDtm` + `TargetAccess`。`target info` と `read` を OEP で動かす(`--probe port:<path>` で OEP の probe を引く)。
3. RV32EC の loader + `flash` / `verify` / `reset`。
4. `arduino monitor` の OEP の console。
5. ブローカー + `broker endpoint`。他のコマンドがブローカーを通るようにする。
6. vendor bulk / HID、`oep://` の discovery、スロットの選び方(spec が追いつき次第)。
