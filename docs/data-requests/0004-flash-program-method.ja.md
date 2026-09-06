# 依頼 0004: main flash の消去/書き込み**手順**の family 別分類

- 状態: **draft**
- 依頼元: ch32rv
- 優先度: 中(対象 family の拡大に直結。実機を持たない family でも実装可否を判断できるようになる)
- 作成日: 2026-09-06

## 背景

ch32rv は flash を 2 経路で書く。1 つは WCH-Link の loader stub(chip 全体の書込専用)、もう 1 つが **halt した hart の program buffer から memory-mapped FLASH controller(`0x4002_2000`)を直接叩く page 単位の経路**で、こちらが `erase --range`(部分消去)・gdb の flash software breakpoint・option byte 書込の土台になっている。

この直接経路は **family ごとに手順が違う**。ch32rv は現在 3 方式を**コードに手書き**している。

| 方式 | 手順(消去済み page への書込) | 適用 family(ch32rv 実装) |
|---|---|---|
| PgStart | `CTLR=FTPG` → 4B ずつ書込(各 word 後 WRBUSY 待ち)→ `CTLR |= PG_STRT`(bit21)→ BUSY 待ち | V20x / V30x |
| Buffered | `CTLR=FTPG` → `BUFRST` → 各 word 書込 + `BUFLOAD` → `FLASH_ADDR=addr` → `STRT`(bit6)→ BUSY 待ち | V003 / CH641、X035 / CH643、L103 |
| 標準 half-word | fast buffer を使わず 16bit `sh` で書き、**各 erase/program 後に未文書の commit** が必須 | V103 |

消去は `FTER`(または `PAGE_ER`)+ `STRT` で全 family 共通なので、**手順を間違えても erase だけは動いてしまう**。

## なぜ要るか(この手書きが起こした実害)

- **X035 を PgStart 方式で実装したところ、program が完全に無反応だった**(消去後の `0xff` のまま。エラーも返らない)。erase は共通で動くので「消えているのに書けない」状態になり、切り分けに時間がかかった。実測で Buffered と判明して修正。
- **V103 は register 定義だけ見ると buffered が使えるように見える**(`FLASH_CTLR` に `BUF_LOAD` / `BUF_RST` がある)。しかし DMI 経由で fast BufLoad を使うと**内容が壊れた**(128bit = 4 word 単位のため)。実際に成立したのは標準 half-word 書込 + 未文書の commit だった。→ **「bit が定義されていること」と「その手順で書けること」は一致しない**。
- したがって欲しいのは bit の有無ではなく、**RM の編程手順が名指している制御 bit と順序**。これは R-30 で `option_bytes.csv` の `write_unit` 列(`half-word (OBPG)` / `fast page, 32-bit buffer writes (FTPG)`)としてすでにやってもらった分類と**同じ形**で、それの main flash 版にあたる。
- 対象 family を増やすたびに実機が要る状態を解消したい(V00X / V006 / M030 / V205 / V407 / X315 / H417 は手元に無い)。

## すでにある(重複依頼ではない部分)

こちらで確認済みなので、**下記は再作成不要**です。

| ある | 内容 |
|---|---|
| `evidence/flash_geometry.csv` | 粒度(`page_erase_bytes` / `fast_erase_bytes` / `fast_program_bytes` / `block_erase_bytes`)と `program_word`(driver に `FLASH_ProgramWord/HalfWord` があるか) |
| `evidence/register_fields.csv` | family ごとの `FLASH_CTLR` bit 定義(`FTPG`/`PAGE_PG`、`BUFLOAD`/`BUF_LOAD`、`PG_STRT`、`FTER`/`PAGE_ER` 等。**綴りが family で揺れている**ことも含めて) |
| `evidence/option_bytes.csv` | **option 側**の書込方式(`write_unit`)と family 別 base 番地 |

**足りないのは「main flash の手順の型」**です。粒度と bit 定義はあるのに、その bit を**どの順で叩くか**が無いため、consumer 側が実機で当てるしかありません。

## 依頼内容

提案: `evidence/flash_geometry.csv` に列を足す(family 粒度なので既存表と同じキー)。別表 `evidence/flash_program_method.csv` でも構いません。

| 列 | 内容 | 例 |
|---|---|---|
| `program_method` | 快速ページ書込の手順を、**RM が名指す制御 bit で**分類 | `fast page, 32-bit buffer writes (FTPG + BUFRST/BUFLOAD, then STRT)` / `fast page, direct writes (FTPG, then PG_STRT)` / `standard half-word (PG)` |
| `program_commit` | 書込の最後に何で起動するか | `STRT (bit6)` / `PG_STRT (bit21)` / `—` |
| `erase_method` | 快速ページ消去の手順(こちらは共通の見込みだが確認したい) | `FTER + STRT` / `PAGE_ER + STRT` |
| `ctlr_bit_names` | その family での綴り(consumer が register_fields と join するときの手掛かり) | `FTPG;BUFLOAD;BUFRST;FTER` / `PAGE_PG;BUF_LOAD;BUF_RST;PAGE_ER` |
| `undocumented_note` | **RM に無いが EVT driver にある必須手順**があれば | V103 の commit(下記) |
| `#` `confidence` `basis` | provenance | RM の闪存章(編程手順)ページ + EVT の flash driver |

- `write_unit` と同じ語彙で書いてもらえると、option 側と main flash 側を同じ規則で読めます。
- **`undocumented_note` が特に効きます**。V103 は EVT `ch32v10x_flash.c` に、各 erase/program の後で `*(uint32_t*)0x40022034 = *(uint32_t*)((addr & ~3) ^ 0x1000)` 相当の操作があり、**これが無いと erase も program も無反応**でした(実測)。RM には記述がありません。同種の副作用が他 family にもあるなら拾ってほしいです。

## 取得方法の提案

R-30 と同じ経路が使えるはずです。RM の**闪存章 → 快速ページ編程の手順**が、順番に制御 bit を名指しています(「置 FTPG 位」「置 BUFRST 位」「置 STRT 位」…)。zh 版を一次に、en は突合のみ(R-31 で英訳の揺れが確認されているため)。EVT の flash driver(`ch32*_flash.c` の `FLASH_ProgramPage_Fast` / `FLASH_ErasePage_Fast`)が実装側の裏取りになります。

## 確認してほしい齟齬 2 件

こちらの実機結果と、そちらの既存データが食い違っている箇所です。**どちらが正しいかはこちらでは決められません**。

1. **`CH32V307` の `FLASH_CTLR` に `PG_STRT`(bit21)が無い**(`CH32V20x` にはある)。V20x と V30x は同じ RM(`CH32FV2x_V3xRM`)のはずで、**ch32rv は V307 実機で bit21 を使って program に成功**しています。EVT header 由来の取りこぼしではないでしょうか。
2. **`CH32V103` の buffered 書込**。`register_fields` には `BUF_LOAD` / `BUF_RST` がありますが、DMI 経由の 4 word 単位書込では内容が壊れ、標準 half-word でしか成立しませんでした。RM が V103 の快速編程をどう書いているか(そもそも快速編程の節があるか)を確認してほしいです。

## 期待値表(受け入れ確認に使ってください)

ch32rv が実機で往復検証済みの 5 family です。RM 側の抽出結果がこれと一致すれば、そのまま受け入れられます。

| family | ch32rv の方式 | 粒度 | 実機検証 |
|---|---|---:|---|
| CH32V20x / V30x | PgStart(`FTPG` → word 書込 → `PG_STRT`) | 256 | CH32V203C8T6 / CH32V307VCT6 |
| CH32V003 / CH641 | Buffered(`FTPG` → `BUFRST` → word+`BUFLOAD` → `ADDR` → `STRT`) | 64 | CH32V003F4P6 |
| CH32X035 / CH643 | Buffered | 256 | CH32X035C8T6 |
| CH32L103 | Buffered | 256 | CH32L103C8T6 |
| CH32V103 | 標準 half-word(`PG`)+ 未文書 commit | 128(消去) | CH32V103R8T6 |

粒度の列はすでに `flash_geometry.csv` の `fast_erase_bytes` と一致しています(6 family で突き合わせ済み。ch32rv 側に乖離を検出する回帰テストがあります)。

## 対象範囲

catalog の全 12 family。特に埋めてほしいのは **ch32rv が実機を持たない側**: V00X / V006 / V205 / M030 / V407 / X315 / H417。この 7 つが埋まると、直接 FLASH controller 経路(部分消去・flash breakpoint・option 書込)をそのまま広げられます。

## 受け入れ方法

依頼 0001〜0003 と同じ。ch32rv は当面コードの手書き表で進め、納品後に `xtask db-gen` 経由の DB 由来に置き換えて、上記 5 family の実機結果と突き合わせます。差分が出たら ch32-device-data 側を正として調査します。

## 関連

- **R-31**(`wch-protocols` から依頼済): flash 消去後の読み出し値(系統 A `0xFFFFFFFF` / B `0xe339e339`)。本依頼と対になるデータで、ch32rv は実機読み 6 family を証拠として提供済み。
- 依頼 [0003](0003-option-byte-layout.ja.md)(R-30)で納品された `option_bytes.csv` の `write_unit` が、本依頼の書式の下敷き。
- 手順そのものの記述は `wch-protocols` の [pc-to-link.ja.md](../../../wch-protocols/protocols/pc-to-link.ja.md) §6 / §6b(protocol 側の一次ソース)。
