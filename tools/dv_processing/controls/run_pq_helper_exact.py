#!/usr/bin/env python3
"""Bit-exactness of the precise CM4 transfer helpers of dv_dm.h, on the GPU.

usage: run_pq_helper_exact.py DM_SHADER_CONTROL SCRATCH_DIR [unfused-fma|no-precise]

Runs dm_shader_control --pq-helper for dfi_eq_finish_eotf and eqpq_oetf3 over the samples of
pq_helper_port.py and compares every result, bit for bit, with the correctly rounded float32 of
the exact ST 2084 function (60 significant decimal digits, rounded exactly). The helpers promise
that value when fma is fused and `precise` is honoured; see pq_helper_port.py.

Three outcomes, on the last line:
  EXACT    (exit 0)  every result is correctly rounded.
  UNFUSED  (exit 3)  not exact, but bit-identical to the same header evaluated on the CPU with
                     fma as a*b+c: the device (or the mutation) does not fuse fma, and everything
                     else, `precise` included, ran as written. lavapipe (Mesa 25.2) is such a
                     device: it lowers fma to a multiply and an add.
  FAIL     (exit 1)  anything else, including a control that did not run.

The optional mutation makes the control break its copy of the header on purpose (fma as a*b+c,
or `precise` removed). A mutated run must not be EXACT, and without `precise` it must be FAIL;
controls/test_dm_precision.py checks that, so a device on which the check has no power is noticed.
"""
import os
import struct
import subprocess
import sys

import pq_helper_port as port

WIDTH = 128


def gpu(control, work, helper, xs, mutation):
    xs = xs + [0.5] * (-len(xs) % (3 * WIDTH))
    height = len(xs) // (3 * WIDTH)
    raster = bytearray()
    for i in range(0, len(xs), 3):
        raster += struct.pack("<4f", xs[i], xs[i + 1], xs[i + 2], 1.0)
    source, out = (os.path.join(work, "pq_helper_" + helper + s) for s in (".in", ".gpu"))
    with open(source, "wb") as stream:
        stream.write(raster)
    if os.path.exists(out):
        os.remove(out)
    run = subprocess.run([control, "--pq-helper", helper, source, str(WIDTH), str(height), out, *mutation],
                         capture_output=True, text=True)
    if run.returncode:
        print(helper, "FAILED", run.stderr.strip())
        return None
    with open(out, "rb") as stream:
        words = struct.unpack("<%dI" % (4 * WIDTH * height), stream.read())
    return [words[4 * (i // 3) + i % 3] for i in range(len(xs))]


def main():
    if len(sys.argv) not in (3, 4) or (len(sys.argv) == 4 and sys.argv[3] not in port.MODES[1:]):
        print(__doc__, file=sys.stderr)
        return 2
    control, work, mutation = sys.argv[1], sys.argv[2], sys.argv[3:]
    os.makedirs(work, exist_ok=True)
    exact = unfused = True
    try:
        unfused_port = port.build(work, "unfused-fma")
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print("no CPU evaluation of the header (%s): an inexact device cannot be classified" % error)
        unfused_port = None
    for helper, (inputs, reference) in port.HELPERS.items():
        xs = inputs()
        got = gpu(control, work, helper, xs, mutation)
        if got is None:
            exact = unfused = False
            continue
        wrong, worst, sample = port.compare(xs, got, [reference(x) for x in xs])
        line = "%-5s %d samples, %d not correctly rounded, worst %d ULP" % (helper, len(xs), wrong, worst)
        if sample:
            line += "  (input %r: GPU 0x%08x, exact 0x%08x)" % sample
        exact = exact and wrong == 0
        if unfused_port is None:
            unfused = False
        else:
            differing = port.compare(xs, got, unfused_port.run(helper, xs))[0]
            line += "; %d differ from the header with fma as a*b+c" % differing
            unfused = unfused and differing == 0
        print(line)
    verdict = "EXACT" if exact else "UNFUSED" if unfused else "FAIL"
    print(verdict)
    return {"EXACT": 0, "UNFUSED": 3, "FAIL": 1}[verdict]


if __name__ == "__main__":
    sys.exit(main())
