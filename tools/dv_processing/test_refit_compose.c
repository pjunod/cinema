// SPDX-License-Identifier: Apache-2.0
// CPU-only checks of refit_compose.h: the FEL composition, the lattice and
// pairing, the statistics and the scene pooling. With a path argument it also
// writes the composer vectors of the deidentified nonidentity P7 mapping
// (controls/p7_mapping_fixture.h), which plurx-core's dv_refit tests replay
// through ComposerCurves::reshape.
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libavutil/mem.h>
#include "refit_compose.h"
#include "controls/p7_mapping_fixture.h"

static struct refit_mapping identity(uint16_t offset, uint64_t slope, uint64_t threshold) {
  struct refit_mapping m;
  memset(&m, 0, sizeof(m));
  for (int c = 0; c < 3; c++) {
    m.curve[c].num_pivots = 2;
    m.curve[c].pivots[1] = 1023;
    m.curve[c].poly[0][1] = 1.0;
    m.nlq[c] = (struct refit_nlq){offset, slope, threshold};
  }
  return m;
}

// The frozen four_model_audit.composition12: clip(4B + c), c = d for d > 0,
// d + 1 for d < 0, 0 at d = 0, with d = EL - 512.
static int frozen(int base, int el) {
  int d = el - 512, c = d > 0 ? d : d < 0 ? d + 1 : 0, v = 4 * base + c;
  return v < 0 ? 0 : v > 4095 ? 4095 : v;
}

// The fixtures' NLQ (offset 512, slope 2048/2^23, threshold 0) on identity
// curves reduces the general formula exactly to the frozen integer one.
static void general_formula_reduces_to_the_frozen_composition(void) {
  struct refit_mapping m = identity(512, 2048, 0);
  for (int b = 0; b <= 1023; b++)
    for (int e = 0; e <= 1023; e++) {
      const uint16_t base[3] = {b, b, b}, el[3] = {e, e, e};
      uint16_t out[3];
      refit_compose(&m, base, el, out);
      for (int c = 0; c < 3; c++)
        assert(out[c] == frozen(b, e));
    }
}

static int64_t floor_div(int64_t a, int64_t b) {
  int64_t q = a / b;
  return (a % b != 0 && (a < 0) != (b < 0)) ? q - 1 : q;
}

// Any admitted LINEAR_DZ parameters, exactly: with m = sign(d)((2|d|-1)S + 2T),
// (B/1024 + m/2^24)·4096 + 1/2 = (16384B + m + 2048) / 4096.
static void general_residual_is_exact(void) {
  const uint16_t offsets[] = {0, 1, 511, 512, 1022, 1023};
  const uint64_t slopes[] = {0, 1, 512, 2048, 8191, (UINT64_C(1) << 23) - 1};
  const uint64_t thresholds[] = {0, 1, 2048, (UINT64_C(1) << 23) - 1};
  const int bases[] = {0, 1, 300, 511, 512, 1022, 1023};
  for (size_t o = 0; o < sizeof(offsets) / sizeof(*offsets); o++)
    for (size_t s = 0; s < sizeof(slopes) / sizeof(*slopes); s++)
      for (size_t t = 0; t < sizeof(thresholds) / sizeof(*thresholds); t++) {
        struct refit_mapping m = identity(offsets[o], slopes[s], thresholds[t]);
        for (size_t i = 0; i < sizeof(bases) / sizeof(*bases); i++)
          for (int e = 0; e <= 1023; e++) {
            int64_t d = e - offsets[o];
            int64_t mag = d ? (2 * (d < 0 ? -d : d) - 1) * (int64_t)slopes[s] +
                                  2 * (int64_t)thresholds[t]
                            : 0;
            int64_t num = d < 0 ? -mag : mag;
            int64_t want = floor_div(16384 * (int64_t)bases[i] + num + 2048, 4096);
            want = want < 0 ? 0 : want > 4095 ? 4095 : want;
            const uint16_t base[3] = {bases[i], bases[i], bases[i]}, el[3] = {e, e, e};
            uint16_t out[3];
            refit_compose(&m, base, el, out);
            assert(out[0] == want && out[1] == want && out[2] == want);
          }
      }
}

// Pieces are chosen by code against the signalled pivots, and every input
// is clamped to its component's pivot range first.
static void pieces_and_pivot_clamps(void) {
  struct refit_mapping m = identity(512, 0, 0);
  struct refit_curve *k = &m.curve[0];
  k->num_pivots = 4;
  const uint16_t pivots[] = {64, 300, 700, 960};
  memcpy(k->pivots, pivots, sizeof(pivots));
  const double poly[3][3] = {{0.01, 0.9, 0.05}, {0.02, 0.8, 0.1}, {-0.03, 1.1, -0.02}};
  memcpy(k->poly, poly, sizeof(poly));
  for (int code = 0; code <= 1023; code++) {
    int c = code < 64 ? 64 : code > 960 ? 960 : code;
    int piece = c >= 700 ? 2 : c >= 300 ? 1 : 0;
    double x = c / 1024.0;
    double s = poly[piece][0] + poly[piece][1] * x + poly[piece][2] * x * x;
    s = s < 0 ? 0 : s > 1 ? 1 : s;
    const uint16_t base[3] = {code, 512, 512}, el[3] = {512, 512, 512};
    uint16_t out[3];
    refit_compose(&m, base, el, out);
    double v = floor(s * 4096 + 0.5);
    assert(out[0] == (v > 4095 ? 4095 : (uint16_t)v));
  }
  assert(refit_piece(k, 299) == 0 && refit_piece(k, 300) == 1 && refit_piece(k, 699) == 1 &&
         refit_piece(k, 700) == 2 && refit_piece(k, 960) == 2);
}

static double dyadic(int k) { return k / 256.0; }

// Independent evaluation of one piece in long double: the MMR written out
// term by term with powl.
static long double direct_mmr(const struct refit_curve *k, int piece, const long double x[3]) {
  long double y = x[0], u = x[1], v = x[2], s = k->mmr_constant[piece];
  for (int order = 1; order <= k->mmr_order[piece]; order++) {
    const long double t[7] = {powl(y, order),     powl(u, order),     powl(v, order),
                              powl(y * u, order), powl(y * v, order), powl(u * v, order),
                              powl(y * u * v, order)};
    for (int j = 0; j < 7; j++)
      s += k->mmr[piece][order - 1][j] * t[j];
  }
  return s < 0 ? 0 : s > 1 ? 1 : s;
}
// The composed code of a mapped value, or -1 within 1e-9 of a rounding tie,
// where two correct evaluation orders may round differently.
static int round12(long double s) {
  long double v = s * 4096 + 0.5L, f = v - floorl(v);
  if (f < 1e-9L || f > 1 - 1e-9L)
    return -1;
  return v >= 4096 ? 4095 : (int)floorl(v);
}

// MMR of orders 1, 2 and 3 in one multi-piece chroma curve, and a
// polynomial chroma curve with interior pivots, against direct evaluation.
static void mmr_orders_pieces_and_polynomial_chroma(void) {
  struct refit_mapping m = identity(512, 0, 0);
  struct refit_curve *cb = &m.curve[1], *cr = &m.curve[2];
  cb->num_pivots = 4;
  const uint16_t cb_pivots[] = {16, 300, 640, 1000};
  memcpy(cb->pivots, cb_pivots, sizeof(cb_pivots));
  for (int i = 0; i < 3; i++) {
    cb->method[i] = REFIT_MMR;
    cb->mmr_order[i] = i + 1;
    cb->mmr_constant[i] = dyadic(3 + i);
    for (int o = 0; o <= i; o++)
      for (int j = 0; j < 7; j++)
        cb->mmr[i][o][j] = dyadic((j % 2 ? -1 : 1) * (17 * (o + 1) + 5 * j + 9 * i) % 97);
  }
  cr->num_pivots = 3;
  const uint16_t cr_pivots[] = {64, 512, 960};
  memcpy(cr->pivots, cr_pivots, sizeof(cr_pivots));
  const double cr_poly[2][3] = {{dyadic(2), dyadic(250), dyadic(3)}, {dyadic(-5), dyadic(270), dyadic(-9)}};
  memcpy(cr->poly, cr_poly, sizeof(cr_poly));
  unsigned checked = 0;
  for (int y = 0; y <= 1023; y += 31)
    for (int u = 0; u <= 1023; u += 7)
      for (int v = 0; v <= 1023; v += 29) {
        const uint16_t base[3] = {y, u, v}, el[3] = {512, 512, 512};
        uint16_t out[3];
        refit_compose(&m, base, el, out);
        double mapped[3];
        refit_map(&m, base, mapped);
        // Clamp each input to its own component's pivot range first.
        int cu = u < 16 ? 16 : u > 1000 ? 1000 : u, cv = v < 64 ? 64 : v > 960 ? 960 : v;
        const long double x[3] = {y / 1024.0L, cu / 1024.0L, cv / 1024.0L};
        int piece = cu >= 640 ? 2 : cu >= 300 ? 1 : 0;
        long double want_cb = direct_mmr(cb, piece, x);
        int p = cv >= 512 ? 1 : 0;
        long double xv = cv / 1024.0L;
        long double want_cr = cr_poly[p][0] + cr_poly[p][1] * xv + cr_poly[p][2] * xv * xv;
        want_cr = want_cr < 0 ? 0 : want_cr > 1 ? 1 : want_cr;
        assert(fabsl(mapped[1] - want_cb) < 1e-12L && fabsl(mapped[2] - want_cr) < 1e-12L);
        int code_cb = round12(want_cb), code_cr = round12(want_cr);
        assert(code_cb < 0 || out[1] == code_cb);
        assert(code_cr < 0 || out[2] == code_cr);
        assert(out[0] == round12(y / 1024.0L));
        checked++;
      }
  assert(checked > 5000);
}

static void write_number(FILE *f, double v, bool comma) {
  assert(isfinite(v));
  fprintf(f, comma ? ",%.17g" : "%.17g", v);
}

// The nonidentity mapping through the decoder's public metadata struct, as
// the helper takes it, and its composition with a zero residual.
static void nonidentity_p7_mapping(const char *vectors) {
  size_t size;
  AVDOVIMetadata *data = av_dovi_metadata_alloc(&size);
  assert(data);
  p7_mapping_fixture(data);
  struct refit_mapping m;
  assert(refit_mapping_from_dovi(data, &m) == NULL);
  assert(m.curve[0].num_pivots == 9 && m.curve[1].method[0] == REFIT_MMR &&
         m.curve[2].mmr_order[0] == 3);
  // The same metadata with an unsupported method is refused.
  av_dovi_get_mapping(data)->curves[1].mapping_idc[0] = 7;
  assert(refit_mapping_from_dovi(data, &m) != NULL);
  p7_mapping_fixture(data);
  av_dovi_get_header(data)->coef_log2_denom = 22;
  assert(refit_mapping_from_dovi(data, &m) != NULL);
  p7_mapping_fixture(data);
  assert(refit_mapping_from_dovi(data, &m) == NULL);
  av_free(data);
  if (!vectors)
    return;
  FILE *f = fopen(vectors, "wx");
  assert(f);
  fputs("{\"source\":\"tools/dv_processing/controls/p7_mapping_fixture.h through "
        "refit_compose.h; regenerate with test_refit_compose PATH\",\"luma\":[", f);
  for (int i = 0; i < 8; i++) {
    fputs(i ? ",[" : "[", f);
    for (int j = 0; j < 3; j++)
      write_number(f, m.curve[0].poly[i][j], j);
    fputc(']', f);
  }
  fputs("],\"chroma\":[", f);
  for (int c = 1; c < 3; c++) {
    fputs(c > 1 ? ",[" : "[", f);
    write_number(f, m.curve[c].mmr_constant[0], false);
    for (int p = 0; p < 3; p++)
      for (int t = 0; t < 7; t++)
        write_number(f, m.curve[c].mmr[0][p][t], true);
    fputc(']', f);
  }
  fputs("],\"samples\":[", f);
  bool first = true;
  const uint16_t el[3] = {512, 512, 512};
  const uint16_t pairs[3][2] = {{512, 512}, {100, 900}, {1023, 0}};
  for (int p = 0; p < 3; p++)
    for (int y = 0; y <= 1023; y++) {
      const uint16_t base[3] = {y, pairs[p][0], pairs[p][1]};
      uint16_t out[3];
      refit_compose(&m, base, el, out);
      fprintf(f, "%s[%u,%u,%u,%u,%u,%u]", first ? "" : ",", base[0], base[1], base[2], out[0],
              out[1], out[2]);
      first = false;
    }
  for (int a = 0; a <= 8; a++)
    for (int b = 0; b <= 8; b++)
      for (int c = 0; c <= 8; c++) {
        const uint16_t base[3] = {a * 1023 / 8, b * 1023 / 8, c * 1023 / 8};
        uint16_t out[3];
        refit_compose(&m, base, el, out);
        fprintf(f, ",[%u,%u,%u,%u,%u,%u]", base[0], base[1], base[2], out[0], out[1], out[2]);
      }
  // A nonzero residual on the same mapping, with other LINEAR_DZ parameters.
  for (int c = 0; c < 3; c++)
    m.nlq[c] = (struct refit_nlq){500, 3000, 1000};
  fputs("],\"residual\":{\"nlq\":[500,3000,1000],\"samples\":[", f);
  first = true;
  for (int a = 0; a <= 1023; a += 73)
    for (int e = 0; e <= 1023; e += 31) {
      const uint16_t base[3] = {a, (a * 7 + 100) % 1024, (a * 3 + 900) % 1024};
      const uint16_t enhancement[3] = {e, (e + 300) % 1024, (1023 - e)};
      uint16_t out[3];
      refit_compose(&m, base, enhancement, out);
      fprintf(f, "%s[%u,%u,%u,%u,%u,%u,%u,%u,%u]", first ? "" : ",", base[0], base[1], base[2],
              enhancement[0], enhancement[1], enhancement[2], out[0], out[1], out[2]);
      first = false;
    }
  fputs("]}}\n", f);
  assert(fclose(f) == 0);
}

static void lattice_and_pairing(void) {
  for (int start = 0; start < 20; start++)
    for (int end = start; end < start + 40; end++)
      for (int phase = 0; phase < 8; phase += 4) {
        int n = 0;
        for (int x = start + phase; x < end; x += REFIT_STRIDE)
          n++;
        assert(refit_axis_count(start, end, phase) == n);
      }
  // The frozen pairing for a half-resolution EL, including its clamps.
  struct refit_sites s = refit_pair(3839, 2159, 3840, 2160);
  assert(s.bl_chroma[0] == 1919 && s.bl_chroma[1] == 1079);
  assert(s.el_luma[0] == 1919 && s.el_luma[1] == 1079);
  assert(s.el_chroma[0] == 959 && s.el_chroma[1] == 539);
  s = refit_pair(5, 7, 3840, 2160);
  assert(s.bl_chroma[0] == 3 && s.bl_chroma[1] == 4 && s.el_luma[0] == 3 &&
         s.el_luma[1] == 3 && s.el_chroma[0] == 1 && s.el_chroma[1] == 2);
}

static void statistics(void) {
  static struct refit_stats a, b;
  const uint16_t samples[3][2][3] = {{{100, 400, 600}, {420, 1700, 2300}},
                                     {{100, 512, 512}, {390, 2048, 2048}},
                                     {{1023, 0, 1023}, {4095, 0, 4095}}};
  for (int i = 0; i < 3; i++)
    refit_stats_add(&a, samples[i][0], samples[i][1]);
  refit_stats_finish_frame(&a);
  assert(a.ycnt[100] == 2 && a.ysum[100] == 810 && a.ysq[100] == 420.0 * 420 + 390.0 * 390);
  assert(a.xtx[0] == 3 && refit_stats_samples(&a) == 3);
  for (int i = 0; i < REFIT_TERMS; i++)
    for (int j = 0; j < REFIT_TERMS; j++)
      assert(a.xtx[i * REFIT_TERMS + j] == a.xtx[j * REFIT_TERMS + i]);
  assert(fabs(a.xty[0] - (1700 + 2048 + 0) / 4095.0) < 1e-15);
  assert(fabs(a.xty[1] - (2300 + 2048 + 4095) / 4095.0) < 1e-15);
  refit_stats_merge(&b, &a);
  refit_stats_merge(&b, &a);
  assert(b.xtx[0] == 6 && b.ycnt[1023] == 2);
  FILE *f = tmpfile();
  assert(f && refit_stats_json(f, &a));
  long length = ftell(f);
  rewind(f);
  char *text = malloc((size_t)length + 1);
  assert(text && fread(text, 1, (size_t)length, f) == (size_t)length);
  text[length] = 0;
  fclose(f);
  assert(!strncmp(text, "{\"ysum\":[0,", 11));
  assert(strstr(text, "],\"ycnt\":[") && strstr(text, "],\"xtx_upper\":[3,") &&
         strstr(text, "],\"xty\":[") && strstr(text, "],\"ysq\":[") &&
         strstr(text, "],\"csq\":[") && strstr(text, "],\"second_moments\":true}"));
  // Every value round-trips through its %.17g text.
  const char *xty = strstr(text, "\"xty\":[") + 7;
  assert(strtod(xty, NULL) == a.xty[0]);
  free(text);
  a.xty[0] = NAN;
  f = tmpfile();
  assert(f && !refit_stats_json(f, &a));
  fclose(f);
}

static int64_t emitted[64][2];
static int emitted_count;
static bool collect(const struct refit_scene *scene, void *unused) {
  (void)unused;
  emitted[emitted_count][0] = scene->first_frame;
  emitted[emitted_count][1] = scene->frames;
  assert(scene->first_pts == 1000 + 42 * scene->first_frame &&
         scene->end_pts == 1000 + 42 * (scene->first_frame + scene->frames) &&
         scene->fit.ycnt[0] == (double)scene->frames);
  emitted_count++;
  return true;
}
static void pool(const int64_t *lengths, int n, const int64_t (*want)[2], int wanted) {
  static struct refit_pool p;
  static struct refit_scene s;
  memset(&p, 0, sizeof(p));
  emitted_count = 0;
  int64_t at = 0;
  for (int i = 0; i < n; i++) {
    memset(&s, 0, sizeof(s));
    s.first_frame = at;
    s.frames = lengths[i];
    s.first_pts = 1000 + 42 * at;
    s.end_pts = 1000 + 42 * (at + lengths[i]);
    s.fit.ycnt[0] = (double)lengths[i];
    at += lengths[i];
    assert(refit_pool_close(&p, &s, collect, NULL));
  }
  assert(refit_pool_finish(&p, collect, NULL));
  assert(p.rpu_scenes == n && p.emitted == wanted && emitted_count == wanted);
  for (int i = 0; i < wanted; i++)
    assert(emitted[i][0] == want[i][0] && emitted[i][1] == want[i][1]);
}
static void scene_pooling(void) {
  // One scene; a short first scene pools forward; a short last one back.
  pool((const int64_t[]){30}, 1, (const int64_t[][2]){{0, 30}}, 1);
  pool((const int64_t[]){10, 30}, 2, (const int64_t[][2]){{0, 40}}, 1);
  pool((const int64_t[]){30, 10}, 2, (const int64_t[][2]){{0, 40}}, 1);
  pool((const int64_t[]){10, 30, 5, 40, 3}, 5, (const int64_t[][2]){{0, 45}, {45, 43}}, 2);
  // Short scenes do not chain: a run of them becomes a group at 24 frames.
  pool((const int64_t[]){48, 10, 10, 10, 48}, 5,
       (const int64_t[][2]){{0, 48}, {48, 30}, {78, 48}}, 3);
  pool((const int64_t[]){48, 10, 10, 48}, 4, (const int64_t[][2]){{0, 68}, {68, 48}}, 2);
  pool((const int64_t[]){24, 23, 24}, 3, (const int64_t[][2]){{0, 47}, {47, 24}}, 2);
  pool((const int64_t[]){5, 5, 5}, 3, (const int64_t[][2]){{0, 15}}, 1);
  // Every frame flagged: 24-frame groups, the remainder joins the last.
  int64_t ones[100];
  for (int i = 0; i < 100; i++)
    ones[i] = 1;
  pool(ones, 100, (const int64_t[][2]){{0, 24}, {24, 24}, {48, 24}, {72, 28}}, 4);
  int64_t lengths[] = {5, 1, 30, 24};
  assert(refit_median(lengths, 4) == 14.5);
  int64_t odd[] = {30, 1, 2};
  assert(refit_median(odd, 3) == 2);
}

int main(int argc, char **argv) {
  assert(argc <= 2);
  general_formula_reduces_to_the_frozen_composition();
  general_residual_is_exact();
  pieces_and_pivot_clamps();
  mmr_orders_pieces_and_polynomial_chroma();
  nonidentity_p7_mapping(argc == 2 ? argv[1] : NULL);
  lattice_and_pairing();
  statistics();
  scene_pooling();
  puts("refit-compose-pass");
  return 0;
}
