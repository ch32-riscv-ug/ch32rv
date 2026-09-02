# 依頼 0001: chip ID(device_id)の evidence 表新設

- 状態: **納品受け入れ済(2026-09-02)**。`evidence/device_ids.csv`(72行)+`device_id_addresses.csv` 納品。実測6台(V003/V103/V203/V307/L103/X035)と突き合わせ、rev bits [7:4] don't-care で全一致。`xtask db-gen` が `crates/target/generated/skus.csv` を生成、`target info` の chip_id→SKU 解決を実装(全5接続台で実機動作)。gap 7 series は未発売でデータ側も未収載(接続でき次第 measured 追記)
- 依頼元: ch32rv(CH32 RISC-V 書き込みツール)
- 優先度: **高**。ch32rv の target 自動検出(chip ID → family/SKU 判定、fail-closed)の一次データであり、M2 のブロッカ
- 作成日: 2026-09-01

## 背景

ch32rv は接続した target を chip ID から family/SKU 判定する。判定できない・曖昧な場合は書き込まずに停止する設計のため、**chip ID の網羅性と正確さがそのまま対応 SKU 範囲になる**。

現状の情報源には次の穴がある。

- `ch32-device-data` は chip ID をほぼ持っていない(CHIPID の番地が `evidence/memory_map.csv` に CH32L103 / CH32V205 の 2 family 分あるのみで、**値は 0 件**)。
- `ch32-rs/ch32-data` は `data/chips/*.yaml` に package 単位の `device_id`(例: CH32V003F4P6 → `0x00300500`)と、`docs/device-ids.md` にビット割り・読み出し番地・取得手順を持つ。**ただし V205 / V407 / V467 / X305 / X315 / M030 / M103 の 7 series が欠落**しており、これは ArduinoCore-CH32 の `[compile only]`(書き込み経路が無い)7 board と同じ集合。

## 依頼内容

device_id の evidence 表を新設してほしい。形式は提案であり、表名・列名・置き場所は ch32-device-data の流儀で決めてよい。

提案: `evidence/device_ids.csv`

| 列 | 内容 | 例 |
|---|---|---|
| `part` | 型番(catalog/products.csv と join できる鍵) | `CH32V203C8T6` |
| `id_addr` | device_id の読み出し番地 | `0x1FFFF704` |
| `device_id` | 32bit 値(hex) | `0x30330504` |
| `dont_care_bits` | マッチ時に無視するビット範囲 | `[7:4]`(silicon revision) |
| `id_source` | 値の取得経路: `memory`(番地読み)/ `attach`(WCH-Link AttachChip 応答) | `memory` |
| `note` | 例外事項 | V103 の下位 16bit は STM32 互換 IDCODE 形式、等 |
| `#` `confidence` `basis` | 既存の provenance 流儀 | `confirmed` / 実測 log や資料の locator |

### 特に記録してほしい点

1. **`memory` と `attach` の値が同一かどうか**。WCH-Link の AttachChip 応答に含まれる chip ID と、番地読みの device_id が同じ値かは公開資料で確定できていない。両方測れる個体では両方を行として残してほしい(probe-rs は attach 応答値をマスク `0xffffff0f` で照合しており、[7:4] don't-care と整合する)。
2. **読み出し番地の family 差**。ch32-data の記載では V003/CH641 = `0x1FFFF7C4`、V103 = `0x1FFFF884`、他はほぼ `0x1FFFF704`。番地自体も evidence として値と同じ行に持ってほしい。
3. **package variant の違い**([19:16] が package を表すため、同 series でも package ごとに値が変わる)。実測できた package を行単位で。

## 対象範囲と優先順位

1. **最優先: gap の 7 series** — CH32V205 / V407 / V467 / X305 / X315 / M030 / M103(実機実測が必要。ch32-data に値が無い)
2. 次点: ch32-data が値を持つ既存 series の**取り込み + 照合**(独立 2 ソースの crosscheck として。`tools/crosscheck_ch32data.py` の既存運用に合う)
3. 実測のたびに: silicon revision([7:4])違いの個体が出たら don't-care の裏付けとして記録

## 取得方法の提案

- 権威順・手順は `ch32-data/docs/device-ids.md` に既存(1. 実シリコンから `wlink status` 等で読む、以下資料)。
- WCH-LinkE 実測を basis にする前例は `curated/debug-data-measured.json`(`hartinfo:wch-linke`)がある。同じ流儀で `device-id:wch-linke` のような basis を想定。
- 実測対象の実機は ch32rv 側(手元の V003/V006/V103/V205/V20x/V307/V407/X035/X315/M030/H417 ボード群)で用意できる。測定コマンドの実行が必要なら依頼してほしい。

## 受け入れ方法

ch32rv は納品まで暫定 overlay(`ch32rv-target/provisional/device_ids`)で開発を進める。納品時に暫定値と突き合わせ、一致で受け入れて暫定側を削除する。差分が出た場合は ch32-device-data 側を正として原因を調査する。

## 参照

- `ch32-data/docs/device-ids.md`(ビット割り: [31:20] family / [19:16] package / [15:8] series・process / [7:4] silicon rev = don't-care / [3:0] sub-family)
- `ch32-data/data/chips/*.yaml`(既存 device_id 値)
- probe-rs `targets/CH32*_Series.yaml` の `chip_detection: !WchLink` ブロック(mask と変換表の実例)
- ArduinoCore-CH32 `boards.txt` の `[compile only]` 7 board(価値の直結先)
