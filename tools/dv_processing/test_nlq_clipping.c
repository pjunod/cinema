// SPDX-License-Identifier: Apache-2.0
#include "nlq_clipping.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>

int main(void) {
  // Real-film parameter family: residual max 0.1248779296875 < 0.125.
  assert(nlq_clip_is_nonbinding(512, 2048, 0, 0, 1048576));
  // A lower limit actually binds and must still refuse.
  assert(!nlq_clip_is_nonbinding(512, 2048, 0, 0, 16384));
  // Even mathematically exact equality is refused without rounding headroom.
  assert(!nlq_clip_is_nonbinding(512, 2048, 0, 0, 1047552));
  assert(nlq_clip_is_nonbinding(512, 0, 0, 0, 0));
  assert(!nlq_clip_is_nonbinding(1024, 0, 0, 1, 0));
  assert(!nlq_clip_is_nonbinding(512, UINT64_MAX, 0, 1, 0));
  assert(!nlq_clip_is_nonbinding(512, 0, UINT64_MAX, 1, 0));
  assert(!nlq_clip_is_nonbinding(512, 0, 0, UINT64_MAX, 0));
  assert(!nlq_clip_is_nonbinding(512, 0, 0, 0, UINT64_MAX));
  // Exhaust every 10-bit code for accepted endpoint/midpoint offsets and
  // several slopes/thresholds; compare the actual binary32 shader expression.
  const unsigned offsets[] = {0, 1, 511, 512, 1022, 1023};
  const unsigned slopes[] = {0, 1, 512, 2048, 8192};
  const unsigned thresholds[] = {0, 1, 2048};
  unsigned checked = 0;
  for (unsigned o = 0; o < sizeof(offsets) / sizeof(*offsets); o++)
    for (unsigned s = 0; s < sizeof(slopes) / sizeof(*slopes); s++)
      for (unsigned t = 0; t < sizeof(thresholds) / sizeof(*thresholds); t++) {
        if (!nlq_clip_is_nonbinding(offsets[o], slopes[s], thresholds[t], 0,
                                    1048576))
          continue;
        float off = offsets[o] / 1023.0f;
        float scale = (float)(1023.0 * slopes[s] / 8388608.0);
        float bias = (float)((thresholds[t] - slopes[s] / 2.0) / 8388608.0);
        for (unsigned code = 0; code <= 1023; code++) {
          float center = code / 1023.0f - off;
          float residual = center == 0 ? 0 : fabsf(center) * scale + bias;
          assert(fabsf(residual) <= 0.125f);
          checked++;
        }
      }
  printf("nonbinding NLQ: %u shader-domain values checked\n", checked);
  return 0;
}
