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
int main(void) {
  const int modes[]={FE_TONEAREST,FE_DOWNWARD,FE_UPWARD,FE_TOWARDZERO};
  unsigned long checked=0;
  for (unsigned mode=0;mode<4;mode++) {
    assert(fesetround(modes[mode])==0);
    check(-0.0f);check(0.0f);check(-1.0f);check(1.0f);check(2.0f);checked+=5;
    for (unsigned i=0;i<65536;i++) {
      float v=((float)i+.5f)/65535;
      check(v);check(nextafterf(v,-INFINITY));check(nextafterf(v,INFINITY));checked+=3;
    }
    uint32_t state=1;
    for (unsigned i=0;i<262144;i++) {
      state=state*1664525U+1013904223U;
      float v;memcpy(&v,&state,sizeof(v));
      if(isfinite(v)){check(v);checked++;}
    }
  }
  assert(fesetround(FE_TONEAREST)==0);
  printf("finite-pack-equivalence-pass %lu inputs across4roundingmodes\n",checked);
}
