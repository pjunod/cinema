// Control-only unit test of the closed-form display management (dv_dm.h, dm_closed_form.h) on the
// double-precision CPU reference. Needs no GPU. Checks family and trim selection from the RPU,
// finite in-range output at every bucket, a monotone grey axis, sensitivity to L1, trims that act
// at their target and fade out at the mastering peak, and pinned golden values of the model.
// (The GPU shader is checked against this reference by dm_shader_control / run_dm_parity.py.)
#include <assert.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libdovi/rpu_parser.h>
#include "../dv_dm.h"

typedef struct {
  DoviVdrDmData dm;
  DoviExtMetadataBlockLevel1 l1; DoviExtMetadataBlockLevel3 l3; DoviExtMetadataBlockLevel6 l6;
  DoviExtMetadataBlockLevel9 l9; DoviExtMetadataBlockLevel254 l254;
  DoviExtMetadataBlockLevel2 l2[4]; const DoviExtMetadataBlockLevel2 *l2p[4];
  DoviExtMetadataBlockLevel8 l8[4]; const DoviExtMetadataBlockLevel8 *l8p[4];
} rpu;

static void base(rpu *r, unsigned mn, unsigned avg, unsigned mx) {
  memset(r, 0, sizeof(*r));
  r->l1.min_pq = mn; r->l1.avg_pq = avg; r->l1.max_pq = mx; r->dm.dm_data.level1 = &r->l1;
  r->l6.min_display_mastering_luminance = 50; r->l6.max_display_mastering_luminance = 4000;
  r->dm.dm_data.level6 = &r->l6;
}
static void add_l2(rpu *r, unsigned tmax, unsigned slope, unsigned offset, unsigned power,
                   unsigned cw, unsigned sat, int ms) {
  size_t n = r->dm.dm_data.level2.len;
  DoviExtMetadataBlockLevel2 *x = &r->l2[n];
  x->target_max_pq = tmax; x->trim_slope = slope; x->trim_offset = offset; x->trim_power = power;
  x->trim_chroma_weight = cw; x->trim_saturation_gain = sat; x->ms_weight = (int16_t)ms;
  r->l2p[n] = x;
  r->dm.dm_data.level2.list = (__typeof__(r->dm.dm_data.level2.list))r->l2p;
  r->dm.dm_data.level2.len = n + 1;
}
static void cm4(rpu *r) {
  r->l3.min_pq_offset = r->l3.avg_pq_offset = r->l3.max_pq_offset = 2048; r->dm.dm_data.level3 = &r->l3;
  r->l9.source_primary_index = 0; r->dm.dm_data.level9 = &r->l9;
  r->l254.dm_version_index = 2; r->dm.dm_data.level254 = &r->l254;
}
static void add_l8(rpu *r, unsigned tid, unsigned slope, unsigned offset, unsigned power,
                   unsigned cw, unsigned sat, unsigned sv0) {
  size_t n = r->dm.dm_data.level8.len;
  DoviExtMetadataBlockLevel8 *x = &r->l8[n];
  memset(x, 0, sizeof(*x));
  x->length = 25; x->target_display_index = tid; x->trim_slope = slope; x->trim_offset = offset;
  x->trim_power = power; x->trim_chroma_weight = cw; x->trim_saturation_gain = sat;
  x->ms_weight = 2048; x->target_mid_contrast = 2048; x->clip_trim = 2048;
  x->saturation_vector_field0 = sv0; x->saturation_vector_field1 = x->saturation_vector_field2 =
  x->saturation_vector_field3 = x->saturation_vector_field4 = x->saturation_vector_field5 = 128;
  x->hue_vector_field0 = x->hue_vector_field1 = x->hue_vector_field2 = x->hue_vector_field3 =
  x->hue_vector_field4 = x->hue_vector_field5 = 128;
  r->l8p[n] = x;
  r->dm.dm_data.level8.list = (__typeof__(r->dm.dm_data.level8.list))r->l8p;
  r->dm.dm_data.level8.len = n + 1;
}

static dv_dm_frame prep(const rpu *r, double nits, enum dm_prim prim) {
  dv_dm_frame f;
  const char *why = dv_dm_prepare(&r->dm, nits, 0.005, prim, 4000, &f);
  if (why) { fprintf(stderr, "prepare refused: %s\n", why); exit(1); }
  return f;
}
static float *render(const dv_dm_frame *f, const float *in, int w, int h) {
  float *out = malloc((size_t)w * h * 16);
  assert(out && dv_dm_cpu_render(f, in, w, h, out));
  return out;
}
static float *grey_ramp(int n) {
  float *p = malloc((size_t)n * 16);
  for (int i = 0; i < n; i++) { p[4*i] = p[4*i+1] = p[4*i+2] = (float)i / (n - 1); p[4*i+3] = 1; }
  return p;
}
static float *lattice(int *w, int *h) {     // 9^3 BT.2020/PQ colours, 81 x 9 raster
  *w = 81; *h = 9;
  float *p = malloc(81 * 9 * 16);
  for (int i = 0; i < 729; i++) {
    p[4*i] = (float)(i % 9) / 8; p[4*i+1] = (float)(i / 9 % 9) / 8; p[4*i+2] = (float)(i / 81) / 8; p[4*i+3] = 1;
  }
  return p;
}
static double maxdiff(const float *a, const float *b, int n) {
  double d = 0;
  for (int i = 0; i < 4 * n; i++) d = fmax(d, fabs((double)a[i] - b[i]));
  return d;
}

static void check_range(const char *name, const dv_dm_frame *f, double nits) {
  const double top = dm_pq_oetf(nits) + 1.0 / 1023;
  // grey axis, detail stage off: pointwise curve, monotone and neutral
  dv_dm_frame point = *f; point.weight = -2;
  enum { N = 1024 };
  float *g = grey_ramp(N), *o = render(&point, g, N, 1);
  for (int i = 0; i < N; i++) {
    for (int c = 0; c < 3; c++) {
      double v = o[4*i+c];
      if (!(isfinite(v) && v >= -1e-6 && v <= top)) { fprintf(stderr, "%s grey %d c%d = %g (top %g)\n", name, i, c, v, top); exit(1); }
    }
    if (fabs(o[4*i] - o[4*i+1]) > 2e-4 || fabs(o[4*i+1] - o[4*i+2]) > 2e-4) { fprintf(stderr, "%s grey %d not neutral\n", name, i); exit(1); }
    // monotone to 1e-5 PQ (0.01 of a 10-bit code: float noise at the black clamp)
    if (i && o[4*i+1] < o[4*(i-1)+1] - 1e-5) { fprintf(stderr, "%s grey not monotone at %d\n", name, i); exit(1); }
  }
  if (!(o[4*(N-1)+1] > o[4*(N/2)+1] && o[4*(N/2)+1] > o[1])) { fprintf(stderr, "%s grey axis flat\n", name); exit(1); }
  free(g); free(o);
  // colour lattice with the production detail stage
  int w, h; float *l = lattice(&w, &h), *lo = render(f, l, w, h);
  for (int i = 0; i < 4 * w * h; i++) {
    if (i % 4 == 3) continue;
    if (!(isfinite(lo[i]) && lo[i] >= -1e-6 && lo[i] <= top)) { fprintf(stderr, "%s lattice %d = %g\n", name, i, lo[i]); exit(1); }
  }
  free(l); free(lo);
}

int main(void) {
  static const unsigned buckets[] = {100, 400, 600, 1000, 1600, 2000, 4000};
  rpu c4, c4n, c29, c29n;
  base(&c4, 0, 1600, 3500); cm4(&c4);
  add_l8(&c4, 1, 2300, 2100, 1800, 2300, 1700, 160);
  add_l8(&c4, 48, 1900, 2000, 2200, 1900, 2300, 110);
  add_l8(&c4, 99, 2600, 2600, 2600, 2600, 2600, 200);   // unknown target, no L10: not used
  base(&c4n, 0, 1600, 3500); cm4(&c4n);                   // CM v4.0, L1 only
  base(&c29, 80, 1528, 3728);
  add_l2(&c29, 2081, 2076, 2030, 2000, 2020, 2030, -1);   // 100 nits
  add_l2(&c29, 2851, 2000, 2040, 2150, 2048, 2294, 2048); // 600 nits
  add_l2(&c29, 3079, 2010, 2180, 2075, 2050, 2179, 1800); // 1000 nits
  base(&c29n, 80, 1528, 3728);

  // 1. family and trim selection follow the RPU
  dv_dm_frame f = prep(&c4, 1000, DM_P3D65);
  assert(f.algo == 4 && f.trim_targets == 2 && !strcmp(f.trim, "l8-interpolated") && f.weight == 0);
  f = prep(&c4n, 1000, DM_P3D65);
  assert(f.algo == 4 && f.trim_targets == 0 && !strcmp(f.trim, "l1-only"));
  f = prep(&c29, 1000, DM_P3D65);
  assert(f.algo == 29 && f.trim_targets == 3 && !strcmp(f.trim, "l2-exact"));
  assert(fabs(f.weight - (1800 - 2048) / 2048.0) < 1e-12);
  f = prep(&c29, 1600, DM_P3D65);
  assert(f.algo == 29 && !strcmp(f.trim, "l2-nearest-lower") && fabs(f.weight - (1800 - 2048) / 2048.0) < 1e-12);
  f = prep(&c29, 600, DM_P3D65);
  assert(!strcmp(f.trim, "l2-exact") && f.weight == 0);
  f = prep(&c29, 100, DM_BT709);
  assert(!strcmp(f.trim, "l2-exact") && f.weight == 0);      // raw ms_weight -1 is the default 0
  rpu hi; base(&hi, 80, 1528, 3728); add_l2(&hi, 3079, 2010, 2180, 2075, 2050, 2179, 1800);
  f = prep(&hi, 600, DM_P3D65);
  assert(f.algo == 29 && f.trim_targets == 1 && !strcmp(f.trim, "l1-only") && f.weight == 0);
  f = prep(&c29n, 600, DM_P3D65);
  assert(f.algo == 29 && f.trim_targets == 0 && !strcmp(f.trim, "l1-only") && f.weight == 0);

  // 2. every bucket: finite, inside the target, monotone neutral grey axis
  const rpu *all[] = {&c4, &c4n, &c29, &c29n};
  const char *names[] = {"cm4-l8", "cm4-l1", "cm29-l2", "cm29-l1"};
  for (unsigned k = 0; k < 4; k++)
    for (unsigned b = 0; b < sizeof(buckets) / sizeof(*buckets); b++) {
      char name[64]; snprintf(name, sizeof(name), "%s@%u", names[k], buckets[b]);
      dv_dm_frame x = prep(all[k], buckets[b], buckets[b] == 100 ? DM_BT709 : DM_P3D65);
      check_range(name, &x, buckets[b]);
    }

  // 3. identical pixels, different L1: different output
  int w, h; float *lat = lattice(&w, &h);
  rpu dark; base(&dark, 0, 1000, 3500); cm4(&dark);
  dv_dm_frame fa = prep(&c4n, 1000, DM_P3D65), fb = prep(&dark, 1000, DM_P3D65);
  float *oa = render(&fa, lat, w, h), *ob = render(&fb, lat, w, h);
  assert(maxdiff(oa, ob, w * h) > 1e-3);
  free(ob);

  // 4. trims act at their target and fade out at the mastering peak
  fb = prep(&c4, 1000, DM_P3D65); ob = render(&fb, lat, w, h);
  assert(maxdiff(oa, ob, w * h) > 1e-3);
  free(oa); free(ob);
  fa = prep(&c4n, 4000, DM_P3D65); fb = prep(&c4, 4000, DM_P3D65);
  oa = render(&fa, lat, w, h); ob = render(&fb, lat, w, h);
  assert(maxdiff(oa, ob, w * h) < 1e-6);
  free(oa); free(ob);

  // 5. colour trims interpolate in display size across gamuts, tone trims in PQ: a BT.709 100-nit
  //    trim rendered to a 400-nit P3 display from a P3 4000-nit master (measured weights)
  rpu one; base(&one, 0, 1600, 3500); cm4(&one);
  add_l8(&one, 1, 2048, 2048, 2048, 2048, 2048 + 1024, 128);     // saturation gain +0.5 only
  f = prep(&one, 400, DM_P3D65);
  assert(fabs(f.c4.tr.sg / 0.5 - 0.3206994) < 1e-6);
  one.l8[0].trim_saturation_gain = 2048; one.l8[0].trim_slope = 2048 + 1024;   // tone trim instead
  f = prep(&one, 400, DM_P3D65);
  assert(f.c4.tr.sg == 0 && fabs(f.c4.tr.gain / 0.5 - 0.6337) < 1e-3);

  // 6. pinned golden values of the model (BT.2020/PQ out for a PQ 0.75 grey and a saturated red).
  // They lock the closed form against silent drift; they are model outputs, not vendor measurements.
  float probe[8] = {0.75f, 0.75f, 0.75f, 1, 0.70f, 0.25f, 0.15f, 1};
  const struct { const rpu *r; unsigned nits; enum dm_prim prim; double g[6]; } golden[] = {
    {&c4, 1000, DM_P3D65, {0.5987818, 0.5987818, 0.5987818, 0.5338017, 0.2809092, 0.1723030}},
    {&c4, 400, DM_P3D65, {0.6024639, 0.6024639, 0.6024639, 0.5333900, 0.2804991, 0.1103325}},
    {&c4, 100, DM_BT709, {0.5080784, 0.5080784, 0.5080784, 0.4620758, 0.2713923, 0.1767924}},
    {&c29, 1000, DM_P3D65, {0.6817431, 0.6817437, 0.6817428, 0.6950256, 0.4095575, 0.1703391}},
    {&c29, 100, DM_BT709, {0.4840483, 0.4840527, 0.4840463, 0.4620758, 0.2713923, 0.1767924}},
  };
  int bad = 0;
  for (unsigned k = 0; k < sizeof(golden) / sizeof(*golden); k++) {
    dv_dm_frame x = prep(golden[k].r, golden[k].nits, golden[k].prim);
    x.weight = -2;   // pointwise: the golden values do not depend on neighbours
    float *o = render(&x, probe, 2, 1);
    double got[6] = {o[0], o[1], o[2], o[4], o[5], o[6]};
    printf("golden %u: %.7f, %.7f, %.7f, %.7f, %.7f, %.7f\n", k, got[0], got[1], got[2], got[3], got[4], got[5]);
    for (int c = 0; c < 6; c++) if (fabs(got[c] - golden[k].g[c]) > 2e-6) bad = 1;
    free(o);
  }
  free(lat);
  if (bad) { fprintf(stderr, "golden values moved\n"); return 1; }
  puts("test_dv_dm: PASS");
  return 0;
}
