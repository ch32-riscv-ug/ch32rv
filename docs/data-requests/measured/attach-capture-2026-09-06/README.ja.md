# attach 時の USB 往復 capture(5 family, 2026-09-06)

- 提供先: `ArduinoCore-CH32` `docs/harness-requirements.ja.md` §6.3(「**欲しい**」と明示)、
  `docs/harness-probe.ja.md` §7-2 の共同実験(**LinkE が attach で線に何を出しているか**)
- 提供元: ch32rv 0.7.0(`--capture`)
- 形式: NDJSON。1 行目が `_meta`、2 行目が `_device`、以降 1 行 1 転送
  (`seq` / `t_us` / `ep` / `chan` / `dir` / `len` / `ok` / hex `data`)
- 測定環境: Linux(WSL2)+ usbipd。**`t_us` は host 側のタイムスタンプ**で、線上の時刻ではない

## この capture の使い方(いちばん重要な点)

**`attach-*.ndjson` には DMI トランザクションが 1 件も含まれていない。**

| ファイル群 | 操作 | 転送 | うち host → probe | **DmiOp(`0x08`)** |
|---|---|---:|---:|---:|
| `attach-*.ndjson` | `target info`(= detach → probe info → set speed → **attach** → chip info → detach) | 12 | 6 | **0** |
| `dmiread-*.ndjson` | `dbg reg read pc`(attach + halt + レジスタ読み) | 28 | 14 | **8** |

つまり **attach の往復で ch32rv が出している DMI は 0 件**。ChipInfo(`0x11 0x05`)は vendor コマンドで、
DMI ではない。

→ **線側 capture で attach 中に DMI トランザクションが観測されたら、それは 100% probe firmware が
自発的に出したもの**。`RCC_CFGR0` と `FLASH ACTLR` を書き換えている犯人は、この差分でそのまま特定できる。

`dmiread-*.ndjson` は **positive control** として置いてある。「ch32rv が DMI を出すとどう見えるか」の
見本で、線側デコーダの突き合わせにも使える(8 往復が線上で何フレームに対応するか)。

## 収録内容

| target | probe | probe firmware | attach | dmiread |
|---|---|---|---|---|
| CH32V003F4P6 | WCH-LinkE `F90E8F067DFD` | 2.22 | `attach-v003.ndjson` | `dmiread-v003.ndjson` |
| CH32V103R8T6 | **WCH-Link(CH549)** `434A124C5596` | **2.12** | `attach-v103.ndjson` | `dmiread-v103.ndjson` |
| CH32V203C8T6 | WCH-LinkE `FBC18F0680B0` | 2.22 | `attach-v203.ndjson` | `dmiread-v203.ndjson` |
| CH32V307VCT6 | WCH-LinkE `38EF8F06BDC2` | 2.22 | `attach-v307.ndjson` | `dmiread-v307.ndjson` |
| CH32L103C8T6 | WCH-LinkE `0E028F0692F1` | 2.22 | `attach-l103.ndjson` | `dmiread-l103.ndjson` |

**CH32X035C8T6 は今回未接続**(ベンチ 6 台目。probe が USB port の都合で外れていた)。接続でき次第追加する。

**CH549 の 1 台が入っている**のが偶然だが有用で、probe firmware の系統が違う個体との比較になる。
`attach-v103.ndjson` の応答は他 4 台と同じ形(転送 12 / DmiOp 0)だった。

## 読み方の補足

- `data` の先頭 `81` = host → probe、`82` = probe → host(§フレーム形式)。続く 1 byte が cmd。
- attach 列に現れる cmd: `0x0d`(DetachChip / GetProbeInfo / AttachChip)、`0x0c`(SetSpeed)、`0x11`(ChipInfo)。
- **`t_us` の間隔は USB スタックの往復コストを含む**。DMI 1 往復の実測(中央 471 µs)はこの capture の
  `dmiread-*` から算出したもの(ch32rv `docs/data-requests/0006-harness-integration.ja.md` §3)。
- ch32rv 自身は同じ NDJSON を `--replay` で再生でき、**記録と違う write が出れば divergence として報告**する。
  同じ形式を harness 側でも使えば、この道具立てがそのまま効く(要求 `H-183`)。

## 再取得の手順

```sh
ch32rv target info    --probe serial:<SERIAL> --capture attach-<name>.ndjson
ch32rv dbg reg read pc --probe serial:<SERIAL> --capture dmiread-<name>.ndjson
```

どちらも**非破壊**(flash も option byte も書かない)。
