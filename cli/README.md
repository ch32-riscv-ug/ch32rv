# ch32rv

A single CLI to **flash and debug WCH CH32 RISC-V microcontrollers** over WCH-Link / WCH-LinkE:
programming, verify/read/write, erase, recovery, option bytes, run-control + a GDB server, runtime
monitors, probe management, a built-in device database, and the Arduino IDE integration protocols.

> **Beta.** The `0.x` line is a beta for downstream projects to integrate against; the CLI may still
> change before the `1.0` formal release. Verified end-to-end on a six-board bench (CH32V003 / V103 /
> V203 / V307 / X035 / L103). Linux x86_64 is verified; macOS / Windows binaries are experimental.

## Install

```sh
cargo install ch32rv
```

Prebuilt binaries are also on the [releases page]. On Linux, grant USB access to the WCH-Link with a
udev rule:

```sh
sudo ch32rv doctor --emit-udev | sudo tee /etc/udev/rules.d/60-ch32rv.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Quick start

```sh
ch32rv probe list                       # find connected WCH-Link probes
ch32rv target info                      # identify the target
ch32rv flash firmware.elf               # program (erase + verify + reset + run)
ch32rv verify firmware.elf              # compare without writing (mismatch: exit 30)
ch32rv read --range 0x08000000+256 --format hex
ch32rv erase --all
ch32rv gdb                              # GDB server on 127.0.0.1:3333
ch32rv monitor --source dmdata          # stream runtime output
```

Run `ch32rv --help` for the full command tree and global options (`--probe`, `--chip`, `--json`,
`--yes`, `--dry-run`). Exit codes and the JSON envelope come from the `ch32rv-contract` crate.

## More

`ch32rv` is built from reusable library crates (`ch32rv-wchlink`, `ch32rv-dmi`, `ch32rv-target`,
`ch32rv-flash`, `ch32rv-debug`, `ch32rv-usb`, `ch32rv-contract`). Full documentation, the command
reference, and the release plan are in the [repository].

## License

MIT.

[releases page]: https://github.com/ch32-riscv-ug/ch32rv/releases
[repository]: https://github.com/ch32-riscv-ug/ch32rv
