# 実測 消去後の flash 読み出し値(R-31 向け, 2026-09-06)

- 測定: ch32rv(WCH-Link / WCH-LinkE 経由の debug read)。消去直後の flash を読んだ生の値
- 用途: `ch32-device-data` `evidence/flash_geometry.csv` の `erased_read_*` 列(R-31)への提供データ
- 相互検証: 系統 B の値は wlink の独立 dump と一致

RM は CH32 を 2 系統に書き分ける — 系統 A = `0xFFFFFFFF`、系統 B = `0xe339e339`。**CH32V003 と CH32V103 は RM に記述が無い**ため、この実測が唯一の一次情報になる。

| part | family byte | 読み値 | 系統 | 根拠(ch32rv 側の観測) |
|---|---|---|:-:|---|
| CH32V003F4P6 | 0x09 | `0xff` fill | A | page erase → read が全 `0xff` |
| CH32V103R8T6 | 0x01 | `0xff` fill | A | erase → read `0xff` → program → erase の往復 |
| CH32X035C8T6 | 0x0D | `0xff` fill | A | PgStart 方式で program が効かなかったとき、消去後の `0xff` のままだった |
| CH32V203C8T6 | 0x05 | `39 e3 39 e3` | B | byte 列。word にすると `0xe339e339` |
| CH32V307VCT6 | 0x06 | `39 e3 39 e3` | B | power-off erase 後。**wlink dump も同値** |
| CH32L103C8T6 | 0x0E | (A として動作) | A | page 単位の read-modify-write(`--restore-unwritten`)が成立。**読み値そのものは未記録** |

- 系統 B の byte 並び(`39 e3 39 e3`)は、RM の「偶アドレス `0x39` / 奇アドレス `0xe3`」と一致する。
- 系統 A は全バイトが `0xff` なので、word 読みは `0xFFFFFFFF`。RM が `字读- 0xFF` と 8bit 幅で書いているのは word 値の意味。
- **L103 だけは間接**(read-modify-write が成立したことから A と判断)。直接の読み値は記録していない。

## 納品との対応(2026-09-06 時点)

`evidence/flash_geometry.csv` は RM 原文の 4 列 + EVT IAP の判定値 `blank_check_word` で納品された。**CH32V103 だけは RM も EVT IAP も値を持たない**(V103 の IAP は blank 判定をしない)ため、当初は空欄だった。

**この実測が DB の basis に採用された**(2026-09-06): `CH32V103` の `blank_check_word=0xFFFFFFFF` は、この文書を third-party measurement として引用した行になっている(RM の `FLASH_STATR.PGERR` = 「非 `0xFFFF` の番地へ書くと置位」が間接的な裏付け、との注記つき)。ch32rv 側の暫定値は削除し、生成 DB から引いている。

**L103 の直接測定は未取得**のまま(A 系として動作することしか記録が無い)。実機はあるので、必要になれば消去 → read で確定できる。
