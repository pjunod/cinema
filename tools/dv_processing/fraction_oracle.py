#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Independent reference for M7 Model B v2's exact FEL composition.

Recomputes every vector refit_exact.h wrote (test_refit_exact PATH) from
libplacebo v7.387.0's composer formula in exact rational arithmetic,
written directly from pl_shader_dovi_reshape and sh_dovi_compose_nlq
(src/shaders/colorspace.c) rather than from the C or Rust integer forms:
inputs clamped to their pivot range and read as code / 2^10, the piece
chosen by code against its pivots, polynomial or MMR of Q23 coefficients,
the mapped value clamped to [0, 1], the LINEAR_DZ residual added, and the
sum rounded to 12 bits. Usage: fraction_oracle.py VECTORS.json
"""
import json
import sys
from fractions import Fraction
from math import floor

Q23 = Fraction(1, 1 << 23)


def mapped(component, x):
    pivots = component["pivots"]
    code = x[component["index"]]
    piece = 0
    while piece + 1 < len(pivots) - 1 and code >= pivots[piece + 1]:
        piece += 1
    p = component["pieces"][piece]
    s = [Fraction(c, 1024) for c in x]
    if component["method"] == "polynomial":
        a = [c * Q23 for c in p["coefficients"]]
        v = s[component["index"]]
        return a[0] + a[1] * v + a[2] * v * v
    terms = [s[0], s[1], s[2], s[0] * s[1], s[0] * s[2], s[1] * s[2], s[0] * s[1] * s[2]]
    value = p["constant"] * Q23
    for order, row in enumerate(p["coefficients"], start=1):
        value += sum(c * Q23 * t ** order for c, t in zip(row, terms))
    return value


def residual(nlq, el):
    d = el - nlq["offset"]
    if d == 0:
        return Fraction(0)
    sign = 1 if d > 0 else -1
    return sign * ((abs(d) - Fraction(1, 2)) * nlq["slope"] + nlq["threshold"]) * Q23


def check(mapping):
    components = mapping["components"]
    for index, component in enumerate(components):
        component["index"] = index
    for sample in mapping["samples"]:
        base, el = sample[0:3], sample[3:6]
        clamped = [min(max(b, c["pivots"][0]), c["pivots"][-1]) for b, c in zip(base, components)]
        for c, component in enumerate(components):
            value = min(max(mapped(component, clamped), Fraction(0)), Fraction(1))
            nlq = component["nlq"]
            r = residual(nlq, el[c])
            # The renderer clamps the residual to vdr_in_max; admission
            # guarantees that never binds, so check rather than apply it.
            assert abs(r) <= nlq["vdr_in_max"] * Q23, (mapping["name"], sample)
            code = min(max(floor((value + r) * 4096 + Fraction(1, 2)), 0), 4095)
            assert code == sample[6 + c], (mapping["name"], sample, c, code)
            assert value * (1 << 113) == int(sample[9 + c]), (mapping["name"], sample, c)
    return len(mapping["samples"])


def main(path):
    with open(path, encoding="utf-8") as handle:
        vectors = json.load(handle)
    total = sum(check(mapping) for mapping in vectors["mappings"])
    print(f"fraction-oracle-pass mappings={len(vectors['mappings'])} samples={total}")


if __name__ == "__main__":
    main(sys.argv[1])
