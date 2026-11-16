#!/usr/bin/env python3
"""Linux regression: a serial driver's drain never completes after a write.

Requires cargo and a C compiler (cc). Uses a PTY, never physical hardware.
The ordinary unit test checks frame delivery and the missing reply timeout;
LD_PRELOAD makes any tcdrain call hang, and a process watchdog bounds the test.
This reproduces the host-side failure, not the USB firmware's triggering fault.
"""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile

TEST = "link::tests::serial_confirm_times_out_when_probe_does_not_answer"
ROOT = Path(__file__).resolve().parents[1]
SOURCE = r"""
#include <unistd.h>
int tcdrain(int fd) {
    (void)fd;
    const char message[] = "injected: tcdrain never completes\n";
    write(2, message, sizeof(message) - 1);
    for (;;) pause();
}
"""


def main():
    if not sys.platform.startswith("linux"):
        sys.exit("This fault injection requires Linux LD_PRELOAD.")
    build = subprocess.run(
        ["cargo", "test", "--locked", "-p", "ch32rv-oep", "--lib", "--no-run",
         "--message-format=json"],
        cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
    )
    binaries = []
    for line in build.stdout.splitlines():
        event = json.loads(line)
        if (event.get("reason") == "compiler-artifact"
                and event.get("profile", {}).get("test")
                and event.get("target", {}).get("name") == "ch32rv_oep"
                and event.get("executable")):
            binaries.append(event["executable"])
    if len(binaries) != 1:
        sys.exit(f"Expected one library test binary, found {binaries}")

    with tempfile.TemporaryDirectory(prefix="ch32rv-serial-drain-") as directory:
        source = Path(directory) / "stalled-drain.c"
        library = Path(directory) / "stalled-drain.so"
        source.write_text(SOURCE)
        subprocess.run(["cc", "-shared", "-fPIC", "-Wall", "-Werror",
                        str(source), "-o", str(library)], check=True)
        env = os.environ.copy()
        env["LD_PRELOAD"] = str(library)
        # Check the injection itself, so an inactive preload cannot pass silently.
        control = Path(directory) / "control.py"
        control.write_text("import ctypes; ctypes.CDLL(None).tcdrain(-1)\n")
        for command, is_control in [
            ([sys.executable, str(control)], True),
            ([binaries[0], "--exact", TEST, "--nocapture"], False),
        ]:
            child = subprocess.Popen(command, env=env, start_new_session=True,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            timed_out = False
            try:
                output, _ = child.communicate(timeout=2 if is_control else 5)
            except subprocess.TimeoutExpired:
                timed_out = True
                os.killpg(child.pid, signal.SIGKILL)
                output, _ = child.communicate()
            print(output.decode(errors="replace"), end="")
            if is_control:
                if not timed_out or b"injected: tcdrain" not in output:
                    sys.exit("FAIL: fault injection did not block tcdrain")
            elif timed_out:
                sys.exit("FAIL: serial confirm hung; watchdog killed only the test process")
            elif child.returncode or b"1 passed" not in output:
                sys.exit("FAIL: serial regression did not pass")
    print("PASS: frame delivered and reply timed out with a stalled-drain fault")


if __name__ == "__main__":
    main()
