# 開発ルール(ch32rv)

ch32rv = WCH CH32 RISC-V MCU の書き込み・デバッグツール(Rust)。**spec-first**: コードより先に docs を固める。仕様索引は [README.ja.md](README.ja.md)。

## 1. 言語ポリシー

- **ソース**: 英語のみ、または `// en:` / `// ja:` マーカー付きの英語+日本語(module doc・設計制約コメントは併記、些末なコメントは英語のみ)。
- **CLI `--help` とユーザー向けメッセージ**: 英語。
- **ドキュメント**: 英語を主とし、相互リンクした `.ja.md` twin を置く。**例外: 内容が変わりうる検討中の文書は、固まるまで日本語のみ**にし、安定したら英語主版を追加する(本 `docs/` 群は現在この段階)。
- **CHANGELOG.md**: 1 ファイルに `- (EN)` / `- (JA)` を対で併記。**分割しない**。初回だけ「何が入っているか」のスナップショット、以降は差分。変わりうること(将来予定・未確定)は書かない。

## 2. ワークフロー規約

- **git コミット / push は、ユーザーが明示的に指示するまでしない**(ユーザーが増分コミットする)。
- **システム変更(apt / driver / udev / usbipd bind 等)はユーザーが実行**する。こちらは**コマンドを依頼形式で提示**する(勝手に実行しない)。
- **device データ(CSV)はこのリポジトリ内で作らない**: [ch32-device-data](https://github.com/ch32-riscv-ug/ch32-device-data) への作成依頼を第一とする(`docs/data-requests/`、1 依頼 1 ファイル=ファイル自体が依頼)。納品までの暫定 overlay は可([architecture.ja.md](architecture.ja.md) §3)。
- **ライセンス**: MIT 単独(dual ではない)。

## 3. ビルドとチェック

```sh
cargo build
cargo test
cargo clippy --all-targets --all-features   # warning ゼロ必須
cargo fmt
cargo deny check
```

- **workspace lints**: `unsafe_code = forbid`、clippy `unwrap_used` / `expect_used` / `panic` / `todo` / `unimplemented` = deny(テストは局所 `#![allow(...)]` 可)。
  - **例外**: `ch32rv-usb-wch-win`(Windows の WCH 純正ドライバ FFI 島)だけ `unsafe_code` を許可する。その crate は `[lints]` を独自定義し、`unsafe_code=forbid` を継承せず clippy denies は手書きで維持する([windows-wch-driver.ja.md](windows-wch-driver.ja.md) §4、architecture §2 の別バックエンド枠)。
- **CLI shape は [cli.ja.md](cli.ja.md) で固定**。exit code と JSON は `ch32rv-contract` から来る。
- **protocol コマンドは実装前に capture 検証**する([protocol/wch-link.ja.md](protocol/wch-link.ja.md) のルール。status: verified / attested / conflict / todo)。

## 4. リリース

- 版・CHANGELOG の bump は `scripts/release.sh`、初回 crate 確保は `scripts/first-publish.sh`、公開は `.github/workflows/release.yml`(workflow_dispatch)。手順の詳細は [release-plan.ja.md](release-plan.ja.md)。
