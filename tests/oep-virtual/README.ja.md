# ch32rvのOEP消費側テスト

ch32rvが使うOEPの通信、session、flash/read/verify/reset、consoleとbrokerの契約を検査します。プローブfirmwareの実装検査と更新は提供側の責務です。設備情報は明示したローカル設定から受け取り、別リポジトリや隣接checkoutを検索しません。

## 仮想プローブ

repository rootでRustとuvを用意して実行します。

```sh
cargo test --locked -p ch32rv-oep
cargo test --locked -p ch32rv --test oep_flash --test oep_monitor --test broker
```

公開Python依存はこのディレクトリの `pyproject.toml` と `uv.lock` でcommitを固定します。Rustテストは `target/` 内に独立したPython環境を作ります。Pythonやbackendを準備できない場合は失敗し、実機が不要な検査をskipしません。CIでも同じ入口を使います。

開発時だけ `OEP_CLIENT_PYTHON=/absolute/path/to/locked-python-project` を明示できます。指定先には `pyproject.toml` と `uv.lock` が必要で、誤った指定から既定環境には戻りません。変更追従時には、この依存と `crates/oep/tests/virtual_bench/uv.rs` の版ラベルを合わせて更新します。

## 実機

[実機ガイド](../hardware/README.ja.md)では、現在インストール済みのプローブと明示したDUTだけを使います。仮想テストのPASSは実配線や実機flashの保証にはなりません。
