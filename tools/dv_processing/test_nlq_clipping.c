// SPDX-License-Identifier: Apache-2.0
#include "nlq_clipping.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>

// CPU evaluation of API387 units, conditional on an already sampled EL code.
// This does not execute a shader or establish texture-filter precision.
static float residual_from_rounded(float rr, float slope, float threshold) {
  return rr == 0 ? 0 : copysignf((fabsf(rr) - .5f) * slope + threshold, rr);
}

static void check_rounded(int rr, unsigned slope, unsigned threshold,
                          float hdr_in_max) {
  // Independent twice-fixed-point numerator; no folded normalized intercept.
  int64_t magnitude = rr < 0 ? -(int64_t)rr : rr;
  int64_t numerator = rr == 0 ? 0 :
      (2 * magnitude - 1) * slope + 2 * (int64_t)threshold;
  double expected = (rr < 0 ? -numerator : numerator) / 16777216.0;
  float actual = residual_from_rounded(rr, slope / 8388608.0f,
                                      threshold / 8388608.0f);
  // This grid's fixed-point numerators are exactly representable in binary32.
  assert(actual == (float)expected);
  // Check BEFORE applying the API387 residual clip: a passing result must not
  // be manufactured by clamping a residual that exceeds the admitted maximum.
  assert(fabsf(actual) <= hdr_in_max);
  assert(fminf(hdr_in_max, fmaxf(-hdr_in_max, actual)) == actual);
}

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
  // Exhaust integer and non-half interpolated codes for the same admitted
  // parameter grid. API387 rounds in integer EL-code space after sampling.
  const unsigned offsets[] = {0, 1, 511, 512, 1022, 1023};
  const unsigned slopes[] = {0, 1, 512, 2048, 8192};
  const unsigned thresholds[] = {0, 1, 2048};
  const float fractions[] = {0, .25f, .75f};
  unsigned integers = 0, interpolated = 0, ties = 0, host_lower = 0, host_upper = 0;
  for (unsigned o = 0; o < sizeof(offsets) / sizeof(*offsets); o++)
    for (unsigned s = 0; s < sizeof(slopes) / sizeof(*slopes); s++)
      for (unsigned t = 0; t < sizeof(thresholds) / sizeof(*thresholds); t++) {
        if (!nlq_clip_is_nonbinding(offsets[o], slopes[s], thresholds[t], 0,
                                    1048576))
          continue;
        float offset = offsets[o];
        float hdr_in_max = 1048576.0f / 8388608.0f;
        for (unsigned code = 0; code <= 1023; code++) {
          for (unsigned f = 0; f < sizeof(fractions) / sizeof(*fractions); f++) {
            float sample = code + fractions[f];
            if (sample > 1023)
              continue;
            float rr = roundf(sample - offset);
            // Independent nearest-code result; these samples are not ties.
            int expected_rr = (int)code + (fractions[f] > .5f) - (int)offsets[o];
            assert(rr == expected_rr);
            check_rounded(expected_rr, slopes[s], thresholds[t], hdr_in_max);
            if (f == 0) integers++; else interpolated++;
          }
          if (code == 1023)
            continue;
          float half = code + .5f - offset;
          int lower = (int)floorf(half), upper = (int)ceilf(half);
          // GLSL round permits either nearest integer at an exact half tie.
          // Check the admission bound for BOTH choices. Host C roundf is
          // classified only; it is not proof of any GPU's tie convention.
          check_rounded(lower, slopes[s], thresholds[t], hdr_in_max);
          check_rounded(upper, slopes[s], thresholds[t], hdr_in_max);
          float host = roundf(half);
          assert(host == lower || host == upper);
          if (host == lower) host_lower++; else host_upper++;
          ties++;
        }
      }
  printf("nonbinding NLQ CPU API387 arithmetic: %u integer, %u non-half "
         "interpolated codes; %u exact ties checked for both nearest choices "
         "(host C roundf lower=%u upper=%u); no GPU tie/filter claim\n",
         integers, interpolated, ties, host_lower, host_upper);
  return 0;
}
