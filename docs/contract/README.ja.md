# ch32rv JSON contract

- 契約版: `5`
- 状態: 凍結候補。v1 の凍結で確定する。CLI の版とは独立に versioning する([cli.ja.md §6](../cli.ja.md))。凍結までは破壊的変更をまとめて入れ、版を上げる

## 構成

| ファイル | 内容 |
|---|---|
| [result.schema.json](result.schema.json) | `--json` 時に stdout へ出る単一 result object の envelope。`result` は commands.schema.json で引く |
| [commands.schema.json](commands.schema.json) | コマンドごとの `result` の形。`$defs` のコマンドの名前(envelope の `cmd`)の項。`result` を出すコマンドはすべてここにある(50 個。`gdb`、`broker serve`、`arduino discovery` / `monitor`、`complete`、`isp reset` は `result` を出さない) |
| [events.schema.json](events.schema.json) | `--progress ndjson` 時に stderr へ流れる 1 行 1 event |

## ルール

1. **field の追加は契約版を変えずに行える**。利用側は未知 field を無視すること。schema は未知 field を禁じない。
2. field の削除・意味変更・型変更は破壊変更であり、契約版(`contract`)を上げる。
3. exit code は [cli.ja.md §3.6](../cli.ja.md) が正で、`error.code` に同じ値が入る。
4. 開いた値:
   - `error.code` と `ev` は値を足すことがある。知らない `ev` の行は読み飛ばし、知らない `error.code` は §3.6 の帯の意味で扱う。
   - `error.kind` と `warnings[].code` は開いた語彙。名前は凍結し(別の意味で使い回さない)、足すことはある。知らない `kind` は同じ `error.code` の帯の意味で扱う。
   - warning は `ok` を変えない。`--json` では warning はすべて `warnings[]` に出る。
   - `msg` と `hint` は人向けで、文面は約束しない。
5. `flash` の結果は経路によらず `result.flash`(`bytes`、`family`(分からなければ null)、`programmer` = `stub` / `controller` / `loader` / `oep-loader` / `hid` / `isp` / `dfu`、`verified` bool か null、`running` bool か null、`skipped`、`scope`、経路が持つもの: `pages`、`rewritten`、`chip_id`、`seconds` など)。`isp flash`、`boot dfu flash`、`boot hid flash` も同じ形。検証の結果はどのコマンドでも `verified`(bool。検証しなかったら null)。
6. probe の照合には `probe.variant`(WCH-Link の安定 ID)を使い、`probe.model` は表示用。`probe.kind`(`wchlink` / `oep`)は probe の種類。firmware の版は `"<major>.<minor>"`(0 埋めしない)。
7. library(`ch32rv-contract` crate)の serde 型がこの schema の実装であり、試験で実際の出力を schema と照合する(cli/tests/contract.rs と、`--json` を読むほかの試験。照合は cli/tests/common/mod.rs)。
