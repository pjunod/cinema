#include "nut_timing.h"
#include <assert.h>
int main(void) {
  int64_t pts = -1, duration = -1;
  assert(nut_scale_interval(42, 41, 64, &pts, &duration));
  assert(pts == 2688 && duration == 2624);
  assert(nut_scale_interval(INT64_MAX / 64 - 1, 1, 64, &pts, &duration));
  assert(!nut_scale_interval(INT64_MAX / 64, 1, 64, &pts, &duration));
  assert(!nut_scale_interval(INT64_MAX / 64 + 1, 1, 64, &pts, &duration));
  assert(!nut_scale_interval(0, INT64_MAX / 64 + 1, 64, &pts, &duration));
  assert(!nut_scale_interval(-1, 1, 64, &pts, &duration));
  assert(!nut_scale_interval(0, 0, 64, &pts, &duration));
  assert(!nut_scale_interval(0, 1, 0, &pts, &duration));
  return 0;
}
