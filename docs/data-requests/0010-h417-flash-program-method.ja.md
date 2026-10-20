# CH32H417 の flash の書き方: conflict の解消と、DBMODE で変わる消去単位

- 依頼先: ch32-device-data
- 状態: 依頼中 (2026-10-20)
- 優先度: 中(ch32rv は暫定の例外で書けている)
- 関連: [0009](0009-h417-core-selection.ja.md)、[CH32H417 識別・データ取得](measured/device-id-h417-2026-10-10.md)

## 1. `flash_program_method` の CH32H417 行の conflict

`index/flash_program_method.csv` の CH32H417 行は `direct,PG_STRT,,conflict` で、`index/conflicts.csv` 10 行目は、RM の「fast page (FTPG, then STRT)」を異議として記録している。
ch32rv はこれを、RM の手順の文の誤りと見ている。

- RM(CH32H417RM.PDF V1.8)46.4 の R32_FLASH_CTLR の表は、bit16 FTPG、bit21 PGSTRT(start)、bit1 PER。bit17 は Reserved で、FTER は無い。
- EVT `ch32h417_flash.c` の `FLASH_ROM_WRITE` は、`CR_PAGE_PG` (0x00010000 = bit16) を立て、64 語を WRBSY を待ちながら書き、`CR_PG_STRT` (0x00200000 = bit21) で確定する。`ROM_ERASE` は `CR_PER_Set` + ADDR + `CR_STRT_Set`。
- RM 46.5.6 の手順 8「Set the FTPG bit ... to start fast page programming」と、46.5.7 の「FTER」は、表と食い違う。他品種の手順の写しが残ったものと見られる。

表と EVT はビット単位で一致するので、行を `direct` / `PG_STRT` のまま conflict から外せないかを確かめてほしい。

## 2. 消去単位が DBMODE で変わる

`index/flash_geometry.csv` の CH32H417 行は、page_erase 4096。
RM 46.4 の PER の説明は「DBMODE=1 なら sector (8KB) を消す、DBMODE=0 なら sector (4KB)」で、BER も DBMODE で 64 KiB / 32 KiB が変わる。
DBMODE は R32_FLASH_CFGR0 (0x4002202C) の bit28(RO)。EVT `FLASH_ROM_ERASE` のコメントも、Dual なら 8 KiB 境界、single なら 4 KiB 境界。

実測(2026-10-20、WCH-LinkE 2.22、serial `49808F06CE30`、CH32H417QEU6、chip id 0x4170053d): FLASH_CFGR0 = 0x96070200(DBMODE = 1)。
4 KiB のつもりで PER を掛けると、0x08000000 から 8 KiB が消えた(0x08001000 以降も 0xe339e339 になった)。

DBMODE ごとの消去単位(と BER の大きさ)を、表で持てる形にしてほしい。今は 4096 の 1 つしか無い。

## ch32rv 側の今の扱い

- `crates/flash/src/loader.rs` の `plan_for_family` は、CH32H417 に限り conflict を例外として通し、X315 と同じ手順(PER の消去、256 バイトずつ PGSTRT で確定)で書く。
- 消去単位は書く前に FLASH_CFGR0 を読み、DBMODE=1 なら 2 倍(8 KiB)にする。
- ローダーは、H417 では 0x20000000 に RAM が無い(RM 表 4-2)ので、共有 SRAM の 0x20120000 に置く。
- 1 と 2 が納品されたら、例外と DBMODE の読み取りを、表の値に置き換える。
