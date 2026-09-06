# 依頼 0005: WCH-Link flash stub(wlink 由来 5 本)を stub 目録に載せてほしい

- 状態: **納品受け入れ・消費済(2026-09-06)**。`stubs.csv` 5 行 + `stubs_hex/` + `stub_disasm/` + `wlink_stub_comparison.csv` + `stub_args.csv`(ABI 8 行)納品。size / fnv1a64 は依頼表と全一致。**§Q3 の指摘を受けて `CH32L103` blob を削除**(`CH643` と同一)。逆質問 3 件への回答は末尾
- 依頼元: ch32rv
- **依頼先**: `wch-protocols`(`references/data/bootloader-survey/`)
- 優先度: 低(急がない。stub を source から build するサブプロジェクトに着手するときの土台)
- 作成日: 2026-09-06

## 背景

WCH-Link の stub 書込経路(probe に loader を送り込んで chip 全体を書く経路)では、**target RAM で走る RISC-V 機械語の blob** を probe へ流す。ch32rv はこれを 5 本持っていて、出所は **wlink `src/flash_op.rs`**(MIT OR Apache-2.0。元は WCH EVT の flash ルーチンとされる)からの**逐語転記**です。

この転記は暫定措置で、ch32rv の方針([architecture.ja.md §3](../architecture.ja.md))は「**stub は in-repo の source から build し、hash を出して再現性を担保する**」です。着手する前に、**同じ blob が他の host tool にもあるのか、どこが family 固有でどこが共通なのか**を知りたい。

bootloader survey の `stubs.csv` は既に minichlink 側(`b003` 系 47 本 + LinkE 用 loader 4 本)を目録化していますが、**wlink 側は入っていません**。同じ「host tool が target へ送り込む blob」なので、同じ表で並ぶのが自然だと思います。

## 依頼内容

`stubs.csv` に wlink 系の行を追加してほしい(`host_tool` = `wlink/flash_op` 等、命名はそちらの流儀で)。既存行と同じく `stubs_hex/` と `stub_disasm/` も揃うと、比較が一気に楽になります。

**blob そのものはこちらが持っています**。下表の 5 本で、`crates/flash/src/stub.rs` に byte 配列として入っています(逐語転記なので wlink upstream と同一のはず)。取りに行く手間は不要です。

| ch32rv での名前 | サイズ | 先頭 4 byte | fnv1a64 | 使う family(AttachChip family byte) |
|---|---:|---|---|---|
| `CH32V307` | 446 | `01 11 02 ce` | `8442a2fdf21f3a2a` | V20x / V30x(`0x05` / `0x06`) |
| `CH32V103` | 494 | `01 11 02 ce` | `742030ff5a123b1b` | V103(`0x01`) |
| `CH32V003` | 498 | **`11 11 22 cc`** | `59bd20460d05dc7b` | V003 / CH641(`0x09` / `0x49`) |
| `CH643` | 488 | `01 11 02 ce` | `e83c3783efcabbc6` | X035 / CH643(`0x0d` / `0x0c`) |
| `CH32L103` | 512 | `01 11 02 ce` | `fae70418d222d66e` | L103(`0x0e`) |

fnv1a64 は ch32rv が `version --json` の再現性表示に使っている指紋(暗号学的ではない同一性の印)です。そちらで別の hash を使うならそれで構いません。

## 特に知りたいこと(この依頼の実質)

行が増えること自体より、**突き合わせの結果**が欲しいです。

1. **`CH32L103` の 512 byte は、minichlink の `linke-flashloader-v1` / `v2`(ともに 512 byte)と同一 blob か**。サイズが一致しているので最初に潰したい点です。同一なら「LinkE 用 loader は 1 系統」と言え、source 化の対象が絞れます。
2. **`CH32V003` だけ先頭が違う**(`11 11 22 cc` / 他 4 本は `01 11 02 ce`)。V003 は RV32EC(x0-x15 のみ)なので**別ビルド**と見ていますが、逆アセンブルで裏を取ってほしい。`stubs.csv` には既に `rv32ec_safe` 列があるので、そこに乗るはずです。
3. **5 本の相互差分**(446 / 488 / 494 / 498 / 512 byte)は family 固有部分だけか、それとも世代違いか。`equiv_group` を振ってもらえると、「1 本を source 化すれば何本ぶんカバーできるか」が分かります。
4. **stub の ABI**(引数の渡し方・エントリ番地・完了の通知方法)。probe が data EP へ blob を流して走らせる契約が読めれば、自前ビルドの stub が同じ契約を満たせるか判断できます。既存の `stub_args.csv` / `stub_framing.csv` の流儀で結構です。

## こちらで確認済みの周辺事実(参考。依頼ではありません)

- **stub 経路は部分書き込みを受け付けない**。chip erase 無しに mid-flash の 1 page を書くと probe が `81 55 01 02`(reason `0x55`)で拒否する。stub 経路は full-region programming 専用で、任意 page は DMI から FLASH controller を直接叩く別経路を使う(ch32rv 実装)。
- family ごとの転送パラメータ(実機確認): V003/CH641 = data packet 64 / write pack 1024、V103 = 128 / 4096、V20x/V30x・X035/CH643・L103 = 256 / 4096。
- 5 本とも **CH32V203 / V103 / V003 / V307 / L103 / X035 の実機で書込 → readback バイト一致**まで確認済み(つまり blob は「動く状態」で転記できている)。

## ライセンスの注意

wlink は MIT OR Apache-2.0 で、blob 自体は WCH EVT の flash ルーチン由来とされています。目録に載せる際は `basis` にこの出所を残してください(`oss:ch32-rs/wlink/src/flash_op.rs`)。ch32rv 側も同じ出所を `stub.rs` の冒頭コメントに書いています。

## 受け入れ方法

ch32rv 側は現状の転記を使い続けるので、**納品を待つ必要はありません**。目録が載ったら、こちらの `stub_digest` と突き合わせて同一性を確認し、`equiv_group` の結果を stub source 化の設計に使います。

## 関連

- ch32rv の blob: `crates/flash/src/stub.rs`(逐語転記であることと将来の方針をファイル冒頭に明記)
- stub 経路の protocol: [../protocol/wch-link.ja.md](../protocol/wch-link.ja.md) §4.2
- 既存の目録: `wch-protocols` `references/data/bootloader-survey/stubs.csv`(minichlink 側 51 行)

## 納品の受け入れ(2026-09-06)

サイズと fnv1a64 は 5 本とも一致。回答のうち **ch32rv 側の実装に効いたのは Q3** でした。

- **`CH643` と `CH32L103` は同一 blob**(先頭 488 byte 一致、差は `0xff`×24 の padding)→ **`stub::CH32L103` を削除**し、family byte `0x0e` は `stub::CH643` を使う。`flash_stub_digest` は `fnv1a64:653eb364100a2911` に変わった
- `rv32ec_safe`: V003 版のみ x0–x15、他 4 本は x28〜x30 を使う → **他 4 本は V003 で走らない**という裏付けが取れた
- `equiv_group` 4 つ = **4 本書けば 9 family byte 全部**。source 化のスコープが確定した
- 共通接頭辞は 42 B / 79 B、接尾辞はほぼ 0 → **共通化できるのは前半(preamble)だけ**

## 逆質問への回答

### 1. ch32rv は `a0` に何を積んでいるか → **積んでいません**

host は stub を **data EP に流すだけ**で、実行も引数の設定も probe firmware 側です。host が渡すのはこれだけ:

| host が送るもの | 内容 |
|---|---|
| `0x01` SetWriteMemoryRegion | `addr_be32 len_be32`(書込先と長さ) |
| `0x02 0x05` WriteFlashOP | 直後に data EP へ stub blob |
| `0x02 0x07` | 確認(応答 payload[0]=`0x07`) |
| `0x02 0x02` WriteFlash | 以降 data EP へ本体データ、chunk ごとに 4 byte ack |
| `0x02 0x08` | End |

**`a0` は USB 上に現れません**。したがって `--capture` を何回取っても bit2/bit3 は埋まりません(capture は host↔probe の往復だけで、probe→target の呼び出しは映らない)。

**確実な経路は probe firmware の逆アセンブル**だと思います。`WCH-LinkE` の application image(`Firmware_Link/FIRMWARE_CH32V305.bin`、109,544 byte)は `probe firmware update` の検証で往復させたものが手元にあり、**stub を呼ぶ側のコードはこの中**にあります。RISC-V の生バイナリなので、そちらの `stub_disasm` と同じ手順で読めるはずです。必要なら image の入手元と SHA-256 を出します。

ヒントとして、host 側 API の形と `a0` のビット割当が対応していそうです — bit0 = unlock は `0x02 0x05`(WriteFlashOP)の前処理、bit1 = 消去は `0x02 0x01`(EraseFlash)に対応する、という読みが自然です。

### 2. `CH643` と `CH32L103` の統合 → **統合しました**

判断材料が 1 つ増えました。**転送時に最終 packet を `0xff` で `data_packet_size` まで埋める**実装なので(`write_data_padded`、両 family とも 256 byte)、

```
CH643    488 B → 256 + (232 + 0xff×24) = 512 B が USB 上に出る
CH32L103 512 B → 256 + 256              = 512 B
```

**USB 上のバイト列まで完全に同一**でした。動作に一切差が無いので削除しています。

### 3. wlink 系 / minichlink 系のどちらが WCH 純正に近いか → **未決。ただし材料を 1 つ**

wlink 系 5 本は **実機 6 台(V003 / V103 / V203 / V307 / L103 / X035)で書込 → readback バイト一致**まで確認済みです。「動く」ことは分かっていますが、それは純正かどうかの証拠にはなりません。

**これも probe firmware image を読むのが早い**と思います。LinkE 自身が内蔵している loader があるなら、それが純正です(逆に stub を毎回 host から流し込む設計なら、そもそも純正 loader という概念が無い)。§1 と同じ image で両方確認できます。
