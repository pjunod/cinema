#!/usr/bin/env python3
"""Every float32 input of the CM4 transfer helpers of dv_dm.h, against the exact ST 2084 functions.

usage: pq_helper_exhaustive.py WORK_DIR [fused|unfused-fma ...]

Compiles the helpers' GLSL, unchanged, as C++ (pq_helper_port.py) for each fma mode and runs them
on every non-negative finite float32 (2^31 - 2^23 inputs a helper; negative inputs take the same
black lanes as -0). Each result is compared with the float32 nearest the double-precision
reference of pq_reference.h. Where that reference lies within its error bound (2^-36 relative)
of a float32 rounding midpoint, the nearest float32 is not decided by it: those inputs are
listed and decided with the 60-digit Decimal reference of pq_helper_port.py instead.

The helpers promise at most 1 ULP from the correctly rounded value on every input, fused or
unfused; the report gives, per helper and mode, how many inputs are correctly rounded, how many
are 1 ULP away and the worst distance. Exit 0 only if no input is further than 1 ULP.
"""
import os
import subprocess
import sys
from pathlib import Path

import pq_helper_port as port

HERE = Path(__file__).resolve().parent

SCAN = r"""
#include <cstdint>
#include <cstdio>
#include <thread>
#include <vector>
#include <atomic>
#include "pq_reference.h"

struct Tally { uint64_t exact = 0, one = 0, more = 0, ambiguous = 0; uint32_t worst = 0, worst_input = 0; };

static uint32_t wbits(float f) { uint32_t w; std::memcpy(&w, &f, 4); return w; }
static float wfloat(uint32_t w) { float f; std::memcpy(&f, &w, 4); return f; }

// The float32 nearest ref, or 0xffffffff if ref is too close to a rounding midpoint to say.
static uint32_t nearest(double ref) {
  if (ref == 0) return 0;
  if (std::isinf(ref)) return 0x7f800000u;
  float want = (float)ref;
  double lo = want == 0 ? 0 : (double)std::nextafterf(want, 0.0f);
  double hi = std::isinf(want) || want == FLT_MAX ? std::ldexp(2.0, 127) : (double)std::nextafterf(want, INFINITY);
  if (std::isinf(want)) lo = FLT_MAX;
  double bound = PQR_BOUND * ref;
  double below = ((double)want + lo) / 2, above = std::isinf(want) ? std::ldexp(2.0, 127) - std::ldexp(1.0, 103) : ((double)want + hi) / 2;
  if (std::isinf(want)) below = std::ldexp(2.0, 127) - std::ldexp(1.0, 103);
  if (std::fabs(ref - below) <= bound || std::fabs(ref - above) <= bound) return 0xffffffffu;
  return wbits(want);
}

template <class Helper, class Reference>
static void scan(const char *name, Helper helper, Reference reference, FILE *ambiguous) {
  unsigned threads = std::thread::hardware_concurrency();
  if (!threads) threads = 4;
  const uint32_t end = 0x7f800000u;
  std::vector<Tally> tallies(threads);
  std::vector<std::vector<uint32_t>> held(threads);
  std::vector<std::thread> pool;
  for (unsigned t = 0; t < threads; t++)
    pool.emplace_back([&, t] {
      Tally &tally = tallies[t];
      for (uint64_t w = t; w < end; w += threads) {
        float x = wfloat((uint32_t)w);
        uint32_t got = wbits(helper(x));
        uint32_t want = nearest(reference((double)x));
        if (want == 0xffffffffu) { tally.ambiguous++; held[t].push_back((uint32_t)w); held[t].push_back(got); continue; }
        uint32_t d = got > want ? got - want : want - got;
        if (got > 0x7f800000u) d = 0xffffffffu;   // NaN
        if (d == 0) tally.exact++; else if (d == 1) tally.one++; else tally.more++;
        if (d > tally.worst) { tally.worst = d; tally.worst_input = (uint32_t)w; }
      }
    });
  for (auto &thread : pool) thread.join();
  Tally sum;
  for (auto &tally : tallies) {
    sum.exact += tally.exact; sum.one += tally.one; sum.more += tally.more; sum.ambiguous += tally.ambiguous;
    if (tally.worst > sum.worst || (tally.worst == sum.worst && tally.worst_input < sum.worst_input)) { sum.worst = tally.worst; sum.worst_input = tally.worst_input; }
  }
  for (auto &list : held)
    for (size_t i = 0; i < list.size(); i += 2) std::fprintf(ambiguous, "%s %08x %08x\n", name, list[i], list[i + 1]);
  std::printf("%s exact %llu one %llu more %llu ambiguous %llu worst %u at %08x\n", name,
              (unsigned long long)sum.exact, (unsigned long long)sum.one, (unsigned long long)sum.more,
              (unsigned long long)sum.ambiguous, sum.worst, sum.worst_input);
}

int main(int argc, char **argv) {
  if (argc != 2) return 2;
  FILE *ambiguous = std::fopen(argv[1], "w");
  if (!ambiguous) return 1;
  scan("eotf", [](float x) { return pqt_eotf(x); }, [](double x) { return pqr_eotf(x); }, ambiguous);
  scan("oetf", [](float x) { return pqt_oetf(x); }, [](double x) { return pqr_oetf(x); }, ambiguous);
  return std::fclose(ambiguous) ? 1 : 0;
}
"""


def build(work, mode):
    source, binary = (work / ("pq_helper_exhaustive_%s.%s" % (mode, suffix)) for suffix in ("cpp", "bin"))
    source.write_text(port.port_source(port.HEADER.read_text()).replace("#include <cmath>", "#include <cmath>\n#include <cfloat>", 1) + SCAN)
    flags = {"fused": ["-ffp-contract=off"], "unfused-fma": ["-ffp-contract=off", "-DUNFUSED_FMA"]}[mode]
    subprocess.run(["c++", "-std=c++17", "-O2", "-pthread", "-Wall", "-Wextra", "-Werror", "-Wno-unused-function",
                    "-Wno-unused-parameter", "-I", str(port.TABLES.parent), "-I", str(HERE), *flags,
                    str(source), "-o", str(binary), "-lm"], check=True)
    return binary


def main():
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    work = Path(sys.argv[1])
    work.mkdir(parents=True, exist_ok=True)
    modes = sys.argv[2:] or ["fused", "unfused-fma"]
    failed = False
    for mode in modes:
        binary = build(work, mode)
        listing = work / ("pq_helper_ambiguous_%s.txt" % mode)
        run = subprocess.run([str(binary), str(listing)], capture_output=True, text=True)
        if run.returncode:
            print(mode, "scan failed:", run.stderr.strip())
            failed = True
            continue
        decided = {"eotf": [0, 0, 0, 0, None], "oetf": [0, 0, 0, 0, None]}   # exact, one, more, worst, input
        for line in listing.read_text().split("\n"):
            if not line:
                continue
            helper, w, got = line.split()
            w, got = int(w, 16), int(got, 16)
            want = (port.exact_eotf if helper == "eotf" else port.exact_oetf)(port.value(w))
            d = abs(got - want) if got <= port.INF else 1 << 32
            row = decided[helper]
            row[min(d, 2)] += 1
            if d > row[3]:
                row[3], row[4] = d, w
        for line in run.stdout.strip().split("\n"):
            name, _, exact, _, one, _, more, _, ambiguous, _, worst, _, at = line.split()
            row = decided[name]
            exact, one, more, worst = int(exact) + row[0], int(one) + row[1], int(more) + row[2], max(int(worst), row[3])
            where = at if int(worst) >= row[3] else "%08x" % row[4]
            print("%-11s %s: %d inputs, %d correctly rounded, %d at 1 ULP, %d further; worst %d ULP (input 0x%s);"
                  " %s decided exactly" % (mode, name, exact + one + more, exact, one, more, worst, where, ambiguous))
            failed = failed or more > 0
    print("FAIL" if failed else "PASS")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
