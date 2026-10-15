# ch32rv-isp

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
