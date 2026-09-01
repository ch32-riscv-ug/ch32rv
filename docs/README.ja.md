# ch32rv 仕様ドキュメント

- 作成日: 2026-09-01
- 状態: 提案(実装未着手)。spec-first の方針に基づき、実装より先にここを固定する

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

## 前提資料

- 原設計案: `../../note/research/new-programming-tool-design.ja.md`(2026-08-31)
- 調査レポート: `../../note/research/wch-linke-host-apps.ja.md`、`../../note/research/programming-tools-and-probes.ja.md`、`../../note/research/programming-probes-and-usb-paths.ja.md`

本ディレクトリの文書は原設計案を置き換えるものではなく、その §4(機能セット)・§6(CLI 案)・§7(アーキテクチャ)・§8(名前)を一次資料の再調査で検証し、完成形まで展開したものである。矛盾する場合は本ディレクトリ側を正とする。
