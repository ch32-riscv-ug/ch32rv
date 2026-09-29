# ch32rv-oep

OEP v1 (Open Embedded Probe) host side for the [ch32rv](https://github.com/ch32-riscv-ug/ch32rv) tool suite: the wire codec (COBS + CRC-16 framing for serial transports, `length(u16)` framing for vendor bulk / HID / TCP, message headers, TLV) and the number registry generated from oep-spec's `registry/oep-v1.toml`.

Status: in development (docs/oep-host.ja.md). The probe knows nothing about the target, and neither does this crate; the CH32 procedures live in `ch32rv-flash` and the CLI.

License: MIT.
