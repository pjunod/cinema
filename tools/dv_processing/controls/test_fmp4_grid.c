// Focused tests exercise the exact guard compiled into the authoring helper.
#define main author_main
#include "tools/author_p81.c"
#undef main
#include <assert.h>
int main(void) {
  assert(fmp4_grid_bounds(48048, 1001, 3));
  assert(fmp4_grid_bounds(INT64_MAX - 3003, 1001, 3));
  assert(!fmp4_grid_bounds(INT64_MAX - 3002, 1001, 3));
  assert(fmp4_grid_bounds(0, INT_MAX - 1, 3));
  assert(!fmp4_grid_bounds(0, INT_MAX, 3));
  assert(!fmp4_grid_bounds(0, (int64_t)INT_MAX + 1, 3));
  assert(!fmp4_grid_bounds(-1, 1001, 3));
  assert(!fmp4_grid_bounds(0, 0, 3));
  assert(!fmp4_grid_bounds(0, 1001, 0));
  assert(!fmp4_grid_bounds(0, 1001, 65));
  // Source 3/1000 becomes unit 1/1000 after the movenc header.
  AVRational source = {3, 1000}, output = {1, 1000};
  int64_t first = av_rescale_q(INT64_MAX / 3 - 2, source, output);
  int64_t step = av_rescale_q(1, source, output);
  assert(first == INT64_MAX - 7 && step == 3);
  assert(!fmp4_grid_bounds(first, step, 3));
  step = av_rescale_q(715827883, source, output);
  assert(step == (int64_t)INT_MAX + 2);
  assert(!fmp4_grid_bounds(0, step, 3));
  step = av_rescale_q(715827882, source, output);
  assert(step == INT_MAX - 1);
  assert(fmp4_grid_bounds(0, step, 3));
  puts("focused-output-clock-guards-pass");
  return 0;
}
