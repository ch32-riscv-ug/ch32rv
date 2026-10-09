# OEP経由の実機CLI契約

明示したOEP個体と保存済みslotに対して、ch32rvのtarget識別、flash/readback検証、独立したread、reset/runを確認します。プローブfirmwareの更新・最低版指定・設定保存・配線探索は行いません。設備の割当てと配線確定は設備管理側、CLIの動作保証と結果記録はこのリポジトリの責務です。

ネイティブLinuxで、repository rootから使います。

```sh
cargo build --locked -p ch32rv
cp tests/hardware/equipment.example.toml tests/hardware/equipment.local.toml
cp tests/hardware/.env.example tests/hardware/.env
# TOMLと.envを実際の設備・テストimage・既存共通lockへ編集
uv run --project tests/hardware --locked --env-file tests/hardware/.env \
  pytest -c tests/hardware/pyproject.toml tests/hardware/test_oep_tool.py -vv
```

設定TOMLはこのCLIの単一slot検査用です。OEPの共有設備graphを直接読むadapterではありません。`example = false`、stable serial path、完全なOEP個体ID、`port:oep://<unit-id>/<slot>`、期待するDUT SKU/chip IDと、明示的な空でない `.bin` を設定します。`chip_id` は部品を識別する値で、物理個体のUIDではありません。DUT UIDを返す経路では `target.uid` も指定できます。UIDなしの配置は作業者が確認した割当てとして記録します。

`.env` は明示的な `--env-file` で読み込みます。TOMLのpathは `CH32RV_HW_CONFIG`、検査するCLI binaryは `CH32RV_TEST_TOOL`、既存の共通lockは `OEP_HW_LOCK`、結果の親ディレクトリは `OEP_HW_RESULTS` です。TOML内のimage相対pathはTOMLの所在から、環境変数の相対pathは実行ディレクトリから解決します。`.env` と `*.local.toml` はGit管理外、雛形とuv.lockはGit管理します。

`CH32RV_HW_CONFIG` がない場合だけ任意の実機検査としてskipします。設定がある場合の不足・個体不一致・設備使用中・操作失敗はFAILです。共通lockを検査の全期間保持し、lockファイルを作成・交換しません。CLI brokerのruntimeは実行ごとに隔離します。他の設備利用者も同じlockを使う必要があります。

プローブ個体とdescribe宣言を照合してからDUTを操作します。実際のfirmware、interface宣言、Python client版、CLI版・DB/stub digest・binary hash、設定hash、image hash、コマンドと結果JSONを新しい結果ディレクトリへ保存します。書込み後はimageの長さだけ読み戻して比較し、DUT識別に成功した後のflash/read失敗でもreset/runを試みます。復帰の失敗は別途FAILとして記録します。

Flash退避・以前のimageへの復元は行いません。指定imageを転送して使用する検査なので、終了後はテストimageがDUTに残ります。UART/GPIO、USB対向機、多ターゲットの同時動作は、この検査では検証していません。

ガードの単独確認はボード不要です。

```sh
uv run --project tests/hardware --locked pytest -c tests/hardware/pyproject.toml tests/hardware/test_guards.py
```
