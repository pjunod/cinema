// SPDX-License-Identifier: Apache-2.0
// Hand-computed vectors for each stage of the independent composer reference.
// Every expected value is worked out in the comment beside it from the clause
// formula, not taken from running any composer.
#include "fel_reference_composer.h"
#include <assert.h>
#include <stdio.h>

#define ONE (INT64_C(1) << 23)  // 1.0 at coefficient_log2_denom = 23

static struct frc_params identity_params(void) {
  // Linear identity mapping, one piece, zero-residual NLQ centred at 512.
  struct frc_params p;
  memset(&p, 0, sizeof(p));
  p.coefficient_log2_denom = 23;
  p.bl_bit_depth = 10;
  p.el_bit_depth = 10;
  p.hdr_bit_depth = 12;
  for (int c = 0; c < 3; c++) {
    p.map[c].num_pivots = 2;
    p.map[c].pivot_value[0] = 0;
    p.map[c].pivot_value[1] = 1023;
    p.map[c].piece[0].mapping_idc = FRC_MAPPING_POLYNOMIAL;
    p.map[c].piece[0].poly_order = 1;
    p.map[c].piece[0].poly_coef[1] = ONE;
    p.nlq[c].offset = 512;
    p.nlq[c].slope = 2048;
    p.nlq[c].hdr_in_max = 1048576;  // 0.125
  }
  return p;
}

static void test_shift_and_pivots(void) {
  // Clause 3.2.2: ">>" is an arithmetic (floor) shift.
  assert(frc_asr(-1, 8) == -1 && frc_asr(-256, 8) == -1 &&
         frc_asr(-257, 8) == -2 && frc_asr(255, 8) == 0);
  // Clause 5.3.2: pivots are running sums of pred_pivot_value.
  int32_t pred[3] = {64, 448, 428}, pivots[3];
  frc_assign_pivot_values(pred, 3, pivots);
  assert(pivots[0] == 64 && pivots[1] == 512 && pivots[2] == 940);
  struct frc_component_mapping m = {.num_pivots = 3};
  memcpy(m.pivot_value, pivots, sizeof(pivots));
  // Clause 5.4.2.2: below pivot[1] -> piece 0; from pivot[1] up -> piece 1.
  assert(frc_pivot_index(&m, 0) == 0);
  assert(frc_pivot_index(&m, 511) == 0);
  assert(frc_pivot_index(&m, 512) == 1);
  assert(frc_pivot_index(&m, 939) == 1);
  // Reading R2: at or above the top pivot the clause's index has no piece;
  // the last piece is used.
  assert(frc_pivot_index(&m, 940) == 1);
  assert(frc_pivot_index(&m, 1023) == 1);
}

static void test_polynomial(void) {
  struct frc_params p = identity_params();
  // Linear 1.0: v = (2^23 * (s << 10)) >> 27 = 64 s.
  assert(frc_map_polynomial(&p, 0, 0, 0) == 0);
  assert(frc_map_polynomial(&p, 0, 0, 512) == 32768);
  assert(frc_map_polynomial(&p, 0, 0, 1023) == 65472);
  // 0.25 + 0.5 x^2 at s = 512 (x = 0.5):
  // vv = 2^21 * 2^20 + 2^22 * 2^18 = 3 * 2^40; v = 3 * 2^40 >> 27 = 24576,
  // which is (0.25 + 0.125) * 65536.
  p.map[0].piece[0].poly_order = 2;
  p.map[0].piece[0].poly_coef[0] = ONE / 4;
  p.map[0].piece[0].poly_coef[1] = 0;
  p.map[0].piece[0].poly_coef[2] = ONE / 2;
  assert(frc_map_polynomial(&p, 0, 0, 512) == 24576);
  // Negative sums clamp to 0; sums past 16 bits clamp to 0xffff.
  p.map[0].piece[0].poly_order = 1;
  p.map[0].piece[0].poly_coef[0] = -ONE;  // int part -1, fraction 0
  p.map[0].piece[0].poly_coef[1] = 0;
  assert(frc_map_polynomial(&p, 0, 0, 700) == 0);
  p.map[0].piece[0].poly_coef[0] = 2 * ONE;  // 2^24 * 2^20 >> 27 = 2^17
  assert(frc_map_polynomial(&p, 0, 0, 700) == 0xffff);
  // Samples outside the pivots are clamped to them first: with pivots
  // 64..940 and linear 1.0, s = 10 maps as 64 and s = 1000 as 940.
  p = identity_params();
  p.map[0].pivot_value[0] = 64;
  p.map[0].pivot_value[1] = 940;
  assert(frc_map_polynomial(&p, 0, 0, 10) == 64 * 64);
  assert(frc_map_polynomial(&p, 0, 0, 1000) == 940 * 64);
  // 8-bit BL: shift starts at 20 and drops by 8, so linear 1.0 at s = 128
  // gives 2^23 * (128 << 12) >> 27 = 2^15 = 32768.
  p = identity_params();
  p.bl_bit_depth = 8;
  p.map[0].pivot_value[1] = 255;
  assert(frc_map_polynomial(&p, 0, 0, 128) == 32768);
}

static void test_luma_downsample(void) {
  // 4x2 luma; output (i, j) = (0, 0) and (1, 0).
  //   row 0: 100 200 300 400
  //   row 1: 500 600 700 800
  const uint16_t y[8] = {100, 200, 300, 400, 500, 600, 700, 800};
  // i = 0 replicates the left edge (x = -1 -> 0):
  //   top = (100 + 200 + 200 + 2) >> 2 = 125; bottom = (500 + 1000 + 600 +
  //   2) >> 2 = 525; (125 + 525 + 1) >> 1 = 325.
  assert(frc_downsample_luma(y, 4, 2, 4, 0, 0) == 325);
  // i = 1: top = (200 + 600 + 400 + 2) >> 2 = 300; bottom = (600 + 1400 +
  //   800 + 2) >> 2 = 700; (300 + 700 + 1) >> 1 = 500.
  assert(frc_downsample_luma(y, 4, 2, 4, 1, 0) == 500);
}

static void test_mmr(void) {
  struct frc_params p = identity_params();
  struct frc_piece *m = &p.map[1].piece[0];
  m->mapping_idc = FRC_MAPPING_MMR;
  // Order 1, coefficient 1.0 on s0 only, constant 0.125.
  // tt1 = 512 << 10 = 2^19; rr = 2^23 * 2^19 + 2^20 * 2^20 = 2^42 + 2^40;
  // v = rr >> 27 = 32768 + 8192 = 40960.
  m->mmr_order = 1;
  m->mmr_constant = ONE / 8;
  m->mmr_coef[0][0] = ONE;
  assert(frc_map_mmr(&p, 1, 0, 512, 512, 512) == 40960);
  // Cross term tt7 = s0 s1 s2: tt4 = 512 * 512 = 2^18, tt3 = 2^19,
  // tt7 = 2^37 >> 20 = 2^17; v = 2^23 * 2^17 >> 27 = 8192 = 0.5^3 * 65536.
  memset(m->mmr_coef, 0, sizeof(m->mmr_coef));
  m->mmr_constant = 0;
  m->mmr_coef[0][6] = ONE;
  assert(frc_map_mmr(&p, 1, 0, 512, 512, 512) == 8192);
  // Order 2, tt14 = tt7^2 >> 20 = 2^14 -> 1024 = 0.5^6 * 65536.
  m->mmr_order = 2;
  m->mmr_coef[0][6] = 0;
  m->mmr_coef[1][6] = ONE;
  assert(frc_map_mmr(&p, 1, 0, 512, 512, 512) == 1024);
  // Order 3, tt21 = tt7 * tt14 >> 20 = 2^11 -> 128 = 0.5^9 * 65536.
  m->mmr_order = 3;
  m->mmr_coef[1][6] = 0;
  m->mmr_coef[2][6] = ONE;
  assert(frc_map_mmr(&p, 1, 0, 512, 512, 512) == 128);
  // Order 2 square of s1: tt9 = 512 * 512 = 2^18 -> 2^41 >> 27 = 16384.
  m->mmr_order = 2;
  m->mmr_coef[2][6] = 0;
  m->mmr_coef[1][1] = ONE;
  assert(frc_map_mmr(&p, 1, 0, 0, 512, 0) == 16384);
  // The largest coefficients the syntax allows overflow 64 bits on the sum
  // (22 terms of up to 2^59); the 128-bit sum still clamps to 0xffff, and a
  // hugely negative sum to 0.
  memset(m->mmr_coef, 0, sizeof(m->mmr_coef));
  m->mmr_order = 3;
  for (int i = 0; i < 3; i++)
    for (int j = 0; j < 7; j++)
      m->mmr_coef[i][j] = INT64_C(65535) << 23;
  m->mmr_constant = INT64_C(65535) << 23;
  assert(frc_map_mmr(&p, 1, 0, 1023, 1023, 1023) == 0xffff);
  for (int i = 0; i < 3; i++)
    for (int j = 0; j < 7; j++)
      m->mmr_coef[i][j] = -(INT64_C(65536) << 23);
  m->mmr_constant = -(INT64_C(65536) << 23);
  assert(frc_map_mmr(&p, 1, 0, 1023, 1023, 1023) == 0);
  // MMR clamps each input to its own component's pivots: chroma pivots
  // 100..900 make s1 = 1000 act as 900. Order-1 coefficient 1.0 on s1:
  // tt2 = 900 << 10; v = 900 * 64 = 57600.
  memset(m->mmr_coef, 0, sizeof(m->mmr_coef));
  m->mmr_constant = 0;
  m->mmr_order = 1;
  m->mmr_coef[0][1] = ONE;
  p.map[1].pivot_value[0] = 100;
  p.map[1].pivot_value[1] = 900;
  assert(frc_map_mmr(&p, 1, 0, 0, 1000, 0) == 57600);
}

static void test_nlq(void) {
  struct frc_params p = identity_params();
  // Dead centre is exactly zero.
  assert(frc_nlq_linear_dz(&p, 0, 512) == 0);
  // e = 513: rr = 1 -> 2 - 1 = 1; dq = 2048; r = 2048 >> 8 = 8, i.e.
  // (1 - 1/2) * 2048 / 2^23 = 2^-13 of full scale = 8 / 65536.
  assert(frc_nlq_linear_dz(&p, 0, 513) == 8);
  assert(frc_nlq_linear_dz(&p, 0, 511) == -8);
  // e = 1023: rr = 511 -> 1021; dq = 1021 * 2048 = 2091008 < RR = 2^21;
  // r = 2091008 >> 8 = 8168.
  assert(frc_nlq_linear_dz(&p, 0, 1023) == 8168);
  // e = 0: rr = -512 -> -1023; dq = -2095104; r = floor(-8184) = -8184.
  assert(frc_nlq_linear_dz(&p, 0, 0) == -8184);
  // Threshold T = 4096 adds 2T * sign: e = 513 -> dq = 2048 + 8192 = 10240,
  // r = 40; e = 511 -> dq = -10240, r = -40.
  p.nlq[0].threshold = 4096;
  assert(frc_nlq_linear_dz(&p, 0, 513) == 40);
  assert(frc_nlq_linear_dz(&p, 0, 511) == -40);
  // The hdr_in_max (vdr_in_max) clamp binds: R = 16384 -> RR = 32768.
  // e = 1023: dq = 1021 * 2048 + 8192 > RR -> 32768 >> 8 = 128.
  // e = 0: clamps to -32768 -> -128. e = 520: rr = 15, dq = 15 * 2048 +
  // 8192 = 38912 > RR -> 128 too: the clamp binds well inside the code range.
  p.nlq[0].hdr_in_max = 16384;
  assert(frc_nlq_linear_dz(&p, 0, 1023) == 128);
  assert(frc_nlq_linear_dz(&p, 0, 0) == -128);
  assert(frc_nlq_linear_dz(&p, 0, 520) == 128);
  // e = 515: rr = 5, dq = 5 * 2048 + 8192 = 18432 < RR -> 72 (not clamped).
  assert(frc_nlq_linear_dz(&p, 0, 515) == 72);
  // Negative values floor: slope 3, T 0, e = 511 -> dq = -3 -> -1, not 0.
  p.nlq[0].threshold = 0;
  p.nlq[0].slope = 3;
  assert(frc_nlq_linear_dz(&p, 0, 511) == -1);
  assert(frc_nlq_linear_dz(&p, 0, 513) == 0);
  // 8-bit EL: rr <<= 2 and T, R << 3; shift is 23 - 5 - 8 = 10.
  // e = offset + 1 with offset 128, S = 2048: rr = 1 << 2 = 4; dq = 8192;
  // r = 8192 >> 10 = 8, the same residual as the 10-bit case.
  p = identity_params();
  p.el_bit_depth = 8;
  p.nlq[0].offset = 128;
  assert(frc_nlq_linear_dz(&p, 0, 129) == 8);
}

static void test_reconstruct(void) {
  struct frc_params p = identity_params();
  // h = (v + r + 8) >> 4 at 12 bits.
  assert(frc_reconstruct(&p, 32768, 8) == 2049);   // 32784 / 16 = 2049
  assert(frc_reconstruct(&p, 32775, 0) == 2048);   // 32783 >> 4 = 2048
  assert(frc_reconstruct(&p, 32776, 0) == 2049);   // half rounds up
  assert(frc_reconstruct(&p, 0, -128) == 0);       // floor(-7.5) -> clamp 0
  assert(frc_reconstruct(&p, 65535, 128) == 4095); // clamps to MAXOUT
  p.disable_residual = true;
  assert(frc_reconstruct(&p, 32768, 4000) == 2048);  // residual ignored
  // 10-bit output: h = (v + r + 32) >> 6.
  p = identity_params();
  p.hdr_bit_depth = 10;
  assert(frc_reconstruct(&p, 32768, 0) == 512);
  assert(frc_reconstruct(&p, 65535, 0) == 1023);
}

static void test_el_filters(void) {
  // Luma vertical, step 0 0 0 1023 1023 1023 (n = 6, out 12).
  const uint16_t step[6] = {0, 0, 0, 1023, 1023, 1023};
  uint16_t out[12];
  frc_el_up_vertical(step, 1, 6, out, 1, false);
  // k = 2: even = (-9 * 1023 + 64) >> 7 = floor(-71.4) -> clip 0;
  //        odd = (29 * 1023 - 3 * 1023 + 64) >> 7 = 26662 >> 7 = 208.
  // k = 3: even = (111 - 9) * 1023 + 64 = 104410 >> 7 = 815;
  //        odd = (111 + 29 - 3) * 1023 + 64 = 140215 >> 7 = 1095, an
  //        overshoot past 1023 that the 0..0xFFFF clip keeps (reading R4).
  assert(out[0] == 0 && out[4] == 0 && out[5] == 208);
  assert(out[6] == 815 && out[7] == 1095);
  assert(out[10] == 1023 && out[11] == 1023);
  // Chroma vertical, 100 200: (64*100 + 192*100 + 128) >> 8 = 100;
  // (192*100 + 64*200 + 128) >> 8 = 125; (64*100 + 192*200 + 128) >> 8 =
  // 175; (192*200 + 64*200 + 128) >> 8 = 200.
  const uint16_t two[2] = {100, 200};
  uint16_t cout[4];
  frc_el_up_vertical(two, 1, 2, cout, 1, true);
  assert(cout[0] == 100 && cout[1] == 125 && cout[2] == 175 && cout[3] == 200);
  // Horizontal, step 0 0 0 0 1000 1000 1000 1000.
  const uint16_t h[8] = {0, 0, 0, 0, 1000, 1000, 1000, 1000};
  uint16_t hout[16];
  frc_el_up_horizontal(h, 8, hout);
  for (int k = 0; k < 8; k++)
    assert(hout[2 * k] == h[k]);  // even outputs copy the input
  // k = 2: (-524 + 94 + 22) * 1000 + 2048 = -405952 >> 12 = -100 -> 0.
  // k = 3: 2048 * 1000 + 2048 = 2050048 >> 12 = 500.
  // k = 4: (2456 + 2456 - 524 + 94 + 22) * 1000 + 2048 >> 12 = 1100.
  assert(hout[5] == 0 && hout[7] == 500 && hout[9] == 1100);
  // A constant plane stays constant through every filter (taps sum to
  // 128, 256 and 4096), so a constant EL keeps its exact code.
  uint16_t flat[2 * 2] = {700, 700, 700, 700}, flat_up[4 * 4];
  assert(frc_el_upsample_plane(flat, 2, 2, 2, false, flat_up));
  for (int k = 0; k < 16; k++)
    assert(flat_up[k] == 700);
}

static void test_compose_picture(void) {
  // 4x4 BL luma ramp, 2x2 chroma; EL at half resolution.
  uint16_t by[16], bu[4], bv[4];
  for (int k = 0; k < 16; k++)
    by[k] = (uint16_t)(64 * k);
  for (int k = 0; k < 4; k++) {
    bu[k] = (uint16_t)(400 + 10 * k);
    bv[k] = (uint16_t)(600 - 10 * k);
  }
  uint16_t ey[4], eu[1], ev[1];
  struct frc_picture bl = {{{by, 4, 4, 4}, {bu, 2, 2, 2}, {bv, 2, 2, 2}}};
  struct frc_picture el = {{{ey, 2, 2, 2}, {eu, 1, 1, 1}, {ev, 1, 1, 1}}};
  uint16_t oy[16], ou[4], ov[4];
  int32_t sy[16], su[4], sv[4];
  struct frc_output out = {{oy, ou, ov}, {4, 2, 2}, {sy, su, sv}};
  struct frc_params p = identity_params();
  // EL at the NLQ offset: zero residual, so h = (64 s + 8) >> 4 = 4 s.
  for (int k = 0; k < 4; k++)
    ey[k] = 512;
  eu[0] = ev[0] = 512;
  assert(frc_compose(&p, &bl, &el, &out) == NULL);
  for (int k = 0; k < 16; k++)
    assert(oy[k] == 4 * by[k]);
  for (int k = 0; k < 4; k++)
    assert(ou[k] == 4 * bu[k] && ov[k] == 4 * bv[k]);
  // Constant EL 513: the filters keep 513, r = 8 everywhere, and
  // h = (64 s + 8 + 8) >> 4 = 4 s + 1.
  for (int k = 0; k < 4; k++)
    ey[k] = 513;
  eu[0] = ev[0] = 513;
  assert(frc_compose(&p, &bl, &el, &out) == NULL);
  for (int k = 0; k < 16; k++)
    assert(oy[k] == 4 * by[k] + 1);
  // The unrounded sum is v + r = 64 s + 8 before clause 5.4.3.3's rounding.
  for (int k = 0; k < 16; k++)
    assert(sy[k] == 64 * by[k] + 8);
  for (int k = 0; k < 4; k++)
    assert(ou[k] == 4 * bu[k] + 1 && ov[k] == 4 * bv[k] + 1);
  // MMR on Cb at chroma (0, 0) with coefficient 1.0 on s0 only: the
  // down-sampled luma there is top = (0 + 0 + 64 + 2) >> 2 = 16, bottom =
  // (256 + 512 + 320 + 2) >> 2 = 272, s0 = (16 + 272 + 1) >> 1 = 144;
  // v = 144 * 64 = 9216; h = (9216 + 8 + 8) >> 4 = 577.
  p.map[1].piece[0].mapping_idc = FRC_MAPPING_MMR;
  p.map[1].piece[0].mmr_order = 1;
  p.map[1].piece[0].mmr_coef[0][0] = ONE;
  assert(frc_compose(&p, &bl, &el, &out) == NULL);
  assert(ou[0] == 577);
  // Inputs outside the reference are refused, not guessed.
  p.map[0].piece[0].mapping_idc = FRC_MAPPING_MMR;
  assert(frc_compose(&p, &bl, &el, &out) != NULL);
  p = identity_params();
  struct frc_picture odd_el = el;
  odd_el.plane[0].width = 3;
  assert(frc_compose(&p, &bl, &odd_el, &out) != NULL);
  assert(frc_compose(&p, &bl, NULL, &out) != NULL);
  p.disable_residual = true;
  assert(frc_compose(&p, &bl, NULL, &out) == NULL);
  for (int k = 0; k < 16; k++)
    assert(oy[k] == 4 * by[k]);
}

static void test_params_check(void) {
  struct frc_params p = identity_params();
  assert(frc_params_check(&p) == NULL);
  p.map[1].pivot_value[1] = 1024;  // one past the 10-bit BL range
  assert(frc_params_check(&p) != NULL);
  p = identity_params();
  p.map[2].pivot_value[0] = -1;
  assert(frc_params_check(&p) != NULL);
  // Clause 5.3.3.2: poly_coef_int in -64..63, so fp in [-64, 64) << denom.
  p = identity_params();
  p.map[0].piece[0].poly_coef[0] = (INT64_C(64) << 23) - 1;
  assert(frc_params_check(&p) == NULL);
  p.map[0].piece[0].poly_coef[0] = INT64_C(64) << 23;
  assert(frc_params_check(&p) != NULL);
  p.map[0].piece[0].poly_coef[0] = -(INT64_C(64) << 23);
  assert(frc_params_check(&p) == NULL);
  // Clause 5.3.3.3: MMR integer parts in -65536..65535.
  p = identity_params();
  p.map[1].piece[0].mapping_idc = FRC_MAPPING_MMR;
  p.map[1].piece[0].mmr_order = 3;
  p.map[1].piece[0].mmr_coef[2][6] = (INT64_C(65536) << 23) - 1;
  assert(frc_params_check(&p) == NULL);
  p.map[1].piece[0].mmr_coef[2][6] = -(INT64_C(65536) << 23) - 1;
  assert(frc_params_check(&p) != NULL);
  p.map[1].piece[0].mmr_coef[2][6] = 0;
  p.map[1].piece[0].mmr_constant = INT64_C(65536) << 23;
  assert(frc_params_check(&p) != NULL);
}

static void test_compose_el_routing(void) {
  // 8x8 flat BL (v = 64 * 256 = 16384 everywhere) and a half-size EL whose
  // planes each have their own structure, so the right filter must reach
  // the right plane. Identity NLQ: r = 8 (2d - 1) for d = e - 512 > 0,
  // -8 (2|d| - 1) for d < 0 (dq = (2d - 1) 2048, shifted by 23 - 5 - 10).
  uint16_t by[64], bu[16], bv[16];
  for (int k = 0; k < 64; k++)
    by[k] = 256;
  for (int k = 0; k < 16; k++)
    bu[k] = bv[k] = 256;
  // EL luma 4x4, rows 512 512 576 576; Cb 2x2, rows 512 576; Cr 2x2,
  // columns 512 576.
  uint16_t ey[16], eu[4] = {512, 512, 576, 576}, ev[4] = {512, 576, 512, 576};
  for (int k = 0; k < 16; k++)
    ey[k] = k < 8 ? 512 : 576;
  struct frc_picture bl = {{{by, 8, 8, 8}, {bu, 4, 4, 4}, {bv, 4, 4, 4}}};
  struct frc_picture el = {{{ey, 4, 4, 4}, {eu, 2, 2, 2}, {ev, 2, 2, 2}}};
  uint16_t oy[64], ou[16], ov[16];
  int32_t sy[64], su[16], sv[16];
  struct frc_output out = {{oy, ou, ov}, {8, 4, 4}, {sy, su, sv}};
  struct frc_params p = identity_params();
  assert(frc_compose(&p, &bl, &el, &out) == NULL);
  for (int x = 0; x < 8; x++) {
    // Luma, 4-tap vertical (rows are constant, so horizontal keeps them):
    // row 2 = (-3*512 + 29*512 + 111*512 - 9*576 + 64) >> 7 = 508, d = -4,
    // r = -56; row 3 = (-9*512 + 111*512 + 29*576 - 3*576 + 64) >> 7 = 525,
    // d = 13, r = 200; row 4 = (-3*512 + 29*512 + 111*576 - 9*576 + 64) >> 7
    // = 563, d = 51, r = 808. (The chroma filter would give 512, 528, 560.)
    assert(sy[2 * 8 + x] == 16384 - 56 && oy[2 * 8 + x] == 1021);
    assert(sy[3 * 8 + x] == 16384 + 200 && oy[3 * 8 + x] == 1037);
    assert(sy[4 * 8 + x] == 16384 + 808 && oy[4 * 8 + x] == 1075);
  }
  for (int x = 0; x < 4; x++) {
    // Cb, (64, 192) vertical: row 1 = (192*512 + 64*576 + 128) >> 8 = 528,
    // d = 16, r = 248; row 2 = (64*512 + 192*576 + 128) >> 8 = 560, d = 48,
    // r = 760. (The luma filter would give 525 at row 1.)
    assert(ou[x] == 1024);
    assert(su[4 + x] == 16384 + 248 && ou[4 + x] == 1040);
    assert(su[8 + x] == 16384 + 760 && ou[8 + x] == 1072);
  }
  for (int y = 0; y < 4; y++) {
    // Cr, 8-tap horizontal on columns 512 576 (vertical keeps them):
    // column 1 = (2048*512 + 2048*576 + 2048) >> 12 = 544, d = 32, r = 504;
    // column 3 = (-408*512 + 4504*576 + 2048) >> 12 = 582, d = 70, r = 1112.
    assert(ov[y * 4] == 1024);
    assert(sv[y * 4 + 1] == 16384 + 504 && ov[y * 4 + 1] == 1056);
    assert(sv[y * 4 + 2] == 16384 + 8 * 127 && ov[y * 4 + 2] == 1088);
    assert(sv[y * 4 + 3] == 16384 + 1112 && ov[y * 4 + 3] == 1094);
  }
}

int main(void) {
  test_shift_and_pivots();
  test_polynomial();
  test_luma_downsample();
  test_mmr();
  test_nlq();
  test_reconstruct();
  test_el_filters();
  test_compose_picture();
  test_compose_el_routing();
  test_params_check();
  puts("FEL reference composer: hand-computed vectors pass");
  return 0;
}
