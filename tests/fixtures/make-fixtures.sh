#!/usr/bin/env bash
# Regenerate the deterministic test fixtures. Run from anywhere; writes into this dir.
# The pattern is toolchain-free: byte[i] = i & 0xFF, so a hex dump shows an obvious 00 01 02 .. ramp
# and `ch32rv verify` / readback compares are trivial. Use it for the flash round-trip test
# (backup -> flash pattern -> verify -> restore), NOT as runnable firmware.
set -euo pipefail
cd "$(dirname "$0")"
python3 - <<'PY'
size = 4096
data = bytes(i & 0xFF for i in range(size))
open("pattern-4k.bin", "wb").write(data)
print(f"wrote pattern-4k.bin ({size} bytes)")
PY
