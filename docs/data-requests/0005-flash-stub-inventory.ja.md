# 依頼 0005: WCH-Link flash stub(wlink 由来 5 本)を stub 目録に載せてほしい

- 状態: **draft**
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
