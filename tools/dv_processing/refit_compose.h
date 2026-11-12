// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_REFIT_COMPOSE_H
#define PLURX_REFIT_COMPOSE_H

// F+ statistics on the CPU: the FEL composition of one lattice pixel in the
// RPU's normative domain, the frozen tools' sample pairing, the sufficient
// statistics plurx-core's dv_refit::RefitStats deserialises, and the scene
// pooling of FEL-RPU-REFIT-BUILD.md §3.1. Arithmetic only: FFmpeg's public
// Dolby Vision metadata struct is the one dependency, and no GPU. The decoding
// side is refit_mode.h; test_refit_compose.c checks this file.
//
// Statistics must not depend on the node's compiler or architecture: every
// expression here is binary64 with no fused multiply-add. GCC does not
// implement the STDC pragma (and ISO C mode already disables contraction);
// every build of this header also passes -ffp-contract=off.
#if defined(__clang__)
#pragma STDC FP_CONTRACT OFF
#endif
#include <libavutil/dovi_meta.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define REFIT_CODES 1024
#define REFIT_TERMS 22
#define REFIT_MAX_PIECES 8
// Lattice: stride 8 from the active area's top-left; the fit lattice at
// phase 0 and the held-out lattice at phase 4 in both x and y.
#define REFIT_STRIDE 8
#define REFIT_FIT_PHASE 0
#define REFIT_HELD_OUT_PHASE 4
// A scene shorter than this pools with its predecessor (§3.1).
#define REFIT_MIN_SCENE_FRAMES 24

enum refit_method { REFIT_POLYNOMIAL = 0, REFIT_MMR = 1 };

// One component's mapping, as the RPU signals it: pivots in base-layer codes,
// and per piece a polynomial or an MMR, coefficients already divided by
// 2^coefficient_log2_denom (exact in binary64 for |coefficient| < 2^30).
struct refit_curve {
  int num_pivots;
  uint16_t pivots[REFIT_MAX_PIECES + 1];
  int method[REFIT_MAX_PIECES];
  double poly[REFIT_MAX_PIECES][3];
  int mmr_order[REFIT_MAX_PIECES];
  double mmr_constant[REFIT_MAX_PIECES];
  double mmr[REFIT_MAX_PIECES][3][7];
};
// LINEAR_DZ parameters in their RPU units: offset in EL codes, slope and
// threshold as fractions of 2^23 (their integer parts are refused upstream).
struct refit_nlq {
  uint16_t offset;
  uint64_t slope, threshold;
};
struct refit_mapping {
  struct refit_curve curve[3];
  struct refit_nlq nlq[3];
};

// A frame's mapping from the decoder's metadata, which the caller has
// already admitted (map_fel_rpu). Returns why it is refused, or NULL.
static inline const char *refit_mapping_from_dovi(const AVDOVIMetadata *data,
                                                  struct refit_mapping *out) {
  const AVDOVIRpuDataHeader *header = av_dovi_get_header(data);
  const AVDOVIDataMapping *mapping = av_dovi_get_mapping(data);
  if (header->coef_data_type != 0 || header->coef_log2_denom != 23 ||
      header->bl_bit_depth != 10 || header->el_bit_depth != 10 ||
      header->vdr_bit_depth != 12 || mapping->nlq_method_idc != AV_DOVI_NLQ_LINEAR_DZ)
    return "refit mapping subset";
  const double denominator = 8388608.0;
  memset(out, 0, sizeof(*out));
  for (int c = 0; c < 3; c++) {
    const AVDOVIReshapingCurve *curve = &mapping->curves[c];
    struct refit_curve *k = &out->curve[c];
    if (curve->num_pivots < 2 || curve->num_pivots > REFIT_MAX_PIECES + 1)
      return "refit mapping pivots";
    k->num_pivots = curve->num_pivots;
    for (int i = 0; i < curve->num_pivots; i++) {
      if (curve->pivots[i] > 1023 || (i && curve->pivots[i] <= curve->pivots[i - 1]))
        return "refit mapping pivots";
      k->pivots[i] = curve->pivots[i];
    }
    for (int i = 0; i < curve->num_pivots - 1; i++) {
      if (curve->mapping_idc[i] == AV_DOVI_MAPPING_POLYNOMIAL) {
        if (curve->poly_order[i] < 1 || curve->poly_order[i] > 2)
          return "refit polynomial order";
        k->method[i] = REFIT_POLYNOMIAL;
        for (int j = 0; j <= curve->poly_order[i]; j++)
          k->poly[i][j] = curve->poly_coef[i][j] / denominator;
      } else if (curve->mapping_idc[i] == AV_DOVI_MAPPING_MMR) {
        if (curve->mmr_order[i] < 1 || curve->mmr_order[i] > 3)
          return "refit MMR order";
        k->method[i] = REFIT_MMR;
        k->mmr_order[i] = curve->mmr_order[i];
        k->mmr_constant[i] = curve->mmr_constant[i] / denominator;
        for (int j = 0; j < curve->mmr_order[i]; j++)
          for (int t = 0; t < 7; t++)
            k->mmr[i][j][t] = curve->mmr_coef[i][j][t] / denominator;
      } else {
        return "refit mapping method";
      }
    }
    const AVDOVINLQParams *n = &mapping->nlq[c];
    if (n->nlq_offset > 1023 || n->linear_deadzone_slope >= (UINT64_C(1) << 23) ||
        n->linear_deadzone_threshold >= (UINT64_C(1) << 23))
      return "refit NLQ parameters";
    out->nlq[c] = (struct refit_nlq){n->nlq_offset, n->linear_deadzone_slope,
                                     n->linear_deadzone_threshold};
  }
  return NULL;
}

static inline int refit_piece(const struct refit_curve *c, unsigned code) {
  int piece = 0;
  while (piece + 1 < c->num_pivots - 1 && code >= c->pivots[piece + 1])
    piece++;
  return piece;
}

// The RPU's mapping of one base-layer pixel, before the residual, as the
// helper's GPU renderer (libplacebo v7.387.0, the master reconstruction)
// computes it:
// - input x = code / 2^10. fel_renderer.h samples each plane as UNORM16 of
//   code << 6 (sample_depth 16, colour depth 10, bit_shift 6), and
//   pl_shader_decode_color rescales it by (2^16 - 1) / 2^6 / 2^10
//   (src/shaders/colorspace.c:349-352, pl_color_repr_code_scale in
//   src/colorspace.c:228-245);
// - pivots are codes / 2^10 too (pl_map_dovi_metadata,
//   src/include/libplacebo/utils/libav_internal.h:985), so choosing the piece
//   by `s >= pivot` (colorspace.c:274) is choosing it by code >= pivot;
// - every component is clamped to its own pivot range before any piece is
//   evaluated, so an MMR reads the other components clamped to theirs
//   (`sig = clamp(color.rgb, pivot_lo, pivot_hi)`, colorspace.c:178);
// - each mapped component is clamped to [0, 1] before the residual is added
//   (colorspace.c:326).
// The renderer evaluates in binary32 on the GPU; this is the same expression
// in binary64. FFmpeg parses the mapping but has no reshaping of its own.
static inline void refit_map(const struct refit_mapping *m, const uint16_t base[3],
                             double out[3]) {
  unsigned code[3];
  double x[3];
  for (int c = 0; c < 3; c++) {
    const struct refit_curve *k = &m->curve[c];
    unsigned b = base[c], lo = k->pivots[0], hi = k->pivots[k->num_pivots - 1];
    code[c] = b < lo ? lo : b > hi ? hi : b;
    x[c] = code[c] / 1024.0;
  }
  for (int c = 0; c < 3; c++) {
    const struct refit_curve *k = &m->curve[c];
    int i = refit_piece(k, code[c]);
    double s;
    if (k->method[i] == REFIT_POLYNOMIAL) {
      s = k->poly[i][0] + k->poly[i][1] * x[c] + k->poly[i][2] * x[c] * x[c];
    } else {
      const double t[7] = {x[0], x[1], x[2], x[0] * x[1], x[0] * x[2], x[1] * x[2],
                           x[0] * x[1] * x[2]};
      double power[7];
      memcpy(power, t, sizeof(power));
      s = k->mmr_constant[i];
      for (int order = 0; order < k->mmr_order[i]; order++) {
        for (int j = 0; j < 7; j++) {
          s += k->mmr[i][order][j] * power[j];
          power[j] *= t[j];
        }
      }
    }
    out[c] = s < 0.0 ? 0.0 : s > 1.0 ? 1.0 : s;
  }
}

// LINEAR_DZ residual of one EL code: zero at the offset, otherwise
// sign(d)·((|d| − ½)·S + T) / 2^23 for d = e − offset. Computed as the
// integer (2|d| − 1)·S + 2T over 2^24, which binary64 holds exactly.
static inline double refit_residual(const struct refit_nlq *n, uint16_t el) {
  int d = (int)el - (int)n->offset;
  if (d == 0)
    return 0.0;
  uint64_t magnitude = (uint64_t)(2 * (d < 0 ? -d : d) - 1) * n->slope + 2 * n->threshold;
  double r = (double)magnitude / 16777216.0;
  return d < 0 ? -r : r;
}

// The 12-bit FEL composition: clip(floor((mapped + r)·4096 + ½), 0, 4095).
// The renderer adds clamp(r, ±hdr_in_max) to the clamped mapped value and
// clamps the sum to [0, 1] (sh_dovi_compose_nlq, colorspace.c:147-150, with
// the EL read as the integer codes it holds); rounding that at 2^12 is this
// expression. hdr_in_max is not applied here: the admission
// (nlq_clip_is_nonbinding) only accepts limits that cannot bind for any EL
// code.
static inline void refit_compose(const struct refit_mapping *m, const uint16_t base[3],
                                 const uint16_t el[3], uint16_t out[3]) {
  double mapped[3];
  refit_map(m, base, mapped);
  for (int c = 0; c < 3; c++) {
    double v = floor((mapped[c] + refit_residual(&m->nlq[c], el[c])) * 4096.0 + 0.5);
    out[c] = v < 0.0 ? 0 : v > 4095.0 ? 4095 : (uint16_t)v;
  }
}

// Sample positions, exactly as the frozen four_model_audit.sample_codes pairs
// them for a half-resolution EL (the only kind it covers, and the only kind
// the caller admits): BL chroma at ((x+1)/2, (y+1)/2), EL luma at
// ((x+1)/2, y/2), EL chroma at ((x+2)/4, (y+1)/4), each clamped to its plane.
struct refit_sites {
  int bl_chroma[2], el_luma[2], el_chroma[2];
};
static inline int refit_min(int a, int b) { return a < b ? a : b; }
static inline struct refit_sites refit_pair(int x, int y, int w, int h) {
  int ew = w / 2, eh = h / 2;
  struct refit_sites s;
  s.bl_chroma[0] = refit_min((x + 1) / 2, w / 2 - 1);
  s.bl_chroma[1] = refit_min((y + 1) / 2, h / 2 - 1);
  s.el_luma[0] = refit_min((x + 1) / 2, ew - 1);
  s.el_luma[1] = refit_min(y / 2, eh - 1);
  s.el_chroma[0] = refit_min((x + 2) / 4, ew / 2 - 1);
  s.el_chroma[1] = refit_min((y + 1) / 4, eh / 2 - 1);
  return s;
}

// Number of lattice positions of one axis: start + phase + 8k < end.
static inline int refit_axis_count(int start, int end, int phase) {
  int first = start + phase;
  return first >= end ? 0 : (end - 1 - first) / REFIT_STRIDE + 1;
}

// dv_refit::RefitStats: per luma code the sum, count and sum of squares of
// composed luma; the chroma MMR's normal equations XᵀX (22×22 row-major, held
// whole here, written as its upper triangle) and XᵀY (22×2, Cb and Cr
// interleaved) on base codes / 1023 and composed / 4095; Σ(composed/4095)²
// per chroma channel.
struct refit_stats {
  double ysum[REFIT_CODES], ycnt[REFIT_CODES], ysq[REFIT_CODES];
  double xtx[REFIT_TERMS * REFIT_TERMS], xty[REFIT_TERMS * 2], csq[2];
};

static inline void refit_basis(const uint16_t base[3], double x[REFIT_TERMS]) {
  double y = base[0] / 1023.0, u = base[1] / 1023.0, v = base[2] / 1023.0;
  const double t[7] = {y, u, v, y * u, y * v, u * v, y * u * v};
  x[0] = 1.0;
  for (int k = 0; k < 7; k++) {
    x[1 + k] = t[k];
    x[8 + k] = t[k] * t[k];
    x[15 + k] = t[k] * t[k] * t[k];
  }
}

// RefitStats::add. Only XᵀX's upper triangle is summed here;
// refit_stats_finish_frame mirrors it before a frame joins its scene.
static inline void refit_stats_add(struct refit_stats *s, const uint16_t base[3],
                                   const uint16_t composed[3]) {
  unsigned code = base[0] > 1023 ? 1023 : base[0];
  double luma = composed[0];
  s->ysum[code] += luma;
  s->ycnt[code] += 1.0;
  s->ysq[code] += luma * luma;
  double x[REFIT_TERMS];
  refit_basis(base, x);
  double cb = composed[1] / 4095.0, cr = composed[2] / 4095.0;
  for (int i = 0; i < REFIT_TERMS; i++) {
    double *row = s->xtx + i * REFIT_TERMS;
    for (int j = i; j < REFIT_TERMS; j++)
      row[j] += x[i] * x[j];
    s->xty[2 * i] += x[i] * cb;
    s->xty[2 * i + 1] += x[i] * cr;
  }
  s->csq[0] += cb * cb;
  s->csq[1] += cr * cr;
}
static inline void refit_stats_finish_frame(struct refit_stats *s) {
  for (int i = 0; i < REFIT_TERMS; i++)
    for (int j = 0; j < i; j++)
      s->xtx[i * REFIT_TERMS + j] = s->xtx[j * REFIT_TERMS + i];
}
static inline void refit_stats_merge(struct refit_stats *into, const struct refit_stats *from) {
  for (int i = 0; i < REFIT_CODES; i++) {
    into->ysum[i] += from->ysum[i];
    into->ycnt[i] += from->ycnt[i];
    into->ysq[i] += from->ysq[i];
  }
  for (int i = 0; i < REFIT_TERMS * REFIT_TERMS; i++)
    into->xtx[i] += from->xtx[i];
  for (int i = 0; i < REFIT_TERMS * 2; i++)
    into->xty[i] += from->xty[i];
  into->csq[0] += from->csq[0];
  into->csq[1] += from->csq[1];
}
static inline double refit_stats_samples(const struct refit_stats *s) {
  double n = 0.0;
  for (int i = 0; i < REFIT_CODES; i++)
    n += s->ycnt[i];
  return n;
}

// %.17g round-trips binary64; a nonfinite value has no JSON form.
static inline bool refit_json_array(FILE *f, const char *name, const double *v, int n) {
  if (fprintf(f, "\"%s\":[", name) < 0)
    return false;
  for (int i = 0; i < n; i++) {
    if (!isfinite(v[i]) || fprintf(f, i ? ",%.17g" : "%.17g", v[i]) < 0)
      return false;
  }
  return fputc(']', f) != EOF;
}
#define REFIT_UPPER (REFIT_TERMS * (REFIT_TERMS + 1) / 2)
// RefitStats' serde layout: xtx_upper is XᵀX's upper triangle, row by row
// (row i from column i), 253 values; plurx-core mirrors it.
static inline bool refit_stats_json(FILE *f, const struct refit_stats *s) {
  double upper[REFIT_UPPER];
  int at = 0;
  for (int i = 0; i < REFIT_TERMS; i++)
    for (int j = i; j < REFIT_TERMS; j++)
      upper[at++] = s->xtx[i * REFIT_TERMS + j];
  return fputc('{', f) != EOF && refit_json_array(f, "ysum", s->ysum, REFIT_CODES) &&
         fputc(',', f) != EOF && refit_json_array(f, "ycnt", s->ycnt, REFIT_CODES) &&
         fputc(',', f) != EOF && refit_json_array(f, "xtx_upper", upper, REFIT_UPPER) &&
         fputc(',', f) != EOF && refit_json_array(f, "xty", s->xty, REFIT_TERMS * 2) &&
         fputc(',', f) != EOF && refit_json_array(f, "ysq", s->ysq, REFIT_CODES) &&
         fputc(',', f) != EOF && refit_json_array(f, "csq", s->csq, 2) &&
         fputs(",\"second_moments\":true}", f) != EOF;
}

// A scene: a run of frames in presentation order, [first_pts, end_pts) in
// source ticks (end_pts is its last frame's pts plus duration), and its
// statistics on both lattices.
struct refit_scene {
  int64_t first_frame, frames, first_pts, end_pts;
  struct refit_stats fit, held_out;
};
// Appends `later`, which starts where `into` ends.
static inline void refit_scene_absorb(struct refit_scene *into, const struct refit_scene *later) {
  into->frames += later->frames;
  into->end_pts = later->end_pts;
  refit_stats_merge(&into->fit, &later->fit);
  refit_stats_merge(&into->held_out, &later->held_out);
}

// Scene pooling, online (FEL-RPU-REFIT-BUILD.md §3.1). Each RPU scene (a run
// starting at a scene_refresh_flag) closes into refit_pool_close:
// - a scene of 24+ frames is a group of its own;
// - consecutive shorter scenes collect in a pending run, which becomes a
//   group of its own once it reaches 24 frames;
// - a pending run still short when a long scene follows joins the group
//   before it, or, at the very start of the range, the group after it;
// - a pending run still short at the end joins the last group.
// So groups are never shorter than 24 frames unless the whole range is, and
// a title that flags every frame gets 24-frame groups, not one scene. A
// group is emitted once the next one exists; at most three scenes are held.
struct refit_pool {
  struct refit_scene group, pending;
  bool have_group, have_pending;
  int64_t rpu_scenes, emitted;
};
typedef bool (*refit_emit_fn)(const struct refit_scene *scene, void *context);

static inline bool refit_pool_start(struct refit_pool *p, const struct refit_scene *group,
                                    refit_emit_fn emit, void *context) {
  if (p->have_group) {
    if (!emit(&p->group, context))
      return false;
    p->emitted++;
  }
  p->group = *group;
  p->have_group = true;
  return true;
}
static inline bool refit_pool_close(struct refit_pool *p, const struct refit_scene *scene,
                                    refit_emit_fn emit, void *context) {
  p->rpu_scenes++;
  if (scene->frames < REFIT_MIN_SCENE_FRAMES) {
    if (p->have_pending) {
      refit_scene_absorb(&p->pending, scene);
    } else {
      p->pending = *scene;
      p->have_pending = true;
    }
    if (p->pending.frames < REFIT_MIN_SCENE_FRAMES)
      return true;
    p->have_pending = false;
    return refit_pool_start(p, &p->pending, emit, context);
  }
  if (p->have_pending) {
    p->have_pending = false;
    if (p->have_group) {
      refit_scene_absorb(&p->group, &p->pending);
    } else {
      refit_scene_absorb(&p->pending, scene);
      return refit_pool_start(p, &p->pending, emit, context);
    }
  }
  return refit_pool_start(p, scene, emit, context);
}
static inline bool refit_pool_finish(struct refit_pool *p, refit_emit_fn emit, void *context) {
  if (p->have_pending) {
    p->have_pending = false;
    if (p->have_group)
      refit_scene_absorb(&p->group, &p->pending);
    else if (!refit_pool_start(p, &p->pending, emit, context))
      return false;
  }
  if (!p->have_group)
    return true;
  p->have_group = false;
  if (!emit(&p->group, context))
    return false;
  p->emitted++;
  return true;
}

// Median length of the RPU's own scenes (before pooling), the mean of the
// two middle lengths when their count is even. Sorts `lengths` in place.
static inline int refit_compare_lengths(const void *a, const void *b) {
  int64_t x = *(const int64_t *)a, y = *(const int64_t *)b;
  return (x > y) - (x < y);
}
static inline double refit_median(int64_t *lengths, size_t n) {
  if (!n)
    return 0.0;
  qsort(lengths, n, sizeof(*lengths), refit_compare_lengths);
  return n % 2 ? (double)lengths[n / 2]
               : ((double)lengths[n / 2 - 1] + (double)lengths[n / 2]) / 2.0;
}

#endif
