# 依頼 0002: debug interface 種別(1線 SWIO / 2線 RVSWD)の明示列

- 状態: **納品受け入れ・消費済(2026-09-02)**。`evidence/debug_wiring.csv`(27行、series×swdio/swclk pad×dual_support)納品。`xtask db-gen` が `generated/debug_wiring.csv`(26 series、wire=1-wire/2-wire/1-or-2-wire を導出: swclk 空→1-wire、dual=yes→両対応、他→2-wire)を生成。`target info` と `db info` が debug 配線行を表示。実機検証: V003=`1-wire (PD1)`、L103=`2-wire (PA13/PA14)`
- 依頼元: ch32rv
- 優先度: 中(M1-M2。当面は core 名・pinout からの導出で代替できるが、導出は例外に弱い)
- 作成日: 2026-09-01

## 背景

ch32rv は probe の capability 判定(例: 旧 WCH-Link(CH549)は 1線 target に接続できない)と attach 手順の選択のために、SKU ごとに「debug 配線が 1線 SWIO か、2線 RVSWD か、両対応か」を知る必要がある。

現状の `ch32-device-data` にはこの**直接の列が無い**。`index/pinout.csv` の DIO/DCK/SWDIO パッドと `catalog/series.csv` の core/isa(QingKe V2A なら 1線系、等)から導出は可能だが、両対応 series(V00X/M007、M030 は公式資料で 1線/2線両対応)や選択条件の存在が導出を壊す。

## 依頼内容

series(必要なら part)単位の evidence 表を新設してほしい。

提案: `evidence/debug_interfaces.csv`

| 列 | 内容 | 例 |
|---|---|---|
| `series`(または `part`) | 対象 | `CH32V003` |
| `debug_if` | `swio`(1線)/ `rvswd`(2線)/ `both` | `swio` |
| `selection` | both の場合の選択機構(option byte / mode 設定 / 固定)と条件 | M030 の資料記載の切替条件、等 |
| `pins` | 使用パッド名(pinout との照合用) | `PD1/SWIO`、`DIO+DCK` |
| `pullup_note` | 外部 pull-up 等の配線要件があれば | 1線系の pull-up 慣行 |
| `#` `confidence` `basis` | provenance | datasheet / RM / WCH-Link manual V2.4 / 実測 |

## 対象範囲

catalog の全 12 family / 27 series。特に確度を上げてほしいもの:

- **両対応とされる系**: V00X / M007、M030(公式資料に 1線/2線両対応の記載)
- **gap 7 series**(V205/V407/V467/X305/X315/M030/M103): ch32rv が最初に書けるようにしたい集合
- H41x(dual core): core ごとの差があるか

## 取得方法の提案

- 一次資料: 各 series の datasheet / reference manual の debug 章、WCH-Link manual V2.4 の対応表。
- 実測での裏取り: WCH-LinkE は attach 時に応答が返るので「その配線で attach できた」ことを basis にできる(`curated/debug-data-measured.json` の流儀)。
- 参考(非一次): minichlink `chips.c` と probe-rs の family 定義が実装上どちらとして扱っているか。

## 受け入れ方法

依頼 0001 と同じ。ch32rv は暫定 overlay(core 名からの導出 + 例外の手書き)で進め、納品時に突き合わせて置き換える。

## 参照

- `../../note/research/programming-tools-and-probes.ja.md` §2.1(系列別 debug 配線の既存調査。この表の出発点にしてよい)
- WCH-Link manual V2.4
