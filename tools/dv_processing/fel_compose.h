// SPDX-License-Identifier: Apache-2.0
// Dual-layer (FEL) composition on the GPU as ETSI GS CCM 001 V1.1.1 specifies
// it, for the pq-ccm001-* representation identities.
//
// Fragment passes, each in exact integer arithmetic:
//   EL vertical (half-size EL only): Annex B.3's vertical filters, once per
//           EL column and output row, into R16 (Y) and RG16 (Cb, Cr);
//   luma:   W x H        BL Y, EL Y          -> R16  holding h << (16 - hdr)
//   chroma: W/2 x H/2    BL Y/Cb/Cr, EL Cb/Cr -> RG16 holding Cb, Cr likewise
// The luma and chroma passes finish Annex B's up-sampling with the horizontal
// filter, map the BL sample (clause 5.4.2: polynomial pieces from a per-frame
// table of every BL code, MMR in the shader with the luma down-sampled to the
// chroma grid), add the LINEAR_DZ residual clamped
// at vdr_in_max (clause 5.4.3.2) and round to hdr_bit_depth (clause 5.4.3.3). The result is the 4:2:0 picture before chroma
// up-sampling. fel_compose_frame() hands it back to libplacebo as a Dolby
// Vision frame with no reshaping, no NLQ and no enhancement layer, so
// libplacebo performs only chroma up-sampling and the RPU colour matrices.
//
// The fixed-point coefficients are used as transmitted, within clause
// 5.3.3's ranges. Mapping sums run in a 96-bit two's-complement accumulator
// built from 32-bit words (MMR terms reach 2^69), so no result depends on GPU
// float precision or on optional 64-bit shader support.
//
// tools/dv_processing/fel_reference_composer.h is the independent CPU
// reference this stage is checked against (controls/fel_compose_control.c);
// the two share no code. docs/streaming/FEL-REFERENCE-COMPOSITION-PLAN.md
// sections 8 and 10 record the clause readings and the parity evidence.
#ifndef PLURX_FEL_COMPOSE_H
#define PLURX_FEL_COMPOSE_H
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
// FEL_COMPOSE_CPU_ONLY keeps the parameters, checks, mapping table, uniform
// packing and GLSL, without FFmpeg or libplacebo: the CPU port of the shader
// (controls/fel_compose_cpu_port.py) builds on it.
#ifndef FEL_COMPOSE_CPU_ONLY
#include <libavutil/dovi_meta.h>
#include <libplacebo/dispatch.h>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/custom.h>
#endif

#define FEL_COMPOSE_METHOD "ccm001-integer-v1"

enum {
  FEL_COMPOSE_PIECES = 8,                        // AV_DOVI_MAX_PIECES
  FEL_COMPOSE_PIVOTS = FEL_COMPOSE_PIECES + 1,
  FEL_COMPOSE_TERMS = 22,                        // MMR constant + 3 x 7
};
#ifndef FEL_COMPOSE_CPU_ONLY
_Static_assert(AV_DOVI_MAX_PIECES == FEL_COMPOSE_PIECES, "FFmpeg's piece count");
#endif

// Exact parameters of one frame, in the specification's fixed-point units.
struct fel_compose_params {
  int coefficient_log2_denom, bl_bit_depth, el_bit_depth, hdr_bit_depth;
  bool disable_residual;
  struct {
    int num_pivots;
    int pivot[FEL_COMPOSE_PIVOTS];
    int mapping_idc[FEL_COMPOSE_PIECES];   // 0 polynomial, 1 MMR
    int order[FEL_COMPOSE_PIECES];         // poly_order or mmr_order (1..)
    // Polynomial: coef[0..order]. MMR: coef[0] is the constant, then
    // coef[1 + 7 * (i - 1) + j] for order i and term j.
    int64_t coef[FEL_COMPOSE_PIECES][FEL_COMPOSE_TERMS];
  } map[3];
  struct {
    int offset;
    uint64_t slope, threshold, hdr_in_max;
  } nlq[3];
};

// Returns NULL when the stage implements the parameters, else why not.
static inline const char *fel_compose_check(const struct fel_compose_params *p) {
  if ((p->bl_bit_depth != 8 && p->bl_bit_depth != 10) ||
      (p->el_bit_depth != 8 && p->el_bit_depth != 10) ||
      (p->hdr_bit_depth != 10 && p->hdr_bit_depth != 12))
    return "CCM 001 composition: bit depths outside clause 5.3.2";
  if (p->coefficient_log2_denom < p->el_bit_depth + 5 || p->coefficient_log2_denom > 32)
    return "CCM 001 composition: coefficient_log2_denom outside clause 5.3.2";
  for (int c = 0; c < 3; c++) {
    int n = p->map[c].num_pivots;
    if (n < 2 || n > FEL_COMPOSE_PIVOTS)
      return "CCM 001 composition: pivot count";
    if (p->map[c].pivot[0] < 0 || p->map[c].pivot[n - 1] > (1 << p->bl_bit_depth) - 1)
      return "CCM 001 composition: pivots outside the BL code range";
    for (int i = 1; i < n; i++)
      if (p->map[c].pivot[i] < p->map[c].pivot[i - 1])
        return "CCM 001 composition: decreasing pivots";
    for (int k = 0; k < n - 1; k++) {
      int idc = p->map[c].mapping_idc[k], order = p->map[c].order[k];
      if (idc == 0 ? (order < 1 || order > 2) :
          idc == 1 ? (c == 0 || order < 1 || order > 3) : true)
        return "CCM 001 composition: mapping method or order";
      // Clause 5.3.3's integer-part ranges: polynomial -64..63, MMR
      // -65536..65535. MMR terms then reach 2^69, inside the accumulator.
      int64_t limit = (idc == 0 ? INT64_C(64) : INT64_C(65536)) << p->coefficient_log2_denom;
      int terms = idc == 0 ? order + 1 : 1 + 7 * order;
      for (int t = 0; t < terms; t++)
        if (p->map[c].coef[k][t] < -limit || p->map[c].coef[k][t] >= limit)
          return "CCM 001 composition: coefficient outside clause 5.3.3's range";
    }
    if (p->nlq[c].offset < 0 || p->nlq[c].offset > (1 << p->el_bit_depth) - 1)
      return "CCM 001 composition: nlq_offset outside clause 5.3.4";
    // 2^12 residual magnitudes times the slope stay below 2^63, so adding the
    // shifted threshold (below 2^63) cannot carry out of 64 bits.
    int up = 11 - p->el_bit_depth;
    if (p->nlq[c].slope >= (UINT64_C(1) << 51) ||
        p->nlq[c].threshold >= (UINT64_C(1) << (63 - up)) ||
        p->nlq[c].hdr_in_max >= (UINT64_C(1) << (63 - up)))
      return "CCM 001 composition: NLQ parameter range";
  }
  return NULL;
}

#ifndef FEL_COMPOSE_CPU_ONLY
// The exact values FFmpeg decoded from the RPU (pivots already accumulated).
static inline const char *fel_compose_from_avdovi(const AVDOVIMetadata *data,
                                                  struct fel_compose_params *p) {
  const AVDOVIRpuDataHeader *h = av_dovi_get_header(data);
  const AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  memset(p, 0, sizeof(*p));
  if (m->nlq_method_idc != AV_DOVI_NLQ_LINEAR_DZ || m->num_x_partitions != 1 ||
      m->num_y_partitions != 1)
    return "CCM 001 composition: LINEAR_DZ with one partition only";
  p->coefficient_log2_denom = h->coef_log2_denom;
  p->bl_bit_depth = h->bl_bit_depth;
  p->el_bit_depth = h->el_bit_depth;
  p->hdr_bit_depth = h->vdr_bit_depth;
  p->disable_residual = h->disable_residual_flag;
  for (int c = 0; c < 3; c++) {
    const AVDOVIReshapingCurve *curve = &m->curves[c];
    if (curve->num_pivots < 2 || curve->num_pivots > FEL_COMPOSE_PIVOTS)
      return "CCM 001 composition: pivot count";
    p->map[c].num_pivots = curve->num_pivots;
    for (int i = 0; i < curve->num_pivots; i++)
      p->map[c].pivot[i] = curve->pivots[i];
    for (int k = 0; k < curve->num_pivots - 1; k++) {
      if (curve->mapping_idc[k] == AV_DOVI_MAPPING_POLYNOMIAL) {
        p->map[c].mapping_idc[k] = 0;
        p->map[c].order[k] = curve->poly_order[k];
        for (int i = 0; i <= curve->poly_order[k] && i < 3; i++)
          p->map[c].coef[k][i] = curve->poly_coef[k][i];
      } else {
        p->map[c].mapping_idc[k] = 1;
        p->map[c].order[k] = curve->mmr_order[k];
        p->map[c].coef[k][0] = curve->mmr_constant[k];
        for (int i = 0; i < curve->mmr_order[k] && i < 3; i++)
          for (int j = 0; j < 7; j++)
            p->map[c].coef[k][1 + 7 * i + j] = curve->mmr_coef[k][i][j];
      }
    }
    p->nlq[c].offset = m->nlq[c].nlq_offset;
    p->nlq[c].slope = m->nlq[c].linear_deadzone_slope;
    p->nlq[c].threshold = m->nlq[c].linear_deadzone_threshold;
    p->nlq[c].hdr_in_max = m->nlq[c].vdr_in_max;
  }
  return fel_compose_check(p);
}
#endif

// Clauses 5.4.2.2 and 5.4.2.3.2 for every BL code of every component: the
// piece the code selects and, for a polynomial piece, its mapped value; an
// MMR piece is marked 1 + its index for the shader, which has the other two
// BL samples MMR needs. Within clause 5.3.3.2's coefficient range each term
// is below 2^58, so int64 is exact. Computed once per frame on the CPU, it
// replaces a per-pixel pivot search and 96-bit polynomial on the GPU.
static inline void fel_compose_table(const struct fel_compose_params *p, uint16_t table[3][1024][2]) {
  memset(table, 0, sizeof(uint16_t) * 3 * 1024 * 2);
  const int bd = p->bl_bit_depth;
  for (int c = 0; c < 3; c++) {
    const int n = p->map[c].num_pivots;
    const int lo = p->map[c].pivot[0], hi = p->map[c].pivot[n - 1];
    for (int s = 0; s < (1 << bd); s++) {
      int k = n - 2;
      for (int i = 0; i < n - 1; i++)
        if (s < p->map[c].pivot[i + 1]) { k = i; break; }
      if (p->map[c].mapping_idc[k] != 0) {
        table[c][s][1] = (uint16_t)(k + 1);
        continue;
      }
      int64_t clamped = s < lo ? lo : s > hi ? hi : s, ss = 1, vv = 0;
      int shift = 20;
      for (int i = 0; i <= p->map[c].order[k]; i++) {
        vv += p->map[c].coef[k][i] * (ss << shift);
        ss *= clamped;
        shift -= bd;
      }
      int64_t v = vv < 0 ? 0 : vv >> (4 + p->coefficient_log2_denom);
      table[c][s][0] = (uint16_t)(v > 0xffff ? 0xffff : v);
    }
  }
}

// Uniform layout, ivec4 slots. An int64 is (low word, high word).
enum {
  FC_U_HEADER = 0,   // (denom, bl depth, el depth, hdr depth)
  FC_U_FLAGS = 1,    // (el scale, disable residual, 16 - bl depth, 16 - el depth)
  FC_U_CHANNEL = 2,  // BL channels (Y, Cb, Cr, -), then EL channels at 3
  FC_U_PIVOTS = 4,   // component c: slots 4 + 3c .. 6 + 3c: count, pivots
  FC_U_PIECES = 13,  // component c: slots 13 + 2c, 14 + 2c: idc << 8 | order
  FC_U_NLQ = 19,     // component c: (offset, slope lo, hi, threshold' lo),
                     //              (threshold' hi, limit lo, hi, -)
  FC_U_COEF = 25,    // ((c * 8 + k) * 22 + t) int64s, two per slot
  FC_U_SIZE = FC_U_COEF + 3 * FEL_COMPOSE_PIECES * FEL_COMPOSE_TERMS / 2,
};

static inline void fel_compose_pack(const struct fel_compose_params *p, int el_scale,
                                    const int bl_channel[3], const int el_channel[3],
                                    int32_t u[FC_U_SIZE][4]) {
  memset(u, 0, sizeof(int32_t) * FC_U_SIZE * 4);
  u[FC_U_HEADER][0] = p->coefficient_log2_denom;
  u[FC_U_HEADER][1] = p->bl_bit_depth;
  u[FC_U_HEADER][2] = p->el_bit_depth;
  u[FC_U_HEADER][3] = p->hdr_bit_depth;
  u[FC_U_FLAGS][0] = el_scale;
  u[FC_U_FLAGS][1] = p->disable_residual;
  u[FC_U_FLAGS][2] = 16 - p->bl_bit_depth;
  u[FC_U_FLAGS][3] = 16 - p->el_bit_depth;
  int32_t *ints = &u[0][0];
  for (int c = 0; c < 3; c++) {
    u[FC_U_CHANNEL][c] = bl_channel[c];
    u[FC_U_CHANNEL + 1][c] = el_channel[c];
    int32_t *pv = ints + 4 * (FC_U_PIVOTS + 3 * c);
    pv[0] = p->map[c].num_pivots;
    for (int i = 0; i < p->map[c].num_pivots; i++)
      pv[1 + i] = p->map[c].pivot[i];
    int32_t *pc = ints + 4 * (FC_U_PIECES + 2 * c);
    for (int k = 0; k < FEL_COMPOSE_PIECES; k++)
      pc[k] = p->map[c].mapping_idc[k] << 8 | p->map[c].order[k];
    int up = 11 - p->el_bit_depth;
    uint64_t threshold = p->nlq[c].threshold << up, limit = p->nlq[c].hdr_in_max << up;
    int32_t *nq = ints + 4 * (FC_U_NLQ + 2 * c);
    nq[0] = p->nlq[c].offset;
    nq[1] = (int32_t)(uint32_t)p->nlq[c].slope;
    nq[2] = (int32_t)(uint32_t)(p->nlq[c].slope >> 32);
    nq[3] = (int32_t)(uint32_t)threshold;
    nq[4] = (int32_t)(uint32_t)(threshold >> 32);
    nq[5] = (int32_t)(uint32_t)limit;
    nq[6] = (int32_t)(uint32_t)(limit >> 32);
    for (int k = 0; k < FEL_COMPOSE_PIECES; k++)
      for (int t = 0; t < FEL_COMPOSE_TERMS; t++) {
        uint64_t bits = (uint64_t)p->map[c].coef[k][t];
        int32_t *slot = ints + 4 * FC_U_COEF + 2 * ((c * FEL_COMPOSE_PIECES + k) * FEL_COMPOSE_TERMS + t);
        slot[0] = (int32_t)(uint32_t)bits;
        slot[1] = (int32_t)(uint32_t)(bits >> 32);
      }
  }
}

// GLSL shared by the passes. Descriptors: bl0..bl2 and el0..el2 (nearest,
// clamped), ev0/ev1 (the vertical pass), lut (fel_compose_table, RG16 1024 x
// 3), uniform fc[] as laid out above. Signed >> in GLSL extends the
// sign, so it is clause 3.2.2's floor shift.
static const char fel_compose_glsl_header[] =
  "int fc_code(sampler2D t, int ch, int shift, ivec2 p) {\n"
  "  p = clamp(p, ivec2(0), textureSize(t, 0) - 1);\n"
  "  return int(round(texelFetch(t, p, 0)[ch] * 65535.0)) >> shift;\n"
  "}\n"
  "int fc_bl(int c, ivec2 p) {\n"
  "  int sh = fc[1].z;\n"
  "  if (c == 0) return fc_code(bl0, fc[2].x, sh, p);\n"
  "  if (c == 1) return fc_code(bl1, fc[2].y, sh, p);\n"
  "  return fc_code(bl2, fc[2].z, sh, p);\n"
  "}\n"
  "int fc_el(int c, ivec2 p) {\n"
  "  int sh = fc[1].w;\n"
  "  if (c == 0) return fc_code(el0, fc[3].x, sh, p);\n"
  "  if (c == 1) return fc_code(el1, fc[3].y, sh, p);\n"
  "  return fc_code(el2, fc[3].z, sh, p);\n"
  "}\n"
  // Annex B.3: vertical EL filter of column x at output row y, clipped.
  "int fc_el_v(int c, int x, int y) {\n"
  "  int k = y >> 1, v;\n"
  "  if (c == 0) {\n"
  "    v = (y & 1) == 0 ?\n"
  "      (-3 * fc_el(0, ivec2(x, k - 2)) + 29 * fc_el(0, ivec2(x, k - 1)) + 111 * fc_el(0, ivec2(x, k)) - 9 * fc_el(0, ivec2(x, k + 1)) + 64) >> 7 :\n"
  "      (-9 * fc_el(0, ivec2(x, k - 1)) + 111 * fc_el(0, ivec2(x, k)) + 29 * fc_el(0, ivec2(x, k + 1)) - 3 * fc_el(0, ivec2(x, k + 2)) + 64) >> 7;\n"
  "  } else {\n"
  "    v = (y & 1) == 0 ?\n"
  "      (64 * fc_el(c, ivec2(x, k - 1)) + 192 * fc_el(c, ivec2(x, k)) + 128) >> 8 :\n"
  "      (192 * fc_el(c, ivec2(x, k)) + 64 * fc_el(c, ivec2(x, k + 1)) + 128) >> 8;\n"
  "  }\n"
  "  return clamp(v, 0, 65535);\n"
  "}\n"
  // Annex B.2 horizontal filter after the vertical one; the clamp of the
  // column index is the edge replication of both filters.
  // The vertical pass's result (ev0: Y, ev1: Cb and Cr), column-clamped,
  // which is the horizontal filter's edge replication.
  "int fc_ev(int c, ivec2 p) {\n"
  "  if (c == 0) { p = clamp(p, ivec2(0), textureSize(ev0, 0) - 1); return int(round(texelFetch(ev0, p, 0).r * 65535.0)); }\n"
  "  p = clamp(p, ivec2(0), textureSize(ev1, 0) - 1);\n"
  "  vec2 v = texelFetch(ev1, p, 0).rg;\n"
  "  return int(round((c == 1 ? v.x : v.y) * 65535.0));\n"
  "}\n"
  "int fc_el_up(int c, ivec2 q) {\n"
  "  int k = q.x >> 1;\n"
  "  if ((q.x & 1) == 0) return fc_ev(c, ivec2(k, q.y));\n"
  "  const int tap[8] = int[8](22, 94, -524, 2456, 2456, -524, 94, 22);\n"
  "  int s = 2048;\n"
  "  for (int i = 0; i < 8; i++) s += tap[i] * fc_ev(c, ivec2(k - 3 + i, q.y));\n"
  "  return clamp(s >> 12, 0, 65535);\n"
  "}\n"
  "int fc_el_at(int c, ivec2 q) { return fc[1].x == 1 ? fc_el(c, q) : fc_el_up(c, q); }\n"
  "int fc_int(int c, int i) { return fc[4 + 3 * c + (i >> 2)][i & 3]; }\n"
  "int fc_npiv(int c) { return fc_int(c, 0); }\n"
  "int fc_piv(int c, int i) { return fc_int(c, 1 + i); }\n"
  "int fc_piece_word(int c, int k) { return fc[13 + 2 * c + (k >> 2)][k & 3]; }\n"
  "ivec2 fc_coef(int c, int k, int t) {\n"
  "  int e = (c * 8 + k) * 22 + t;\n"
  "  ivec4 s = fc[25 + (e >> 1)];\n"
  "  return (e & 1) == 0 ? s.xy : s.zw;\n"
  "}\n"
  "int fc_clamp_piv(int c, int s) { return clamp(s, fc_piv(c, 0), fc_piv(c, fc_npiv(c) - 1)); }\n"
  // a (96-bit two's complement, x lowest) += coefficient * t, t < 2^21.
  "void fc_madd(inout uvec3 a, ivec2 coef, uint t) {\n"
  "  uint m, l, c0, c1, c2;\n"
  "  umulExtended(uint(coef.x), t, m, l);\n"
  "  a.x = uaddCarry(a.x, l, c0);\n"
  "  a.y = uaddCarry(a.y, m, c1);\n"
  "  a.y = uaddCarry(a.y, c0, c2);\n"
  "  a.z += c1 + c2;\n"
  "  int mh, lh;\n"
  "  imulExtended(coef.y, int(t), mh, lh);\n"
  "  a.y = uaddCarry(a.y, uint(lh), c0);\n"
  "  a.z += uint(mh) + c0;\n"
  "}\n"
  // max(a, 0) >> n, clipped to 0xFFFF (clauses 5.4.2.3.2 and 5.4.2.3.3).
  "int fc_mapped(uvec3 a, int n) {\n"
  "  if (int(a.z) < 0) return 0;\n"
  "  uvec3 r;\n"
  "  if (n >= 64) { r = uvec3(a.z >> (n - 64), 0u, 0u); }\n"
  "  else if (n >= 32) { int k = n - 32; r = uvec3(k == 0 ? a.y : (a.y >> k) | (a.z << (32 - k)), a.z >> k, 0u); }\n"
  "  else { int k = n; r = uvec3(k == 0 ? a.x : (a.x >> k) | (a.y << (32 - k)), k == 0 ? a.y : (a.y >> k) | (a.z << (32 - k)), a.z >> k); }\n"
  "  if (r.y != 0u || r.z != 0u || r.x > 65535u) return 65535;\n"
  "  return int(r.x);\n"
  "}\n"
  // (a * b) >> 20 for a, b < 2^21.
  "uint fc_mul20(uint a, uint b) { uint m, l; umulExtended(a, b, m, l); return (m << 12) | (l >> 20); }\n"
  // Clause 5.4.2.3.3; s0 is the down-sampled BL luma.
  "int fc_mmr(int c, int k, int s0, int s1, int s2) {\n"
  "  int bd = fc[0].y, order = fc_piece_word(c, k) & 255;\n"
  "  uint a0 = uint(fc_clamp_piv(0, s0)), a1 = uint(fc_clamp_piv(1, s1)), a2 = uint(fc_clamp_piv(2, s2));\n"
  "  uint t[22];\n"
  "  for (int i = 0; i < 22; i++) t[i] = 0u;\n"
  "  t[0] = 1u << 20;\n"
  "  uint b1 = uint(20 - bd), b2 = uint(20 - 2 * bd);\n"
  "  t[1] = a0 << b1; t[2] = a1 << b1; t[3] = a2 << b1;\n"
  "  t[4] = (a0 * a1) << b2; t[5] = (a0 * a2) << b2; t[6] = (a1 * a2) << b2;\n"
  "  t[7] = fc_mul20(t[4], t[3]);\n"
  "  if (order >= 2) {\n"
  "    t[8] = (a0 * a0) << b2; t[9] = (a1 * a1) << b2; t[10] = (a2 * a2) << b2;\n"
  "    t[11] = fc_mul20(t[4], t[4]); t[12] = fc_mul20(t[5], t[5]); t[13] = fc_mul20(t[6], t[6]); t[14] = fc_mul20(t[7], t[7]);\n"
  "  }\n"
  "  if (order >= 3) {\n"
  "    t[15] = fc_mul20(t[1], t[8]); t[16] = fc_mul20(t[2], t[9]); t[17] = fc_mul20(t[3], t[10]);\n"
  "    t[18] = fc_mul20(t[4], t[11]); t[19] = fc_mul20(t[5], t[12]); t[20] = fc_mul20(t[6], t[13]); t[21] = fc_mul20(t[7], t[14]);\n"
  "  }\n"
  "  uvec3 a = uvec3(0u);\n"
  "  int n = 1 + 7 * order;\n"
  "  for (int i = 0; i < n; i++) fc_madd(a, fc_coef(c, k, i), t[i]);\n"
  "  return fc_mapped(a, 4 + fc[0].x);\n"
  "}\n"
  // Clause 5.4.2.3.3's BL luma at chroma position q: [1 2 1] on two rows.
  "int fc_down(ivec2 q) {\n"
  "  int x = 2 * q.x, y = 2 * q.y;\n"
  "  int top = (fc_bl(0, ivec2(x - 1, y)) + 2 * fc_bl(0, ivec2(x, y)) + fc_bl(0, ivec2(x + 1, y)) + 2) >> 2;\n"
  "  int bot = (fc_bl(0, ivec2(x - 1, y + 1)) + 2 * fc_bl(0, ivec2(x, y + 1)) + fc_bl(0, ivec2(x + 1, y + 1)) + 2) >> 2;\n"
  "  return (top + bot + 1) >> 1;\n"
  "}\n"
  // Clause 5.4.3.2 LINEAR_DZ, clamped at vdr_in_max; saturated at 2^30,
  // beyond which clause 5.4.3.3 clips the same way.
  "int fc_nlq(int c, int e) {\n"
  "  ivec4 n0 = fc[19 + 2 * c], n1 = fc[20 + 2 * c];\n"
  "  int rr = e - n0.x;\n"
  "  if (rr == 0) return 0;\n"
  "  uint mag = uint(2 * abs(rr) - 1) << uint(10 - fc[0].z);\n"
  "  uint hi, lo, cy;\n"
  "  umulExtended(mag, uint(n0.y), hi, lo);\n"
  "  hi += mag * uint(n0.z);\n"
  "  lo = uaddCarry(lo, uint(n0.w), cy);\n"
  "  hi += uint(n1.x) + cy;\n"
  "  uint lh = uint(n1.z), ll = uint(n1.y);\n"
  "  if (hi > lh || (hi == lh && lo > ll)) { hi = lh; lo = ll; }\n"
  "  int k = fc[0].x - 5 - fc[0].z;\n"
  "  if (rr < 0) { uint add = (1u << uint(k)) - 1u; lo = uaddCarry(lo, add, cy); hi += cy; }\n"
  "  uint qhi = k == 0 ? hi : hi >> uint(k);\n"
  "  uint qlo = k == 0 ? lo : (lo >> uint(k)) | (hi << uint(32 - k));\n"
  "  int q = (qhi != 0u || qlo >= (1u << 30)) ? (1 << 30) : int(qlo);\n"
  "  return rr < 0 ? -q : q;\n"
  "}\n"
  // Clause 5.4.3.3 for a PQ base layer; returns h << (16 - hdr) / 65535.
  "float fc_out(int v, int r) {\n"
  "  int hdr = fc[0].w;\n"
  "  int h = v + (fc[1].y != 0 ? 0 : r) + (1 << (15 - hdr));\n"
  "  h = clamp(h >> (16 - hdr), 0, (1 << hdr) - 1);\n"
  "  return float(h << (16 - hdr)) / 65535.0;\n"
  "}\n"
  // The frame's table (fel_compose_table): the mapped value of a
  // polynomial piece, or 1 + the index of an MMR piece, which needs the
  // three BL samples and is computed here.
  "int fc_map(int c, ivec2 q, int s) {\n"
  "  vec2 e = texelFetch(lut, ivec2(s, c), 0).rg;\n"
  "  int k = int(round(e.y * 65535.0));\n"
  "  if (k == 0) return int(round(e.x * 65535.0));\n"
  "  return fc_mmr(c, k - 1, fc_down(q), fc_bl(1, q), fc_bl(2, q));\n"
  "}\n";

// Annex B.3's vertical EL filters: EL width x BL height (Y) or half of each
// (Cb, Cr), clipped to 0..0xFFFF.
static const char fel_compose_glsl_v_luma[] =
  "ivec2 q = ivec2(gl_FragCoord.xy);\n"
  "color = vec4(float(fc_el_v(0, q.x, q.y)) / 65535.0, 0.0, 0.0, 1.0);\n";

static const char fel_compose_glsl_v_chroma[] =
  "ivec2 q = ivec2(gl_FragCoord.xy);\n"
  "color = vec4(float(fc_el_v(1, q.x, q.y)) / 65535.0, float(fc_el_v(2, q.x, q.y)) / 65535.0, 0.0, 1.0);\n";

static const char fel_compose_glsl_luma[] =
  "ivec2 q = ivec2(gl_FragCoord.xy);\n"
  "int v = fc_map(0, q, fc_bl(0, q));\n"
  "color = vec4(fc_out(v, fc_nlq(0, fc_el_at(0, q))), 0.0, 0.0, 1.0);\n";

static const char fel_compose_glsl_chroma[] =
  "ivec2 q = ivec2(gl_FragCoord.xy);\n"
  "int vb = fc_map(1, q, fc_bl(1, q)), vr = fc_map(2, q, fc_bl(2, q));\n"
  "color = vec4(fc_out(vb, fc_nlq(1, fc_el_at(1, q))), fc_out(vr, fc_nlq(2, fc_el_at(2, q))), 0.0, 1.0);\n";

#ifndef FEL_COMPOSE_CPU_ONLY
// One layer's planes: the texture and channel holding each component, as
// UNORM16 with the code in the top bits (code << (16 - bit depth)).
struct fel_compose_layer {
  pl_tex tex[3];
  int channel[3];
};

static struct {
  pl_dispatch dispatch;
  pl_tex luma, chroma, v_luma, v_chroma, unused, table;
  int32_t u[FC_U_SIZE][4];
  uint16_t table_data[3][1024][2];
  // Controls only: when set, each pass's GPU time (ns, from the previous
  // frames; 0 until the GPU reports it) accumulates in timing_ns.
  bool timing;
  pl_gpu gpu_for_timing;
  pl_timer timer[4];
  uint64_t timing_ns[4];
} fel_compose_gpu;

static inline pl_tex fel_compose_target(pl_gpu gpu, pl_tex *t, int w, int h, int comps) {
  if (*t && ((*t)->params.w != w || (*t)->params.h != h)) pl_tex_destroy(gpu, t);
  if (!*t) {
    pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_UNORM, comps, 16, 16,
                             PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR);
    if (!fmt) return NULL;
    *t = pl_tex_create(gpu, pl_tex_params(.w = w, .h = h, .format = fmt,
                                          .sampleable = true, .renderable = true,
                                          .host_readable = fmt->caps & PL_FMT_CAP_HOST_READABLE));
  }
  return *t;
}

// ev0/ev1 are the vertical pass's results, or (an EL at BL size, or the
// vertical pass itself) a 1x1 placeholder the body never reads. libplacebo
// binds each texture once, and the layers often share one (Cb and Cr
// interleaved in a mapped P010 surface), so each distinct texture becomes
// one descriptor fct<n> and the shader's names are #defined onto them.
static inline bool fel_compose_pass(int index, pl_tex target, const struct fel_compose_layer *bl,
                                    const struct fel_compose_layer *el, pl_tex ev0, pl_tex ev1,
                                    const char *body) {
  pl_timer timer = NULL;
  if (fel_compose_gpu.timing) {
    timer = fel_compose_gpu.timer[index];
    if (timer) fel_compose_gpu.timing_ns[index] += pl_timer_query(fel_compose_gpu.gpu_for_timing, timer);
  }
  static const char *names[9] = {"bl0", "bl1", "bl2", "el0", "el1", "el2", "ev0", "ev1", "lut"};
  static const char *slots[9] = {"fct0", "fct1", "fct2", "fct3", "fct4", "fct5", "fct6", "fct7", "fct8"};
  const pl_tex objects[9] = {bl->tex[0], bl->tex[1], bl->tex[2], el->tex[0], el->tex[1], el->tex[2],
                             ev0 ? ev0 : fel_compose_gpu.unused, ev1 ? ev1 : fel_compose_gpu.unused,
                             fel_compose_gpu.table};
  struct pl_shader_desc d[9];
  int count = 0;
  char header[sizeof(fel_compose_glsl_header) + 9 * 24];
  int used = 0;
  for (int i = 0; i < 9; i++) {
    int slot = 0;
    while (slot < count && d[slot].binding.object != objects[i]) slot++;
    if (slot == count)
      d[count++] = (struct pl_shader_desc){
          .desc = {.name = slots[slot], .type = PL_DESC_SAMPLED_TEX},
          .binding = {.object = objects[i],
                      .address_mode = PL_TEX_ADDRESS_CLAMP,
                      .sample_mode = PL_TEX_SAMPLE_NEAREST}};
    used += snprintf(header + used, sizeof(header) - used, "#define %s %s\n", names[i], slots[slot]);
  }
  snprintf(header + used, sizeof(header) - used, "%s", fel_compose_glsl_header);
  struct pl_var var = pl_var_ivec4("fc");
  var.dim_a = FC_U_SIZE;
  struct pl_shader_var v = {.var = var, .data = fel_compose_gpu.u, .dynamic = true};
  pl_shader sh = pl_dispatch_begin(fel_compose_gpu.dispatch);
  bool ok = pl_shader_custom(sh, &(struct pl_custom_shader){
      .description = FEL_COMPOSE_METHOD, .header = header, .body = body,
      .input = PL_SHADER_SIG_NONE, .output = PL_SHADER_SIG_COLOR,
      .descriptors = d, .num_descriptors = count, .variables = &v, .num_variables = 1,
      .output_w = target->params.w, .output_h = target->params.h});
  if (!ok) {
    pl_dispatch_abort(fel_compose_gpu.dispatch, &sh);
    return false;
  }
  return pl_dispatch_finish(fel_compose_gpu.dispatch,
                            pl_dispatch_params(.shader = &sh, .target = target, .timer = timer));
}

// Composes one frame. width x height is the BL; the EL is the same size or
// half of it. On success *luma (R16) and *chroma (RG16, Cb then Cr) hold the
// composed codes << (16 - hdr_bit_depth). Returns NULL or why it refused.
static inline const char *fel_compose_render(pl_log log, pl_gpu gpu,
                                             const struct fel_compose_params *p,
                                             const struct fel_compose_layer *bl,
                                             const struct fel_compose_layer *el,
                                             int width, int height, int el_width, int el_height,
                                             pl_tex *luma, pl_tex *chroma) {
  const char *why = fel_compose_check(p);
  if (why) return why;
  if (gpu->glsl.version < 400 || !gpu->glsl.vulkan)
    return "CCM 001 composition needs Vulkan GLSL 4.00 integer arithmetic";
  if (width < 2 || height < 2 || (width & 1) || (height & 1))
    return "CCM 001 composition: BL must have positive even dimensions";
  int scale = el_width == width && el_height == height ? 1 :
              2 * el_width == width && 2 * el_height == height ? 2 : 0;
  if (!scale) return "CCM 001 composition: EL must be BL size or half of it";
  if (!fel_compose_gpu.dispatch) fel_compose_gpu.dispatch = pl_dispatch_create(log, gpu);
  if (fel_compose_gpu.timing && !fel_compose_gpu.timer[0]) {
    fel_compose_gpu.gpu_for_timing = gpu;
    for (int i = 0; i < 4; i++) fel_compose_gpu.timer[i] = pl_timer_create(gpu);
  }
  if (!fel_compose_gpu.dispatch ||
      !fel_compose_target(gpu, &fel_compose_gpu.luma, width, height, 1) ||
      !fel_compose_target(gpu, &fel_compose_gpu.chroma, width / 2, height / 2, 2) ||
      !fel_compose_target(gpu, &fel_compose_gpu.unused, 1, 1, 2))
    return "CCM 001 composition: renderable UNORM16 R and RG targets";
  if (!fel_compose_gpu.table) {
    pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_UNORM, 2, 16, 16, PL_FMT_CAP_SAMPLEABLE);
    if (fmt)
      fel_compose_gpu.table = pl_tex_create(gpu, pl_tex_params(.w = 1024, .h = 3, .format = fmt,
                                                                .sampleable = true, .host_writable = true));
  }
  fel_compose_table(p, fel_compose_gpu.table_data);
  if (!fel_compose_gpu.table ||
      !pl_tex_upload(gpu, pl_tex_transfer_params(.tex = fel_compose_gpu.table, .ptr = fel_compose_gpu.table_data)))
    return "CCM 001 composition: mapping table upload";
  fel_compose_pack(p, scale, bl->channel, el->channel, fel_compose_gpu.u);
  pl_tex ev0 = NULL, ev1 = NULL;
  if (scale == 2) {
    if (!(ev0 = fel_compose_target(gpu, &fel_compose_gpu.v_luma, el_width, height, 1)) ||
        !(ev1 = fel_compose_target(gpu, &fel_compose_gpu.v_chroma, el_width / 2, height / 2, 2)))
      return "CCM 001 composition: renderable UNORM16 R and RG targets";
    if (!fel_compose_pass(0, ev0, bl, el, NULL, NULL, fel_compose_glsl_v_luma) ||
        !fel_compose_pass(1, ev1, bl, el, NULL, NULL, fel_compose_glsl_v_chroma))
      return "CCM 001 composition shader dispatch";
  }
  if (!fel_compose_pass(2, fel_compose_gpu.luma, bl, el, ev0, ev1, fel_compose_glsl_luma) ||
      !fel_compose_pass(3, fel_compose_gpu.chroma, bl, el, ev0, ev1, fel_compose_glsl_chroma))
    return "CCM 001 composition shader dispatch";
  *luma = fel_compose_gpu.luma;
  *chroma = fel_compose_gpu.chroma;
  return NULL;
}

// The composed picture as a libplacebo frame: Dolby Vision colour with the
// RPU's matrices and offsets, no reshaping (no pivots), no NLQ and no
// enhancement layer. *dovi receives the metadata the frame points at.
static inline struct pl_frame fel_compose_frame(pl_tex luma, pl_tex chroma, int hdr_bit_depth,
                                                const struct pl_dovi_metadata *rpu,
                                                struct pl_dovi_metadata *dovi) {
  *dovi = *rpu;
  for (int c = 0; c < 3; c++)
    dovi->comp[c].num_pivots = 0;
  dovi->nlq_active = false;
  return (struct pl_frame){
      .num_planes = 2,
      .planes = {{.texture = luma, .components = 1,
                  .component_mapping = {0, PL_CHANNEL_NONE, PL_CHANNEL_NONE, PL_CHANNEL_NONE}},
                 {.texture = chroma, .components = 2,
                  .component_mapping = {1, 2, PL_CHANNEL_NONE, PL_CHANNEL_NONE}}},
      .repr = {.sys = PL_COLOR_SYSTEM_DOLBYVISION, .levels = PL_COLOR_LEVELS_FULL, .dovi = dovi,
               .bits = {.sample_depth = 16, .color_depth = hdr_bit_depth,
                        .bit_shift = 16 - hdr_bit_depth}}};
}

static inline void fel_compose_destroy(pl_gpu gpu) {
  pl_tex_destroy(gpu, &fel_compose_gpu.luma);
  pl_tex_destroy(gpu, &fel_compose_gpu.chroma);
  pl_tex_destroy(gpu, &fel_compose_gpu.v_luma);
  pl_tex_destroy(gpu, &fel_compose_gpu.v_chroma);
  pl_tex_destroy(gpu, &fel_compose_gpu.unused);
  pl_tex_destroy(gpu, &fel_compose_gpu.table);
  for (int i = 0; i < 4; i++) pl_timer_destroy(gpu, &fel_compose_gpu.timer[i]);
  pl_dispatch_destroy(&fel_compose_gpu.dispatch);
}
#endif  // FEL_COMPOSE_CPU_ONLY
#endif
