# 依頼 0003: option bytes の書き込みレイアウトと工場出荷値

- 状態: **納品受け入れ済(2026-09-02)**。`evidence/option_byte_fields.csv`(106行、12 family×byte×bit×default、RM 由来 confirmed)納品。`xtask db-gen` が `generated/option_fields.csv`(USER byte 43 fields)を生成、`target option get` の USER decode を family-aware 化(L103 の CFGCANM 等 RM 準拠、interim warning 解消)。構造化 `option set`(kv)は後続
- 依頼元: ch32rv
- 優先度: 中(M2。`target option get/set`(構造化 read-modify-write)と `recover unbrick`(工場値書き戻し)で必要)
- 作成日: 2026-09-01

## 背景

ch32rv は option bytes を「生 hex の read/write」だけでなく `target option set rdp=off nrst=gpio split=160/32` のような**構造化操作**として提供する。そのためには family ごとに次が必要になる。

`ch32-device-data` には既に (a) OB ブロックのベース番地(`evidence/register_blocks.csv`: 多くは `0x1FFFF800`、M030 のみ `0x1FFFF300`)、(b) `FLASH_OBR_*` 等の**読み出し側** bit 定義(`evidence/register_fields.csv`)、(c) FLASH/SRAM 分割の組合せ(`evidence/memory_configs.csv`)があるが、**OB 領域そのものの書き込みレイアウト**——各バイトの意味と補数バイトの配置——を手順として使える形の表が無い。

## 依頼内容

### 表 1: OB 領域のバイトレイアウト

提案: `evidence/option_byte_layout.csv`

| 列 | 内容 | 例 |
|---|---|---|
| `family`(または `series`) | 対象 | `CH32V203` |
| `byte_offset` | OB ベース番地からのオフセット | `0x00` |
| `name` | バイト名 | `RDPR` / `USER` / `DATA0` / `DATA1` / `WRPR0`.. |
| `complement_offset` | 補数バイトのオフセット(無ければ空) | `0x01`(nRDPR) |
| `bit` | USER 等のビット割り当て(1 bit 1 行でも、範囲表記でも) | `IWDG_SW [0]`、`STANDBY_RST [2]`、SRAM split `[7:5]` 等 |
| `write_unit` | 書込単位・制約(half-word 単位、補数同時書込、等) | |
| `#` `confidence` `basis` | provenance | RM の option bytes 章 |

家系差で特に確認したいもの: NRST/GPIO 切替ビットの位置(V003/V00X 系)、SRAM 分割ビット(V20x/V30x: `memory_configs.csv` の `option_byte_bits` と同じものを書き込み側視点で)、debug 無効化(二線 debug disable)に関与するビット、WRPR の粒度(何 sector/KB per bit か)。

### 表 2: 工場出荷値

提案: `evidence/option_byte_defaults.csv`

| 列 | 内容 |
|---|---|
| `family` | 対象 |
| `defaults` | OB 領域の工場出荷値(補数込みの生バイト列。V003 なら 16 byte) |
| `#` `confidence` `basis` | RM 記載値か、新品個体の実測ダンプか |

用途は `target option reset`(工場値へ戻す)と `recover unbrick`(minichlink の unbrick が V003 系で工場値 16 byte を書き戻す手順の一般化)。RM 記載値と新品実測が一致するかも確認できると良い。

## 対象範囲

全 family。優先は (1) V003/V00X/CH641(unbrick 頻度が高い 1線系)、(2) V20x/V30x(SRAM 分割)、(3) gap 7 series。

## 取得方法の提案

- 一次資料: 各 reference manual の FLASH / option bytes 章。`evidence/register_fields.csv` の `FLASH_OBR_*` との整合を取ると読み出し側と往復可能になる。
- 実測: 新品または unprotect 直後の個体から OB 領域を dump(WCH-LinkE で読める。basis は `curated/debug-data-measured.json` の流儀)。
- 参考(非一次): minichlink `minichlink.c` の unbrick が書く工場値、probe-rs の `ob_code_ram_splits`(`0x1ffff802` の `&0xe0` 参照)。

## 受け入れ方法

依頼 0001 と同じ(暫定 overlay → 納品時突き合わせ → 置き換え)。

## 参照

- `evidence/register_blocks.csv`(OB ベース番地)、`evidence/register_fields.csv`(OBR 読み出し側)、`evidence/memory_configs.csv`(分割組合せ)
- `../../note/research/new-programming-tool-design.ja.md` §4.2(option bytes 構造化 read/write の要件)
