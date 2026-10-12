// Focused equivalence over binary32 finite inputs and all conversion rounding modes.
#include <assert.h>
#include <fenv.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
static uint16_t old_pack(float value) {
  return (uint16_t)lrintf(fminf(1, fmaxf(0, value)) * 65535);
}
#include "rgb48.h"
static void check(float v) { assert(isfinite(v)); assert(old_pack(v)==dv_pack_finite_rgb48(v)); }
static void check_bulk(float value) {
  float input[13] = {0, value, .25f, .75f, 1, -1, value, 2, -0.0f,
                    .125f, .875f, value, .5f};
  uint8_t output[20];
  memset(output, 0xa5, sizeof(output));
  assert(dv_pack_rgba_rgb48(input + 1, 3, output + 1, 18));
  assert(output[0] == 0xa5 && output[19] == 0xa5);
  for (size_t pixel = 0; pixel < 3; pixel++)
    for (size_t component = 0; component < 3; component++) {
      uint16_t code = old_pack(input[1 + 4 * pixel + component]);
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
int main(void) {
  const int modes[]={FE_TONEAREST,FE_DOWNWARD,FE_UPWARD,FE_TOWARDZERO};
  unsigned long checked=0;
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
}
