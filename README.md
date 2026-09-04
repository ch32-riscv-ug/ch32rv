# ch32rv

English | [日本語](README.ja.md)

[![crates.io](https://img.shields.io/crates/v/ch32rv.svg)](https://crates.io/crates/ch32rv)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A single CLI (and a set of reusable Rust crates) to **flash and debug WCH CH32 RISC-V
microcontrollers** over WCH-Link / WCH-LinkE. It consolidates functionality otherwise spread across
probe-rs, wlink, minichlink, WCH OpenOCD, WCH-LinkUtility, and wchisp into one tool: programming,
verify/read/write, erase, recovery, option bytes, run-control + a GDB server, runtime monitors, probe
management, a built-in device database, and the Arduino IDE integration protocols.

> **Beta.** The `0.x` line is a beta for downstream projects (e.g. ArduinoCore-CH32) to integrate
> against; the CLI and library APIs may still change before the `1.0` formal release.
>
> **Verified scope.** Exercised end-to-end on a six-board bench — CH32V003, V103, V203, V307, X035,
> and L103. Prebuilt binaries are provided for Linux / macOS / Windows; **Linux x86_64 and Windows
> x86_64 are verified**, macOS and the arm targets are **experimental** (not yet validated on real
> hardware). On Windows it works with WCH's stock driver as installed by WCH-LinkUtility — **no Zadig
> / WinUSB swap needed** (see [Windows](#windows-usb-driver)).

## Install

### From crates.io

```sh
cargo install ch32rv
```

### Prebuilt binaries

Download the archive for your platform from the [Releases] page and put `ch32rv` on your `PATH`.
Each archive ships with a `.sha256` checksum.

### Linux: USB permissions (udev)

Non-root access to the WCH-Link needs a udev rule. The prebuilt tarball bundles `60-ch32rv.rules`:

```sh
sudo cp 60-ch32rv.rules /etc/udev/rules.d/            # from the extracted tarball
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Installed via `cargo install` (no tarball)? Emit the same rule from the binary:

```sh
ch32rv doctor --emit-udev | sudo tee /etc/udev/rules.d/60-ch32rv.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

`ch32rv doctor` diagnoses enumeration, permissions, firmware, and probe mode, and suggests the next step.

### Windows: USB driver

No driver swap is needed. `ch32rv` works with either:

- **WCH's stock driver** (the one WCH-LinkUtility installs) — `ch32rv` reaches the probe through it
  directly, so `ch32rv` and WCH-LinkUtility coexist and **you do not need Zadig**. This path is
  Windows x86_64 only and somewhat slower than WinUSB.
- **WinUSB** — if the probe already presents as a WinUSB device (a clean machine often auto-installs
  it), `ch32rv` uses that. Only reach for Zadig if the probe shows up with no usable driver at all.

`ch32rv` tries WinUSB first and falls back to the stock WCH driver automatically. Run `ch32rv doctor`
if a probe is not found.

## Quick start

```sh
ch32rv probe list                       # find connected WCH-Link probes
ch32rv target info                      # identify the target (SKU / family / wiring / sizes)

ch32rv flash firmware.elf               # program (auto format, erase + verify + reset + run)
ch32rv flash app.bin --offset 0x08000000
ch32rv verify firmware.elf              # compare without writing (mismatch: exit 30)

ch32rv read --range 0x08000000+256 --format hex-dump
ch32rv erase --all
ch32rv reset

ch32rv gdb                              # GDB server on 127.0.0.1:3333 (HW + flash breakpoints)
ch32rv monitor --source dmdata          # stream runtime output (uart / sdi / dmdata)
ch32rv capabilities                     # what this probe + target combination supports
```

Common global options (see `ch32rv --help`): `--probe <selector>` to pick a probe, `--chip <SKU|family>`
to pin the target (auto-detected otherwise, fail-closed on ambiguity), `--json` for machine-readable
output, `--yes` to skip confirmation on destructive operations, and `--dry-run` to plan without opening
a device. Exit codes and the JSON envelope are defined by the `ch32rv-contract` crate.

## Commands

| Command | Purpose |
|---|---|
| `flash` | Program the target with erase / verify / reset / confirm-run policies (`--preverify`, `--restore-unwritten`, `--repeat`, `--sdi`, `--monitor`) |
| `verify` / `read` / `write` | Compare against an image · dump / blank-check · raw memory or flash write |
| `erase` / `reset` | Erase (`--all` / `--region` / `--range`) · reset and run |
| `recover` | Recovery: power-off, NRST, unprotect (mass-erase unbrick of read-protected parts) |
| `probe` | Manage the probe: `list`, `info`, firmware `info` / `check` / `update` (rewrite the probe's own firmware over IAP) / `exit-iap`, `mode get` |
| `target` | `info`, structured `option` bytes (`get` / `set` / `write-raw` / `reset`), `protect` |
| `dbg` / `gdb` | One-shot control (halt / resume / step / regs / reg / dmi) · GDB server |
| `monitor` | Runtime I/O: uart / sdi / dmdata |
| `db` / `capabilities` | Inspect the built-in device DB · probe×firmware×target capability matrix |
| `doctor` / `version` / `complete` | Environment diagnosis · versions · shell completions |
| `arduino` | Arduino IDE integration (`discovery` / `monitor` Pluggable protocols) |

Some routes advertised in `--help` — `run` (HIL), `dap`, `isp`, `boot`, `monitor rtt` — are planned for
a later `0.x` and are not part of the verified surface yet.

## Library crates

`ch32rv` is built from crates that are published independently so other tools can reuse them:

| Crate | What it provides |
|---|---|
| [`ch32rv-contract`](https://crates.io/crates/ch32rv-contract) | Exit codes, JSON result envelope, NDJSON progress events, operation policies |
| [`ch32rv-usb`](https://crates.io/crates/ch32rv-usb) | USB enumeration, probe selectors, per-device locking, transaction capture (nusb) |
| [`ch32rv-wchlink`](https://crates.io/crates/ch32rv-wchlink) | WCH-Link USB protocol (bulk protocol + IAP) |
| [`ch32rv-dmi`](https://crates.io/crates/ch32rv-dmi) | RISC-V Debug Module Interface + direct FLASH-controller access |
| [`ch32rv-target`](https://crates.io/crates/ch32rv-target) | Generated CH32 device DB: chip detection, flash geometry, option-byte layouts |
| [`ch32rv-flash`](https://crates.io/crates/ch32rv-flash) | Erase / program / verify / confirm-run orchestration |
| [`ch32rv-debug`](https://crates.io/crates/ch32rv-debug) | Run control, breakpoints, GDB server |

## Arduino IDE

The Arduino integration is protocol-level: `ch32rv arduino discovery` and `ch32rv arduino monitor`
implement the Pluggable Discovery / Monitor protocols; upload itself is a plain `ch32rv flash`.

## Documentation

The design is spec-first. The specification index is [docs/README.ja.md](docs/README.ja.md) (currently
Japanese; English mains are added as documents stabilize). The release plan is
[docs/release-plan.ja.md](docs/release-plan.ja.md); the changelog is [CHANGELOG.md](CHANGELOG.md)
(English / Japanese paired).

## Build from source

```sh
cargo build
cargo test
cargo clippy --all-targets --all-features   # warning-free
```

On Linux the build needs `libudev-dev` and `pkg-config` (used by the serial monitor):

```sh
sudo apt-get install -y libudev-dev pkg-config
```

## License

[MIT](LICENSE).

[Releases]: https://github.com/ch32-riscv-ug/ch32rv/releases
