# 外部リポジトリへのデータ・調査依頼

- 運用開始: 2026-09-01
- 方針の根拠: [architecture.ja.md §3](../architecture.ja.md)(データ調達の原則)

## 位置づけ

ch32rv が必要とするデータは ch32rv 内部で作らず、**資料の持ち主のリポジトリへ依頼**する。このディレクトリの各ファイルが**依頼書そのもの**で、ファイル単位で相手へ渡す。

| 依頼先 | 扱う資料 |
|---|---|
| `ch32-device-data` | device データ(chip ID・geometry・option byte・debug 配線など、RM / datasheet / EVT 由来の表) |
| `wch-protocols` | protocol と実装横断の調査資料(bootloader survey、stub の目録、capture) |

**依頼書はこの repo に置く**。依頼先のリポジトリには、先方から依頼が無い限り書き込まない。

## 運用ルール

1. 1 依頼 1 ファイル。連番 + 内容のスラッグで命名する(`0001-device-id.ja.md` 等)。冒頭に**依頼先**を書く。
2. 依頼書は**受け手が ch32rv の文書を読まなくても作業できる**self-contained な内容にする: 背景、欲しい表の形(列・形式)、対象範囲、取得方法の提案、受け入れ方法、優先度。
3. 表の形式は依頼先の流儀(CSV、`#` 列の右に `confidence` / `basis` の provenance)に合わせた**提案**であり、最終的な表名・列名・置き場所の決定は依頼先に委ねる。採番(`R-xx` / `U-xx`)も依頼先の台帳に従う。
4. 各依頼書の冒頭に状態を持つ: `draft` → `依頼済` → `納品` → `受け入れ済`。
5. 納品まで ch32rv は暫定値で開発を進めてよいが、**暫定は必ず出所付きで隔離**し(生成物の `*` 印や `xtask` の `PROVISIONAL_*` 定数)、納品時に突き合わせて削除する。差分が出たら依頼先を正として調査する。

## 依頼一覧

| # | 依頼先 | 依頼 | 状態 | 優先度 |
|---|---|---|---|---|
| [0001](0001-device-id.ja.md) | ch32-device-data | chip ID(device_id)の evidence 表新設 | 依頼済 | **高**(M2 の target 自動検出のブロッカ) |
| [0002](0002-debug-interface.ja.md) | ch32-device-data | debug interface 種別(1線/2線)の明示列 | 依頼済 | 中(M1-M2。当面は core 名からの導出で代替可) |
| [0003](0003-option-byte-layout.ja.md) | ch32-device-data | option bytes の書き込みレイアウトと工場出荷値 | 依頼済 | 中(M2 の `target option` / `recover unbrick` で必要) |
| [0004](0004-flash-program-method.ja.md) | ch32-device-data | main flash の消去/書き込み**手順**の family 別分類 | **納品受け入れ・消費済** | 中 |
| — | ch32-device-data | **flash 消去後の読み出し値**(系統 A `0xFFFFFFFF` / B `0xe339e339`) | `wch-protocols` から依頼(R-31)→ **納品受け入れ・消費済**(V103 の値は[こちらの実測](measured/erased-read-2026-09-06.md)が DB の basis に採用された) | 中 |
| [0005](0005-flash-stub-inventory.ja.md) | wch-protocols | WCH-Link flash stub(wlink 由来 5 本)の目録化 | **納品受け入れ・消費済**(重複 blob 1 本を削除。逆質問 3 件に回答済) | 低 |

将来の依頼候補(まだ依頼書にしない): WCH-Link firmware の hash→版対応の継続拡充(既存 `evidence/link_firmware.csv` の新版追従)、UF2 family ID / DFU VID:PID 等の bootloader 識別子表(P2 の `boot` 実装時)。
