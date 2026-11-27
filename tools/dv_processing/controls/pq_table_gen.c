// Generates dm_pq_tables.h: the segment tables of the CM4 transfer helpers in dv_dm.h.
//
// usage: pq_table_gen OUT_HEADER
//
// Each helper maps a positive float32 x (the helper's own variable, below) to a value through
// one segment of its binade: the binade's entry gives the segment size 2^s floats and the
// first segment, and within a segment x = x_j + tau * width with tau in [0, 1) taken exactly
// from x's low s mantissa bits. A segment holds a degree-6 polynomial in tau,
//   N + c1 tau + c2 tau^2 + ... + c6 tau^6,  with N = N_hi + N_lo and c1 = c1_hi + c1_lo,
// which the shader evaluates as N_hi + c1_hi tau (exact, by fma) plus a small float32 tail.
// The variables, chosen so no subtraction of the shader loses a bit (Sterbenz):
//   EOTF region A (black..2 E0F): u = e - E0F, values scaled by 2^64 (results reach subnormal)
//   EOTF region B (2 E0F..1):     e
//   EOTF region C (1..pole):      v = EPF - e
//   OETF (0..10000 nits):         y, subnormal y normalised by integer operations
// Each binade takes the largest segment size whose fit, evaluated as the shader does, stays
// within 2^-27 of the reference (pq_reference.h) on 257 points a segment, before the shader's
// final rounding, and whose tail stays below N/16. The tables are data: their promise is checked
// on every float32 input by pq_helper_exhaustive.py, not by this program. Another platform's
// libm can change the fitted low bits, so a regenerated header must pass that check again.
//
// Build: cc -O2 -I. pq_table_gen.c -lm (the committed dm_pq_tables.h: macOS arm64 libm).
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "pq_reference.h"
#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

#define DEG 6
#define WIDTH 1024
#define ZERO_KEY 0x35443723u   // EOTF: below this, zero light (unchanged from the precise helpers)
#define POLE_KEY 0x3ffefbaeu   // EOTF: from this on, +inf
#define E0F 0x354436e8u        // float32 nearest e0; region A's origin
#define EPF 0x3ffefbd3u        // float32 nearest ep, above every finite input; region C's origin
#define WHITE_KEY 0x461c4000u  // OETF: 10000 nits
#define SCALE_A 64

static float fval(uint32_t w) { float f; memcpy(&f, &w, 4); return f; }
static uint32_t fbits(float f) { uint32_t w; memcpy(&w, &f, 4); return w; }

enum { EOTF_A, EOTF_B, EOTF_C, OETF_Y, REGIONS };
static const char *region_name[REGIONS] = {"EOTF_A", "EOTF_B", "EOTF_C", "OETF"};
// The region variable's binade range [emin, emax] (biased float32 exponent fields; the OETF's
// subnormal y continue below 1 as virtual fields), its first and last float32 (lo, hi) and
// their positions in the region's (field, mantissa) sequence.
struct region { int emin, emax; uint32_t lo, hi; uint64_t pos_lo, pos_hi; int base; };
static struct region regions[REGIONS];

// The region variable for virtual exponent field e and mantissa m.
static double region_x(int e, uint32_t m) {
  return ldexp(1.0 + m / 8388608.0, e - 127);
}
static double region_value(int region, double x) {
  switch (region) {
  case EOTF_A: return ldexp(pqr_eotf((double)fval(E0F) + x), SCALE_A);
  case EOTF_B: return pqr_eotf(x);
  case EOTF_C: return pqr_eotf((double)fval(EPF) - x);
  default: return pqr_oetf(x);
  }
}
// Relative weight floor: below the smallest normal (region scale applied) error is absolute.
static double region_floor(int region) {
  return region == EOTF_A ? ldexp(1.0, -126 + SCALE_A) : ldexp(1.0, -126);
}

struct segment { float n_hi, n_lo, c1_hi, c1_lo, c[DEG - 1]; };   // c[0] is c2
static struct segment *segments;
static int nsegments, capacity;
static int binade_base[4096], binade_shift[4096], nbinades;

// The shader's evaluation, operation for operation (fma fused), before its final rounding:
// the shader returns s + tail rounded once, so the fit is judged on the exact sum.
static double evaluate(const struct segment *g, float tau) {
  float r = g->c[DEG - 2];
  for (int k = DEG - 3; k >= 0; k--) r = fmaf(r, tau, g->c[k]);
  r = fmaf(r, tau, g->c1_lo);
  float rest = r * tau;
  float ph = g->c1_hi * tau;
  float pl = fmaf(g->c1_hi, tau, -ph);
  float s = g->n_hi + ph;
  float v = s - g->n_hi;
  float e1 = (g->n_hi - (s - v)) + (ph - v);
  float tail = e1 + (g->n_lo + (pl + rest));
  return (double)s + (double)tail;
}

// Weighted least squares on Chebyshev points (Householder QR), degree DEG in tau.
static void fit(int region, double x0, double width, double ta, double tb, double a[DEG + 1]) {
  enum { M = 48 };
  double A[M][DEG + 1], b[M];
  for (int i = 0; i < M; i++) {
    double t = ta + (tb - ta) * 0.5 * (1 - cos(M_PI * (i + 0.5) / M));
    double f = region_value(region, x0 + t * width);
    double w = 1 / fmax(fabs(f), region_floor(region));
    double p = 1;
    for (int k = 0; k <= DEG; k++) { A[i][k] = p * w; p *= t; }
    b[i] = f * w;
  }
  for (int k = 0; k <= DEG; k++) {
    double norm = 0;
    for (int i = k; i < M; i++) norm += A[i][k] * A[i][k];
    norm = sqrt(norm);
    if (norm == 0) continue;
    double alpha = A[k][k] > 0 ? -norm : norm, v[M];
    for (int i = 0; i < M; i++) v[i] = i < k ? 0 : A[i][k];
    v[k] -= alpha;
    double vv = 0;
    for (int i = k; i < M; i++) vv += v[i] * v[i];
    if (vv == 0) continue;
    for (int j = k; j <= DEG; j++) {
      double d = 0;
      for (int i = k; i < M; i++) d += v[i] * A[i][j];
      for (int i = k; i < M; i++) A[i][j] -= 2 * d / vv * v[i];
    }
    double d = 0;
    for (int i = k; i < M; i++) d += v[i] * b[i];
    for (int i = k; i < M; i++) b[i] -= 2 * d / vv * v[i];
  }
  for (int k = DEG; k >= 0; k--) {
    double s = b[k];
    for (int j = k + 1; j <= DEG; j++) s -= A[k][j] * a[j];
    a[k] = A[k][k] != 0 ? s / A[k][k] : 0;
  }
}

static struct segment pack(const double a[DEG + 1]) {
  struct segment g;
  g.n_hi = (float)a[0]; g.n_lo = (float)(a[0] - g.n_hi);
  g.c1_hi = (float)a[1]; g.c1_lo = (float)(a[1] - g.c1_hi);
  for (int k = 2; k <= DEG; k++) g.c[k - 2] = (float)a[k];
  return g;
}

// Fit one segment [m0, m0 + 2^s) of binade e; false if it misses the targets.
static int make_segment(int region, int e, uint32_t m0, int s, struct segment *out) {
  const struct region *r = &regions[region];
  double width = ldexp(1.0, e - 127 - 23 + s), x0 = region_x(e, m0);
  uint64_t first = ((uint64_t)(e - r->emin) << 23) + m0, count = 1ull << s;
  // Valid inputs of this segment, as offsets from its first float.
  uint64_t lo_off = 0, hi_off = count - 1;
  if (first + count - 1 < r->pos_lo || first > r->pos_hi) { memset(out, 0, sizeof(*out)); return 1; }
  if (first < r->pos_lo) lo_off = r->pos_lo - first;
  if (first + count - 1 > r->pos_hi) hi_off = r->pos_hi - first;
  double ta = (double)lo_off / count, tb = (double)hi_off / count;
  // A segment whose largest value rounds to zero light holds zeros.
  double peak = fmax(fabs(region_value(region, x0 + ta * width)), fabs(region_value(region, x0 + tb * width)));
  if (region == EOTF_A && peak < ldexp(1.0, -152 + SCALE_A)) { memset(out, 0, sizeof(*out)); return 1; }
  double a[DEG + 1];
  fit(region, x0, width, ta, tb == ta ? ta + 1.0 / count : tb, a);
  struct segment g = pack(a);
  *out = g;
  const float *words = &g.n_hi;
  for (size_t k = 0; k < sizeof(g) / sizeof(float); k++)
    if (!isfinite(words[k])) return 0;
  int checks = count < 257 ? (int)count : 257;
  for (int i = 0; i < checks; i++) {
    uint64_t off = lo_off + (hi_off - lo_off) * (uint64_t)i / (checks > 1 ? checks - 1 : 1);
    float tau = (float)off / (float)count;
    double f = region_value(region, x0 + (double)off / count * width);
    double got = evaluate(&g, tau);
    double scale = fmax(fabs(f), region_floor(region));
    if (!(fabs(got - f) <= ldexp(scale, -27))) return 0;   // NaN fails too
    double tail = got - g.n_hi - (double)g.c1_hi * tau;
    if (!(fabs(tail) <= fabs(g.n_hi) / 16) && fabs(f) > region_floor(region)) return 0;
  }
  return 1;
}

static void add_segment(struct segment g) {
  if (nsegments == capacity) {
    capacity = capacity ? 2 * capacity : 4096;
    segments = realloc(segments, capacity * sizeof(*segments));
    if (!segments) { perror("realloc"); exit(1); }
  }
  segments[nsegments++] = g;
}

static void build_region(int region) {
  struct region *r = &regions[region];
  r->base = nbinades;
  for (int e = r->emin; e <= r->emax; e++) {
    int s = 23;
    for (;; s--) {
      if (s < 0) { fprintf(stderr, "%s binade %d: no segment size fits\n", region_name[region], e); exit(1); }
      int ok = 1;
      struct segment g;
      for (uint64_t m0 = 0; m0 < (1u << 23) && ok; m0 += 1u << s) ok = make_segment(region, e, (uint32_t)m0, s, &g);
      if (ok) break;
    }
    binade_shift[nbinades] = s;
    binade_base[nbinades] = nsegments;
    nbinades++;
    for (uint64_t m0 = 0; m0 < (1u << 23); m0 += 1u << s) {
      struct segment g;
      make_segment(region, e, (uint32_t)m0, s, &g);
      add_segment(g);
    }
    fprintf(stderr, "%-6s binade %4d: s=%2d, %6d segments\n", region_name[region], e - 127, s, 1 << (23 - s));
  }
}

static int biased(uint32_t w) { return (int)(w >> 23); }

int main(int argc, char **argv) {
  if (argc != 2) { fprintf(stderr, "usage: pq_table_gen OUT_HEADER\n"); return 2; }
  // Region A: e in [ZERO_KEY, 2 E0F), u = e - E0F.
  uint32_t two_e0 = fbits(2 * fval(E0F));
  regions[EOTF_A].lo = fbits(fval(ZERO_KEY) - fval(E0F));
  regions[EOTF_A].hi = fbits(fval(two_e0 - 1) - fval(E0F));
  regions[EOTF_B].lo = two_e0;
  regions[EOTF_B].hi = 0x3f7fffffu;
  regions[EOTF_C].lo = fbits(fval(EPF) - fval(POLE_KEY - 1));
  regions[EOTF_C].hi = fbits(fval(EPF) - 1.0f);
  // OETF: y in (0, 10000); subnormal y have virtual fields top - 22 (from -22).
  for (int k = 0; k < 3; k++) {
    struct region *r = &regions[k];
    r->emin = biased(r->lo); r->emax = biased(r->hi);
    r->pos_lo = r->lo & 0x7fffff;
    r->pos_hi = ((uint64_t)(r->emax - r->emin) << 23) + (r->hi & 0x7fffff);
  }
  regions[OETF_Y].emin = -22; regions[OETF_Y].emax = biased(WHITE_KEY - 1);
  regions[OETF_Y].pos_lo = 0;   // the smallest subnormal: virtual field -22, mantissa 0
  regions[OETF_Y].pos_hi = ((uint64_t)(regions[OETF_Y].emax + 22) << 23) + ((WHITE_KEY - 1) & 0x7fffff);
  for (int k = 0; k < REGIONS; k++) build_region(k);

  int texels = nbinades + 3 * nsegments;
  int rows = (texels + WIDTH - 1) / WIDTH;
  uint32_t *words = calloc((size_t)rows * WIDTH * 4, 4);
  for (int b = 0; b < nbinades; b++) {
    words[4 * b] = fbits((float)(nbinades + 3 * binade_base[b]));
    words[4 * b + 1] = fbits((float)binade_shift[b]);
  }
  for (int i = 0; i < nsegments; i++) {
    const struct segment *g = &segments[i];
    float v[12] = {g->n_hi, g->n_lo, g->c1_hi, g->c1_lo, g->c[0], g->c[1], g->c[2], g->c[3], g->c[4], 0, 0, 0};
    for (int k = 0; k < 12; k++) words[4 * nbinades + 12 * i + k] = fbits(v[k] == 0 ? 0.0f : v[k]);
  }
  FILE *out = fopen(argv[1], "w");
  if (!out) { perror(argv[1]); return 1; }
  fprintf(out, "// Generated by controls/pq_table_gen.c from controls/pq_reference.h; do not edit.\n"
               "// Segment tables of the CM4 transfer helpers (dv_dm.h): %d binades, %d segments.\n"
               "#ifndef PLURX_DM_PQ_TABLES_H\n#define PLURX_DM_PQ_TABLES_H\n#include <stdint.h>\n"
               "#define DM_PQT_WIDTH %d\n#define DM_PQT_HEIGHT %d\n", nbinades, nsegments, WIDTH, rows);
  fprintf(out, "// GLSL constants of the helpers: region origins, bounds and first binade entries.\n"
               "#define DM_PQT_GLSL \\\n");
  fprintf(out, "  \"const uint PQT_ZERO=0x%08xu, PQT_POLE=0x%08xu, PQT_TWO_E0=0x%08xu, PQT_E0F=0x%08xu, PQT_EPF=0x%08xu;\\n\" \\\n",
          ZERO_KEY, POLE_KEY, two_e0, E0F, EPF);
  for (int k = 0; k < REGIONS; k++)
    fprintf(out, "  \"const int PQT_%s=%d;\\n\" \\\n", region_name[k], regions[k].base - regions[k].emin);
  fprintf(out, "  \"const int PQT_WIDTH_LOG2=%d;\\n\"\n", 10);
  fprintf(out, "static const uint32_t dm_pqt_words[%d] = {\n", rows * WIDTH * 4);
  for (int i = 0; i < rows * WIDTH * 4; i++) fprintf(out, "0x%08x,%s", words[i], i % 8 == 7 ? "\n" : "");
  fprintf(out, "};\n#endif\n");
  fclose(out);
  fprintf(stderr, "%d binades, %d segments, %d texels (%d x %d)\n", nbinades, nsegments, texels, WIDTH, rows);
  return 0;
}
