# CH32V205 device ID の追加

- 依頼先: ch32-device-data
- 状態: draft (ローカルで準備済み、未送信)
- 優先度: 高
- 関連: [0001](0001-device-id.ja.md)、[実測と資料の照合](measured/device-id-v205-2026-10-08.md)

## 背景と必要な表

CH32V205実機からAttachChip family=`0xce`、chip_id=`0x20510510`を取得。
メモリ`0x1ffff704`のIDとも一致し、WCH EVTの表からCH32V205RCT6と解決できる。
現在 `index/parts.csv`は以下の4型番と容量を持つが、`index/device_ids.csv`にIDが無いため
ch32rvの生成SKU DBから欠落する。

`device_ids`の既存列へ次の行を追加してほしい。IDは比較用正規化値で、
`dont_care_bits=0xffffff0f`、`id_addr=0x1ffff704`。全行familyはCH32V205。

| part_number | device_id | id_source / basis |
|---|---|---|
| CH32V205CCT6 | `0x20520500` | WCH EVT |
| CH32V205RCT6 | `0x20510500` | WCH EVT + AttachChip実測`0x20510510` + memory実測 |
| CH32V205VCT6 | `0x20500500` | WCH EVT |
| CH32V203CCT6 | `0x20540500` | WCH EVT |

## 根拠と取得方法

WCH公式 [CH32V205EVT.ZIP](https://www.wch.cn/downloads/CH32V205EVT_ZIP.html) の
`EVT/EXAM/SRC/Peripheral/src/ch32v205_dbgmcu.c`、`DBGMCU_GetCHIPID`のID表を利用。
`inc/ch32v205.h`はIDの読み取り番地を定義する。
Flashは4型番とも262144 bytes、SRAMは32768 bytesで既存parts表と一致する。
RCT6のIDのみ実機確認済み。他パッケージの実機確認は未実施。
資料版、容量照合、実測コマンド、captureの場所と生応答は上記実測文書に記載。

## 受け入れ

ch32rvでは暫定 `crates/target/provisional/skus.csv`に4行を収載し、出力に暫定を明記。
納品時に `cargo xtask db-gen`でID・family・series・容量を照合し、一致した暫定行を削除する。
生成DBへ移した時もRCT6の `verified=true`を維持するため、xtaskのMEASURED表へ追加する。
`0xce`のfamily mappingはAttachChip実測としてch32rvに登録済み。
