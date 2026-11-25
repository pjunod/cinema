#!/usr/bin/env python3
"""Build the composition shader (fel_compose.h) as a CPU program.

The GLSL of every pass is taken from fel_compose.h's string literals and
compiled unchanged as C++20 (whose signed right shift is GLSL's sign-extending
one) against a small shim of the GLSL it uses: ivec/uvec/vec types with the
swizzles it reads, texelFetch/textureSize over normalised 16-bit planes,
clamp/round and the extended-integer built-ins (umulExtended, imulExtended,
uaddCarry). A driver runs each pass over its target exactly as the GPU stage
dispatches them, storing every result through UNORM16 as a render target
does, and takes the same arguments as fel_compose_control:

    PARAMS BL.yuv EL.yuv FRAME WIDTH HEIGHT EL_WIDTH EL_HEIGHT OUT.raw [interleaved]

so run_fel_compose_parity.py can check the shader's arithmetic against the
reference without a GPU (tests/operations/test_fel_compose_stage.py). Driver
behaviour, precision and descriptor binding remain the GPU control's job.

usage: fel_compose_cpu_port.py OUT_BINARY
"""
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
HEADER = HERE.parent / 'fel_compose.h'
PASSES = ('fel_compose_glsl_header', 'fel_compose_glsl_v_luma', 'fel_compose_glsl_v_chroma',
          'fel_compose_glsl_luma', 'fel_compose_glsl_chroma')

SHIM = r'''
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <vector>
typedef unsigned uint;
struct ivec2 { int x, y; ivec2() : x(0), y(0) {} explicit ivec2(int v) : x(v), y(v) {} ivec2(int a, int b) : x(a), y(b) {} };
struct vec2 { float x, y; };
struct vec4 {
  union { struct { float x, y, z, w; }; struct { float r, g, b, a; }; vec2 rg; vec2 xy; };
  vec4() : x(0), y(0), z(0), w(0) {}
  vec4(float a_, float b_, float c_, float d_) : x(a_), y(b_), z(c_), w(d_) {}
  float operator[](int i) const { return i == 0 ? x : i == 1 ? y : i == 2 ? z : w; }
};
struct ivec4 {
  union { struct { int x, y, z, w; }; struct { ivec2 xy, zw; }; };
  ivec4() : x(0), y(0), z(0), w(0) {}
  int operator[](int i) const { return i == 0 ? x : i == 1 ? y : i == 2 ? z : w; }
};
struct uvec3 { uint x, y, z; uvec3() : x(0), y(0), z(0) {} explicit uvec3(uint v) : x(v), y(v), z(v) {}
               uvec3(uint a, uint b, uint c) : x(a), y(b), z(c) {} };
static inline ivec2 ivec2_of(vec2 v) { return ivec2((int)v.x, (int)v.y); }
static inline ivec2 operator-(ivec2 a, int b) { return ivec2(a.x - b, a.y - b); }
static inline int clamp(int v, int lo, int hi) { return v < lo ? lo : v > hi ? hi : v; }
static inline ivec2 clamp(ivec2 v, ivec2 lo, ivec2 hi) { return ivec2(clamp(v.x, lo.x, hi.x), clamp(v.y, lo.y, hi.y)); }
static inline float round_(float v) { return __builtin_roundf(v); }
static inline void umulExtended(uint a, uint b, uint &msb, uint &lsb) {
  uint64_t p = (uint64_t)a * b; msb = (uint)(p >> 32); lsb = (uint)p; }
static inline void imulExtended(int a, int b, int &msb, int &lsb) {
  int64_t p = (int64_t)a * b; msb = (int)(p >> 32); lsb = (int)(uint32_t)p; }
static inline uint uaddCarry(uint a, uint b, uint &carry) { uint s = a + b; carry = s < a; return s; }
struct sampler2D { const float *data; int w, h, comps; };
static inline vec4 texelFetch(sampler2D t, ivec2 p, int) {
  const float *v = t.data + ((size_t)p.y * t.w + p.x) * t.comps;
  return vec4(v[0], t.comps > 1 ? v[1] : 0.0f, 0.0f, 1.0f); }
static inline ivec2 textureSize(sampler2D t, int) { return ivec2(t.w, t.h); }
static sampler2D bl0, bl1, bl2, el0, el1, el2, ev0, ev1, lut;
static ivec4 fc[FC_U_SIZE];
static vec4 color, gl_FragCoord;
'''

DRIVER = r'''
struct plane { std::vector<float> v; int w, h, comps; sampler2D s() const { return {v.data(), w, h, comps}; } };

static plane make(int w, int h, int comps) { plane p; p.w = w; p.h = h; p.comps = comps; p.v.assign((size_t)w * h * comps, 0.0f); return p; }

// One pass over its target; every result is stored as UNORM16, as on the GPU.
static void run(void (*body)(), plane &target) {
  for (int y = 0; y < target.h; y++)
    for (int x = 0; x < target.w; x++) {
      gl_FragCoord = vec4(x + 0.5f, y + 0.5f, 0.0f, 1.0f);
      body();
      for (int c = 0; c < target.comps; c++) {
        float v = color[c] < 0 ? 0 : color[c] > 1 ? 1 : color[c];
        target.v[((size_t)y * target.w + x) * target.comps + c] = (float)(uint16_t)__builtin_lrintf(v * 65535.0f) / 65535.0f;
      }
    }
}

static void layer(const uint16_t *d, int w, int h, int depth, bool interleaved, plane *planes, sampler2D *s, int *ch) {
  planes[0] = make(w, h, 1);
  for (size_t i = 0; i < (size_t)w * h; i++) planes[0].v[i] = (float)(d[i] << (16 - depth)) / 65535.0f;
  size_t ny = (size_t)w * h, nc = ny / 4;
  if (interleaved) {
    planes[1] = make(w / 2, h / 2, 2);
    for (size_t i = 0; i < nc; i++) {
      planes[1].v[2 * i] = (float)(d[ny + i] << (16 - depth)) / 65535.0f;
      planes[1].v[2 * i + 1] = (float)(d[ny + nc + i] << (16 - depth)) / 65535.0f;
    }
    s[0] = planes[0].s(); s[1] = s[2] = planes[1].s(); ch[0] = 0; ch[1] = 0; ch[2] = 1;
    return;
  }
  for (int c = 1; c < 3; c++) {
    planes[c] = make(w / 2, h / 2, 1);
    for (size_t i = 0; i < nc; i++) planes[c].v[i] = (float)(d[ny + (c - 1) * nc + i] << (16 - depth)) / 65535.0f;
  }
  for (int c = 0; c < 3; c++) { s[c] = planes[c].s(); ch[c] = 0; }
}

int main(int argc, char **argv) {
  if (argc != 10 && !(argc == 11 && !strcmp(argv[10], "interleaved")))
    fail("usage: PARAMS BL.yuv EL.yuv FRAME WIDTH HEIGHT EL_WIDTH EL_HEIGHT OUT.raw [interleaved]");
  bool interleaved = argc == 11;
  struct fel_compose_params p;
  read_params(argv[1], &p);
  int frame = atoi(argv[4]), w = atoi(argv[5]), h = atoi(argv[6]), ew = atoi(argv[7]), eh = atoi(argv[8]);
  const char *why = fel_compose_check(&p);
  if (why) fail(why);
  int scale = ew == w && eh == h ? 1 : 2 * ew == w && 2 * eh == h ? 2 : 0;
  if (!scale || (w & 1) || (h & 1)) fail("CCM 001 composition: EL must be BL size or half of it");
  uint16_t *bl = read_frame(argv[2], frame, w, h), *el = read_frame(argv[3], frame, ew, eh);
  plane blp[3], elp[3];
  sampler2D bls[3], els[3];
  int blc[3], elc[3];
  layer(bl, w, h, p.bl_bit_depth, interleaved, blp, bls, blc);
  layer(el, ew, eh, p.el_bit_depth, interleaved, elp, els, elc);
  bl0 = bls[0]; bl1 = bls[1]; bl2 = bls[2]; el0 = els[0]; el1 = els[1]; el2 = els[2];
  static int32_t u[FC_U_SIZE][4];
  fel_compose_pack(&p, scale, blc, elc, u);
  for (int i = 0; i < FC_U_SIZE; i++) { fc[i].x = u[i][0]; fc[i].y = u[i][1]; fc[i].z = u[i][2]; fc[i].w = u[i][3]; }
  static uint16_t table[3][1024][2];
  fel_compose_table(&p, table);
  plane lutp = make(1024, 3, 2);
  for (int c = 0; c < 3; c++)
    for (int s = 0; s < 1024; s++)
      for (int k = 0; k < 2; k++) lutp.v[((size_t)c * 1024 + s) * 2 + k] = table[c][s][k] / 65535.0f;
  lut = lutp.s();
  plane unused = make(1, 1, 2), vy = make(1, 1, 1), vc = make(1, 1, 2);
  ev0 = unused.s(); ev1 = unused.s();
  if (scale == 2) {
    vy = make(ew, h, 1); vc = make(ew / 2, h / 2, 2);
    run(body_v_luma, vy);
    run(body_v_chroma, vc);
    ev0 = vy.s(); ev1 = vc.s();
  }
  plane y = make(w, h, 1), c = make(w / 2, h / 2, 2);
  run(body_luma, y);
  run(body_chroma, c);
  size_t ny = (size_t)w * h, nc = ny / 4;
  std::vector<uint16_t> out(ny + 2 * nc);
  int shift = 16 - p.hdr_bit_depth;
  for (size_t i = 0; i < ny; i++) out[i] = (uint16_t)__builtin_lrintf(y.v[i] * 65535.0f) >> shift;
  for (size_t i = 0; i < nc; i++) {
    out[ny + i] = (uint16_t)__builtin_lrintf(c.v[2 * i] * 65535.0f) >> shift;
    out[ny + nc + i] = (uint16_t)__builtin_lrintf(c.v[2 * i + 1] * 65535.0f) >> shift;
  }
  FILE *f = fopen(argv[9], "wb");
  if (!f || fwrite(out.data(), 2, out.size(), f) != out.size() || fclose(f)) fail("output write");
  printf("{\"kind\":\"fel_compose_cpu_port\",\"method\":\"%s\",\"width\":%d,\"height\":%d,\"interleaved\":%s}\n",
         FEL_COMPOSE_METHOD, w, h, interleaved ? "true" : "false");
  free(bl); free(el);
  return 0;
}
'''


def glsl(header_text, name):
    """The text of one `static const char name[] = "..." "...";` literal."""
    m = re.search(rf'static const char {name}\[\] =(.*?);\n', header_text, re.S)
    if not m:
        raise ValueError(f'{name} not found in fel_compose.h')
    literals = re.findall(r'^\s*("(?:[^"\\\n]|\\.)*")', m.group(1), re.M)
    return ''.join(json.loads(literal) for literal in literals)


def to_cpp(code):
    """The few GLSL forms that are not already C++ (with the shim)."""
    code = code.replace('inout uvec3 a', 'uvec3 &a')
    code = re.sub(r'int\[8\]\(([^)]*)\)', r'{\1}', code)
    code = code.replace('ivec2(gl_FragCoord.xy)', 'ivec2_of(gl_FragCoord.xy)')
    return re.sub(r'\bround\(', 'round_(', code)


def source(mutation=None):
    """The port's C++ source; mutation=(old, new) edits fel_compose.h first."""
    text = HEADER.read_text()
    if mutation:
        if text.count(mutation[0]) != 1:
            raise ValueError(f'mutation target not unique: {mutation[0]!r}')
        text = text.replace(*mutation)
    parts = {name: to_cpp(glsl(text, name)) for name in PASSES}
    bodies = ''.join(f'static void body_{n[len("fel_compose_glsl_"):]}() {{\n{parts[n]}}}\n' for n in PASSES[1:])
    return ('#define FEL_COMPOSE_CPU_ONLY\n#define FEL_COMPOSE_IO_NAME "fel_compose_cpu_port"\n'
            '#include "fel_compose.h"\n#include "controls/fel_compose_io.h"\n' + SHIM +
            parts['fel_compose_glsl_header'] + bodies + DRIVER)


def build(out, cxx='c++', mutation=None):
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / 'fel_compose_cpu_port.cpp'
        src.write_text(source(mutation))
        subprocess.run([cxx, '-std=c++20', '-O2', '-fwrapv', '-Wall', '-Wno-unused-function',
                        '-Wno-missing-field-initializers', '-Wno-gnu-anonymous-struct', '-Wno-nested-anon-types',
                        '-Wno-pedantic', '-I', str(HERE.parent), str(src), '-o', str(out)], check=True, timeout=300)
    return Path(out)


if __name__ == '__main__':
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    build(sys.argv[1])
