# 0006: 自作 probe(DUT harness)との連携 — ch32rv 側の事実と論点

- 状態: **要件出し中(draft)**。**まとめない**。要件の調整とマージは `wch-protocols` 側で行われる予定なので、
  本書は**そこへ持ち込む材料**を ch32rv 側で失わないように置いておくもの。実装・trait 設計には着手しない。
- 依頼元 / 提供元: ch32rv
- 相手: `wch-protocols`(要件調整の場)/ `ArduinoCore-CH32`(発議元)/ `ch32rv-probe`(実装の置き場)
- 作成日: 2026-09-06
- 発議側の文書: `ArduinoCore-CH32/docs/harness-probe.ja.md`(採否評価と依頼 5-1〜5-9)、
  `ArduinoCore-CH32/docs/harness-testing.ja.md`(駆動方式と依頼 5-10〜5-16)

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
