# Changelog

## Unreleased

- (EN) Add the specification set: requirements with the tool-absorption map (9 tool lineages), the complete CLI command tree with the output contract (JSON envelope, NDJSON events, exit codes, probe selectors), architecture (Rust verification and crate layout), and naming decisions.
- (JA) 仕様一式を追加: 9系統ツールの吸収マップ付き要件、出力契約(JSON envelope・NDJSON event・exit code・probe selector)込みのCLIコマンド体系、アーキテクチャ(Rust検証とcrate分割)、命名の決定。
- (EN) Add data-request documents for the ch32-device-data repository (device IDs for the 7 gap series, debug interface types, option-byte write layouts and factory defaults), with a provisional-overlay acceptance flow.
- (JA) ch32-device-data への依頼書を追加(gap 7系列のdevice ID、debug interface種別、option byte書き込みレイアウトと工場出荷値)。暫定overlayでの受け入れフロー付き。
- (EN) Add WCH-Link protocol notes with per-item verification status (verified / attested / conflict / todo) and JSON contract schemas (result envelope, NDJSON events) as contract version 1.
- (JA) 項目ごとの検証状態(verified / attested / conflict / todo)付きWCH-Link protocolノートと、契約版1のJSON schema(result envelope・NDJSON event)を追加。
- (EN) Scaffold the Cargo workspace: 10 library crates plus the CLI binary defining the full command tree; the contract crate (exit codes, envelope, events, policies, progress/cancel) and the probe-selector grammar are implemented and tested; `version` works and every other command exits 70 (unimplemented).
- (JA) Cargo workspaceの雛形を追加: library crate 10個とCLI(全コマンドツリー定義済み)。contract crate(exit code・envelope・event・policy・進捗/中断)とprobe selector文法は実装・テスト済み。`version` のみ動作し、他はexit 70(未実装)。
- (EN) Rename the whole-chip erase flag to `erase --all` (the global `--chip <SKU>` selector conflicts with an `erase --chip` flag).
- (JA) チップ全消去のフラグを `erase --all` に変更(グローバルの `--chip <SKU>` と `erase --chip` が衝突するため)。
