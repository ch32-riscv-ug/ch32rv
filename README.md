# ch32rv

English | [日本語](README.ja.md)

Flashing and debugging tool for WCH CH32 RISC-V microcontrollers (under development, pre-implementation).

ch32rv aims to consolidate the functionality currently spread across probe-rs, wlink, minichlink, WCH OpenOCD, WCH-LinkUtility, wchisp, and wlink-iap into a single CLI plus reusable Rust library crates.

- Specifications: [docs/README.ja.md](docs/README.ja.md) — spec-first; the documents are currently Japanese-only while under consideration, and English mains will be added as they stabilize
- Changelog: [CHANGELOG.md](CHANGELOG.md) (bilingual)
- Language: Rust
- License: [MIT](LICENSE)

## Status

| Stage | Content | State |
|---|---|---|
| Specification | Requirements, CLI tree, architecture, naming | Fixed in docs/ (2026-09-01) |
| M0 | Protocol notes, contract schemas, workspace scaffold | In progress |
| M1+ | Staged plan in the original design note | Not started |

## Build

```sh
cargo build
cargo test
./target/debug/ch32rv --help
```

The full command tree is already defined; `version` works, everything else exits 70 (unimplemented) until its milestone lands.
