# ch32rv-boot

> **A part of the [ch32rv](https://crates.io/crates/ch32rv) CLI, published so that `cargo install ch32rv` can build.** Its API is not promised: any release, including a patch release, may change it, and the crate follows no semver promise. The ch32rv binary depends on it with an exact version (`=X.Y.Z`).
>
> ja: ch32rv の CLI の部品。`cargo install ch32rv` のために公開している。API は約束せず(パッチ版でも変わりうる)、semver の約束もしない。

Custom bootloader clients for the [ch32rv](https://github.com/ch32-riscv-ug/ch32rv) tool suite.

The implemented backend programs CH32V003 user flash through the rv003usb/b003fun HID scratchpad
bootloader, including UIAPduino (`1209:b803`) and rv003usb (`1209:b003`). Licensed under MIT.
