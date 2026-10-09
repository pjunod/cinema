#ifndef PLURX_DV_NUT_TIMING_H
#define PLURX_DV_NUT_TIMING_H
#include <stdbool.h>
#include <stdint.h>

/* NUT may refine the source clock. Preserve integer ticks and the complete
 * interval without allowing FFmpeg's rescaler to saturate to AV_NOPTS_VALUE. */
static bool nut_scale_interval(int64_t pts, int64_t duration, int64_t scale,
                               int64_t *scaled_pts, int64_t *scaled_duration) {
  if (pts < 0 || duration <= 0 || scale <= 0)
    return false;
  int64_t limit = INT64_MAX / scale;
  if (duration > limit || pts > limit - duration)
    return false;
  *scaled_pts = pts * scale;
  *scaled_duration = duration * scale;
  return true;
}
#endif
