// Control-only unit test of the closed-form display management (dv_dm.h, dm_closed_form.h) on the
// double-precision CPU reference. Needs no GPU. Checks family and trim selection from the RPU,
// finite in-range output at every bucket, a monotone grey axis, sensitivity to L1, trims that act
// at their target and fade out at the mastering peak, and pinned golden values of the model.
// (The GPU shader is checked against this reference by dm_shader_control / run_dm_parity.py.)
#include <assert.h>
#ifdef NDEBUG
#error "test_dv_dm relies on assert() side effects; build without NDEBUG"
#endif
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libdovi/rpu_parser.h>
static void require(bool ok, const char *why) {
  if (!ok) { fprintf(stderr, "refused: %s\n", why); exit(1); }
}
#include "../display_target.h"   // DV_TARGET_TRIMS_CM4/_CM29, and dv_dm.h

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

static dv_dm_frame prep_t(const rpu *r, double nits, enum dm_prim prim, bool trims) {
  dv_dm_frame f;
  const char *why = dv_dm_prepare(&r->dm, nits, 0.005, prim, 4000, trims, trims, &f);
  if (why) { fprintf(stderr, "prepare refused: %s\n", why); exit(1); }
  return f;
}
static dv_dm_frame prep(const rpu *r, double nits, enum dm_prim prim) { return prep_t(r, nits, prim, true); }
// trims on, for a title mastered at `smast` nits
static dv_dm_frame prep_m(const rpu *r, double nits, enum dm_prim prim, double smast) {
  dv_dm_frame f;
  const char *why = dv_dm_prepare(&r->dm, nits, 0.005, prim, smast, true, true, &f);
  if (why) { fprintf(stderr, "prepare refused: %s\n", why); exit(1); }
  return f;
}
// as served: each family's trims as display_target.h applies them
static dv_dm_frame prep_served(const rpu *r, double nits, enum dm_prim prim) {
  dv_dm_frame f;
  const char *why = dv_dm_prepare(&r->dm, nits, 0.005, prim, 4000, DV_TARGET_TRIMS_CM4, DV_TARGET_TRIMS_CM29, &f);
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

// level 2: range, neutral, monotone (1e-5 PQ), not flat; 1: range, neutral, monotone to 1e-4 PQ
// (0.1 of a 10-bit code: float noise on the flat curves of degenerate frames); 0: range only
static void check_range(const char *name, const dv_dm_frame *f, double nits, int level) {
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
    if (level >= 1 && (fabs(o[4*i] - o[4*i+1]) > 2e-4 || fabs(o[4*i+1] - o[4*i+2]) > 2e-4)) { fprintf(stderr, "%s grey %d not neutral\n", name, i); exit(1); }
    // monotone to 1e-5 PQ (0.01 of a 10-bit code: float noise at the black clamp)
    if (level >= 1 && i && o[4*i+1] < o[4*(i-1)+1] - (level >= 2 ? 1e-5 : 1e-4)) { fprintf(stderr, "%s grey not monotone at %d\n", name, i); exit(1); }
  }
  if (level >= 2 && !(o[4*(N-1)+1] > o[4*(N/2)+1] && o[4*(N/2)+1] > o[1])) { fprintf(stderr, "%s grey axis flat\n", name); exit(1); }
  free(g); free(o);
  // colour lattice with the production detail stage
  int w, h; float *l = lattice(&w, &h), *lo = render(f, l, w, h);
  for (int i = 0; i < 4 * w * h; i++) {
    if (i % 4 == 3) continue;
    if (!(isfinite(lo[i]) && lo[i] >= -1e-6 && lo[i] <= top)) { fprintf(stderr, "%s lattice %d = %g\n", name, i, lo[i]); exit(1); }
  }
  free(l); free(lo);
}

// The analytic gamut maps black planes to neutral black for every source chroma.
// Exercise the actual pixel and finish functions with the pre-gamut intensity at black.
static void black_plane_is_neutral(const rpu *c29) {
  const struct { double nits; enum dm_prim prim; double grid_nits, decoded_nits; } t[] = {
    {600, DM_P3D65, 0, 0}, {2000, DM_P3D65, 0, 0}, {1000, DM_BT2020, 0, 0},
    {100, DM_BT709, 0.005, 0.005}, {1000, DM_BT709, 0.005, 0.005},
    {100, DM_BT709, 0.005, 0.0005}, {100, DM_BT709, 0.005, 0.0001},
  };
  const double probes[][3] = {{0.018, 0, 0}, {0, 0.018, 0}, {0, 0, 0.018}, {0.015, 0.008, 0}};
  for (unsigned k = 0; k < sizeof(t) / sizeof(*t); k++) {
    dv_dm_frame f;
    const double black = t[k].prim == DM_BT709 ? t[k].decoded_nits : 0.005;
    require(dv_dm_prepare(&c29->dm, t[k].nits, black, t[k].prim, 4000, true, true, &f) == NULL,
            "black-plane target prepares");
    const double io = t[k].grid_nits > 0 ? dm_pq_oetf(t[k].grid_nits) : 0, expected = dm_pq_oetf(t[k].decoded_nits);
    double reference[3] = {0, 0, 0};
    for (unsigned i = 0; i < sizeof(probes) / sizeof(*probes); i++) {
      dm_cm29_pix p; double out[3];
      dm_cm29_pixel_pre(&f.c29, probes[i], &p);
      dm_cm29_finish(&f.c29, &p, p.Ib, io, out);
      for (int c = 0; c < 3; c++) {
        if (!isfinite(out[c]) || fabs(out[c] - expected) > 2e-6) {
          fprintf(stderr, "black plane %g nits prim %d: probe %u channel %d = %.9g\n",
                  t[k].nits, t[k].prim, i, c, out[c]); exit(1);
        }
      }
      if (i == 0) memcpy(reference, out, sizeof(reference));
      else for (int c = 0; c < 3; c++) if (fabs(out[c] - reference[c]) > 2e-6) {
        fprintf(stderr, "black plane depends on source chroma: %g nits prim %d\n", t[k].nits, t[k].prim); exit(1);
      }
    }
  }
  // Ordinary midtones retain bounded color through the full renderer path.
  const struct { double nits; enum dm_prim prim; } mid[] = {{600, DM_P3D65}, {1000, DM_BT2020}, {100, DM_BT709}};
  const float in[4] = {0.50f, 0.30f, 0.25f, 1};
  for (unsigned k = 0; k < sizeof(mid) / sizeof(*mid); k++) {
    dv_dm_frame f = prep(c29, mid[k].nits, mid[k].prim); f.weight = -2;
    float *out = render(&f, in, 1, 1);
    double a[3] = {in[0], in[1], in[2]}, b[3] = {out[0], out[1], out[2]};
    dm_cm29_pix pi, po; dm_cm29_pixel_pre(&f.c29, a, &pi); dm_cm29_pixel_pre(&f.c29, b, &po);
    const double top = dm_pq_oetf(mid[k].nits);
    if (!(po.C > 0 && isfinite(po.C)) || !isfinite(b[0]) || !isfinite(b[1]) || !isfinite(b[2]) ||
        b[0] < 0 || b[1] < 0 || b[2] < 0 || b[0] > top || b[1] > top || b[2] > top) {
      fprintf(stderr, "mid-tone color invalid %g nits prim %d: C %.9g RGB %.9g %.9g %.9g\n",
              mid[k].nits, mid[k].prim, po.C, b[0], b[1], b[2]); exit(1);
    }
    free(out);
  }
}

// The retained P7 fixture (controls/tags/p7-frame0.nal) through the real parser: the same
// selection the Rust tests pin (FIXTURE_DM in dv_runtime.rs), so the C and Rust family rules are
// checked on identical bytes.
static const DoviVdrDmData *read_rpu(const char *path, DoviRpuOpaque **out) {
  FILE *fp = fopen(path, "rb"); assert(fp);
  static uint8_t buf[1 << 16]; size_t len = fread(buf, 1, sizeof(buf), fp); fclose(fp);
  assert(len > 0 && len < sizeof(buf));
  DoviRpuOpaque *raw = dovi_parse_unspec62_nalu(buf, len);
  assert(raw && !dovi_rpu_get_error(raw));
  const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(raw);
  assert(dm);
  *out = raw;
  return dm;
}
// algo, trim and count with trims on; served output keeps them only for the families that apply trims
static void check_fixture(const char *path, int algo, const char *trim, int n, double weight) {
  DoviRpuOpaque *raw; const DoviVdrDmData *dm = read_rpu(path, &raw);
  dv_dm_frame f;
  assert(dv_dm_prepare(dm, 1000, 0.005, DM_P3D65, 4000, true, true, &f) == NULL);
  if (!(f.algo == algo && !strcmp(f.trim, trim) && f.trim_targets == n && fabs(f.weight - weight) < 1e-12)) {
    fprintf(stderr, "%s: trims-on selection algo %d %s %d %g differs from the Rust pin\n", path, f.algo, f.trim, f.trim_targets, f.weight);
    exit(1);
  }
  assert(dv_dm_prepare(dm, 1000, 0.005, DM_P3D65, 4000, DV_TARGET_TRIMS_CM4, DV_TARGET_TRIMS_CM29, &f) == NULL);
  (void)dv_target;
  bool served = algo == 4 ? DV_TARGET_TRIMS_CM4 : DV_TARGET_TRIMS_CM29;
  const char *served_trim = served ? trim : "l1-only";
  assert(f.algo == algo && !strcmp(f.trim, served_trim) && f.trim_targets == (served ? n : 0) &&
         fabs(f.weight - (served ? weight : 0)) < 1e-12);
  dovi_rpu_free_vdr_dm_data(dm);
  dovi_rpu_free(raw);
  printf("fixture %s: cm%d-%s served cm%d-%s\n", path, algo, trim, algo, served_trim);
}

int main(int argc, char **argv) {
  if (argc != 2) { fprintf(stderr, "usage: test_dv_dm tools/dv_processing/controls/tags\n"); return 2; }
  // the selections the Rust tests pin on the same committed bytes (dv_runtime.rs FIXTURE_DM,
  // display_target.rs display_management_selection_follows_the_raw_rpu_exactly)
  char path[4096];
  snprintf(path, sizeof(path), "%s/p7-frame0.nal", argv[1]); check_fixture(path, 4, "l1-only", 0, 0);
  snprintf(path, sizeof(path), "%s/cm4-l8-trims.nal", argv[1]); check_fixture(path, 4, "l8-interpolated", 2, 0);
  snprintf(path, sizeof(path), "%s/cm29-l2-trims.nal", argv[1]);
  check_fixture(path, 29, "l2-exact", 2, (1800 - 2048) / 2048.0);
  {
    // the interpolated selections display_target.rs pins on the same bytes
    DoviRpuOpaque *raw; const DoviVdrDmData *dm = read_rpu(path, &raw);
    const double nits[] = {600, 1600, 2000};
    const double pin[] = {-0.09346839266980962, -0.07994135942825827, -0.060392435898949166};
    for (int i = 0; i < 3; i++) {
      dv_dm_frame f;
      assert(dv_dm_prepare(dm, nits[i], 0.005, DM_P3D65, 4000, true, true, &f) == NULL);
      assert(f.algo == 29 && !strcmp(f.trim, "l2-interpolated") && f.trim_targets == 2 && fabs(f.weight - pin[i]) < 1e-12);
    }
    dovi_rpu_free_vdr_dm_data(dm);
    dovi_rpu_free(raw);
  }
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
  {
    dv_dm_frame g = prep_m(&c4, 1000, DM_P3D65, 1000);   // CM v4 at the mastering peak: faded out
    assert(g.algo == 4 && !strcmp(g.trim, "l1-only") && g.trim_targets == 0);
    g = prep_m(&c4, 600, DM_P3D65, 1000);
    assert(g.algo == 4 && !strcmp(g.trim, "l8-interpolated") && g.trim_targets == 2);
  }
  f = prep(&c29, 1600, DM_P3D65);   // between the 1000-nit trim and neutral at the 4000-nit master, in PQ
  assert(f.algo == 29 && !strcmp(f.trim, "l2-interpolated") && fabs(f.weight - -0.07994135942825827) < 1e-12);
  f = prep(&c29, 600, DM_P3D65);
  assert(!strcmp(f.trim, "l2-exact") && f.weight == 0);
  f = prep(&c29, 100, DM_BT709);
  assert(!strcmp(f.trim, "l2-exact") && f.weight == 0);      // raw ms_weight -1 is the default 0
  rpu hi; base(&hi, 80, 1528, 3728); add_l2(&hi, 3079, 2010, 2180, 2075, 2050, 2179, 1800);
  f = prep(&hi, 600, DM_P3D65);    // below the lowest L2 target: faded towards neutral at PQ 0
  assert(f.algo == 29 && f.trim_targets == 1 && !strcmp(f.trim, "l2-interpolated") &&
         fabs(f.weight - -0.1121395094623157) < 1e-12);
  f = prep(&hi, 4000, DM_P3D65);   // at the mastering peak the trims have faded out
  assert(f.algo == 29 && !strcmp(f.trim, "l1-only") && f.weight == 0);
  // a title mastered at 1000 nits (display_target.rs pins the same): the 100-nit trim fades to
  // neutral at 1000; the 3079 trim lies above PQ(1000), so at 600 nothing is left to interpolate
  f = prep_m(&hi, 600, DM_P3D65, 1000);
  assert(f.algo == 29 && !strcmp(f.trim, "l1-only") && f.weight == 0 && f.trim_targets == 0);
  f = prep_m(&hi, 1000, DM_P3D65, 1000);   // an L2 authored for the mastering peak is not applied at it
  assert(f.algo == 29 && !strcmp(f.trim, "l1-only") && f.weight == 0 && f.trim_targets == 0);
  {
    rpu low; base(&low, 80, 1528, 3728); add_l2(&low, 2081, 2010, 2180, 2075, 2050, 2179, 1800);
    f = prep_m(&low, 600, DM_P3D65, 1000);
    assert(f.algo == 29 && !strcmp(f.trim, "l2-interpolated") && fabs(f.weight - -0.02760024702986694) < 1e-12);
    f = prep_m(&low, 600, DM_P3D65, 4000);   // the same trim fades over a longer span on a 4000-nit master
    assert(!strcmp(f.trim, "l2-interpolated") && f.weight < -0.06);
  }
  f = prep(&c29n, 600, DM_P3D65);
  assert(f.algo == 29 && f.trim_targets == 0 && !strcmp(f.trim, "l1-only") && f.weight == 0);

  // 2. every bucket: finite, inside the target, monotone neutral grey axis
  const rpu *all[] = {&c4, &c4n, &c29, &c29n};
  const char *names[] = {"cm4-l8", "cm4-l1", "cm29-l2", "cm29-l1"};
  for (unsigned k = 0; k < 4; k++)
    for (unsigned b = 0; b < sizeof(buckets) / sizeof(*buckets); b++) {
      char name[64]; snprintf(name, sizeof(name), "%s@%u", names[k], buckets[b]);
      dv_dm_frame x = prep(all[k], buckets[b], buckets[b] == 100 ? DM_BT709 : DM_P3D65);
      check_range(name, &x, buckets[b], 2);
    }
  // CM v2.9 grey neutrality and monotonicity hold across all target primaries and peak classes.
  {
    static const unsigned peaks[] = {100, 600, 1000, 2000};
    static const enum dm_prim prims[] = {DM_BT709, DM_P3D65, DM_BT2020};
    for (unsigned p = 0; p < sizeof(peaks) / sizeof(*peaks); p++)
      for (unsigned c = 0; c < sizeof(prims) / sizeof(*prims); c++) {
        char name[64]; snprintf(name, sizeof(name), "cm29-neutral@%u-prim%d", peaks[p], prims[c]);
        dv_dm_frame x = prep(&c29, peaks[p], prims[c]);
        check_range(name, &x, peaks[p], 2);
      }
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

  // 6. served output applies both families' trims (identical to trims on); with no trim in the RPU
  //    it is the untrimmed mapping
  {
    int w2, h2; float *l2 = lattice(&w2, &h2);
    assert(DV_TARGET_TRIMS_CM4 && DV_TARGET_TRIMS_CM29);
    dv_dm_frame s4 = prep_served(&c4, 400, DM_P3D65), t4 = prep(&c4, 400, DM_P3D65);
    assert(s4.algo == 4 && s4.trim_targets == 2 && !strcmp(s4.trim, "l8-interpolated"));
    float *a1 = render(&s4, l2, w2, h2), *b1 = render(&t4, l2, w2, h2);
    assert(maxdiff(a1, b1, w2 * h2) == 0);
    free(a1); free(b1);
    dv_dm_frame s29 = prep_served(&c29, 1000, DM_P3D65), t29 = prep(&c29, 1000, DM_P3D65);
    assert(s29.algo == 29 && strcmp(s29.trim, "l1-only") && s29.trim_targets == t29.trim_targets);
    a1 = render(&s29, l2, w2, h2); b1 = render(&t29, l2, w2, h2);
    assert(maxdiff(a1, b1, w2 * h2) == 0);
    free(a1); free(b1);
    dv_dm_frame s29n = prep_served(&c29n, 1000, DM_P3D65), u29 = prep_t(&c29, 1000, DM_P3D65, false);
    assert(s29n.algo == 29 && !strcmp(s29n.trim, "l1-only") && s29n.trim_targets == 0);
    a1 = render(&s29n, l2, w2, h2); b1 = render(&u29, l2, w2, h2);
    assert(maxdiff(a1, b1, w2 * h2) == 0);
    free(a1); free(b1); free(l2);
  }

  // 7. black, flat, clipped and nonsensical metadata still map (finite, in range) instead of refusing
  {
    static const unsigned l1s[][3] = {{0, 0, 0}, {1000, 1000, 1000}, {4095, 4095, 4095}, {0, 0, 4095}, {0, 4095, 4095},
                                      {62, 62, 63}, {4094, 4095, 4095}};
    for (unsigned k = 0; k < sizeof(l1s) / sizeof(*l1s); k++)
      for (int fam = 0; fam < 2; fam++)
        for (int b = 0; b < 3; b++) {
          static const unsigned nb[3] = {100, 600, 4000};
          rpu d; base(&d, l1s[k][0], l1s[k][1], l1s[k][2]);
          if (fam) {
            cm4(&d);
            if (l1s[k][1] >= 4000) d.l3.avg_pq_offset = 2048 + 100;   // avg + L3 beyond the top code
            add_l8(&d, 48, 2048, 2048, 2048, 2048, 2048, 128);
            d.l8[0].clip_trim = 0;                                    // most negative highlight clipping
            add_l8(&d, 1, 0, 0, 0, 0, 4095, 255);                     // slope/offset/power codes 0
          } else {
            add_l2(&d, 2081, 0, 0, 0, 4095, 4095, 4095);
          }
          char name[64]; snprintf(name, sizeof(name), "degenerate%u-%s@%u", k, fam ? "cm4" : "cm29", nb[b]);
          dv_dm_frame x = prep(&d, nb[b], nb[b] == 100 ? DM_BT709 : DM_P3D65);
          check_range(name, &x, nb[b], 0);                          // nonsensical trims: range only
          x = prep_t(&d, nb[b], nb[b] == 100 ? DM_BT709 : DM_P3D65, false);
          check_range(name, &x, nb[b], 1);   // untrimmed: range, neutral and monotone (no pole near black)
        }
  }

  // 8. dark CM v2.9 shots: a fine near-black ramp (where a pole would sit) stays monotone and dark
  {
    static const unsigned dark[][3] = {{0, 0, 16}, {0, 0, 8}, {10, 10, 26}, {0, 4, 32}};
    enum { M = 2048 };
    float *ramp = malloc((size_t)M * 16);
    for (int i = 0; i < M; i++) { ramp[4*i] = ramp[4*i+1] = ramp[4*i+2] = 0.02f * i / (M - 1); ramp[4*i+3] = 1; }
    for (unsigned k = 0; k < sizeof(dark) / sizeof(*dark); k++) {
      rpu d; base(&d, dark[k][0], dark[k][1], dark[k][2]);
      dv_dm_frame x = prep_served(&d, 600, DM_P3D65); x.weight = -2;
      float *o = render(&x, ramp, M, 1);
      for (int i = 1; i < M; i++)
        if (o[4*i+1] < o[4*(i-1)+1] - 1e-5 || dm_pq_eotf(o[4*i+1]) > 1.0) {
          fprintf(stderr, "dark shot %u: pole or downturn at %d (%g)\n", k, i, o[4*i+1]); exit(1);
        }
      free(o);
    }
    free(ramp);
  }

  // 8b. Black planes stay neutral across chromatic source probes and target classes.
  black_plane_is_neutral(&c29);

  // 9. Pinned CM29 model outputs for float32 PQ probes [0.75,0.75,0.75] and
  // [0.62,0.45,0.38]. Reproduced from dm_closed_form.h SHA-256
  // 68719148b379a948e7db324289778fa22cfca5ba218a0efebb4b583ff13715f3 with L1
  // [80,1528,3728]/4095, source 4000/.005, ms=-1, and the exact L2 codes below.
  // CM4 pins above remain unchanged; all values here are model outputs, not vendor measurements.
  float probe[8] = {0.75f, 0.75f, 0.75f, 1, 0.62f, 0.45f, 0.38f, 1};
  const struct { const rpu *r; unsigned nits; enum dm_prim prim; double g[6]; } golden[] = {
    {&c4, 1000, DM_P3D65, {0.5987818, 0.5987818, 0.5987818, 0.4802569, 0.3451082, 0.2869328}},
    {&c4, 400, DM_P3D65, {0.6024639, 0.6024639, 0.6024639, 0.5226588, 0.3718402, 0.3081956}},
    {&c4, 100, DM_BT709, {0.5080784, 0.5080784, 0.5080784, 0.4660859, 0.3247151, 0.2794885}},
    {&c29, 1000, DM_P3D65, {0.6817444, 0.6817445, 0.6817446, 0.6095730, 0.4369125, 0.3669640}}, // L2 2010/2180/2075/2050/2179
    {&c29, 100, DM_BT709, {0.4840604, 0.4840600, 0.4840603, 0.4678732, 0.3429843, 0.2862816}}, // L2 2076/2030/2000/2020/2030
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
