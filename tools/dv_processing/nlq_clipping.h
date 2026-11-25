// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_NLQ_CLIPPING_H
#define PLURX_NLQ_CLIPPING_H

#include <stdbool.h>
#include <stdint.h>

// The supported 10-bit LINEAR_DZ subset: offset an EL code, slope and
// threshold fractions of 2^23, vdr_in_max at most 2 - 2^-23. The pq-ccm001-*
// composition (fel_compose.h) applies clause 5.4.3.2's vdr_in_max clamp, so
// this is all it needs.
static inline bool nlq_clip_is_bounded(uint16_t offset, uint64_t slope,
                                       uint64_t threshold,
                                       uint64_t maximum_integer,
                                       uint64_t maximum_fraction) {
  const uint64_t denominator = UINT64_C(1) << 23;
  return offset <= 1023 && slope < denominator && threshold < denominator &&
         maximum_integer <= 1 && maximum_fraction < denominator;
}

// Admission for the pq-bilinear-* composition, which does not implement
// vdr_in_max clipping: admit only a limit which cannot bind for ANY decoded
// EL code, not just the pixels observed in a sample frame.
static inline bool nlq_clip_is_nonbinding(uint16_t offset, uint64_t slope,
                                         uint64_t threshold,
                                         uint64_t maximum_integer,
                                         uint64_t maximum_fraction) {
  const uint64_t denominator = UINT64_C(1) << 23;
  if (!nlq_clip_is_bounded(offset, slope, threshold, maximum_integer,
                           maximum_fraction))
    return false;
  uint64_t distance = offset > 1023 - offset ? offset : 1023 - offset;
  // For a nonzero integer distance d, |residual| = ((d - 1/2)*S + T)/2^23.
  // S and T are nonnegative, so the farthest endpoint is the exact maximum.
  // Work in twice the fixed-point numerator to avoid floating-point decisions.
  uint64_t worst = (2 * distance - 1) * slope + 2 * threshold;
  uint64_t limit = 2 * (maximum_integer * denominator + maximum_fraction);
  if (worst == 0)
    return true;
  // Restrict reconstructed residual magnitude to <= 1 and reserve 2^-16. The
  // reserve was sized for the earlier renderer, which dequantized an unrounded
  // interpolated sample in binary32. The pinned libplacebo release rounds the
  // sample to its integer code first, so the reserve is now only conservative;
  // it is kept so admission does not widen with the renderer.
  const uint64_t rounding_margin = 2 * denominator / 65536;
  return worst <= 2 * denominator && worst + rounding_margin <= limit;
}

#endif
