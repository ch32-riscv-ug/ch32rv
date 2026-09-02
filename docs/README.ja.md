# ch32rv 仕様ドキュメント

- 作成日: 2026-09-01
- 状態: 仕様は固定済み、実装進行中。spec-first の方針に基づき、実装より先にここを固定する(実装状況は [CHANGELOG](../CHANGELOG.md) と [direction-review-2026-09-01.ja.md](direction-review-2026-09-01.ja.md) を参照)

## 構成

| 文書 | 内容 |
|---|---|
| [requirements.ja.md](requirements.ja.md) | 巻き取り対象ツールの全機能インベントリと吸収マップ。2026-09-01 の一次資料再調査結果 |
| [cli.ja.md](cli.ja.md) | コマンド体系仕様。全機能を最終的に実装する前提の完成形ツリーと共通契約(JSON、exit code、selector) |
| [architecture.ja.md](architecture.ja.md) | 実装言語の検証(Rust 採用判断)と crate 分割、library API 規約、target DB 生成、配布 |
| [naming.ja.md](naming.ja.md) | repository 名・CLI 名・crate 名の決定 |
| [contract/](contract/README.ja.md) | JSON contract(result envelope・NDJSON event の schema)。契約版 1 |
| [protocol/wch-link.ja.md](protocol/wch-link.ja.md) | WCH-Link USB protocol ノート(骨組み。capture で verified 化する) |
| [data-requests/](data-requests/README.ja.md) | ch32-device-data への CSV 作成依頼書(ファイル単位で依頼に使う) |
| [direction-review-2026-09-01.ja.md](direction-review-2026-09-01.ja.md) | ArduinoCore-CH32 側からの方向性レビュー記録。リリース方針(probe-rs 非同梱・ch32rv 一本化・Linux 先行ドッグフーディング)とギャップ・開始条件 |
| [release-plan.ja.md](release-plan.ja.md) | v0.1.0 リリース計画。線引き(IN/OUT)・crates.io 依存順 publish・全OSバイナリ(cargo-dist)・リリース前チェック |

## 前提資料

- 原設計案: `../../note/research/new-programming-tool-design.ja.md`(2026-08-31)
- 調査レポート: `../../note/research/wch-linke-host-apps.ja.md`、`../../note/research/programming-tools-and-probes.ja.md`、`../../note/research/programming-probes-and-usb-paths.ja.md`

本ディレクトリの文書は原設計案を置き換えるものではなく、その §4(機能セット)・§6(CLI 案)・§7(アーキテクチャ)・§8(名前)を一次資料の再調査で検証し、完成形まで展開したものである。矛盾する場合は本ディレクトリ側を正とする。

## 言語ポリシー

[CLAUDE.md](../CLAUDE.md) の言語ルールに従う。

- 文書は英語を主とし、`.ja.md` の日本語版と相互リンクする。**ただし内容が変わりうる検討中の文書は日本語のみ**とし、固まった時点で英語主版を追加する。本ディレクトリの仕様群は現在この「検討中」段階にある。
- ソースコードは英語のみ、または `// en:` / `// ja:` マーカー付きの英語+日本語。
- CHANGELOG は分裂を避けるため 1 ファイルに `- (EN)` / `- (JA)` 併記。
