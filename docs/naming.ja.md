# 名前の決定: repository・CLI・crate

- 検討日: 2026-09-01
- 状態: 提案(原設計案 §8 の再検証と、配布単位への展開)
- 前提: [原設計案 §8](../../note/research/new-programming-tool-design.ja.md) の評価軸(射程の正確さ / 検索性 / 非衝突 / 打鍵 / suite 性)

## 1. 決めるものは 4 つ

| 対象 | 使われる場所 | 制約 |
|---|---|---|
| GitHub repository 名 | URL、clone、Board Manager の参照 | org 内で一意、変更コストが最大 |
| CLI binary 名 | recipe、CI、毎日の打鍵、PATH 上の衝突 | OS のコマンド名として一意 |
| crate 名(bin + library 群) | crates.io、他プロジェクトの依存 | crates.io で一意(`-`/`_` 同一視) |
| 表記 | 文書、`--help`、README | 大文字小文字の揺れを作らない |

## 2. 再検証(2026-09-01 時点)

原設計案 §8 は 2026-08-31 に空きを確認済み。今回の再調査で追加確認・追加論点は次のとおり。

1. **crates.io の空きを API で再確認**: `ch32rv`・`wch-link`・`ch32rv-cli` いずれも 404(未登録)。`wlink`・`wchisp` は既存 crate(別物)。
2. **射程が広がった**: 本仕様は probe 経路に加えて factory ISP(`isp`)と custom bootloader(`boot`)経路を含む([cli.ja.md](cli.ja.md))。probe を意味する語(`link` 等)を含む名前は**多経路化でさらに名前負けする**ようになった。§8 で `rvlink` を退けた判断は強まった。
3. **CH32F(Arm)を名乗らない**条件は変わらない(`ch32tool` / `ch32ctl` 不採用の理由)。ISP 経路は CH32F103 も物理的には書けるが、本 tool の target DB は RISC-V 系のみを持つ(non-goal 維持)。
4. **org との整合**: ArduinoCore-CH32 の FQBN vendor は `ch32-riscv-ug`、本 repo の LICENSE も `CH32 RISC-V User Group`。`ch32-riscv-ug/ch32rv` は org 名が「CH32 RISC-V」を、repo 名が product を表す構成で一貫する。

## 3. 結論

| 対象 | 名前 | 補足 |
|---|---|---|
| GitHub repository | **`ch32rv`**(現状のまま。`ch32-riscv-ug/ch32rv`) | |
| CLI binary | **`ch32rv`** | `ch32rv flash blink.elf` が tool 名として読める(§8.4 の判断を維持) |
| bin crate | **`ch32rv`**(`cargo install ch32rv`) | |
| library crates | **`ch32rv-` prefix で統一**: `ch32rv-contract` / `ch32rv-usb` / `ch32rv-wchlink` / `ch32rv-dmi` / `ch32rv-target` / `ch32rv-flash` / `ch32rv-debug` / `ch32rv-monitor` / `ch32rv-isp` / `ch32rv-boot` | [architecture.ja.md §2](architecture.ja.md) |
| 表記 | 小文字 `ch32rv` 固定。文頭でも大文字化しない | wlink/wchisp と同じ流儀 |

**repository 名 = CLI 名 = bin crate 名 = crate prefix を一致させる。** 1 repo 1 product で、利用者が覚える名前を 1 つにする。

## 4. 検討した代替と棄却理由

| 案 | 内容 | 棄却理由 |
|---|---|---|
| repo と CLI を分ける(例: repo `ch32rv-tools`、bin `ch32rv`) | suite であることを repo 名で示す | 名前が 2 つになる説明コストに利得が見合わない。crate prefix が既に suite 性を担う |
| CLI だけ短縮名(`crv` 等) | 打鍵削減 | 検索性ゼロ、既存コマンドとの衝突リスク、recipe/CI では 1 回しか打たない。alias は利用者が勝手に張れる |
| `ch32rvtool` | genre marker 明示 | `ch32rv flash` で既に tool と読める。crate prefix `ch32rvtool-` は冗長 |
| `wchrv` | CH5xx まで射程に入れる将来対応 | CH5xx は non-goal 維持(requirements §4)。会社名を冠する判断も §8 のとおり不採用 |
| `rvlink` 系 | probe 連想 | ISP/bootloader 経路を含む多経路 tool には §2-2 のとおり不適 |
| protocol crate だけ説明的な独立名(`wch-link`) | 単独 publish 時の発見性 | 既存 crate `wlink` と紛らわしく、suite の prefix 一貫性が崩れる。`ch32rv-wchlink` に keywords(`wch-link`, `wch`)を付ければ crates.io 検索は足りる。**名前だけの placeholder 確保(squatting)はしない** |
| Windows ドライバ経路 crate だけ独立名(`wch-ch375` 等) | 汎用部品としての発見性 | 同上の判断を踏襲し **`ch32rv-usb-wch-win` に確定(2026-09-02)**。汎用利用は keywords(`wch`, `ch375`, `wch-link`, `usb`, `windows`)と README で担保(候補 `wch-ch375` / `ch375` / `ch375-driver` は当時 crates.io 空きを確認の上で不採用) |

## 5. M0 での確保作業

1. crates.io に `ch32rv`(bin)を 0.0.x placeholder として publish(実体: `version` だけ動く最小 CLI)。library crate 群は実装が入る時に publish(prefix の事前 squatting はしない)。
2. `ch32-riscv-ug/ch32rv` は取得済み(本 repo)。
3. publish 前に Debian/Homebrew/AUR 等に同名 package・コマンドが無いことを一度確認する(GitHub / crates.io は確認済み、distro 側は未確認)。
