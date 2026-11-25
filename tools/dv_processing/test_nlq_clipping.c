// SPDX-License-Identifier: Apache-2.0
#include "nlq_clipping.h"
#include "fel_reference_composer.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>

// Clause 5.4.3.2 through the CCM 001 reference: every 10-bit EL code's
// residual for one LINEAR_DZ parameter set, with its vdr_in_max and with the
// limit at its ceiling. Returns how many codes the clamp changes; asserts the
// clamped residual never exceeds the limit and equals the unclamped one
// wherever the clamp does not bind.
static unsigned clamped_codes(unsigned offset, int64_t slope, int64_t threshold,
                              int64_t maximum) {
  struct frc_params p = {.coefficient_log2_denom = 23, .el_bit_depth = 10};
  p.nlq[0] = (struct frc_nlq){(int32_t)offset, slope, threshold, maximum};
  struct frc_params open = p;
  open.nlq[0].hdr_in_max = (INT64_C(2) << 23) - 1;
  // The limit in the residual's 16-bit domain: clause 5.4.3.2's final shift.
  const int64_t high = frc_asr(maximum * 2, 23 - 5 - 10);
  const int64_t low = frc_asr(-maximum * 2, 23 - 5 - 10);
  unsigned changed = 0;
  for (int32_t code = 0; code <= 1023; code++) {
    int32_t r = frc_nlq_linear_dz(&p, 0, code);
    int32_t u = frc_nlq_linear_dz(&open, 0, code);
    assert(r <= high && r >= low);
    if (r != u) {
      assert(u > high || u < low);
      assert(r == high || r == low);
      changed++;
    }
  }
  return changed;
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

  // Clamp-binding limits: pq-bilinear-* refuses them, pq-ccm001-* admits
  // them, and the reference's clamp changes the residual of some codes.
  const struct {
    unsigned offset;
    uint64_t slope, threshold, maximum;
  } binding[] = {
      {512, 2048, 0, 16384},      // the refusal case above
      {512, 2048, 0, 1047552},    // exact equality, no rounding headroom
      {512, 8192, 2048, 4096},    // binding-tight's +-2-code limit
      {0, 8192, 0, 1048576},      // offset at an end doubles the reach
      {1023, 8388607, 8388607, 8388607},  // largest fractions
  };
  for (unsigned i = 0; i < sizeof(binding) / sizeof(*binding); i++) {
    uint64_t integer = binding[i].maximum >> 23;
    uint64_t fraction = binding[i].maximum & ((UINT64_C(1) << 23) - 1);
    assert(!nlq_clip_is_nonbinding(binding[i].offset, binding[i].slope,
                                   binding[i].threshold, integer, fraction));
    assert(nlq_clip_is_bounded(binding[i].offset, binding[i].slope,
                               binding[i].threshold, integer, fraction));
    unsigned changed = clamped_codes(binding[i].offset, (int64_t)binding[i].slope,
                                     (int64_t)binding[i].threshold,
                                     (int64_t)binding[i].maximum);
    // The exact-equality case is refused for headroom, not because the clamp
    // binds: the reference changes nothing there.
    assert(i == 1 ? changed == 0 : changed > 0);
  }
  // An admitted nonbinding limit is one the reference's clamp never changes.
  assert(clamped_codes(512, 2048, 0, 1048576) == 0);
  // The clamped composition keeps the field bounds.
  assert(nlq_clip_is_bounded(512, 0, 0, 1, 8388607));
  assert(!nlq_clip_is_bounded(1024, 0, 0, 0, 0));
  assert(!nlq_clip_is_bounded(512, 8388608, 0, 0, 0));
  assert(!nlq_clip_is_bounded(512, 0, 8388608, 0, 0));
  assert(!nlq_clip_is_bounded(512, 0, 0, 2, 0));
  assert(!nlq_clip_is_bounded(512, 0, 0, 0, 8388608));
  printf("binding NLQ: %u limits admitted for the clamped composition\n",
         (unsigned)(sizeof(binding) / sizeof(*binding)));
  return 0;
}
