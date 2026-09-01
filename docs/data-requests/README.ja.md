# ch32-device-data へのデータ作成依頼

- 運用開始: 2026-09-01
- 方針の根拠: [architecture.ja.md §3](../architecture.ja.md)(データ調達の原則)

## 位置づけ

ch32rv が必要とする device データは ch32rv 内部で作らず、`ch32-device-data` リポジトリへの CSV 追加を依頼する。このディレクトリの各ファイルが**依頼書そのもの**であり、ファイル単位で ch32-device-data 側へ渡す。

## 運用ルール

1. 1 依頼 1 ファイル。連番 + 内容のスラッグで命名する(`0001-device-id.ja.md` 等)。
2. 依頼書は**受け手が ch32rv の文書を読まなくても作業できる**self-contained な内容にする: 背景、欲しい表の形(列・形式)、対象範囲、取得方法の提案、受け入れ方法、優先度。
3. 表の形式は ch32-device-data の流儀(CSV、`#` 列の右に `confidence` / `basis` の provenance)に合わせた**提案**であり、最終的な表名・列名・置き場所(evidence/index)の決定は ch32-device-data 側に委ねる。
4. 各依頼書の冒頭に状態を持つ: `draft` → `依頼済` → `納品` → `受け入れ済`。
5. 納品まで ch32rv は `ch32rv-target/provisional/` の暫定 overlay で開発を進め、納品時に突き合わせて受け入れる。差分が出たら ch32-device-data 側を正として調査する。受け入れ後、暫定側は削除する。

## 依頼一覧

| # | 依頼 | 状態 | 優先度 |
|---|---|---|---|
| [0001](0001-device-id.ja.md) | chip ID(device_id)の evidence 表新設 | draft | **高**(M2 の target 自動検出のブロッカ) |
| [0002](0002-debug-interface.ja.md) | debug interface 種別(1線/2線)の明示列 | draft | 中(M1-M2。当面は core 名からの導出で代替可) |
| [0003](0003-option-byte-layout.ja.md) | option bytes の書き込みレイアウトと工場出荷値 | draft | 中(M2 の `target option` / `recover unbrick` で必要) |

将来の依頼候補(まだ依頼書にしない): WCH-Link firmware の hash→版対応の継続拡充(既存 `evidence/link_firmware.csv` の新版追従)、UF2 family ID / DFU VID:PID 等の bootloader 識別子表(P2 の `boot` 実装時)。
