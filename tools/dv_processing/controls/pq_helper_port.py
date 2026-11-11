"""The precise CM4 transfer helpers of dv_dm.h: exact reference, sample inputs and a CPU build.

dfi_eq_finish_eotf (PQ code -> nits, extended above code 1 up to its pole) and eqpq_oetf3
(nits -> PQ code) are split-float (double-word) evaluations of ST 2084. The EOTF rounds its
double-word result to float32 itself; the OETF returns the high word of a normalised double-word.
With a fused fma and `precise` honoured, both equal the correctly rounded float32 of the exact
function on every sample here (the dense ramps, the helpers' branch boundaries, subnormals, values
past both ends and the EOTF's pole). Non-finite inputs are not sampled: the helpers pass NaN (and
the EOTF +inf) to the legacy float functions, which promise no bits.

build() compiles the GLSL text of the header, unchanged, as C++ against a small vec3 shim, so the
same source can be run with a fused fma ("fused"), with fma as a*b+c ("unfused-fma") and with the
compiler free to reassociate, which is what dropping `precise` allows ("no-precise").
Used by tests/operations/test_dv_dm_pq_precision.py (no GPU) and run_pq_helper_exact.py (GPU).
"""
from decimal import Decimal, getcontext
from pathlib import Path
import ctypes
import json
import re
import shutil
import struct
import subprocess

HEADER = Path(__file__).resolve().parents[1] / "dv_dm.h"
MODES = ("fused", "unfused-fma", "no-precise")
INF = 0x7F800000
M1, M2 = Decimal("0.1593017578125"), Decimal("78.84375")
C1, C2, C3 = Decimal("0.8359375"), Decimal("18.8515625"), Decimal("18.6875")
F32_MAX_ROUND = Decimal(2) ** 128 - Decimal(2) ** 103   # at or above this, nearest-even gives +inf


def bits(x):
    return struct.unpack("<I", struct.pack("<f", x))[0]


def value(word):
    return struct.unpack("<f", struct.pack("<I", word))[0]


def round_f32(exact):
    """Bit pattern of the float32 nearest a non-negative exact Decimal."""
    if exact >= F32_MAX_ROUND:
        return INF
    guess = bits(float(exact))
    best = None
    for word in (guess - 1, guess, guess + 1):
        if not 0 <= word < INF:
            continue
        distance = abs(Decimal(value(word)) - exact)   # Decimal(float) is exact
        if best is None or distance < best[0] or (distance == best[0] and word % 2 == 0):
            best = (distance, word)
    return best[1]


def exact_eotf(code):
    """Correctly rounded extended ST 2084 EOTF in nits, as float32 bits; +inf from the pole on."""
    getcontext().prec = 60
    if code <= 0:
        return 0
    p = Decimal(code) ** (1 / M2)
    if p <= C1:
        return 0
    denominator = C2 - C3 * p
    if denominator <= 0:
        return INF
    return round_f32(10000 * ((p - C1) / denominator) ** (1 / M1))


def exact_oetf(nits):
    """Correctly rounded ST 2084 inverse EOTF, as float32 bits."""
    getcontext().prec = 60
    if nits <= 0:
        return round_f32(C1 ** M2)
    if nits >= 10000:
        return bits(1.0)
    p = (Decimal(nits) / 10000) ** M1
    return round_f32(((C1 + C2 * p) / (1 + C3 * p)) ** M2)


def _around(word, reach=3):
    return [value(w) for w in range(word - reach, word + reach + 1)]


def _padded(xs):
    xs = [value(bits(x)) for x in xs]          # the float32 the helper receives
    return xs + [0.5] * (-len(xs) % 3)


def eotf_inputs():
    xs = [0.0, -0.0, -1.0, -1e-30, value(1), value(0x007FFFFF), value(0x00800000), 1e-20, 1e-10]
    for word in (0x35443723, 0x354436E8, bits(0.0012), 0x3F800000, 0x3FFEFBAE, 0x3FFEFBD3, bits(0.5), bits(0.25)):
        xs += _around(word)
    xs += [2.0, 10.0, value(0x7F7FFFFF)]
    xs += [i / 4095 for i in range(4096)]                         # every 12-bit code
    xs += [10 ** (-6.2 + 6.2 * i / 2999) for i in range(3000)]    # black threshold to 1, log-spaced
    xs += [1 + 0.9921 * i / 1499 for i in range(1500)]            # above 1 to the pole
    xs += [value(0x3FFEFBAE - 37 * i) for i in range(1, 400)]     # approaching the pole
    xs += [value(0x35443723 + 61 * i) for i in range(1, 400)]     # leaving black: subnormal results
    return _padded(xs)


def oetf_inputs():
    xs = [0.0, -0.0, -1.0, -1e-30, value(1), value(2), value(3), value(0x00000100), value(0x00400000),
          value(0x007FFFFF), value(0x00800000), 20000.0, value(0x7F7FFFFF), float("inf")]
    for word in (0x461C4000, 0x3F800000, 0x3FB504F3, 0x3F3504F3, bits(100.0), bits(0.005)):
        xs += _around(word)
    xs += [10000 * i / 4095 for i in range(4096)]
    xs += [10 ** (-45 + 49 * i / 3999) for i in range(4000)]      # subnormal light to the peak, log-spaced
    xs += [10 ** (-4 + 8 * i / 1999) for i in range(2000)]        # the range pictures use
    xs += [value(0x461C4000 - 53 * i) for i in range(1, 400)]     # approaching the peak
    return _padded(xs)


HELPERS = {"eotf": (eotf_inputs, exact_eotf), "oetf": (oetf_inputs, exact_oetf)}


def compare(xs, got, want):
    """(count of differing words, worst distance in ULPs, the worst sample or None)."""
    wrong, worst, sample = 0, 0, None
    for x, g, w in zip(xs, got, want):
        if g != w:
            wrong += 1
            # Non-negative float32 patterns are ordered, so their difference counts ULPs.
            ulps = abs(g - w) if g <= INF and w <= INF else INF
            if ulps >= worst:
                worst, sample = ulps, (x, g, w)
    return wrong, worst, sample


SHIM = r"""
#include <cmath>
#include <cstring>
typedef unsigned uint;
#define precise
struct bvec3 { bool x, y, z; bvec3() {} explicit bvec3(bool a) : x(a), y(a), z(a) {} bvec3(bool a, bool b, bool c) : x(a), y(b), z(c) {} };
struct vec3; struct uvec3;
struct ivec3 { int x, y, z; ivec3() {} explicit ivec3(int a) : x(a), y(a), z(a) {} ivec3(int a, int b, int c) : x(a), y(b), z(c) {}
  explicit ivec3(bvec3 b) : x(b.x), y(b.y), z(b.z) {} explicit ivec3(const vec3 &v); explicit ivec3(const uvec3 &v); };
struct uvec3 { uint x, y, z; uvec3() {} explicit uvec3(uint a) : x(a), y(a), z(a) {} uvec3(uint a, uint b, uint c) : x(a), y(b), z(c) {}
  explicit uvec3(ivec3 v) : x((uint)v.x), y((uint)v.y), z((uint)v.z) {} };
struct vec3 { float x, y, z; vec3() {} explicit vec3(float a) : x(a), y(a), z(a) {} vec3(float a, float b, float c) : x(a), y(b), z(c) {}
  explicit vec3(ivec3 v) : x((float)v.x), y((float)v.y), z((float)v.z) {} };
inline ivec3::ivec3(const vec3 &v) : x((int)v.x), y((int)v.y), z((int)v.z) {}
inline ivec3::ivec3(const uvec3 &v) : x((int)v.x), y((int)v.y), z((int)v.z) {}
inline bvec3 bvec3_of(ivec3 v) { return bvec3(v.x != 0, v.y != 0, v.z != 0); }
#define LANES(T, R, name, expr) inline R name(T a, T b) { return R(expr(a.x, b.x), expr(a.y, b.y), expr(a.z, b.z)); }
#define OP(T, op) inline T operator op(T a, T b) { return T(a.x op b.x, a.y op b.y, a.z op b.z); }
#define OPS(T, S, op) OP(T, op) inline T operator op(T a, S b) { return a op T(b); } inline T operator op(S a, T b) { return T(a) op b; }
OPS(vec3, float, +) OPS(vec3, float, -) OPS(vec3, float, *) OPS(vec3, float, /)
OPS(ivec3, int, +) OPS(ivec3, int, -) OPS(ivec3, int, &) OPS(ivec3, int, |)
OPS(uvec3, uint, +) OPS(uvec3, uint, *) OPS(uvec3, uint, &) OPS(uvec3, uint, |) OPS(uvec3, uint, >>) OPS(uvec3, uint, <<)
inline vec3 operator-(vec3 a) { return vec3(-a.x, -a.y, -a.z); }
inline ivec3 operator-(ivec3 a) { return ivec3(-a.x, -a.y, -a.z); }
inline ivec3 &operator+=(ivec3 &a, ivec3 b) { a = a + b; return a; }
inline ivec3 &operator-=(ivec3 &a, ivec3 b) { a = a - b; return a; }
#define CMP(T, name, op) inline bvec3 name(T a, T b) { return bvec3(a.x op b.x, a.y op b.y, a.z op b.z); }
#define CMPS(name, op) CMP(vec3, name, op) CMP(ivec3, name, op) CMP(uvec3, name, op)
CMPS(lessThan, <) CMPS(greaterThan, >) CMPS(greaterThanEqual, >=) CMPS(equal, ==) CMPS(notEqual, !=)
inline bvec3 not_(bvec3 a) { return bvec3(!a.x, !a.y, !a.z); }
#define MIX(T) inline T mix(T a, T b, bvec3 c) { return T(c.x ? b.x : a.x, c.y ? b.y : a.y, c.z ? b.z : a.z); }
MIX(vec3) MIX(ivec3) MIX(uvec3)
inline float uintBitsToFloat(uint w) { float f; std::memcpy(&f, &w, 4); return f; }
inline uint floatBitsToUint(float f) { uint w; std::memcpy(&w, &f, 4); return w; }
inline vec3 uintBitsToFloat(uvec3 w) { return vec3(uintBitsToFloat(w.x), uintBitsToFloat(w.y), uintBitsToFloat(w.z)); }
inline uvec3 floatBitsToUint(vec3 f) { return uvec3(floatBitsToUint(f.x), floatBitsToUint(f.y), floatBitsToUint(f.z)); }
inline vec3 abs(vec3 a) { return vec3(std::fabs(a.x), std::fabs(a.y), std::fabs(a.z)); }
inline vec3 roundEven(vec3 a) { return vec3(std::nearbyint(a.x), std::nearbyint(a.y), std::nearbyint(a.z)); }
inline vec3 ldexp(vec3 a, ivec3 k) { return vec3(std::ldexp(a.x, k.x), std::ldexp(a.y, k.y), std::ldexp(a.z, k.z)); }
inline vec3 frexp(vec3 a, ivec3 &k) { return vec3(std::frexp(a.x, &k.x), std::frexp(a.y, &k.y), std::frexp(a.z, &k.z)); }
inline int findMSB(uint a) { return a ? 31 - __builtin_clz(a) : -1; }
inline ivec3 findMSB(uvec3 a) { return ivec3(findMSB(a.x), findMSB(a.y), findMSB(a.z)); }
#ifdef UNFUSED_FMA
inline vec3 fma(vec3 a, vec3 b, vec3 c) { return a * b + c; }
#else
inline vec3 fma(vec3 a, vec3 b, vec3 c) { return vec3(std::fma(a.x, b.x, c.x), std::fma(a.y, b.y, c.y), std::fma(a.z, b.z, c.z)); }
#endif
inline float max(float a, float b) { return std::fmax(a, b); }
inline float clamp(float x, float a, float b) { return std::fmin(std::fmax(x, a), b); }
inline float pow(float a, float b) { return std::pow(a, b); }
inline float log(float a) { return std::log(a); }
inline float exp(float a) { return std::exp(a); }
"""

EXPORTS = r"""
extern "C" void port_eotf(const float *in, float *out, int triples) {
  for (int i = 0; i < triples; i++) { vec3 r = dfi_eq_finish_eotf(vec3(in[3*i], in[3*i+1], in[3*i+2])); out[3*i] = r.x; out[3*i+1] = r.y; out[3*i+2] = r.z; }
}
extern "C" void port_oetf(const float *in, float *out, int triples) {
  for (int i = 0; i < triples; i++) { vec3 r = eqpq_oetf3(vec3(in[3*i], in[3*i+1], in[3*i+2])); out[3*i] = r.x; out[3*i+1] = r.y; out[3*i+2] = r.z; }
}
"""


def glsl_literals(header, macro=None):
    """The GLSL text of every C string literal in the header, or of one macro's literals."""
    if macro:
        header = re.search(r"^#define %s \\\n((?:.*\\\n)*.*\n)" % macro, header, re.MULTILINE).group(1)
    return "".join(json.loads(literal) for literal in re.findall(r'^\s*("(?:[^"\\\n]|\\.)*")', header, re.MULTILINE))


def glsl_function(shader, signature):
    start, = re.finditer(signature + r"\s*\{", shader)
    depth = 1
    for brace in re.finditer(r"[{}]", shader[start.end():]):
        depth += 1 if brace.group() == "{" else -1
        if depth == 0:
            return shader[start.start():start.end() + brace.end()]
    raise ValueError("unterminated GLSL function " + signature)


def float_literals(glsl):
    # Unsuffixed GLSL floating literals are binary32; C and C++ default to binary64.
    decimal = r"(?<![\w.])((?:\d+\.\d*|\.\d+)(?:[eE][+-]?\d+)?|\d+[eE][+-]?\d+)(?![\w.])"
    return re.sub(decimal, r"\1f", glsl)


def port_source(header):
    """C++ for the header's precise helpers: the GLSL as written, with only its spelling adapted."""
    legacy = "\n".join(glsl_function(glsl_literals(header), r"\bfloat\s+%s\s*\(float\s+\w+\)" % name)
                       for name in ("pq_log1p", "pq_expm1", "pq_eotf_ext", "pq_oetf"))
    precise = glsl_literals(header, "DV_DM_GLSL_CM4_PQ")
    if "fma(" not in precise or "precise " not in precise:
        raise ValueError("the precise helpers no longer use fma and precise")
    precise = re.sub(r"\bnot\(", "not_(", precise)                              # `not` is a C++ token
    precise = re.sub(r"\bbvec3\((ivec3\([^()]*\)[&|]ivec3\([^()]*\))\)", r"bvec3_of(\1)", precise)
    precise = re.sub(r"=uint\[\d+\]\(([^()]*)\);", r"={\1};", precise)          # array constructor
    return SHIM + float_literals(legacy + "\n" + precise) + EXPORTS


class Port:
    def __init__(self, library):
        self.library = ctypes.CDLL(str(library))

    def run(self, helper, xs):
        """float32 bit patterns of the helper for each input (len(xs) a multiple of three)."""
        n = len(xs)
        source, out = (ctypes.c_float * n)(*xs), (ctypes.c_float * n)()
        getattr(self.library, "port_" + helper)(source, out, n // 3)
        return list(struct.unpack("<%dI" % n, bytes(out)))


def build(work, mode, header=None):
    """Compile the helpers for one of MODES into `work` and load them."""
    compiler = shutil.which("c++")
    if not compiler:
        raise RuntimeError("C++ compiler required for the precise PQ helper port")
    work = Path(work)
    source, library = work / ("pq_helper_port_%s.cpp" % mode), work / ("pq_helper_port_%s.so" % mode)
    source.write_text(port_source(HEADER.read_text() if header is None else header))
    # -ffp-contract=off: no fusing beyond what the source asks for, as `precise` requires.
    # -ffast-math: reassociation and contraction allowed, as without `precise`.
    flags = {"fused": ["-ffp-contract=off"], "unfused-fma": ["-ffp-contract=off", "-DUNFUSED_FMA"],
             "no-precise": ["-ffast-math"]}[mode]
    subprocess.run([compiler, "-std=c++17", "-O2", "-fPIC", "-shared", "-Wall", "-Wextra", "-Werror",
                    "-Wno-unused-function", "-Wno-unused-parameter", *flags, str(source), "-o", str(library), "-lm"],
                   check=True)
    return Port(library)
