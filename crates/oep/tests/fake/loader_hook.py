"""ch32rv's RAM loader, played for the fake probe (oep-client-python fake_serve --run-hook).

The fake has no flash and knows no loader; this plays ch32rv's (docs/oep-host.ja.md §5.1): copy a5
pages of a2 bytes from the buffer at a1 to the pages from a0, and stop at the ebreak at pc + 4 with
a0 = 0.
Fault injection, from the environment (fake_serve passes its environment on):

  CH32RV_FAKE_NOSTART=N   the first N runs do not start (stop at pc, as a missed resumereq)
  CH32RV_FAKE_GARBLE=N    the N-th run (1-based, counting the ones that did not start) writes one
                          wrong word, once
  CH32RV_FAKE_RESUME_MISSES=N   the first run also makes the next N resumes not take (a CH32V006
                          now and then), to test the host's resume rule
"""

import os

_runs = {"n": 0, "nostart": int(os.environ.get("CH32RV_FAKE_NOSTART", "0"))}
_GARBLE = int(os.environ.get("CH32RV_FAKE_GARBLE", "0"))
_MISSES = int(os.environ.get("CH32RV_FAKE_RESUME_MISSES", "0"))

A0, A1, A2, A5 = 0x100A, 0x100B, 0x100C, 0x100F


def loader(target, pc, regs):
    _runs["n"] += 1
    if _runs["n"] == 1 and _MISSES:
        target.resume_misses = _MISSES
    if _runs["nostart"] > 0:
        _runs["nostart"] -= 1
        return True, pc, 5
    dst, src = regs.get(A0, 0), regs.get(A1, 0)
    n = regs.get(A2, 0) * max(1, regs.get(A5, 1))
    for i in range(0, n, 4):
        target.mem[dst + i] = target.mem.get(src + i, 0)
    if _runs["n"] == _GARBLE:
        target.mem[dst] = target.mem.get(dst, 0) ^ 0x5A5A5A5A
    regs[A0] = 0
    return True, pc + 4, 4750
