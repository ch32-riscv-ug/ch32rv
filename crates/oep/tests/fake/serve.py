"""Serve oep-client-python's fake probe (v1 endpoint.Endpoint) on a TCP port for ch32rv's tests.

The spec side's fake is the one "working spec" ch32rv, the Python client and the firmware are checked
against (ArduinoCore-CH32 decision, 2026-09-29), so this file only adds a byte stream around it:

  --framing cobs    COBS + CRC-16, 0x00-delimited, as on a serial port (answers as 0x00 <frame> 0x00)
  --framing length  length(u16) message, as on vendor bulk / TCP
  --noise TEXT      also write TEXT (console bytes) before every answer, to exercise the host's filtering
  --drop N          do not answer the N-th request (1-based) once, to exercise the host's resend
  --profile NAME    p4_x035 (default) or esp32_v003
  --target-id HEX   the WCH DMI 0x7F value attach reports (target_id scheme 1)
  --resume-misses N the first N resumes do not take (a CH32V006 now and then)
  --loader-sim      a run acts like ch32rv's flash loader: copy a2 bytes from a1 to a0, stop at
                    pc + 4 with a0 = 0 (the fake has no flash; this checks the host's procedure)
  --loader-nostart N   the first N loader runs do not start (stop at pc, as a missed resumereq)
  --loader-garble N    the N-th loader run (1-based) writes one wrong word, once

Prints "PORT <n>" on stdout once listening, serves one connection, then exits.
Run with: uv run --project <oep-client-python> python serve.py ...
"""

from __future__ import annotations

import argparse
import socket
import struct
import sys
import time

from oep_client.v1 import cobs, endpoint, fake


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--framing", choices=["cobs", "length"], default="cobs")
    ap.add_argument("--noise", default="")
    ap.add_argument("--drop", type=int, default=0)
    ap.add_argument("--profile", default="p4_x035")
    ap.add_argument("--target-id", type=lambda v: int(v, 0), default=None)
    ap.add_argument("--resume-misses", type=int, default=0)
    ap.add_argument("--loader-sim", action="store_true")
    ap.add_argument("--loader-nostart", type=int, default=0)
    ap.add_argument("--loader-garble", type=int, default=0)
    a = ap.parse_args()

    probe = getattr(fake, a.profile)()
    start = time.monotonic()
    ep = endpoint.Endpoint(probe, lambda: int((time.monotonic() - start) * 1000))
    ep.target_id = a.target_id
    ep.target.resume_misses = a.resume_misses
    if a.loader_sim:
        runs = {"n": 0, "nostart": a.loader_nostart}
        tg = ep.target

        def hook(pc, regs):
            runs["n"] += 1
            if runs["nostart"] > 0:
                runs["nostart"] -= 1
                return True, pc, 5
            dst, src, n = regs.get(0x100A, 0), regs.get(0x100B, 0), regs.get(0x100C, 0)
            for i in range(0, n, 4):
                tg.mem[dst + i] = tg.mem.get(src + i, 0)
            if runs["n"] == a.loader_garble:
                tg.mem[dst] = tg.mem.get(dst, 0) ^ 0x5A5A5A5A
            regs[0x100A] = 0
            return True, pc + 4, 4750

        tg.run_hook = hook

    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    print(f"PORT {srv.getsockname()[1]}", flush=True)
    conn, _ = srv.accept()
    conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

    buf = bytearray()
    seen = 0
    dropped = False

    def answer(msg: bytes) -> None:
        nonlocal seen, dropped
        seen += 1
        if a.drop and seen == a.drop and not dropped:
            dropped = True
            return
        out = ep.handle(msg)
        if out is None:
            return
        wire = a.noise.encode()
        if a.framing == "cobs":
            wire += b"\x00" + cobs.frame(out)
        else:
            wire += struct.pack("<H", len(out)) + out
        conn.sendall(wire)

    while True:
        data = conn.recv(65536)
        if not data:
            break
        buf += data
        if a.framing == "cobs":
            while b"\x00" in buf:
                i = buf.index(b"\x00")
                chunk, buf = bytes(buf[:i]), buf[i + 1:]
                if not chunk:
                    continue
                try:
                    msg = cobs.unframe(chunk)
                except Exception:
                    continue            # console bytes or a broken frame: the probe drops it
                answer(msg)
        else:
            while len(buf) >= 2:
                n = struct.unpack_from("<H", buf)[0]
                if n == 0:
                    del buf[:2]
                    continue
                if len(buf) < 2 + n:
                    break
                msg = bytes(buf[2:2 + n])
                del buf[:2 + n]
                answer(msg)
    sys.exit(0)


if __name__ == "__main__":
    main()
