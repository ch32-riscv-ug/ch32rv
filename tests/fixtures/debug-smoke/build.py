"""Build the UART-free host transport fixtures with the existing CH32 GCC."""
import argparse
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
gcc_bin = os.environ.get("CH32_GCC_BIN")
if not gcc_bin:
    bins = sorted((root.parent / "ArduinoCore-CH32RV/.tools/xpack-riscv-none-elf-gcc").glob("*/bin"))
    if not bins:
        parser.error("set CH32_GCC_BIN to the directory containing riscv-none-elf-gcc and objcopy")
    gcc_bin = str(bins[-1])
source = Path(__file__).resolve().parent
for channel, name in enumerate(["dmdata", "dmseq", "rtt"]):
    elf = args.out / f"{name}.elf"
    subprocess.run([
        str(Path(gcc_bin) / "riscv-none-elf-gcc"), "-march=rv32ic_zicsr", "-mabi=ilp32",
        "-Os", "-nostdlib", "-nostartfiles", "-ffreestanding", "-fno-builtin",
        "-msmall-data-limit=0", "-Wl,--no-relax", f"-Wl,-T{source / 'link.ld'}",
        f"-DCHANNEL={channel}", str(source / "start.S"), str(source / "smoke.c"),
        "-o", str(elf),
    ], check=True)
    subprocess.run([str(Path(gcc_bin) / "riscv-none-elf-objcopy"), "-O", "binary",
                    str(elf), str(args.out / f"{name}.bin")], check=True)
    print(elf)
