# 実測 option bytes(依頼 0003 / R-30 の残件, 2026-09-06)

- 測定: ch32rv `target option get`(DMI 経由で family の option base から 16 byte 読み)
- 用途: `ch32-device-data` R-30 が待っている「実測ダンプとの照合」。RM の復位値(`evidence/option_byte_fields.csv`)と突き合わせる
- 対象: ベンチ 5 台。**いずれも工場出荷のまま**(本 project は option byte を書き換える試験を `write-raw` の round-trip 以外では行っていない)
- **CH32X035 は今回未接続**(6 台目。接続でき次第追記)

## 生データ

| part | family | raw 16 byte | USER | nUSER |
|---|---|---|---|---|
| CH32V003F4P6 | CH32V003 | `a55af708ff00ff00ff00ff00ff00ff00` | **`0xf7`** | `0x08` |
| CH32V103R8T6 | CH32V103 | `a55aff00ffffffffffffffffffffffff` | `0xff` | `0x00` |
| CH32V203C8T6 | CH32V20x | `a55a3fc0ff00ff00ff00ff00ff00ff00` | **`0x3f`** | `0xc0` |
| CH32V307VCT6 | CH32V307 | `a55abf40ff00ff00ff00ff00ff00ff00` | **`0xbf`** | `0x40` |
| CH32L103C8T6 | CH32L103 | `a55aff00ff00ff00ff00ff00ff00ff00` | `0xff` | `0x00` |

RDPR は 5 台とも `0xa5`(読み出し保護 off)、補数 `0x5a`。USER の補数も 5 台とも `0xFF ^ USER` と一致。

## RM の復位値との突き合わせ

`evidence/option_byte_fields.csv` の USER byte と照合しました。

| family | RM の復位値 | 実測 USER | 一致 |
|---|---|---|:-:|
| CH32V003 | `[7:6]`=`11b` / `START_MODE`(5)=1 / **`RST_MODE`(`[4:3]`)=`10b`** / `STANDYRST`=1 / `[1]`=1 / `IWDGSW`=1 → `0xf7` | `0xf7` | ✓ |
| CH32V20x | **`RAM_CODE_MOD`(`[7:5]`)=`x`**(RM が値を書かない)/ `[4:3]`=`11b` / 下位 3 bit=1 → `0x?f` | `0x3f`(`[7:5]`=`001`) | ✓(不定部を除き) |
| CH32V307 | 同上 | `0xbf`(`[7:5]`=`101`) | ✓(同) |
| CH32V103 | — | `0xff` | — |
| CH32L103 | — | `0xff` | — |

**RM が `x`(不定)としている `RAM_CODE_MOD` は、実際に部品で違いました**(V203=`001` / V307=`101`)。SRAM/flash 分割の設定なので、**個体ごとの出荷値であって「共通の工場出荷値」は存在しない**と読むべきです。R-30 が「生 16 byte 列の合成は導出なのでしない」と判断したのは、この意味で正しかったことになります。

## CH32V103 の特異点 — Data/WRPR に補数が無い

V103 だけ後半が `ffffffff…` で、他 family の `ff00` 繰り返しと違います。

```
V103 : a55a ff00 ffff ffff ffff ffff ffff ffff
他   : a55a ff00 ff00 ff00 ff00 ff00 ff00 ff00
```

つまり **V103 は出荷状態で Data0/Data1/WRPR0-3 の補数バイトを持たない**(未使用で `0xff` のまま)。`option_bytes.csv` の `complement_address` 列が V103 でも補数を予定しているなら、出荷状態とは食い違います。**確認してほしい点**です。

> **注**: 上表は**出荷状態のダンプ**です。その後の検証で `option reset` を通したため、この個体の補数バイトは現在 `ff00…` になっています(値バイトは不変。どちらも `0xff` なので意味は変わらない)。**書き込み可能**であることは分かりました。

## この測定で見つかった ch32rv 側のバグ

`recover --method unprotect` と `target option reset` は「工場出荷値」として **全 family 共通で `USER=0xFF`** を書きます(`cli/src/cmd_flash.rs` の `FACTORY` 定数)。しかし実測のとおり:

- **CH32V003**: 出荷値は `0xf7`。`0xFF` を書くと **`RST_MODE` が `10b` → `11b`** に変わる(NRST ピンの機能設定)
- **CH32V20x / V307**: 出荷値は `0x3f` / `0xbf`。`0xFF` を書くと **`RAM_CODE_MOD` が `111b`** になり、**SRAM/flash 分割が変わる**

読み出し保護を解除するだけのつもりの操作が、NRST の機能や SRAM 分割を書き換えてしまいます。

**実機で検証済み(2026-09-06)**: ベンチ 4 台(V003 `0xf7` / V20x `0x3f` / V307 `0xbf` / L103 `0xff`)で `target option reset --yes` を実行し、**USER が 1 bit も変わらないこと**と flash 無傷を確認。V203 では `recover --method unprotect --yes` も通し、USER=`0x3f` 保持を確認した。

**修正済み(2026-09-06)**: `recover` の 2 経路は**現在値を読んで RDPR だけ差し替える**方式にした(読めない/壊れている場合のみ一律 image + 警告)。`target option reset` は **DB が定義する bit を RM の復位値へ戻し、DB が知らない bit(`RST_MODE` / `RAM_CODE_MOD`)は現在値を保つ**。上表の実測値がそのまま単体テストの固定値になっている。

## もう 1 つ見つかったバグ — option 書込の verify が偽の失敗を出していた

CH32V103(CH549 Link 経由)で `target option set STOPRST=0` が **`verify-mismatch`(exit 30)で失敗を報告**しました。ところが読み直すと **書き込みは成功**しています。

```
$ ch32rv target option set STOPRST=0 --yes
error[verify-mismatch]: option byte 2 reads back 0xff, not the requested 0xfd
$ ch32rv target option get
raw: a55afd02...   ← 0xfd。書けている
```

**原因**: V103 は option 領域を **reset するまで書込前の像で読み続ける**(reset 時に再ロードされる shadow を読んでいると思われる)。50ms×4 の再読み込みでは足りず、**新しい session(= attach による reset)で初めて新しい値が見えました**。

**修正**: 検証を**書込直後ではなく soft reset の後**に行うようにしました。option bytes はどのみち reset で反映されるので、意味的にもこちらが正しい。修正後は V103 で `STOPRST=0` → `=1` の往復がどちらも exit 0、表示値も実際の値と一致します。L103(LinkE)でも往復を確認し、副作用がないことを見ています。

**他 family でも同じか**は未確認です(V103 以外は USER を変える書込を通していないため)。reset 後に読む方式なら family に関係なく正しいので、実害はありません。
