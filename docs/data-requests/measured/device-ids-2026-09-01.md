# 実測 device_id(依頼 0001 向け, 2026-09-01)

- 測定: ch32rv `target info`(WCH-LinkE 経由の AttachChip 応答 = `attach` source)
- 用途: [依頼 0001](../0001-device-id.ja.md) の evidence 表への提供データ。`id_source=attach`
- 相互検証: 各 UID と part 名は `/run/board-identify/by-id/`([[board-identify]] の独立読取)と一致

`chip_id` は AttachChip 応答の 32bit 値。silicon revision([7:4])は照合時 don't-care。

| part(by-id) | family byte | chip_id (attach) | UID | flash_kb(probe) | probe serial |
|---|---|---|---|---|---|
| CH32V003F4P6 | 0x09 | `0x00300500` | f9e1abcd6201bc53 | 16 | F90E8F067DFD (LinkE) |
| CH32V103R8T6 | 0x01 | `0x2500410f` | d39eabcd3bc4bc59 | 64 | 434A124C5596 (CH549) |
| CH32V203C8T6 | 0x05 | `0x20310500` | b661abcd1e91bc63 | 64 | FBC18F0680B0 (LinkE) |
| CH32V307VCT6 | 0x06 | `0x30700528` | 7bbed00a9c1c5054 | 288 | 38EF8F06BDC2 (LinkE) |
| CH32L103C8T6 | 0x0E | `0x10310710` | 3a6dabcda282bc48 | 64 | 0E028F0692F1 (LinkE) |
| CH32X035C8T6 | 0x0D | `0x03510601` | 1ff9abcd880ebc48 | 62 | FC928F068181 (LinkE) |

## 照合メモ(既存の chip_id 表との一致)

wlink `chips.rs` / board-identify `wch_chips.py`(いずれも probe-rs 由来)のマスク付き表と照合:

- `0x00300500` → CH32V003F4P6(完全一致)
- `0x2500410f` → mask 下位 16bit `0x410f` = CH32F103R8T6 相当だが family 0x01=CH32V103、part は CH32V103R8T6。**V103 は低 16bit が STM32 互換 IDCODE 形式**(依頼 0001 の note 事項の実例)
- `0x20310500` → CH32V203C8T6(完全一致)
- `0x30700528` → mask `0xffffff0f` で `0x30700508` = CH32V307(系列一致。VCT6 の package 差)
- `0x10310710` → mask `0xffffff0f` で `0x10310700` = CH32L103C8T6(完全一致、[7:4] rev=1)
- `0x03510601` → CH32X035C8T6(完全一致)

## 未取得(依頼 0001 の gap 7 series)

手元に無い: V205 / V407 / V467 / X305 / X315 / M030 / M103。接続でき次第、同じ手順(`ch32rv target info --json`)で追記する。

## attach 値と memory 値の同一性(2026-09-01 実測、6/6 一致)

依頼 0001 が確認を求めていた「AttachChip 応答値と memory 番地(`0x1FFFF7xx`)読みの device_id が一致するか」を `ch32rv read --range <addr>+4` で全 6 ボード実測した。**全て完全一致**。memory 読みは little-endian バイト列なので u32 化して比較する。

読み出し番地(ch32-data `docs/device-ids.md` 由来、実測で確認): V003/CH641 = `0x1FFFF7C4`、V103 = `0x1FFFF884`、V20x/V30x/L103/X035 ほか = `0x1FFFF704`。

| part | mem bytes(LE) | mem u32 | attach u32 | 一致 |
|---|---|---|---|---|
| CH32V003F4P6 | `00 05 30 00` | `0x00300500` | `0x00300500` | ✓ |
| CH32V103R8T6 | `0f 41 00 25` | `0x2500410f` | `0x2500410f` | ✓ |
| CH32V203C8T6 | `00 05 31 20` | `0x20310500` | `0x20310500` | ✓ |
| CH32V307VCT6 | `28 05 70 30` | `0x30700528` | `0x30700528` | ✓ |
| CH32L103C8T6 | `10 07 31 10` | `0x10310710` | `0x10310710` | ✓ |
| CH32X035C8T6 | `01 06 51 03` | `0x03510601` | `0x03510601` | ✓ |

**結論**: この 6 family では AttachChip 応答値 = memory 読み値。target DB はどちらを source にしても同じ値になる(依頼 0001 の `id_source=attach` と `id_source=memory` は一致)。gap 7 series での同一性は未確認(実機が無いため)。
