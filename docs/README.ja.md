# ch32rv 仕様ドキュメント

ch32rv の CLI、契約、設計を書いた文書の索引。どれも今の ch32rv を書く。版ごとの変化は [CHANGELOG](../CHANGELOG.md) にある。

## 構成

| 文書 | 内容 |
|---|---|
| [development.ja.md](development.ja.md) | 開発ルール(言語ポリシー・ワークフロー規約・ビルド/チェック・リリース・ライブラリ API 規約) |
| [requirements.ja.md](requirements.ja.md) | 巻き取り対象ツールの機能インベントリと吸収マップ |
| [cli.ja.md](cli.ja.md) | コマンド体系と共通契約(JSON、exit code、selector) |
| [freeze-decisions.ja.md](freeze-decisions.ja.md) | 契約の決まり(`--chip` の語彙、port address、exit code、JSON の形など)。コードの注釈が節番号で引く |
| [architecture.ja.md](architecture.ja.md) | 実装言語(Rust)、crate 分割、library API 規約、target DB 生成、配布 |
| [naming.ja.md](naming.ja.md) | repository 名・CLI 名・crate 名 |
| [contract/](contract/README.ja.md) | JSON contract(result envelope・コマンドごとの result・NDJSON event の schema) |
| [oep-host.ja.md](oep-host.ja.md) | OEP の probe での書き込み・モニタ・ブローカー |
| [windows-wch-driver.ja.md](windows-wch-driver.ja.md) | Windows で WCH 純正ドライバのまま WCH-Link を使う経路(CH375 IOCTL、WinUSB 置換なし) |
| [testing.ja.md](testing.ja.md) | 全 OS の実機テスト手順。転送用 fixture(pattern + family 別 runtest)は `tests/fixtures/` |
| [release-plan.ja.md](release-plan.ja.md) | リリースの線引き・crates.io への publish・バイナリ・手順 |
| [data-requests/](data-requests/README.ja.md) | ch32-device-data への CSV 作成依頼書と、その根拠の実測の記録(1 依頼 1 ファイル) |

## 言語ポリシー

開発ルールの一次ソースは [development.ja.md](development.ja.md) §1(要約):

- `docs/` の文書は日本語のみ。
- ソースコードは英語のみ、または `// en:` / `// ja:` マーカー付きの英語+日本語。
- CHANGELOG は 1 ファイルに `- (EN)` / `- (JA)` 併記。
