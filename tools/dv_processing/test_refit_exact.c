// SPDX-License-Identifier: Apache-2.0
// CPU-only checks of refit_exact.h, M7 Model B v2's exact FEL composition:
// - identity curves give today's frozen integer composition byte for byte,
//   and agree with refit_compose.h's binary64 renderer model for every
//   LINEAR_DZ parameter set and every BL/EL code;
// - nonidentity mappings agree with that renderer model wherever binary64
//   is not within 1e-9 of a 12-bit rounding tie;
// - admission refuses everything it does not compose.
// With a path argument it writes the vectors that plurx-core's
// dv_m7_scoring::composer replays and fraction_oracle.py recomputes.
#include <assert.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <libavutil/mem.h>
#include "refit_compose.h"
#include "refit_exact.h"
#include "controls/p7_mapping_fixture.h"
#include "controls/p7_mmr_single_fixture.h"

static int frozen(int base, int el) {
  int d = el - 512, c = d > 0 ? d : d < 0 ? d + 1 : 0, v = 4 * base + c;
  return v < 0 ? 0 : v > 4095 ? 4095 : v;
}

static AVDOVIMetadata *metadata(void) {
  size_t size;
  AVDOVIMetadata *data = av_dovi_metadata_alloc(&size);
  assert(data);
  return data;
}

// Identity curves with the given LINEAR_DZ parameters, through the
// decoder's public struct (the helper's input) from the P7 fixture header.
static void identity_metadata(AVDOVIMetadata *data, uint16_t offset, uint64_t slope,
                              uint64_t threshold, uint64_t maximum) {
  p7_mapping_fixture(data);
  AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  for (int c = 0; c < 3; c++) {
    AVDOVIReshapingCurve *k = &m->curves[c];
    memset(k, 0, sizeof(*k));
    k->num_pivots = 2;
    k->pivots[1] = 1023;
    k->mapping_idc[0] = AV_DOVI_MAPPING_POLYNOMIAL;
    k->poly_order[0] = 1;
    k->poly_coef[0][1] = INT64_C(1) << 23;
    m->nlq[c] = (AVDOVINLQParams){.nlq_offset = offset,
                                  .vdr_in_max = maximum,
                                  .linear_deadzone_slope = slope,
                                  .linear_deadzone_threshold = threshold};
  }
}

static void identity_is_the_frozen_composition(void) {
  AVDOVIMetadata *data = metadata();
  identity_metadata(data, 512, 2048, 0, 1048576);
  struct refit_exact exact;
  struct refit_mapping binary64;
  assert(refit_exact_from_dovi(data, &exact) == NULL);
  assert(refit_mapping_from_dovi(data, &binary64) == NULL);
  for (int b = 0; b <= 1023; b++)
    for (int e = 0; e <= 1023; e++) {
      const uint16_t base[3] = {b, 1023 - b, b}, el[3] = {e, e, 1023 - e};
      uint16_t out[3], model[3];
      refit_exact_compose(&exact, base, el, out);
      refit_compose(&binary64, base, el, model);
      for (int c = 0; c < 3; c++) {
        assert(out[c] == frozen(base[c], el[c]));
        assert(out[c] == model[c]);
      }
    }
  // Every admitted LINEAR_DZ parameter set on identity curves is exact in
  // binary64 too, so the two must agree everywhere.
  const uint16_t offsets[] = {0, 1, 511, 512, 1023};
  const uint64_t slopes[] = {0, 1, 2048, 2304, 8191};
  const uint64_t thresholds[] = {0, 1, 4096};
  for (size_t o = 0; o < sizeof(offsets) / sizeof(*offsets); o++)
    for (size_t s = 0; s < sizeof(slopes) / sizeof(*slopes); s++)
      for (size_t t = 0; t < sizeof(thresholds) / sizeof(*thresholds); t++) {
        identity_metadata(data, offsets[o], slopes[s], thresholds[t], UINT64_C(1) << 23);
        assert(refit_exact_from_dovi(data, &exact) == NULL);
        assert(refit_mapping_from_dovi(data, &binary64) == NULL);
        for (int b = 0; b <= 1023; b += 3)
          for (int e = 0; e <= 1023; e++) {
            const uint16_t base[3] = {b, b, b}, el[3] = {e, e, e};
            uint16_t out[3], model[3];
            refit_exact_compose(&exact, base, el, out);
            refit_compose(&binary64, base, el, model);
            assert(memcmp(out, model, sizeof(out)) == 0);
          }
      }
  av_free(data);
}

// Exact and binary64 may only disagree where the exact value is within
// 1e-9 code of a 12-bit rounding boundary, which binary64's rounding of
// the MMR's 2^-113 terms can cross; such disagreements must also be rare.
static bool near_boundary(const struct refit_exact *m, const refit_i128 mapped[3],
                          const uint16_t el[3], int c) {
  const refit_i128 unit = refit_exact_pow2(101);
  refit_i128 v = mapped[c] + refit_exact_residual(&m->nlq[c], el[c]) * refit_exact_pow2(89) +
                 refit_exact_pow2(100);
  refit_i128 r = v % unit;
  if (r < 0)
    r += unit;
  const refit_i128 window = unit / 1000000000;
  return r < window || unit - r < window;
}

static unsigned agree_with_renderer_model(AVDOVIMetadata *data) {
  struct refit_exact exact;
  struct refit_mapping binary64;
  assert(refit_exact_from_dovi(data, &exact) == NULL);
  assert(refit_mapping_from_dovi(data, &binary64) == NULL);
  unsigned checked = 0, differ = 0;
  for (int y = 0; y <= 1023; y += 11)
    for (int u = 0; u <= 1023; u += 13)
      for (int v = 0; v <= 1023; v += 17) {
        const uint16_t base[3] = {y, u, v};
        const uint16_t el[3] = {(y * 7 + u) % 1024, (u * 5 + 300) % 1024, (v + 512) % 1024};
        refit_i128 mapped[3];
        uint16_t out[3], model[3];
        refit_exact_compose_mapped(&exact, base, el, mapped, out);
        refit_compose(&binary64, base, el, model);
        for (int c = 0; c < 3; c++) {
          if (out[c] != model[c]) {
            assert(near_boundary(&exact, mapped, el, c));
            assert(out[c] - model[c] == 1 || model[c] - out[c] == 1);
            differ++;
          }
          checked++;
        }
      }
  assert(differ * 100000 < checked);
  return checked;
}

static void nonidentity_mappings_agree_with_the_renderer_model(void) {
  AVDOVIMetadata *data = metadata();
  p7_mapping_fixture(data);
  assert(agree_with_renderer_model(data) > 500000);
  for (int variant = 0; variant < P7_MMR_SINGLE_VARIANTS; variant++) {
    p7_mmr_single_fixture(data, variant);
    assert(agree_with_renderer_model(data) > 500000);
  }
  av_free(data);
}

static void admission_refuses_what_it_does_not_compose(void) {
  AVDOVIMetadata *data = metadata();
  struct refit_exact exact;
#define REFUSED(edit)                                       \
  do {                                                      \
    p7_mmr_single_fixture(data, 0);                         \
    AVDOVIRpuDataHeader *h = av_dovi_get_header(data);      \
    AVDOVIDataMapping *m = av_dovi_get_mapping(data);       \
    (void)h;                                                \
    (void)m;                                                \
    edit;                                                   \
    assert(refit_exact_from_dovi(data, &exact) != NULL);    \
  } while (0)
  p7_mmr_single_fixture(data, 0);
  assert(refit_exact_from_dovi(data, &exact) == NULL);
  REFUSED(h->disable_residual_flag = 1);
  REFUSED(h->coef_data_type = 1);
  REFUSED(h->coef_log2_denom = 22);
  REFUSED(h->bl_bit_depth = 8);
  REFUSED(h->el_bit_depth = 8);
  REFUSED(h->vdr_bit_depth = 14);
  REFUSED(h->bl_video_full_range_flag = 1);
  REFUSED(m->mapping_color_space = 1);
  REFUSED(m->mapping_chroma_format_idc = 1);
  REFUSED(m->num_x_partitions = 2);
  REFUSED(m->num_y_partitions = 2);
  REFUSED(m->nlq_method_idc = 1);
  REFUSED(m->curves[0].num_pivots = 1);
  REFUSED(m->curves[0].num_pivots = 10);
  REFUSED(m->curves[0].pivots[3] = m->curves[0].pivots[2]);
  REFUSED(m->curves[0].pivots[8] = 1024);
  REFUSED(m->curves[0].mapping_idc[4] = AV_DOVI_MAPPING_MMR);
  REFUSED(m->curves[1].mapping_idc[0] = 7);
  REFUSED(m->curves[0].poly_order[2] = 3);
  REFUSED(m->curves[0].poly_order[2] = 0);
  REFUSED(m->curves[1].mmr_order[0] = 4);
  REFUSED(m->curves[1].mmr_order[0] = 0);
  REFUSED(m->curves[0].poly_coef[1][0] = REFIT_EXACT_COEF_LIMIT);
  REFUSED(m->curves[0].poly_coef[1][1] = -REFIT_EXACT_COEF_LIMIT);
  REFUSED(m->curves[2].mmr_constant[0] = REFIT_EXACT_COEF_LIMIT);
  REFUSED(m->curves[2].mmr_coef[0][2][6] = -REFIT_EXACT_COEF_LIMIT);
  // A limit the farthest EL code would reach, and an integer slope part.
  REFUSED(m->nlq[1].vdr_in_max = 1023 * 2048 / 2);
  REFUSED(m->nlq[0].linear_deadzone_slope = UINT64_C(1) << 23);
  REFUSED(m->nlq[2].nlq_offset = 1024);
#undef REFUSED
  // Just inside the coefficient range is admitted and cannot overflow.
  p7_mmr_single_fixture(data, 0);
  AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  m->curves[1].mmr_constant[0] = REFIT_EXACT_COEF_LIMIT - 1;
  for (int p = 0; p < 3; p++)
    for (int t = 0; t < 7; t++)
      m->curves[1].mmr_coef[0][p][t] = REFIT_EXACT_COEF_LIMIT - 1;
  assert(refit_exact_from_dovi(data, &exact) == NULL);
  const uint16_t base[3] = {1023, 1023, 1023}, el[3] = {512, 512, 512};
  refit_i128 mapped[3];
  uint16_t out[3];
  refit_exact_compose_mapped(&exact, base, el, mapped, out);
  assert(mapped[1] == refit_exact_pow2(113) && out[1] == 4095);
  av_free(data);
}

static void write_i128(FILE *f, refit_i128 v) {
  char digits[48];
  int n = 0;
  bool negative = v < 0;
  do {
    int d = (int)(v % 10);
    digits[n++] = (char)('0' + (d < 0 ? -d : d));
    v /= 10;
  } while (v);
  fputc('"', f);
  if (negative)
    fputc('-', f);
  while (n)
    fputc(digits[--n], f);
  fputc('"', f);
}

// One mapping and its samples. Coefficients are the RPU's Q23 integers;
// "mapped" is each component's clamped value over 2^113.
static void write_mapping(FILE *f, const char *name, const AVDOVIMetadata *data, bool comma) {
  struct refit_exact m;
  assert(refit_exact_from_dovi(data, &m) == NULL);
  const AVDOVIDataMapping *raw = av_dovi_get_mapping(data);
  fprintf(f, "%s{\"name\":\"%s\",\"components\":[", comma ? "," : "", name);
  for (int c = 0; c < 3; c++) {
    const struct refit_exact_curve *k = &m.curve[c];
    fprintf(f, "%s{\"pivots\":[", c ? "," : "");
    for (int i = 0; i < k->num_pivots; i++)
      fprintf(f, "%s%u", i ? "," : "", k->pivots[i]);
    fprintf(f, "],\"method\":\"%s\",\"pieces\":[", k->method ? "mmr" : "polynomial");
    for (int i = 0; i < k->num_pivots - 1; i++) {
      fprintf(f, "%s{\"order\":%d,", i ? "," : "", k->order[i]);
      if (k->method == REFIT_EXACT_POLYNOMIAL) {
        fprintf(f, "\"coefficients\":[%" PRId64 ",%" PRId64 ",%" PRId64 "]}", k->poly[i][0],
                k->poly[i][1], k->poly[i][2]);
      } else {
        fprintf(f, "\"constant\":%" PRId64 ",\"coefficients\":[", k->constant[i]);
        for (int p = 0; p < k->order[i]; p++) {
          fputs(p ? ",[" : "[", f);
          for (int t = 0; t < 7; t++)
            fprintf(f, "%s%" PRId64, t ? "," : "", k->mmr[i][p][t]);
          fputc(']', f);
        }
        fputs("]}", f);
      }
    }
    const AVDOVINLQParams *n = &raw->nlq[c];
    fprintf(f, "],\"nlq\":{\"offset\":%u,\"slope\":%" PRIu64 ",\"threshold\":%" PRIu64
               ",\"vdr_in_max\":%" PRIu64 "}}",
            n->nlq_offset, n->linear_deadzone_slope, n->linear_deadzone_threshold,
            n->vdr_in_max);
  }
  fputs("],\"samples\":[", f);
  // Every luma pivot and its lower neighbour, both ends of every code, and
  // a fixed pseudo-random spread; EL codes around each offset and at the
  // extremes for the structured sites, uniform for the spread.
  uint16_t sites[96][6];
  int count = 0;
  for (int i = 1; i < m.curve[0].num_pivots - 1; i++)
    for (int below = 0; below < 2; below++) {
      uint16_t y = (uint16_t)(m.curve[0].pivots[i] - below);
      const uint16_t site[6] = {y, (uint16_t)(1023 - y), (uint16_t)(y / 2 + 256),
                                (uint16_t)(raw->nlq[0].nlq_offset + below),
                                (uint16_t)raw->nlq[1].nlq_offset,
                                (uint16_t)(raw->nlq[2].nlq_offset > 0 ? raw->nlq[2].nlq_offset - 1 : 0)};
      memcpy(sites[count++], site, sizeof(site));
    }
  for (int corner = 0; corner < 64; corner++) {
    uint16_t site[6];
    for (int k = 0; k < 6; k++)
      site[k] = (corner >> k) & 1 ? 1023 : 0;
    memcpy(sites[count++], site, sizeof(site));
  }
  uint32_t state = 20261126;
  bool first = true;
  for (int n = 0; n < count + 640; n++) {
    uint16_t base[3], el[3];
    if (n < count) {
      memcpy(base, sites[n], sizeof(base));
      memcpy(el, sites[n] + 3, sizeof(el));
    } else {
      for (int k = 0; k < 6; k++) {
        state = state * 1664525u + 1013904223u;
        uint16_t code = (uint16_t)(state >> 22);
        if (k < 3)
          base[k] = code;
        else
          el[k - 3] = code;
      }
    }
    refit_i128 mapped[3];
    uint16_t out[3];
    refit_exact_compose_mapped(&m, base, el, mapped, out);
    fprintf(f, "%s[%u,%u,%u,%u,%u,%u,%u,%u,%u,", first ? "" : ",", base[0], base[1], base[2],
            el[0], el[1], el[2], out[0], out[1], out[2]);
    for (int k = 0; k < 3; k++) {
      write_i128(f, mapped[k]);
      fputc(k < 2 ? ',' : ']', f);
    }
    first = false;
  }
  fputs("]}", f);
}

static void write_vectors(const char *path) {
  FILE *f = fopen(path, "wx");
  assert(f);
  fputs("{\"source\":\"tools/dv_processing/test_refit_exact.c through refit_exact.h; "
        "regenerate with test_refit_exact PATH\",\"mappings\":[", f);
  AVDOVIMetadata *data = metadata();
  identity_metadata(data, 512, 2048, 0, 1048576);
  write_mapping(f, "identity", data, false);
  p7_mapping_fixture(data);
  write_mapping(f, "p7-two-piece-mmr", data, true);
  for (int variant = 0; variant < P7_MMR_SINGLE_VARIANTS; variant++) {
    char name[32];
    snprintf(name, sizeof(name), "p7-single-mmr-%d", variant);
    p7_mmr_single_fixture(data, variant);
    write_mapping(f, name, data, true);
  }
  av_free(data);
  fputs("]}\n", f);
  assert(fclose(f) == 0);
}

int main(int argc, char **argv) {
  assert(argc <= 2);
  identity_is_the_frozen_composition();
  nonidentity_mappings_agree_with_the_renderer_model();
  admission_refuses_what_it_does_not_compose();
  if (argc == 2)
    write_vectors(argv[1]);
  puts("refit-exact-pass");
  return 0;
}
