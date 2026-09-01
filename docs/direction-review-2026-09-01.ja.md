# ch32rv 方向性レビュー(2026-09-01)

- 日付: 2026-09-01
- 状態: 記録。ArduinoCore-CH32(組み込み)側から仕様と実装を突き合わせたレビュー結果と、同日のリリース方針決定を残す
- レビュー範囲: docs/(requirements・cli・architecture・naming・contract・protocol)、CHANGELOG、crates/・cli/ の実装、ArduinoCore-CH32 側の platform.txt・ADR-0008/0011/0014・upload-and-fixture.ja.md

## 1. 結論

方向性は妥当。吸収マップ(requirements)→ CLI 体系(cli)→ crate 分割(architecture)の導出が一次資料ベースで一貫しており、既存ツールの契約上の欠陥への構造的な回答が設計に織り込まれている。実装は README の Status 表示より大きく先行し、CHANGELOG の全項目に実機検証が付いている。修正を要する方向性の問題は見つからなかった。残る主要ギャップは機能ではなく、回帰保護(record/replay harness)・排他制御(device lock)・配布(cargo-dist、Windows 検証)。

## 2. リリース方針(2026-09-01 決定)

1. ArduinoCore-CH32 も ch32rv も、品質を高めてからリリースする(期日優先にしない)。
2. probe-rs を同梱した状態でのコアのリリースは行わない。**コア同梱のアップローダは ch32rv に一本化**する(probe-rs 併走の opt-in 案は採らない)。
3. ch32rv の **Linux 版をコアの実機ベンチで先行ドッグフーディング**する。開始は「もう少し安定してから」(開始条件の提案は §5)。

現行の ArduinoCore-CH32 platform.txt(probe-rs recipe、ADR-0008)は置換までそのまま。置換時にコア側で ADR の追記(または新 ADR)が必要になる。

## 3. 検証したこと(確認済み事実)

1. **Arduino recipe 制約の織り込み**: [cli.ja.md](cli.ja.md) §5 の flash 呼び出し形は、現行 platform.txt の probe-rs recipe 行と 1:1 で対応する(probe selector を空にできる 1 変数 `{upload.probe_args}`、`--non-interactive`、`--progress none`、`--chip` 変数)。矛盾なし。
2. **配布枠の整合**: ch32rv は自前 release を持つため、ArduinoCore-CH32 ADR-0011 の `mirror-` 枠で Board Manager へ再配布できる([architecture.ja.md](architecture.ja.md) §5 の記載と ADR-0011 の実態が一致。ADR-0014 の `build-` 枠は不要)。
3. **実装の実態**: CHANGELOG の全項目に実機検証の記載がある — 6 board × 6 probe の flash matrix(V003/V103/V203/V307/L103/X035)、GDB server(動的 trigger 検出、RAM/HW/flash SW breakpoint、detach 時 pristine 復元、RV32E 対応)、monitor 3 経路(sdi/dmdata/uart)、doctor、target option get、recover。protocol の capture-verified 規律([protocol/wch-link.ja.md](protocol/wch-link.ja.md))もソース冒頭の宣言どおり運用されている。
4. **既存ツールの契約欠陥への回答が設計に入っている**: probe-rs の firmware 版比較バグ → 版比較を契約+test で固定、wlink の verify 欠如 → verify 既定 on、minichlink の overflow → `forbid(unsafe_code)` + fuzz 方針、`hwbreak+` 偽装 → trigger slot の動的検出と正直な広告。
5. **未実装の確認**(2026-09-01 時点):
   - capture/replay harness と per-device advisory lock(`crates/usb/src/lib.rs` 冒頭に「Not yet」と明記)
   - 配布パイプライン(`.github/workflows` 無し、dist 設定無し)。実機検証はすべて Linux ベンチで、Windows(WinUSB)/macOS は未知
   - `arduino discovery` / `arduino monitor`(P1)
   - `target option set/reset/write-raw` / `protect`(get のみ実装)
   - V103 の buffered fast-program quirk(日常 flash は stub 経路で動作)、V003/V103 の flash SW breakpoint
6. **README の Status 表示が実態と乖離**: 「pre-implementation」「M0 in progress」とあるが、実態は P0 ほぼ完了+P1 相当(gdb/monitor)まで実機検証済み。

## 4. ギャップと推奨順序(提案)

1. **README / Status の実態同期**(対外表示。作業量最小)
2. **record/replay harness**([architecture.ja.md](architecture.ja.md) §4 は「最初に作る」としていた。現状の回帰保護はベンチ実機のみで、利用者が付く前に戻すのが安い)
3. **device lock**(ベンチの常時運転と `arduino discovery/monitor` の前提)
4. **配布(cargo-dist)+ Windows 実機検証**(コア同梱リリースのクリティカルパス。ドッグフーディングは Linux で先行できるため 2・3 より後でよい)
5. V103 quirk・option set 系・isp/boot(P2)は、capability 判定で正直に弾ければリリースをブロックしない

## 5. ドッグフーディング開始条件(提案)

ArduinoCore-CH32 のベンチ(tests/manual/smoke)を ch32rv 経路に切り替える前提条件として提案する。確定は別途。

- exit code(cli §3.6)と `--json` envelope を add-only として凍結した版タグが 1 つある(ベンチが exit code に依存するため)
- device lock 実装済み(monitor と upload の同一 probe 並行アクセスがベンチの常態)
- `--capture` 実装済み(ベンチで踏んだ protocol 問題を replay fixture として報告できる)
- コア側の受け入れ作業は ArduinoCore-CH32 側の依頼文書(§6)を参照

## 6. 参照

- 組み込み側の依頼事項: `../../ArduinoCore-CH32/docs/ch32rv-requests.ja.md`
- `../../ArduinoCore-CH32/platform.txt`(現行 probe-rs recipe)、`docs/adr/0008`(現行 upload 方針)、`docs/adr/0011`(mirror- 配布枠)、`docs/upload-and-fixture.ja.md`(「ch32-upload(仮称)」構想 — ch32rv が実体)
- [requirements.ja.md](requirements.ja.md) / [cli.ja.md](cli.ja.md) / [architecture.ja.md](architecture.ja.md) / [CHANGELOG.md](../CHANGELOG.md)
