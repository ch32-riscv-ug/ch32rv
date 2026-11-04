# ch32rv

A single CLI to **flash and debug WCH CH32 RISC-V microcontrollers** over WCH-Link / WCH-LinkE,
OEP (Open Embedded Probe) probes, and the chips' own bootloaders: programming, verify/read/write, erase, recovery, option bytes, run-control + a GDB server, runtime
monitors, probe management, a built-in device database, and the Arduino IDE integration protocols.

> **Beta.** The `0.x` line is a beta for downstream projects to integrate against; the CLI may still
> change before the `1.0` formal release. Verified end-to-end on the bench with CH32V003, V006,
> V103, V203, V205, V307, X035, X315, L103 and H417. Linux x86_64 and Windows x86_64 are verified;
> macOS and the arm targets are experimental.

## Install

```sh
cargo install ch32rv
```

Prebuilt binaries are also on the [releases page]. On Linux, grant USB access to the probes and bootloaders
with a udev rule:

```sh
ch32rv doctor --emit-udev | sudo tee /etc/udev/rules.d/60-ch32rv.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

## Quick start

```sh
ch32rv probe list                       # find connected debug probes (WCH-Link, OEP)
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

`ch32rv` is built from the `ch32rv-*` crates in its repository (parts of the CLI, with no API or
semver promise of their own). Full documentation, the command
reference, and the release plan are in the [repository].

## License

MIT.

[releases page]: https://github.com/ch32-riscv-ug/ch32rv/releases
[repository]: https://github.com/ch32-riscv-ug/ch32rv
