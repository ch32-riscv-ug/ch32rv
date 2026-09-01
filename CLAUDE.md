# CLAUDE.md

Project conventions for ch32rv (flashing/debugging tool for WCH CH32 RISC-V MCUs, Rust).
Spec index: [docs/README.ja.md](docs/README.ja.md). Spec-first: docs are fixed before code.

## Language policy

- Source files: English only, or English + Japanese with `// en:` / `// ja:` markers
  (module docs and design-constraint comments are bilingual; trivial comments English only).
- CLI `--help` text and user-facing messages: English.
- Documents: English is the main language with a cross-linked `.ja.md` twin. Exception:
  documents still under consideration (whose content may change) stay **Japanese-only**
  until they stabilize; add the English main when they settle.
- CHANGELOG.md: a single file with paired `- (EN)` / `- (JA)` bullets. Never split it.

## Workflow rules

- Do NOT commit or push unless the user explicitly asks.
- System changes (apt, drivers, udev) are executed by the user — present the commands as a
  request instead of running them.
- Device data (CSV) must not be authored inside this repo: file a request to the
  ch32-device-data repository (docs/data-requests/, one file per request, the file itself is
  the request). Provisional overlays are allowed until delivery (docs/architecture.ja.md §3).
- License: MIT (single license, not dual).

## Build and checks

```sh
cargo build
cargo test
cargo clippy --all-targets --all-features   # must be warning-free
cargo fmt
cargo deny check
```

- Workspace lints: `unsafe_code = forbid`, clippy `unwrap_used` / `expect_used` / `panic` /
  `todo` / `unimplemented` = deny (tests may `#[allow]` locally).
- The CLI shape is fixed by docs/cli.ja.md; exit codes and JSON come from `ch32rv-contract`.
- Protocol commands must be capture-verified before implementation
  (docs/protocol/wch-link.ja.md rules; status: verified / attested / conflict / todo).
