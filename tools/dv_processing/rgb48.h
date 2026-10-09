#ifndef PLURX_DV_RGB48_H
#define PLURX_DV_RGB48_H

#include <math.h>
#include <stdint.h>

// The caller validates finite input before conversion. Preserve lrintf's
// rounding mode without paying for general NaN-aware fminf/fmaxf calls.
static inline uint16_t dv_pack_finite_rgb48(float value) {
  if (value < 0)
    value = 0;
  if (value > 1)
    value = 1;
  return (uint16_t)lrintf(value * 65535);
}
#endif
