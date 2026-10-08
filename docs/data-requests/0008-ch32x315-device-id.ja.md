# CH32X315 device ID の追加

- 依頼先: ch32-device-data
- 状態: 納品・受け入れ済み (2026-10-08、ch32-device-data@773f929)
- 優先度: 高
- 関連: [0001](0001-device-id.ja.md)、[実測資料](measured/device-id-x315-2026-10-08.md)

## 欲しいデータ

既存 `index/parts.csv` にCH32X315/X305の型番と容量はあるが、`index/device_ids.csv`にはIDが無い。
WCH公式 [CH32X315EVT.ZIP](https://www.wch.cn/downloads/CH32X315EVT_ZIP.html) の
`EVT/EXAM/SRC/Peripheral/src/ch32x3x5_dbgmcu.c`、`DBGMCU_GetCHIPID`の表を収載してほしい。
全行familyはCH32X315、`id_addr=0x1ffff704`、`dont_care_bits=0xffffff0f`。

| 型番 | 正規化device_id | 根拠 |
|---|---|---|
| CH32X315MCU6 | `0x31500000` | EVT + AttachChip + memory実測 |
| CH32X315CCU6 | `0x31510001` | EVT |
| CH32X315WCU6 | `0x31520002` | EVT |
| CH32X305RCT6 | `0x31530003` | EVT |

実測はLinkE 2.22、serial=`49878F06CE37`、family byte=`0xe6`。
AttachChip/ChipInfo echoとメモリ`0x1ffff704`は`0x31500000`で一致。
MCU6のみ実機確認済み。他パッケージは資料のみ。
再現コマンド・生応答・capture・資料版とページは上記実測資料に記載。

## 容量の注意と受け入れ

MCU6実機のFLACAP/ChipInfo容量欄は消去済みパターン`0xe339`だった。
UIDとIDは取得できるので、全応答を壊れた値として捨てない。
容量は既存parts表のゼロウェイト領域196608 bytes、SRAM65536 bytesを使用する。
総Flashは480 KiB (192 + 288)であり、parts表の容量とは区別する。

ch32rvの暫定overlayに4行を登録済み。納品時に生成DBとID/family/series/容量を照合し、
一致した暫定行を削除する。MCU6のID実機確認はxtaskのMEASURED表で維持する。
family名`0xe6`は実測根拠でch32rvに登録済み。Flash書き込みは未検証。

## 2026-10-08 ローカル受け入れ

公式EVTからIDを抽出し、`index/device_ids.csv`へ収載。ID・番地・family・series・容量を
暫定行と照合して一致を確認し、生成SKU DBへ移行した。暫定8行は削除。
実測2型番の`verified=true`はMEASURED表へ移して維持した。書き込み検証は未実施。
