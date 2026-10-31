# ch32rv-isp

> **A part of the [ch32rv](https://crates.io/crates/ch32rv) CLI, published so that `cargo install ch32rv` can build.** Its API is not promised: any release, including a patch release, may change it, and the crate follows no semver promise. The ch32rv binary depends on it with an exact version (`=X.Y.Z`).
>
> ja: ch32rv の CLI の部品。`cargo install ch32rv` のために公開している。API は約束せず(パッチ版でも変わりうる)、semver の約束もしない。

WCH factory ISP bootloader client for the [ch32rv](https://github.com/ch32-riscv-ug/ch32rv) tool
suite. Licensed under MIT.

This crate talks to the bootloader WCH keeps in the chip's BOOT area over USB bulk
(`1a86:55e0` / `4348:55e0`, EP `0x02` / `0x82`): Identify, ReadConfig / WriteConfig (option
bytes), the XOR key, Erase, Program, Verify and IspEnd. It is bench-verified on CH32X035F8U6
(bootloader version 2.60); byte-level notes are in the repository's
`docs/protocol/factory-isp.ja.md`.

Most users should use the CLI:

```text
ch32rv isp flash firmware.elf --entry auto --app-device port:<serial port>
```

The protocol is implemented in-house; wchisp (GPL-2.0) is not vendored. The UART transport is
not implemented yet.
