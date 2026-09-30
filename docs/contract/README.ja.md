# ch32rv JSON contract

- 契約版: `4`(2026-10-01、docs/freeze-decisions.ja.md §7)
- 状態: 凍結候補。v1 の凍結で確定する。CLI の版とは独立に versioning する([cli.ja.md §6](../cli.ja.md))。凍結までは破壊的変更をまとめて入れ、版を上げる

## 構成

| ファイル | 内容 |
|---|---|
| [result.schema.json](result.schema.json) | `--json` 時に stdout へ出る単一 result object の envelope |
| [events.schema.json](events.schema.json) | `--progress ndjson` 時に stderr へ流れる 1 行 1 event |

## ルール

1. **field の追加は契約版を変えずに行える**。利用側は未知 field を無視すること。
2. field の削除・意味変更・型変更は破壊変更であり、契約版(`contract`)の major を上げる。
3. exit code は [cli.ja.md §3.6](../cli.ja.md) が正で、`error.code` に同じ値が入る。
4. command ごとの `result` の中身(`flash` / `probe` / `target` 等)の schema は per-command で追加していく。envelope と event はここで固定する。`flash` の結果は経路によらず `result.flash`(`bytes`、`family`、`programmer` = `stub` / `controller` / `loader` / `oep-loader` / `hid`、`verified` bool か null、`running`、`skipped`、`scope`、経路が持つもの)。検証の結果はどのコマンドでも `verified`(bool)。probe の照合には `probe.variant`(安定 ID)を使い、`probe.model` は表示用。firmware の版は `"<major>.<minor>"`(0 埋めしない)。
5. library(`ch32rv-contract` crate)の serde 型がこの schema の実装であり、試験で実際の出力を schema と照合する(cli/tests/contract.rs)。
