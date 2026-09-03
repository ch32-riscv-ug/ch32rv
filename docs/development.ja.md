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
- **read/write(transport)経路を変えたら、他 OS へ引き継ぐ前に Linux で probe 横断の回帰を通す**。特に **CH549 Link(WCH-Link 無印、fw 2.12)を必ず含める**: LinkE(fw 2.22)では出ない癖を持つ(0.4.0 の高速バルク read は CH549 で stub 直後に stale flash を返し偽 verify-mismatch を起こした=[testing.ja.md](testing.ja.md) §6)。probe ごとに `flash <fixture> --erase auto --confirm-run pc` を **10 回以上反復**して間欠不具合を炙り出す。

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

## 5. ライブラリ API 規約

公開 crate は crates.io の安定契約。破壊的変更は 0.x の minor でまとめて行い([consistency-audit.ja.md](consistency-audit.ja.md))、以下の規約に従う。

- **エラー型**: 1 crate 1 エラー enum(`〜Error` 接尾辞)+ `thiserror` + `#[non_exhaustive]`。分類は Display 文字列でなく**型付き variant** で行う(部分文字列マッチ禁止)。`ch32rv-usb` だけは境界の性質上 4 エラー型(`UsbError`/`LockError`/`ResolveError`/`SelectorParseError`)を意図的に持つ。CLI 側の exit code 分類は `cmd_probe::session_error` に集約。
- **戻り値**: 到達可否・欠損が「データ」なら status enum(`ChipInfoStatus`/`DmiStatus`/`target::Resolution`)、失敗が「エラー」なら `Result`。public 経路に `unwrap`/`expect`/`panic` を置かない。
- **命名**: target 番地は `u32`、host 長は `usize`。番地引数は `addr`、byte 範囲読みは `read_mem(addr, len)`、書きは `write_*(addr, data, …)`(addr 先)。単語/半語アクセスは `read_mem32`/`write_mem16` 等と別名にする。enum→文字列は `as_str(&self) -> &'static str`(data を持つ enum は `name()->String` 可)。
- **単位**: サイズは **bytes**(`flash_bytes`/`sram_bytes`)。KiB は表示境界でのみ導出する。
- **constructor 動詞**: HW を掴むものは `open`、lock は `acquire`、純粋/借用は `new`、名前付き生成は `builtin`/`parse`。
- **pub フィールド**: plain data 構造体はフィールド公開でよいが、不変条件を持ちうる型(`FlashParams`/`FlashCtrlProfile` 等)は将来 accessor 化が破壊になる点に注意。UsbDeviceInfo 等 backend を包む型は accessor。
- **timeout モデル**: `WchLink` はセッションに timeout を持つ(`set_timeout`)。`--timeout` は transport(1 転送)専用、`--duration` は streaming コマンドの実行長。
- **contract**: JSON の envelope/キー/exit code は `ch32rv-contract`。`result` のキーは snake_case、同一概念は同名(`addr`/`family`/`scope`/`verified`/`firmware`/`firmware_mode`/`flash_bytes`)、二値は bool。exit code 数値は凍結(`exit.rs` の test が固定)。破壊時のみ `CONTRACT_VERSION` を上げる。
