# ch32rv

WCH CH32 RISC-V マイコン向けの書き込み・デバッグツール(開発中・実装未着手)。

既存ツール群(probe-rs / wlink / minichlink / WCH OpenOCD / WCH-LinkUtility / wchisp / wlink-iap ほか)に分散している書き込み・復旧・monitor・debug・probe 管理・ISP・bootloader 書き込みを、1 つの CLI と再利用可能な Rust crate 群に統合することを目標とする。

- 仕様: [docs/README.ja.md](docs/README.ja.md)(spec-first。実装より先に仕様を固定している)
- 言語: Rust
- License: [MIT](LICENSE)

## 状態

| 段階 | 内容 | 状態 |
|---|---|---|
| 仕様 | 要件・CLI 体系・アーキテクチャ・命名 | docs/ に確定(2026-09-01) |
| M0 | protocol.md 整備、placeholder publish、workspace 雛形 | 進行中 |
| M1 以降 | [原設計案 §10](../note/research/new-programming-tool-design.ja.md) の段階計画 | 未着手 |
