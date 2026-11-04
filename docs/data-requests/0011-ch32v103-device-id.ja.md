# 依頼 0011: CH32V103 の chip ID は C8T6 と R8T6 を分けない

- **依頼先**: ch32-device-data
- **状態**: 依頼済
- **優先度**: 高(Arduino IDE で CH32V103C8T6 を選ぶと、本物の C8T6 に書けない)

## 背景

`index/device_ids.csv` は CH32V103 の 2 つの型番に別の ID を置いている。

| part_number | device_id | basis |
|---|---|---|
| CH32V103C8T6 | `0x25004102` | EVT `ch32v10x_dbgmcu.c` の `DBGMCU_GetCHIPID` のコメント(`CH32V103C8T-0x25004102`) |
| CH32V103R8T6 | `0x2500410F` | 同じコメント(`CH32V103R8T-0x2500410F`) |

どちらも `reference` で、出所は EVT のコメント 2 行だけ。

## 実測

0x1FFFF7E0 から 0x14 バイトを WCH-LinkE(2.22)で読んだ。値は `ch32rv 0.28.0` の `read --range 0x1ffff7e0+0x14` と `target info` による。

| 板 | 刻印(パッケージ) | Link | `0x1FFFF884`(chip ID) | FLACAP | UID |
|---|---|---|---|---|---|
| CH32V103C-R1-1v1(48 ピンの C 板) | CH32V103C8T6(LQFP48) | LinkE FB868F068059 | `0x2500410f` | 64 KiB | `1035abcd7869bc67` |
| bench の V103 板 | CH32V103R8T6(LQFP64) | Link CH549 434A124C5596 | `0x2500410f` | 64 KiB | `d39eabcd3bc4bc59` |

C8T6 の ID も `0x2500410f` で、R8T6 と同じ値になった。今ある 2 個体では、ID が C8T6 と R8T6 を分けていない。EVT の `GPIO_IPD_Unused` は C8T6 を `0x25004102` で見分けるが、bench の C8T6 はその値を返さない(つまり EVT のこの分岐は、この個体では働かない)。

`0x25004102` を返す個体を、こちらはまだ見ていない。

## 頼みたいこと

1. `device_ids` の CH32V103 の行を、資料と実測の両方に合わせて直してほしい。こちらの事実は「C8T6 も R8T6 も `0x2500410f` を返す」。`0x25004102` を返す V103 があるかは分からない。
2. 形は依頼先に任せる。こちらが受けられる形は次のどれか。
   - CH32V103C8T6 にも `0x2500410F` を置く(1 つの ID を 2 型番が共有する)。共有の形で納品されたら、ch32rv はその ID を family 止まり(候補 C8T6 / R8T6)に解決し、どちらの `--chip` でも通すよう直す。
   - 「1 つに決まらない」として C8T6 の行を外す(CH32V203F6P6 と同じ扱い)。この場合 R8T6 の行の basis か note に「C8T6 も同じ値を返す実測あり」と残してほしい。ID から R8T6 と言い切れなくなるため。
   - 1 型番に複数の ID を許す形(`0x25004102` を残しつつ `0x2500410F` も足す)。この形にする場合は、列か行の形を先に知らせてほしい。ch32rv の db-gen を合わせる。
3. 上の実測を basis に入れる場合は、`measured(ch32rv 0011, LinkE FB868F068059)` のように書いてほしい。

## ch32rv が受け取った後にすること

- `cargo xtask db-gen` で取り込み、`--chip CH32V103C8T6` が bench の C8T6 板(FB868F068059)で通ることを確かめる。
- `xtask` の `MEASURED`(`CH32V103R8T6` を「実機で ID を確認済」とする印)を、納品の形に合わせて見直す。
