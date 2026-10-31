# ch32rv-oep

> **A part of the [ch32rv](https://crates.io/crates/ch32rv) CLI, published so that `cargo install ch32rv` can build.** Its API is not promised: any release, including a patch release, may change it, and the crate follows no semver promise. The ch32rv binary depends on it with an exact version (`=X.Y.Z`).
>
> ja: ch32rv の CLI の部品。`cargo install ch32rv` のために公開している。API は約束せず(パッチ版でも変わりうる)、semver の約束もしない。

OEP v1 (Open Embedded Probe) host side for the [ch32rv](https://github.com/ch32-riscv-ug/ch32rv) tool suite: the wire codec (COBS + CRC-16 framing for serial transports, `length(u16)` framing for vendor bulk / HID / TCP, message headers, TLV) and the number registry generated from oep-spec's `registry/oep-v1.toml`.

Status: in development (docs/oep-host.ja.md). The probe knows nothing about the target, and neither does this crate; the CH32 procedures live in `ch32rv-flash` and the CLI.

License: MIT.
