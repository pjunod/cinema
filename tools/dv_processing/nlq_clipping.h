// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_NLQ_CLIPPING_H
#define PLURX_NLQ_CLIPPING_H

#include <stdbool.h>
#include <stdint.h>

// Admission for the supported 10-bit LINEAR_DZ subset. The renderer does not
// implement vdr_in_max clipping, so admit only a limit which cannot bind for
// ANY decoded EL code, not just the pixels observed in a sample frame.
static inline bool nlq_clip_is_nonbinding(uint16_t offset, uint64_t slope,
                                         uint64_t threshold,
                                         uint64_t maximum_integer,
                                         uint64_t maximum_fraction) {
  const uint64_t denominator = UINT64_C(1) << 23;
  if (offset > 1023 || slope >= denominator || threshold >= denominator ||
      maximum_integer > 1 || maximum_fraction >= denominator)
    return false;
  uint64_t distance = offset > 1023 - offset ? offset : 1023 - offset;
  // For a nonzero integer distance d, |residual| = ((d - 1/2)*S + T)/2^23.
  // S and T are nonnegative, so the farthest endpoint is the exact maximum.
  // Work in twice the fixed-point numerator to avoid floating-point decisions.
  uint64_t worst = (2 * distance - 1) * slope + 2 * threshold;
  uint64_t limit = 2 * (maximum_integer * denominator + maximum_fraction);
  if (worst == 0)
    return true;
  // Restrict reconstructed residual magnitude to <= 1 and reserve 2^-16 for
  // binary32 shader rounding. With this bound, the folded slope is below 2.01;
  // normalization/subtraction/multiply/add error is well below this margin.
  const uint64_t rounding_margin = 2 * denominator / 65536;
  return worst <= 2 * denominator && worst + rounding_margin <= limit;
}

#endif
