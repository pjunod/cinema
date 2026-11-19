// Focused equivalence over binary32 finite inputs and all conversion rounding modes.
#include <assert.h>
#include <fenv.h>
#include <math.h>
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
static uint16_t exact_pack_mode(float value, int mode) {
  uint32_t bits;
  memcpy(&bits, &value, sizeof(bits));
  if (bits == 0 || (bits >> 31))
    return 0;
  if (bits >= UINT32_C(0x3f800000))
    return 65535;
  unsigned exponent = bits >> 23;
  if (exponent < 110)
    return mode == FE_UPWARD ? 1 : 0;
  uint64_t numerator = (UINT64_C(0x800000) + (bits & 0x7fffffu)) * 65535;
  uint64_t denominator = UINT64_C(1) << (150 - exponent);
  unsigned quotient = (unsigned)(numerator / denominator);
  uint64_t remainder = numerator % denominator;
  bool increment = mode == FE_UPWARD ? remainder != 0 :
                   mode == FE_TONEAREST && (remainder * 2 > denominator ||
                     (remainder * 2 == denominator && (quotient & 1u)));
  return (uint16_t)(quotient + increment);
}
static uint16_t exact_pack(float value) { return exact_pack_mode(value, fegetround()); }
#include "rgb48.h"
static void check(float v) { assert(isfinite(v)); assert(exact_pack(v)==dv_pack_finite_rgb48(v)); }
static void check_bulk(float value) {
  float input[13] = {0, value, .25f, .75f, 1, -1, value, 2, -0.0f,
                    .125f, .875f, value, .5f};
  uint8_t output[20];
  memset(output, 0xa5, sizeof(output));
  assert(dv_pack_rgba_rgb48(input + 1, 3, output + 1, 18));
  assert(output[0] == 0xa5 && output[19] == 0xa5);
  for (size_t pixel = 0; pixel < 3; pixel++)
    for (size_t component = 0; component < 3; component++) {
      uint16_t code = exact_pack(input[1 + 4 * pixel + component]);
      assert(output[1 + 6 * pixel + 2 * component] == (uint8_t)code);
      assert(output[2 + 6 * pixel + 2 * component] == (uint8_t)(code >> 8));
    }
  memset(output, 0xa5, sizeof(output));
  assert(!dv_pack_rgba_rgb48(input + 1, 3, output + 1, 17));
  for (size_t i = 0; i < sizeof(output); i++) assert(output[i] == 0xa5);
  assert(dv_pack_rgba_rgb48(input + 1, 0, output + 1, 0));
  assert(dv_pack_rgba_rgb48(input + 1, 1, output + 1, 6));
}
static void check_nonfinite(void) {
  const float invalid[] = {NAN, INFINITY, -INFINITY};
  for (size_t lane = 0; lane < 4; lane++)
    for (size_t kind = 0; kind < 3; kind++) {
      float input[] = {.1f, .2f, .3f, 1};
      uint8_t output[6];
      input[lane] = invalid[kind];
      assert(!dv_pack_rgba_rgb48(input, 1, output, sizeof(output)));
    }
}
// The shared software plane contract: code << 6 in UNORM16 (bits 16/10/6).
// Every bilinear quarter tap between two stored codes, scaled by the
// renderer's 65535/64 code scale, is the exact quarter code, so the rounded
// enhancement-layer code never depends on storage normalization.
static void check_shifted_plane(void) {
  uint16_t codes[1032], actual[1032];
  for (size_t i = 0; i < 1032; i++) codes[i] = i % 1024;
  for (size_t count = 0; count < 1031; count++) {
    memset(actual, 0xee, sizeof(actual));
    assert(dv_shift_10bit_plane(codes + 1, count, actual + 1));
    for (size_t i = 0; i < count; i++) assert(actual[i + 1] == codes[i + 1] * 64);
    assert(actual[0] == 0xeeee && actual[count + 1] == 0xeeee);
  }
  for (size_t lane = 0; lane < 17; lane++) {
    uint16_t old = codes[lane];
    codes[lane] = 1024;
    assert(!dv_shift_10bit_plane(codes, 17, actual));
    codes[lane] = 65535;
    assert(!dv_shift_10bit_plane(codes, 17, actual));
    codes[lane] = old;
  }
  assert(dv_shift_10bit_plane(codes, 1024, actual));
  for (unsigned a = 0; a < 1024; a++)
    for (unsigned step = 1; step <= 3 && a + step < 1024; step++)
      for (unsigned n = 1; n < 4; n++) {
        // Bilinear weights at 2x are quarters; the tap is an integer over 4.
        unsigned tap4 = (4 - n) * actual[a] + n * actual[a + step];
        assert(tap4 % 64 == 0);
        float sampled = (float)((double)tap4 / (4 * 65535.0));
        float code = sampled * (65535.0f / 64.0f);
        assert(fabsf(code - (a + n * step / 4.0f)) < 1e-3f);
      }
  // The old storage (code/65535, bits 16/10/0) cannot hold such a tap: the
  // quarter tap between adjacent codes is not a multiple of the texel step.
  assert(((4 - 1) * 512u + 1 * 513u) % 4 != 0);
}
int main(void) {
  const int modes[]={FE_TONEAREST,FE_DOWNWARD,FE_UPWARD,FE_TOWARDZERO};
  unsigned long checked=0;
  check_shifted_plane();
  for (unsigned mode=0;mode<4;mode++) {
    assert(fesetround(modes[mode])==0);
    check(-0.0f);check(0.0f);check(-1.0f);check(1.0f);check(2.0f);checked+=5;
    check_bulk(-0.0f);check_bulk(0);check_bulk(-1);check_bulk(1);check_bulk(2);
    check_nonfinite();
    for (unsigned i=0;i<65536;i++) {
      float v=((float)i+.5f)/65535;
      check(v);check(nextafterf(v,-INFINITY));check(nextafterf(v,INFINITY));checked+=3;
      check_bulk(v);check_bulk(nextafterf(v,-INFINITY));check_bulk(nextafterf(v,INFINITY));
    }
    uint32_t state=1;
    for (unsigned i=0;i<262144;i++) {
      state=state*1664525U+1013904223U;
      float v;memcpy(&v,&state,sizeof(v));
      if(isfinite(v)){check(v);check_bulk(v);checked++;}
    }
  }
  assert(fesetround(FE_TONEAREST)==0);
  printf("finite-pack-equivalence-pass %lu inputs across4roundingmodes\n",checked);
  return 0;
}
