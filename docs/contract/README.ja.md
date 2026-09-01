# ch32rv JSON contract

- 契約版: `1`(draft)
- 状態: 提案。実装より先に固定し、CLI の版とは独立に versioning する([cli.ja.md §6](../cli.ja.md))

## 構成

| ファイル | 内容 |
|---|---|
| [result.schema.json](result.schema.json) | `--json` 時に stdout へ出る単一 result object の envelope |
| [events.schema.json](events.schema.json) | `--progress ndjson` 時に stderr へ流れる 1 行 1 event |

## ルール

1. **field の追加は契約版を変えずに行える**。利用側は未知 field を無視すること。
2. field の削除・意味変更・型変更は破壊変更であり、契約版(`contract`)の major を上げる。
3. exit code は [cli.ja.md §3.6](../cli.ja.md) が正で、`error.code` に同じ値が入る。
4. command ごとの `result` の中身(`flash` / `probe` / `target` 等)の schema は実装時に per-command で追加する。envelope とevent はここで先に固定する。
5. library(`ch32rv-contract` crate)の serde 型がこの schema の実装であり、CI で schema との一致を検査する。
