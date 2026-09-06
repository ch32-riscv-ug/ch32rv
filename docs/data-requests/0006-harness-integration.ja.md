# 0006: 自作 probe(DUT harness)との連携 — ch32rv 側の事実と論点

- 状態: **要件出し中(draft)**。**まとめない**。要件の調整とマージは `wch-protocols` 側で行われる予定なので、
  本書は**そこへ持ち込む材料**を ch32rv 側で失わないように置いておくもの。実装・trait 設計には着手しない。
- 依頼元 / 提供元: ch32rv
- 相手: `wch-protocols`(要件調整の場)/ `ArduinoCore-CH32`(発議元)/ `ch32rv-probe`(実装の置き場)
- 作成日: 2026-09-06
- 発議側の文書: `ArduinoCore-CH32/docs/harness-probe.ja.md`(採否評価と依頼 5-1〜5-9)、
  `ArduinoCore-CH32/docs/harness-testing.ja.md`(駆動方式と依頼 5-10〜5-16)、
  **`ArduinoCore-CH32/docs/harness-requirements.ja.md`(要求カタログ `H-001`〜`H-178` と相反 `C-1`〜`C-11`)**、
  `EmbedBench/docs/HARNESS_REQUESTS.ja.md`(`E-1`〜`E-7`)
- 所在の索引: `wch-protocols/references/harness-index.ja.md`

---

## 1. 相手の 2 文書の間で前提がずれている(最初に潰したい)

| 文書 | 言っていること |
|---|---|
| harness-probe §0-3 / §4 | 当面は **LinkE が焼き、harness は観測と刺激だけ**。作る順は `L`(capture)が 1 番、**`W`(DMI/書込)は最後**でよい |
| harness-testing §4.2 / §5 | **agent の制御チャネルを UART から DMI(RTT/DMDATA)へ移す**。`dut_agent()` は DMI 経由 |

後者は **harness が DMI を持っていること**が前提。前者はそれを最後に回している。したがって
**harness-testing の中核(USART の解放、§5 のテストの見え方)は `L` 段階では成立しない**。

同じずれが「時間軸が 1 本になる」(harness-probe §3.1-3)にも出る。**LinkE が焼き harness が観る構成では、
DMI 側の事象は LinkE の時計、capture は harness の時計**で、時計は 2 本のまま。1 本になるのは
harness が DMI を持ってから。

→ **決めるべきは「DMI 能力をどの段階で要求するか」**。

## 2. 排他(harness-testing §11-5 への回答)

### 2.1 ch32rv の lock は probe 単位で、harness 構成では足りない

- キーは **probe serial**(無ければ bus topology)。OS の `flock`。取得できなければ **exit 13(`device-busy`)**。
- 保持するのは attach 経路(`flash` / `target` / `dbg` / `write` / `monitor --source dmdata` / `capabilities` /
  `arduino`)、`gdb` server、`monitor --source uart` / `sdi`。
- 異常終了しても OS が解放する(stale 掃除が要らない)。

**harness と LinkE は別の USB device なので、この lock は競合しない。** ch32rv は harness の存在を知らず、
両者が同じ DUT に別経路で同時に触れる。**排他のキーが probe ではなく DUT でなければならない場面は、
この構成で初めて出てくる**。

### 2.2 lock だけでは足りない — core の状態にも排他がある

| 用途 | core の状態 |
|---|---|
| `SerialDMDATA` / RTT の polling | **running のまま**(attach 後に resume が要る) |
| flash 書込・memory 読み・breakpoint・option byte | **halt 必須** |

agent 制御を DMI に載せると、**agent チャネルが生きている間は halt を伴う操作ができない**。
ロックの取り合いではなく「core をどちらの状態に置くか」の調停なので、
**セッション protocol 側に明示的な release / re-acquire が要る**。

### 2.3 「ch32rv = CLI 1 回 1 操作」は半分だけ正しい

harness-testing §3.1 の分類は概ね正しいが、**ch32rv には既にセッション型の経路がある**:
**gdb server**(起動から終了まで attach を保持)と `monitor`(streaming)。
つまり対立軸は「CLI 型 vs セッション型」ではなく、**gdb server と harness セッションが同じ DUT を取り合う**
という §11-5 と同じ問題。

## 3. 実測値(相手の見積りの裏づけ)

harness-testing §9.2 の表が「host 往復 0.5〜2 ms(USB CDC)」を仮定している。ベンチで実測した。

```
DMI 1 往復(USB out → in): 中央 471 µs(WCH-LinkE)/ 461 µs(WCH-Link CH549)
  n=8 ずつ、`dbg reg read pc --capture` の NDJSON の t_us 差分から算出
  → probe 種別でほぼ差が無い = USB スタック側の往復コスト
  ※ WSL2 + usbipd 経由。native Linux はこれより速いはずで、上限側の値として扱う

レジスタ 1 本読み(pc) = 転送 28 件 / DMI 8 往復 ≈ 4 ms
```

**推定の範囲内**で、結論(SPI と UART は host backed が原理的に不可)は変わらない。

関連する既知の数字(ch32rv 実測):

| 操作 | LinkE(専用コマンドあり) | DMI だけで組むと |
|---|---|---|
| 32 KiB read | **0.71 秒**(バルク read) | word ごとの DMI 往復で **>120 秒でタイムアウト** |
| flash page 消去 | — | **~100 ms/page**(32 KB = 128 page ≈ 12.8 秒) |

→ `dmi-bridge.ja.md` §4.3 の **`batch` は既に仕様化済み**。足りないのはサイズを決める数字のほうで、
**「レジスタ 1 本 = 8 op」を 1 往復に畳めるか**が最初の分水嶺。Core 要件の「`batch` 8 op 以上」と整合する。

## 4. probe backend が実装すべき面(ch32rv 側の現状)

### 4.1 いま存在する境界は `DtmAccess` だけ

```rust
pub trait DtmAccess {           // crates/dmi/src/lib.rs
    fn dmi_read(&mut self, addr: u8) -> Result<u32, DmiError>;
    fn dmi_write(&mut self, addr: u8, value: u32) -> Result<(), DmiError>;
    fn dmi_nop(&mut self) -> Result<(), DmiError>;
}
```

**`ProbeService` はコードに存在しない**([architecture.ja.md](../architecture.ja.md) の将来計画)。
相手の文書は「`DtmAccess` / `ProbeService` の trait 境界がある」と書いているので、**この点は訂正して伝える**。

### 4.2 CLI が probe に投げている操作(実測 19 種)の分類

trait 設計はしないが、**面の一覧は事実として出せる**。

| 分類 | メソッド | harness ではどうなるか |
|---|---|---|
| **DMI で代替できる**(host 側へ移せる) | `chip_info` / `read_mem` / `write_flash` / `erase_flash` / `soft_reset` | chip id は memory 番地から読める(番地は `ch32-device-data` にある)。read/write は batch DMI。erase は FLASH controller |
| **物理的に probe にしかできない** | `set_power` / `erase_code_flash_by_power_off` / `erase_code_flash_by_rst` / `set_speed` | 電源・NRST・線の clock |
| **LinkE 固有で harness には不要** | `attach_chip` / `detach_chip` / `redetect_chip` / `set_sdi_print_enabled` / `switch_to_dap` / `enter_iap` / `probe_info` | 「probe が chip を掴む」概念自体が LinkE 固有 |

## 5. 再利用してほしい ch32rv の contract 面

harness-probe §5-6(e) は exit code と JSON envelope を挙げているが、**凍結済みの面はもっと広い**。
「2 つ目の方言を作らない」を全部に適用してほしい。

| 面 | 実体 |
|---|---|
| exit code | 数値は凍結(`ch32rv-contract` の `exit.rs`、test が固定)。`device-busy`=13 / `capability-unsupported`=24 / `verify-mismatch`=30 / `transfer-failed`=40 等 |
| JSON envelope とキー命名 | `result` は snake_case、同一概念は同名(`addr` / `family` / `scope` / `verified` / `firmware` / `flash_bytes`)、二値は bool |
| **capability の語彙** | `capabilities --json` が probe×target で可否と**理由**を出す(connect / flash / erase-range / flash-bp / gdb HW breakpoints / monitor sdi / monitor dmdata / recover power-off)。**ここが相手の依頼に抜けている** |
| advisory lock | §2.1 のとおり |

## 6. mock(harness-testing §7)の段 3 は ch32rv に実在する

`--capture <file>` で USB 往復を NDJSON に記録し、**`--replay <file>` で実機なしに再生**できる。
記録と違う write が出れば **divergence として報告**する(= コードが capture と違う protocol を出した合図)。
offline CI 回帰に使っている(`tests/fixtures/replay/`)。

→ **harness が同じ NDJSON 形で自分の往復を記録すれば、この道具立てがそのまま効く**。
「形式が分岐すると replay 層をもう一度作り直す」(harness-probe §5-4 と同じ論法)がここにも当てはまる。

## 7. attach で target を汚す実例(harness-probe §5-1 の材料)

相手の文書は `RCC_CFGR0` / `FLASH ACTLR`(LinkE)と flash 先頭 48 byte(WCH OpenOCD)を挙げている。
**もう 1 件、ch32rv が root cause 済みのものを足せる**。

- **CH32V103 で `AttachChip` が生きた GPR `s1`(x9)を chip id で上書きし、復元しない**(dscratch にも退避しない)。
  resume 後に program が s1 を使った瞬間 fault する。halt→resume だけでも起きる。V003/V203/X035 では起きない。
- ch32rv の対処: **attach 後に soft-reset** して program にレジスタを再構築させる(`attach_corrupts_regs` で gate)。
- **dumb DMI bridge は `AttachChip` そのものを持たないので、この破壊は起きない**。
  「書かない実装ができる」の具体例として使える。

## 8. 制御チャネルを DMI へ移す代償(harness-testing §4.2)

USART を解放する代わりに、**debug 線を占有する**。

`CH32V003` は **1 線 SWIO で pad は `PD1`**(`ch32-device-data` の `evidence/debug_wiring.csv`、
出典は WCH-Link User Manual、confidence: confirmed)。SOP8 は GPIO が 6 本しかないので、
**`PD1` を GPIO として試験したい場合、制御チャネルが死ぬ**。

これは ch32rv が `recover --method power-off`(app が SWDIO/SWCLK を転用して attach できなくなった target の復旧)
を用意している、まさにその状況。**USART の問題が debug 線に移動する**側面があるので、利点表に代償の行が要る。

## 9. データ rev の固定(resolver 設計に効く)

harness-testing §1 の resolver は `index/pinout.csv` / `evidence/remap_routes.csv` を
**`ch32-device-data` から直接**引く形。一方 **ch32rv は pin 系を生成物として持っていない**
(持っているのは skus / option_fields / debug_wiring / flash_geometry / flash_program_method / option_bytes の 6 つで、
**生成時の rev をヘッダに刻んでいる**)。

つまり同じベンチの中で、**ch32rv は pin された rev、resolver は floating な作業コピー**を見る。
2026-09-06 に実際にこれで踏んだ: data repo が動いて ch32rv の `db-check` が stale になり、
**リリース後に気づいた**(対処として release workflow に drift 検査を入れた)。

→ **「束縛をどの rev のデータで決めたか」を fixture manifest に残すか**は要件段階で決めておきたい
(`probe_firmware` を残すのと同じ理由)。

## 10. こちらから出せるもの(相手が要るなら)

| 出せるもの | 状態 |
|---|---|
| attach 時の USB 往復 capture(6 family ぶん) | `--capture` で即取得可。**線側 capture との突き合わせ材料**(harness-probe §7-2 の実験) |
| DMI 往復・bulk read・page 消去の実測値 | §3 のとおり |
| `capabilities --json` の語彙 | 実装済み |
| exit code / envelope の contract | `ch32rv-contract` として crate 化済み |
| 19 メソッドの分類 | §4.2 |

## 11. 本書の扱い

- **要件が固まってからプロトコル repo で調整・マージされる**。本書はその入力で、**ここで結論を出さない**。
- ch32rv 側の実装(`ProbeService` の trait 化、CLI の呼び出し置換)は**要件が固まるまで着手しない**。
- 相手の文書が更新されたら、本書の §1〜§9 のうち解消した項目に取り消し線を引く。

---

# 追記(2026-09-06): 要求カタログを読んだ結果

`harness-requirements.ja.md`(H-001〜H-178 / C-1〜C-11)と `EmbedBench/HARNESS_REQUESTS.ja.md`(E-1〜E-7)を
読み直した。ch32rv 側から出せるものを **H-ID に紐づけ直した**(そのまま突き合わせに使える形)。

## 12. 前提が古くなっている要求(訂正が要る)

| ID | 根拠として書かれていること | いまの事実 |
|---|---|---|
| **H-126** | 「LinkE は **Windows 専用ツールでしか更新できず**、それが 2.11/2.12 問題を長引かせた」 | **解消済み**。ch32rv `probe firmware update` が **Linux / macOS から LinkE の firmware を更新・ダウングレードできる**(0.5.0、WCH の IAP 経路を実装。2.22 ⇔ 2.13 を実機で往復検証)。IAP 滞留からの救出(`probe firmware exit-iap`)もある。**UF2 が望ましい理由は残る**が、「全 OS で更新できない」という根拠は成り立たない |
| **H-002** | 「LinkE は `wlink status` に聞くしかなく、それが 2.11/2.12 問題を見えなくしていた」 | 半分解消。ch32rv は `probe info --json` / `probe list` で **probe 種別・firmware 版(raw / 正規化 / WCH 表記の三重)**を machine-readable に出し、`capabilities --json` が probe×target の可否と理由を出す。**要求そのものは有効**(harness も同じことをすべき)だが、根拠の書き方は更新できる |

## 13. ch32rv の実装が既に答えている要求(参照実装として使える)

| ID | 要求 | ch32rv での実体 |
|---|---|---|
| H-001 / H-006 | 一意 serial + 候補複数なら fail closed | probe selector は `serial:` / `usb:<bus>-<ports>` / `name:` / `index:`。**曖昧なら中止**。`--device 0` 相当は無い |
| H-002 | 種別 / 版 / capability を machine-readable | `probe info --json` / `capabilities --json`(§5) |
| H-003 | per-device advisory lock | `flock`、キーは probe serial(無ければ bus topology)、timeout 超過は **exit 13**、stale 掃除不要。**ただし §2.1 の限界がある** |
| H-005 | exit code / envelope | `ch32rv-contract` crate(数値は test で凍結) |
| H-040 / H-041 / H-153 | 無損失で落とせる / provenance 同梱 / replay で実機なし回帰 | `--capture` の NDJSON(`seq` / `t_us` / `chan` / `dir` / `len` / `ok` / hex `data` + `_meta` ヘッダ + `_device` 行)と **`--replay`**。**記録と違う write は divergence として報告**する |
| H-110 / H-112 | SDI を probe 非依存に / DMDATA を native に | `monitor --source dmdata` は **DM の DATA0/DATA1 を DMI で polling するだけ**なので probe 非依存。LinkE 固有なのは「probe が肩代わりして CDC に出す」部分だけ(`81 0d 02 ee 00`) |
| H-121 | power-off erase / RST erase | 実装済み(`recover --method power-off` / `nrst`)。**LinkE/LinkW 専用**という制約も込みで |

## 14. カタログに無い要求候補(ch32rv の実測から)

**どれも「読んだ値が確定しているとは限らない」系**で、H-107(壊れ読み値)では覆えない。

### 14-1. 書込後の read がいつ確定するかを仕様に持つ

実例が 2 つある。**どちらも「書けているのに検証が失敗する」**という同じ失敗の形。

| 事例 | 症状 |
|---|---|
| **CH549 Link の stale fast-read** | stub 実行直後の高速 bulk read が **program 前の flash 像**(`0xff` やゴミの ramp)を返す。CH549 で ~7 回中 2-3 回、LinkE では未発生。**偽の `verify-mismatch`** の原因 |
| **CH32V103 の option 領域** | option byte を書いた直後、**reset するまで書込前の像を読み返す**。50 ms × 4 の再読み込みでは足りず、**新しい session(attach = reset)で初めて新しい値が見えた** |

ch32rv の対処は前者が「不一致なら権威ある DMI 読みで再確認」、後者が「**検証を soft reset の後に行う**」。
**harness は「自分が書いたものを自分で読み返して確かめる」設計になりやすい**ので、
**確定するまでの規則(reset を挟むのか、bounded retry か、権威ある経路で再確認か)を probe/protocol 側の仕様に持ってほしい**。

### 14-2. attach の監査対象に GPR / CSR を含める

H-100 / H-101 は「target に**書く**もの」を対象にしているが、**メモリだけを見ていると取りこぼす**。

- **CH32V103 で `AttachChip` が生きた GPR `s1`(x9)を chip id で上書きする**(§7)。メモリは 1 byte も変わらない。
- 監査項目を「**メモリ + GPR + CSR の差分**」と書いておけば、この種の破壊が仕様の網に入る。
- ついでに: この検出は **harness が自分でできる**(attach 前後で全 GPR を読んで diff するだけ)。H-103(自己観測で証明)の安価な変種。

### 14-3. ch32rv を経路に残すならセッション化が要る(**C 候補**)

**H-030(テスト本体はセッション。CLI 毎回起動にしない)と H-120(flash/verify/reset を `ch32rv-probe-<name>` backend として)は、
組み合わせると張力を持つ。**

- H-030 の根拠は `reg_probe` の実測(1 レジスタ 1 プロセス起動で 30〜130 秒)。この Reader は **`Ch32rvReader`**、つまり **ch32rv の CLI を毎回起動している**。
- harness がテスト本体を持てば ch32rv は経路から外れるので張力は消える。**ただし §11-5(セッション中に誰が DUT を焼くか)で ch32rv が残ると、そこだけプロセス起動コストが戻る**。
- ch32rv には既にセッション型の口が 1 つある(**gdb server**)。**「ch32rv に持続セッションの口を足す」ことが要求になるかどうか**は、まだどこにも書かれていない。**要求になるなら早めに言ってほしい**(こちらの CLI 設計に効く)。

## 15. C-n への材料(裁定はしない)

| # | ch32rv から出せる材料 |
|---|---|
| **C-2**(multi-lane vs 1 target 専有) | ch32rv の lock は **probe 単位**。multi-lane にすると「1 lane を別プロセスが使用中」を表現できない。**lane 単位の排他が要る**なら lock のキー設計に効く(§2.1) |
| **C-4**(capture 帯域 vs control 応答性) | 実測: **DMI 1 往復 471 µs**(§3)。control を USB FS の同じ device に載せる場合、capture の bulk がこれを押しのける。ch32rv 側は「flash の bulk 転送中は他の往復が止まる」構造なので、**同居させると同じ問題が出る**という前例として使える |
| **C-10**(エミュ vs 実物) | 材料なし |
| **C-11**(配線は利用者の責任 vs 静かな skip) | ch32rv の `capabilities` が同型の問題を扱っている: **「できない」を理由つきで出す**ことで、静かに落ちるのを防いでいる。H-154 のカバレッジ報告と同じ発想 |

## 16. 索引 §6-4(attach 副作用の還流)について

`harness-index` §6 は「**attach 副作用の還流(`RCC_CFGR0` / `FLASH ACTLR`)→ `protocols/pc-to-link.ja.md` 未反映、材料はライタ `0006` §7**」を残件にしている。

- **`RCC_CFGR0` / `FLASH ACTLR` の実測はコア側の成果**(V307、probe-rs と ch32rv の双方で同じ)。ch32rv は**同じ現象を見ている**という裏づけを出せる。
- **ch32rv 固有の材料は §7 の `s1`(x9)破壊**のほう。こちらは root cause と対処まで確定している。
- **書き込みは protocol repo 側で行うもの**なので、こちらからは触らない。必要なら文面はいつでも出せる。
