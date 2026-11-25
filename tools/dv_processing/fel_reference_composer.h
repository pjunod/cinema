// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_FEL_REFERENCE_COMPOSER_H
#define PLURX_FEL_REFERENCE_COMPOSER_H

// Independent integer reference for dual-layer composition.
//
// Every formula below is transcribed from ETSI GS CCM 001 V1.1.1 (2017-02)
// and cites its clause. It shares no code with the renderer it checks
// (fel_renderer.h / libplacebo) and was not derived from any other composer.
// The clause table, the ambiguities and the readings chosen are recorded in
// docs/streaming/FEL-REFERENCE-COMPOSITION-PLAN.md section 8.
//
// Scope: PQ base layer (clause 5.5's BT.1886 conversion is out of scope),
// 4:2:0 BL and EL, an EL at full or half resolution in each direction, any
// number of pivots, polynomial and MMR mapping, NLQ_LINEAR_DZ. Output is the
// reconstructed 4:2:0 picture at hdr_bit_depth, before chroma upsampling.
//
// The arithmetic is exact: intermediates use 64-bit integers and the MMR sum
// a 128-bit accumulator, so no result depends on a register width the
// specification leaves unstated. Right shifts of signed values are floor
// shifts, which is what clause 3.2.2 defines for ">>".

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

__extension__ typedef __int128 frc_i128;

enum {
  FRC_MAX_PIVOTS = 17,  // num_pivots_minus2 <= 15 (clause 5.3.2)
  FRC_MAX_PIECES = FRC_MAX_PIVOTS - 1,
};

enum frc_mapping_idc { FRC_MAPPING_POLYNOMIAL = 0, FRC_MAPPING_MMR = 1 };

// One piece of the piecewise BL mapping (clauses 5.3.3.1-5.3.3.3). All
// coefficients are the fixed-point fp_* values: (int << denom) + fraction.
struct frc_piece {
  int mapping_idc;
  int poly_order;              // poly_order_minus1 + 1: 1 or 2
  int64_t poly_coef[3];        // fp_poly_coef[i], i = 0..poly_order
  int mmr_order;               // mmr_order_minus1 + 1: 1, 2 or 3
  int64_t mmr_constant;        // fp_mmr_constant
  int64_t mmr_coef[3][7];      // fp_mmr_coef[i][j] stored at [i - 1][j]
};

struct frc_component_mapping {
  int num_pivots;              // num_pivots_minus2 + 2
  int32_t pivot_value[FRC_MAX_PIVOTS];
  struct frc_piece piece[FRC_MAX_PIECES];
};

// Clause 5.3.4. hdr_in_max is the RPU's vdr_in_max.
struct frc_nlq {
  int32_t offset;
  int64_t slope;               // fp_linear_deadzone_slope
  int64_t threshold;           // fp_linear_deadzone_threshold
  int64_t hdr_in_max;          // fp_hdr_in_max
};

struct frc_params {
  int coefficient_log2_denom;
  int bl_bit_depth;            // 8 or 10
  int el_bit_depth;            // 8 or 10
  int hdr_bit_depth;           // 10 or 12
  bool disable_residual;
  struct frc_component_mapping map[3];
  struct frc_nlq nlq[3];
};

// Clause 3.2.2: arithmetic right shift, i.e. floor division by 2^n.
static inline int64_t frc_asr(int64_t x, int n) {
  return x >= 0 ? x >> n : -((-(x + 1)) >> n) - 1;
}

static inline int64_t frc_clip3(int64_t lo, int64_t hi, int64_t x) {
  return x < lo ? lo : x > hi ? hi : x;
}

// Clause 5.3.2, Assign_pivot_values: pivots are the running sum of the
// transmitted pred_pivot_value differences.
static inline void frc_assign_pivot_values(const int32_t *pred, int num_pivots,
                                           int32_t *pivot_value) {
  pivot_value[0] = pred[0];
  for (int i = 1; i < num_pivots; i++)
    pivot_value[i] = pivot_value[i - 1] + pred[i];
}

// Returns NULL when the parameters are inside what this reference implements,
// otherwise the reason they are not.
static inline const char *frc_params_check(const struct frc_params *p) {
  if (p->bl_bit_depth != 8 && p->bl_bit_depth != 10)
    return "BL_bit_depth must be 8 or 10 (clause 5.3.2)";
  if (p->el_bit_depth != 8 && p->el_bit_depth != 10)
    return "EL_bit_depth must be 8 or 10 (clause 5.3.2)";
  if (p->hdr_bit_depth != 10 && p->hdr_bit_depth != 12)
    return "hdr_bit_depth must be 10 or 12 (clause 5.3.2)";
  // Clause 5.3.2 bounds the denominator to EL_bit_depth + 5 .. 32; the lower
  // bound is what keeps clause 5.4.3.2's final shift nonnegative.
  if (p->coefficient_log2_denom < p->el_bit_depth + 5 ||
      p->coefficient_log2_denom > 32)
    return "coefficient_log2_denom outside clause 5.3.2's range";
  for (int cmp = 0; cmp < 3; cmp++) {
    const struct frc_component_mapping *m = &p->map[cmp];
    if (m->num_pivots < 2 || m->num_pivots > FRC_MAX_PIVOTS)
      return "num_pivots_minus2 outside 0..15 (clause 5.3.2)";
    for (int i = 1; i < m->num_pivots; i++)
      if (m->pivot_value[i] < m->pivot_value[i - 1])
        return "pivot values must not decrease";
    // A pivot is a BL code; outside 0..2^BL_bit_depth-1 the mapped sample,
    // and so the 64-bit polynomial intermediates, would exceed the range
    // that keeps them exact.
    if (m->pivot_value[0] < 0 ||
        m->pivot_value[m->num_pivots - 1] > (1 << p->bl_bit_depth) - 1)
      return "pivot values outside 0..2^BL_bit_depth-1 (clause 5.3.2)";
    // Clause 5.3.3's integer-part ranges bound every fp coefficient; inside
    // them the polynomial sum stays below 2^60, so int64 is exact.
    const int64_t poly_limit = INT64_C(64) << p->coefficient_log2_denom;
    const int64_t mmr_limit = INT64_C(65536) << p->coefficient_log2_denom;
    for (int k = 0; k < m->num_pivots - 1; k++) {
      const struct frc_piece *piece = &m->piece[k];
      if (piece->mapping_idc == FRC_MAPPING_POLYNOMIAL) {
        if (piece->poly_order < 1 || piece->poly_order > 2)
          return "poly_order_minus1 outside 0..1 (clause 5.3.3.2)";
        for (int i = 0; i <= piece->poly_order; i++)
          if (piece->poly_coef[i] < -poly_limit || piece->poly_coef[i] >= poly_limit)
            return "poly_coef_int outside -64..63 (clause 5.3.3.2)";
      } else if (piece->mapping_idc == FRC_MAPPING_MMR) {
        if (cmp == 0)
          return "luma mapping_idc must be 0 (clause 5.3.3.1)";
        if (piece->mmr_order < 1 || piece->mmr_order > 3)
          return "mmr_order_minus1 outside 0..2 (clause 5.3.3.3)";
        if (piece->mmr_constant < -mmr_limit || piece->mmr_constant >= mmr_limit)
          return "mmr_constant_int outside -65536..65535 (clause 5.3.3.3)";
        for (int i = 0; i < piece->mmr_order; i++)
          for (int j = 0; j < 7; j++)
            if (piece->mmr_coef[i][j] < -mmr_limit || piece->mmr_coef[i][j] >= mmr_limit)
              return "mmr_coef_int outside -65536..65535 (clause 5.3.3.3)";
      } else {
        return "mapping_idc must be 0 or 1 (table 1)";
      }
    }
    if (p->nlq[cmp].offset < 0 ||
        p->nlq[cmp].offset > (1 << p->el_bit_depth) - 1)
      return "nlq_offset outside 0..2^EL_bit_depth-1 (clause 5.3.4)";
  }
  return NULL;
}

// Clause 5.4.2.2: the piece whose upper pivot the sample is below. For a
// sample at or above the top pivot the clause yields num_pivots_minus2 + 1,
// one past the last piece that carries coefficients; the mapping then clamps
// the sample to the top pivot, so the last piece is used (reading R2).
static inline int frc_pivot_index(const struct frc_component_mapping *m,
                                  int32_t s) {
  for (int idx = 0; idx < m->num_pivots - 1; idx++)
    if (s < m->pivot_value[idx + 1])
      return idx;
  return m->num_pivots - 2;
}

static inline int32_t frc_clamp_to_pivots(const struct frc_component_mapping *m,
                                          int32_t s) {
  if (s < m->pivot_value[0])
    s = m->pivot_value[0];
  if (s > m->pivot_value[m->num_pivots - 1])
    s = m->pivot_value[m->num_pivots - 1];
  return s;
}

// Clause 5.4.2.3.2: polynomial mapping to the 16-bit mapped BL domain.
static inline int32_t frc_map_polynomial(const struct frc_params *p, int cmp,
                                         int pivot_idx, int32_t s) {
  const struct frc_component_mapping *m = &p->map[cmp];
  const struct frc_piece *piece = &m->piece[pivot_idx];
  s = frc_clamp_to_pivots(m, s);
  int64_t ss = 1, vv = 0;
  int shift = 20;  // 2 * (maximum BL_bit_depth)
  for (int i = 0; i <= piece->poly_order; i++) {
    vv += piece->poly_coef[i] * (ss << shift);
    ss *= s;
    shift -= p->bl_bit_depth;
  }
  vv = vv < 0 ? 0 : vv;
  int64_t v = vv >> (4 + p->coefficient_log2_denom);
  return (int32_t)(v > 0xffff ? 0xffff : v);
}

// Clause 5.4.2.3.3: BL luma down-sampled to the chroma grid for MMR. In the
// formula s[i][j], i is the horizontal and j the vertical index (reading R1
// in the plan): a [1 2 1] horizontal tap on two rows, averaged. Edges
// replicate.
static inline int32_t frc_downsample_luma(const uint16_t *y, int width,
                                          int height, ptrdiff_t stride,
                                          int i, int j) {
#define FRC_Y(x, r)                                                        \
  ((int32_t)y[(ptrdiff_t)frc_clip3(0, height - 1, (r)) * stride +         \
              frc_clip3(0, width - 1, (x))])
  int32_t top = (FRC_Y(2 * i - 1, 2 * j) + 2 * FRC_Y(2 * i, 2 * j) +
                 FRC_Y(2 * i + 1, 2 * j) + 2) >> 2;
  int32_t bottom = (FRC_Y(2 * i - 1, 2 * j + 1) + 2 * FRC_Y(2 * i, 2 * j + 1) +
                    FRC_Y(2 * i + 1, 2 * j + 1) + 2) >> 2;
#undef FRC_Y
  return (top + bottom + 1) >> 1;
}

// Clause 5.4.2.3.3: MMR mapping of a chroma sample. s0 is the down-sampled
// luma, s1 and s2 the BL chroma at the same position.
static inline int32_t frc_map_mmr(const struct frc_params *p, int cmp,
                                  int pivot_idx, int32_t s0, int32_t s1,
                                  int32_t s2) {
  const struct frc_piece *piece = &p->map[cmp].piece[pivot_idx];
  const int bd = p->bl_bit_depth;
  s0 = frc_clamp_to_pivots(&p->map[0], s0);
  s1 = frc_clamp_to_pivots(&p->map[1], s1);
  s2 = frc_clamp_to_pivots(&p->map[2], s2);
  int64_t tt[22] = {0};
  tt[0] = INT64_C(1) << 20;
  if (piece->mmr_order >= 1) {
    tt[1] = (int64_t)s0 << (20 - bd);
    tt[2] = (int64_t)s1 << (20 - bd);
    tt[3] = (int64_t)s2 << (20 - bd);
    tt[4] = ((int64_t)s0 * s1) << (20 - 2 * bd);
    tt[5] = ((int64_t)s0 * s2) << (20 - 2 * bd);
    tt[6] = ((int64_t)s1 * s2) << (20 - 2 * bd);
    tt[7] = (tt[4] * tt[3]) >> 20;
  }
  if (piece->mmr_order >= 2) {
    tt[8] = ((int64_t)s0 * s0) << (20 - 2 * bd);
    tt[9] = ((int64_t)s1 * s1) << (20 - 2 * bd);
    tt[10] = ((int64_t)s2 * s2) << (20 - 2 * bd);
    tt[11] = (tt[4] * tt[4]) >> 20;
    tt[12] = (tt[5] * tt[5]) >> 20;
    tt[13] = (tt[6] * tt[6]) >> 20;
    tt[14] = (tt[7] * tt[7]) >> 20;
  }
  if (piece->mmr_order >= 3) {
    tt[15] = (tt[1] * tt[8]) >> 20;
    tt[16] = (tt[2] * tt[9]) >> 20;
    tt[17] = (tt[3] * tt[10]) >> 20;
    tt[18] = (tt[4] * tt[11]) >> 20;
    tt[19] = (tt[5] * tt[12]) >> 20;
    tt[20] = (tt[6] * tt[13]) >> 20;
    tt[21] = (tt[7] * tt[14]) >> 20;
  }
  frc_i128 rr = (frc_i128)piece->mmr_constant * tt[0];
  int cnt = 1;
  for (int i = 1; i <= piece->mmr_order; i++)
    for (int j = 0; j < 7; j++)
      rr += (frc_i128)piece->mmr_coef[i - 1][j] * tt[cnt++];
  rr = rr < 0 ? 0 : rr;
  frc_i128 v = rr >> (4 + p->coefficient_log2_denom);
  return (int32_t)(v > 0xffff ? 0xffff : v);
}

// Clause 5.4.3.2, NLQ_LINEAR_DZ: the signed 17-bit residual in the 16-bit
// mapped domain. The clamp to +/-hdr_in_max is the vdr_in_max clamp.
static inline int32_t frc_nlq_linear_dz(const struct frc_params *p, int cmp,
                                        int32_t e) {
  const struct frc_nlq *q = &p->nlq[cmp];
  const int el = p->el_bit_depth;
  int64_t rr = (int64_t)e - q->offset;
  if (rr == 0)
    return 0;
  int64_t sign = rr < 0 ? -1 : 1;
  rr *= 2;  // rr <<= 1, written as a multiply: rr may be negative
  rr -= sign;
  rr *= INT64_C(1) << (10 - el);
  int64_t dq = rr * q->slope;
  dq += q->threshold * (INT64_C(1) << (10 - el + 1)) * sign;
  int64_t limit = q->hdr_in_max * (INT64_C(1) << (10 - el + 1));
  if (dq > limit)
    dq = limit;
  else if (dq < -limit)
    dq = -limit;
  return (int32_t)frc_asr(dq, p->coefficient_log2_denom - 5 - el);
}

// Clause 5.4.3.3 for a PQ base layer (out_bit_depth = hdr_bit_depth).
static inline uint16_t frc_reconstruct(const struct frc_params *p, int32_t v,
                                       int32_t r) {
  const int out = p->hdr_bit_depth;
  int64_t h = v;
  if (!p->disable_residual)
    h += r;
  h += INT64_C(1) << (15 - out);
  h = frc_asr(h, 16 - out);
  return (uint16_t)frc_clip3(0, (INT64_C(1) << out) - 1, h);
}

// Annex B.3 (informative) vertical EL up-sampling of one column, cmp 0.
// in has n samples at in_stride; out receives 2n samples at out_stride.
static inline void frc_el_up_vertical(const uint16_t *in, ptrdiff_t in_stride,
                                      int n, uint16_t *out,
                                      ptrdiff_t out_stride, bool chroma) {
#define FRC_IN(k) ((int64_t)in[frc_clip3(0, n - 1, (k)) * in_stride])
  for (int k = 0; k < n; k++) {
    int64_t even, odd;
    if (!chroma) {
      even = frc_asr(-3 * FRC_IN(k - 2) + 29 * FRC_IN(k - 1) +
                         111 * FRC_IN(k) - 9 * FRC_IN(k + 1) + 64, 7);
      odd = frc_asr(-9 * FRC_IN(k - 1) + 111 * FRC_IN(k) +
                        29 * FRC_IN(k + 1) - 3 * FRC_IN(k + 2) + 64, 7);
    } else {
      even = frc_asr(64 * FRC_IN(k - 1) + 192 * FRC_IN(k) + 128, 8);
      odd = frc_asr(192 * FRC_IN(k) + 64 * FRC_IN(k + 1) + 128, 8);
    }
    out[(ptrdiff_t)(2 * k) * out_stride] = (uint16_t)frc_clip3(0, 0xFFFF, even);
    out[(ptrdiff_t)(2 * k + 1) * out_stride] = (uint16_t)frc_clip3(0, 0xFFFF, odd);
  }
#undef FRC_IN
}

// Annex B.2 SPATIAL_RESAMPLING_FILTER_H, applied to every EL component
// (Annex B.3). in has n samples; out receives 2n.
static inline void frc_el_up_horizontal(const uint16_t *in, int n,
                                        uint16_t *out) {
#define FRC_IN(k) ((int64_t)in[frc_clip3(0, n - 1, (k))])
  for (int k = 0; k < n; k++) {
    out[2 * k] = in[k];
    int64_t odd = frc_asr(22 * FRC_IN(k - 3) + 94 * FRC_IN(k - 2) -
                              524 * FRC_IN(k - 1) + 2456 * FRC_IN(k) +
                              2456 * FRC_IN(k + 1) - 524 * FRC_IN(k + 2) +
                              94 * FRC_IN(k + 3) + 22 * FRC_IN(k + 4) + 2048,
                          12);
    out[2 * k + 1] = (uint16_t)frc_clip3(0, 0xFFFF, odd);
  }
#undef FRC_IN
}

// Annex B.3: vertical first, then horizontal. out is (2w)x(2h), tightly
// packed. Returns false on allocation failure.
static inline bool frc_el_upsample_plane(const uint16_t *in, int w, int h,
                                         ptrdiff_t stride, bool chroma,
                                         uint16_t *out) {
  uint16_t *tall = (uint16_t *)malloc(sizeof(uint16_t) * (size_t)w * 2 * h);
  if (!tall)
    return false;
  for (int x = 0; x < w; x++)
    frc_el_up_vertical(in + x, stride, h, tall + x, w, chroma);
  for (int r = 0; r < 2 * h; r++)
    frc_el_up_horizontal(tall + (ptrdiff_t)r * w, w,
                         out + (ptrdiff_t)r * 2 * w);
  free(tall);
  return true;
}

struct frc_plane {
  const uint16_t *data;
  int width, height;
  ptrdiff_t stride;  // in samples
};

struct frc_picture {
  struct frc_plane plane[3];  // Y, Cb, Cr; chroma is 4:2:0
};

struct frc_output {
  uint16_t *data[3];
  ptrdiff_t stride[3];  // in samples; planes are the BL plane sizes
  // Optional, for measurement only: when set, also receives v + r, the
  // 16-bit-domain sum before clause 5.4.3.3 adds the rounding term, shifts
  // to hdr_bit_depth and clamps. Same strides as data.
  int32_t *unrounded[3];
};

// Composes one picture (clause 5.4). The EL may be NULL only when
// disable_residual is set (clause 5.3.2). Returns NULL on success, otherwise
// why the inputs are outside this reference.
static inline const char *frc_compose(const struct frc_params *p,
                                      const struct frc_picture *bl,
                                      const struct frc_picture *el,
                                      const struct frc_output *out) {
  const char *why = frc_params_check(p);
  if (why)
    return why;
  const int w = bl->plane[0].width, h = bl->plane[0].height;
  if (w <= 0 || h <= 0 || (w & 1) || (h & 1))
    return "BL luma must have positive even dimensions";
  for (int c = 1; c < 3; c++)
    if (bl->plane[c].width != w / 2 || bl->plane[c].height != h / 2)
      return "BL chroma must be 4:2:0 (clause 5.2.2 b)";
  const bool residual = !p->disable_residual && el;
  if (!p->disable_residual && !el)
    return "EL required unless disable_residual_flag is set";
  int scale = 1;
  if (residual) {
    if (el->plane[0].width == w && el->plane[0].height == h)
      scale = 1;
    else if (el->plane[0].width * 2 == w && el->plane[0].height * 2 == h)
      scale = 2;
    else
      return "EL must be BL size or half of it in each direction";
    for (int c = 1; c < 3; c++)
      if (el->plane[c].width * 2 != el->plane[0].width ||
          el->plane[c].height * 2 != el->plane[0].height)
        return "EL chroma must be 4:2:0 (clause 5.2.2 b)";
  }

  // Bring the EL to the BL's resolution (clause 4 note 1, annex B.3).
  uint16_t *up[3] = {NULL, NULL, NULL};
  const uint16_t *e[3] = {NULL, NULL, NULL};
  ptrdiff_t e_stride[3] = {0, 0, 0};
  if (residual) {
    for (int c = 0; c < 3; c++) {
      const struct frc_plane *src = &el->plane[c];
      if (scale == 1) {
        e[c] = src->data;
        e_stride[c] = src->stride;
        continue;
      }
      up[c] = (uint16_t *)malloc(sizeof(uint16_t) * (size_t)src->width * 2 *
                                 (size_t)src->height * 2);
      if (!up[c] || !frc_el_upsample_plane(src->data, src->width, src->height,
                                           src->stride, c != 0, up[c])) {
        for (int k = 0; k < 3; k++)
          free(up[k]);
        return "out of memory";
      }
      e[c] = up[c];
      e_stride[c] = (ptrdiff_t)src->width * 2;
    }
  }

  const struct frc_plane *by = &bl->plane[0];
  for (int c = 0; c < 3; c++) {
    const struct frc_plane *s = &bl->plane[c];
    for (int j = 0; j < s->height; j++) {
      for (int i = 0; i < s->width; i++) {
        int32_t sample = s->data[(ptrdiff_t)j * s->stride + i];
        int k = frc_pivot_index(&p->map[c], sample);
        int32_t v;
        if (p->map[c].piece[k].mapping_idc == FRC_MAPPING_MMR) {
          int32_t s0 = frc_downsample_luma(by->data, by->width, by->height,
                                           by->stride, i, j);
          int32_t s1 = bl->plane[1].data[(ptrdiff_t)j * bl->plane[1].stride + i];
          int32_t s2 = bl->plane[2].data[(ptrdiff_t)j * bl->plane[2].stride + i];
          v = frc_map_mmr(p, c, k, s0, s1, s2);
        } else {
          v = frc_map_polynomial(p, c, k, sample);
        }
        int32_t r = 0;
        if (residual)
          r = frc_nlq_linear_dz(p, c, e[c][(ptrdiff_t)j * e_stride[c] + i]);
        out->data[c][(ptrdiff_t)j * out->stride[c] + i] =
            frc_reconstruct(p, v, r);
        if (out->unrounded[c])
          out->unrounded[c][(ptrdiff_t)j * out->stride[c] + i] =
              p->disable_residual ? v : v + r;
      }
    }
  }
  for (int c = 0; c < 3; c++)
    free(up[c]);
  return NULL;
}

#endif
