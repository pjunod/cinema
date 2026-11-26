// SPDX-License-Identifier: Apache-2.0
// M7 Model B v2: the exact 12-bit FEL composition of one pixel for any
// mapping the pinned renderer composes and this header admits. Integers
// only, so the result does not depend on the compiler, the architecture or
// the evaluation order. plurx-core's dv_m7_scoring::composer is the same
// arithmetic in Rust; test_refit_compose.c writes the vectors both replay.
// Design: docs/streaming/M7-MODEL-B-GENERAL-COMPOSITION.md.
//
// The reference is libplacebo v7.387.0's composer (pl_shader_dovi_reshape
// and sh_dovi_compose_nlq, src/shaders/colorspace.c), the same master
// reconstruction refit_compose.h documents, evaluated in exact rational
// arithmetic instead of the GPU's binary32:
// - every component's code is clamped to its own pivot range, and x = code/2^10;
// - a component's piece is the last whose lower pivot is <= its own code;
// - polynomial: a0 + a1 x + a2 x^2; MMR: constant plus, for each order
//   p <= mmr_order, sum_t m[p][t] T_t^p over T = (x0, x1, x2, x0x1, x0x2,
//   x1x2, x0x1x2); coefficients are the RPU's Q23 integers / 2^23;
// - the mapped value is clamped to [0, 1];
// - LINEAR_DZ residual sign(d)((|d| - 1/2) S + T) / 2^23, d = EL - offset,
//   zero at d = 0; admission makes vdr_in_max nonbinding for every EL code;
// - the composed code is clip(floor((mapped + residual) * 4096 + 1/2), 0, 4095).
// Every value is a fraction over 2^113 (= 2^23 * 1024^9): with |Q23
// coefficient| < 2^31 and codes <= 1023, each of the at most 22 terms is
// below 2^121, so a signed 128-bit sum is exact.
#ifndef PLURX_REFIT_EXACT_H
#define PLURX_REFIT_EXACT_H

#include <libavutil/dovi_meta.h>
#include <stdbool.h>
#include <stdint.h>
#include <string.h>

#include "nlq_clipping.h"

#define REFIT_EXACT_MAX_PIECES 8
#define REFIT_EXACT_COEF_LIMIT (INT64_C(1) << 31)

__extension__ typedef __int128 refit_i128;

enum refit_exact_method { REFIT_EXACT_POLYNOMIAL = 0, REFIT_EXACT_MMR = 1 };

// One component: pivots in BL codes and its pieces' Q23 coefficients. The
// pinned libdovi cross-check (refit_scene_refresh) carries one method per
// component, so admission requires every piece of a component to share it.
struct refit_exact_curve {
  int num_pivots, method;
  uint16_t pivots[REFIT_EXACT_MAX_PIECES + 1];
  int order[REFIT_EXACT_MAX_PIECES];
  int64_t poly[REFIT_EXACT_MAX_PIECES][3];
  int64_t constant[REFIT_EXACT_MAX_PIECES];
  int64_t mmr[REFIT_EXACT_MAX_PIECES][3][7];
};
// LINEAR_DZ in RPU units: offset in EL codes, slope and threshold in Q23.
struct refit_exact_nlq {
  uint16_t offset;
  int64_t slope, threshold;
};
struct refit_exact {
  struct refit_exact_curve curve[3];
  struct refit_exact_nlq nlq[3];
};

static inline refit_i128 refit_exact_pow2(int k) { return (refit_i128)1 << k; }

static inline bool refit_exact_coef(int64_t value) {
  return value > -REFIT_EXACT_COEF_LIMIT && value < REFIT_EXACT_COEF_LIMIT;
}

// Admits exactly the subset this header composes; returns why not, or NULL.
static inline const char *refit_exact_from_dovi(const AVDOVIMetadata *data,
                                                struct refit_exact *out) {
  const AVDOVIRpuDataHeader *h = av_dovi_get_header(data);
  const AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  memset(out, 0, sizeof(*out));
  if (h->disable_residual_flag || h->coef_data_type != 0 || h->coef_log2_denom != 23 ||
      h->bl_bit_depth != 10 || h->el_bit_depth != 10 || h->vdr_bit_depth != 12 ||
      h->bl_video_full_range_flag || m->mapping_color_space != 0 ||
      m->mapping_chroma_format_idc != 0 || m->num_x_partitions != 1 ||
      m->num_y_partitions != 1 || m->nlq_method_idc != AV_DOVI_NLQ_LINEAR_DZ)
    return "M7 exact composition requires a 10/10/12-bit Q23 FEL with one partition";
  for (int c = 0; c < 3; c++) {
    const AVDOVIReshapingCurve *curve = &m->curves[c];
    struct refit_exact_curve *k = &out->curve[c];
    if (curve->num_pivots < 2 || curve->num_pivots > REFIT_EXACT_MAX_PIECES + 1)
      return "M7 exact composition pivots";
    k->num_pivots = curve->num_pivots;
    for (int i = 0; i < curve->num_pivots; i++) {
      if (curve->pivots[i] > 1023 || (i && curve->pivots[i] <= curve->pivots[i - 1]))
        return "M7 exact composition pivots";
      k->pivots[i] = curve->pivots[i];
    }
    k->method = curve->mapping_idc[0];
    for (int i = 0; i < curve->num_pivots - 1; i++) {
      if ((int)curve->mapping_idc[i] != k->method)
        return "M7 exact composition requires one method per component";
      if (k->method == AV_DOVI_MAPPING_POLYNOMIAL) {
        int order = curve->poly_order[i];
        if (order < 1 || order > 2)
          return "M7 exact composition polynomial order";
        k->order[i] = order;
        for (int j = 0; j <= order; j++) {
          if (!refit_exact_coef(curve->poly_coef[i][j]))
            return "M7 exact composition coefficient range";
          k->poly[i][j] = curve->poly_coef[i][j];
        }
      } else if (k->method == AV_DOVI_MAPPING_MMR) {
        int order = curve->mmr_order[i];
        if (order < 1 || order > 3)
          return "M7 exact composition MMR order";
        k->order[i] = order;
        if (!refit_exact_coef(curve->mmr_constant[i]))
          return "M7 exact composition coefficient range";
        k->constant[i] = curve->mmr_constant[i];
        for (int j = 0; j < order; j++) {
          for (int t = 0; t < 7; t++) {
            if (!refit_exact_coef(curve->mmr_coef[i][j][t]))
              return "M7 exact composition coefficient range";
            k->mmr[i][j][t] = curve->mmr_coef[i][j][t];
          }
        }
      } else {
        return "M7 exact composition mapping method";
      }
    }
    const AVDOVINLQParams *n = &m->nlq[c];
    const uint64_t fraction = (UINT64_C(1) << 23) - 1;
    if (!nlq_clip_is_nonbinding(n->nlq_offset, n->linear_deadzone_slope,
                                n->linear_deadzone_threshold, n->vdr_in_max >> 23,
                                n->vdr_in_max & fraction))
      return "M7 exact composition requires nonbinding linear-deadzone NLQ";
    out->nlq[c] = (struct refit_exact_nlq){n->nlq_offset, (int64_t)n->linear_deadzone_slope,
                                           (int64_t)n->linear_deadzone_threshold};
  }
  return NULL;
}

static inline int refit_exact_piece(const struct refit_exact_curve *k, int64_t code) {
  int piece = 0;
  while (piece + 1 < k->num_pivots - 1 && code >= k->pivots[piece + 1])
    piece++;
  return piece;
}

// The mapped value of component c before clamping, as a numerator over 2^113.
static inline refit_i128 refit_exact_mapped(const struct refit_exact *m, int c,
                                            const int64_t code[3]) {
  const struct refit_exact_curve *k = &m->curve[c];
  int i = refit_exact_piece(k, code[c]);
  if (k->method == REFIT_EXACT_POLYNOMIAL) {
    refit_i128 x = code[c];
    return k->poly[i][0] * refit_exact_pow2(90) + k->poly[i][1] * x * refit_exact_pow2(80) +
           k->poly[i][2] * x * x * refit_exact_pow2(70);
  }
  static const int degree[7] = {1, 1, 1, 2, 2, 2, 3};
  const int64_t t[7] = {code[0], code[1], code[2], code[0] * code[1], code[0] * code[2],
                        code[1] * code[2], code[0] * code[1] * code[2]};
  refit_i128 power[7], sum = k->constant[i] * refit_exact_pow2(90);
  for (int j = 0; j < 7; j++)
    power[j] = t[j];
  for (int p = 1; p <= k->order[i]; p++) {
    for (int j = 0; j < 7; j++) {
      sum += k->mmr[i][p - 1][j] * power[j] * refit_exact_pow2(90 - 10 * degree[j] * p);
      power[j] *= t[j];
    }
  }
  return sum;
}

// The LINEAR_DZ residual as a numerator over 2^24.
static inline int64_t refit_exact_residual(const struct refit_exact_nlq *n, uint16_t el) {
  int64_t d = (int64_t)el - n->offset;
  if (d == 0)
    return 0;
  int64_t magnitude = (2 * (d < 0 ? -d : d) - 1) * n->slope + 2 * n->threshold;
  return d < 0 ? -magnitude : magnitude;
}

// The clamped mapped value of every component over 2^113, then the 12-bit
// composition: floor((N + R * 2^89) / 2^101 + 1/2), clipped to [0, 4095].
static inline void refit_exact_compose_mapped(const struct refit_exact *m,
                                              const uint16_t base[3], const uint16_t el[3],
                                              refit_i128 mapped[3], uint16_t out[3]) {
  int64_t code[3];
  for (int c = 0; c < 3; c++) {
    const struct refit_exact_curve *k = &m->curve[c];
    int64_t b = base[c], lo = k->pivots[0], hi = k->pivots[k->num_pivots - 1];
    code[c] = b < lo ? lo : b > hi ? hi : b;
  }
  const refit_i128 one = refit_exact_pow2(113);
  for (int c = 0; c < 3; c++) {
    refit_i128 n = refit_exact_mapped(m, c, code);
    mapped[c] = n < 0 ? 0 : n > one ? one : n;
    refit_i128 v = mapped[c] + refit_exact_residual(&m->nlq[c], el[c]) * refit_exact_pow2(89) +
                   refit_exact_pow2(100);
    // v is nonnegative wherever the quotient is used, so division is floor.
    out[c] = v < 0 ? 0 : v >= 4096 * refit_exact_pow2(101) ? 4095
                                                           : (uint16_t)(v / refit_exact_pow2(101));
  }
}

static inline void refit_exact_compose(const struct refit_exact *m, const uint16_t base[3],
                                       const uint16_t el[3], uint16_t out[3]) {
  refit_i128 mapped[3];
  refit_exact_compose_mapped(m, base, el, mapped, out);
}

#endif
