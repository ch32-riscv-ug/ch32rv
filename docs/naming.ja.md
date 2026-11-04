# 名前: repository・CLI・crate

評価の軸は、射程の正確さ / 検索性 / 非衝突 / 打鍵 / suite 性。

## 1. 名前を持つもの

| 対象 | 使われる場所 | 制約 |
|---|---|---|
| GitHub repository 名 | URL、clone、Board Manager の参照 | org 内で一意、変更コストが最大 |
| CLI binary 名 | recipe、CI、毎日の打鍵、PATH 上の衝突 | OS のコマンド名として一意 |
| crate 名(bin + library 群) | crates.io、他プロジェクトの依存 | crates.io で一意(`-`/`_` 同一視) |
| 表記 | 文書、`--help`、README | 大文字小文字の揺れを作らない |

## 2. 判断の材料

1. **射程**: ch32rv は probe 経路に加えて factory ISP(`isp`)と custom bootloader(`boot`)の経路を持つ([cli.ja.md](cli.ja.md))。probe を意味する語(`link` 等)を含む名前は、経路が増えるほど名前負けする。
2. **CH32F(Arm)を名乗らない**: ISP 経路は CH32F103 も物理的には書けるが、target DB は RISC-V 系だけを持つ。`ch32tool` / `ch32ctl` のような CH32 全体を指す名前は使わない。
3. **org との整合**: ArduinoCore-CH32RV の FQBN vendor は `ch32-riscv-ug`、本 repo の LICENSE も `CH32 RISC-V User Group`。`ch32-riscv-ug/ch32rv` は、org 名が「CH32 RISC-V」を、repo 名が product を表す。
4. **既存の名前**: crates.io の `wlink`・`wchisp` は別の既存 crate。

## 3. 決めた名前

| 対象 | 名前 | 補足 |
|---|---|---|
| GitHub repository | **`ch32rv`**(`ch32-riscv-ug/ch32rv`) | |
| CLI binary | **`ch32rv`** | `ch32rv flash blink.elf` が tool 名として読める |
| bin crate | **`ch32rv`**(`cargo install ch32rv`) | |
| library crates | **`ch32rv-` prefix で統一**(`ch32rv-usb`、`ch32rv-wchlink` など。一覧は workspace の members、`Cargo.toml`) | [architecture.ja.md §2](architecture.ja.md) |
| 表記 | 小文字 `ch32rv` 固定。文頭でも大文字化しない | wlink/wchisp と同じ流儀 |

**repository 名 = CLI 名 = bin crate 名 = crate prefix を一致させる。** 1 repo 1 product で、利用者が覚える名前を 1 つにする。

## 4. 採らなかった案と理由

| 案 | 内容 | 採らない理由 |
|---|---|---|
| repo と CLI を分ける(例: repo `ch32rv-tools`、bin `ch32rv`) | suite であることを repo 名で示す | 名前が 2 つになる説明コストに利得が見合わない。crate prefix が既に suite 性を担う |
| CLI だけ短縮名(`crv` 等) | 打鍵削減 | 検索性ゼロ、既存コマンドとの衝突リスク、recipe/CI では 1 回しか打たない。alias は利用者が勝手に張れる |
| `ch32rvtool` | genre marker 明示 | `ch32rv flash` で既に tool と読める。crate prefix `ch32rvtool-` は冗長 |
| `wchrv` | CH5xx まで射程に入れる | CH5xx は対象外([requirements.ja.md](requirements.ja.md) §4)。会社名を冠するのも避ける |
| `rvlink` 系 | probe 連想 | ISP/bootloader 経路を含む多経路 tool には §2-1 のとおり不適 |
| protocol crate だけ説明的な独立名(`wch-link`) | 単独 publish 時の発見性 | 既存 crate `wlink` と紛らわしく、suite の prefix 一貫性が崩れる。`ch32rv-wchlink` に keywords(`wch-link`, `wch`)を付ければ crates.io 検索は足りる |
| Windows ドライバ経路 crate だけ独立名(`wch-ch375` / `ch375` / `ch375-driver`) | 汎用部品としての発見性 | 同上。**`ch32rv-usb-wch-win`** にし、汎用の利用は keywords(`wch`, `ch375`, `wch-link`, `usb`, `windows`)と README で補う([windows-wch-driver.ja.md](windows-wch-driver.ja.md)) |

## 5. crates.io での名前

- `ch32rv` と `ch32rv-` prefix の library crate は本 project が publish する(publish する crate と順番は [release-plan.ja.md](release-plan.ja.md) §1)。
- 名前だけの placeholder 確保(squatting)はしない。
