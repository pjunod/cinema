#include <assert.h>
#include <stdio.h>
#include "title_constants.h"

static void l6_gap(bool missing_first, bool conflict) {
  struct dv_title_constants s = {0};
  if (missing_first) dv_title_observe_l6(&s, false, 0, 0);
  dv_title_observe_l6(&s, true, 50, 4000);
  if (!missing_first) dv_title_observe_l6(&s, false, 0, 0);
  dv_title_observe_l6(&s, true, 50, conflict ? 1000 : 4000);
  assert(s.l6_state == (conflict ? 3 : 2));
  dv_title_constants_unknown(&s);
  assert(s.l6_state == (conflict ? 3 : 2));
}

static void l9_gap(bool missing_first, bool conflict, int coordinate) {
  struct dv_title_constants s = {0};
  uint16_t xy[8] = {100, 200, 300, 400, 500, 600, 700, 800};
  unsigned index = coordinate < 0 ? 0 : 255;
  if (missing_first) dv_title_observe_l9(&s, false, 0, NULL);
  dv_title_observe_l9(&s, true, index, xy);
  if (!missing_first) dv_title_observe_l9(&s, false, 0, NULL);
  if (conflict) {
    if (coordinate < 0) index = 2;
    else xy[coordinate]++;
  }
  dv_title_observe_l9(&s, true, index, xy);
  assert(s.l9_state == (conflict ? 3 : 2));
  dv_title_constants_unknown(&s);
  assert(s.l9_state == (conflict ? 3 : 2));
}

int main(void) {
  unsigned cases = 0;
  for (int missing_first = 0; missing_first <= 1; missing_first++) {
    for (int conflict = 0; conflict <= 1; conflict++) {
      l6_gap(missing_first, conflict);
      cases++;
      for (int coordinate = -1; coordinate < 8; coordinate++) {
        l9_gap(missing_first, conflict, coordinate);
        cases++;
      }
    }
  }
  struct dv_title_constants s = {0};
  dv_title_observe_l6(&s, true, 50, 4000);
  dv_title_observe_l6(&s, true, 50, 4000);
  dv_title_observe_l9(&s, true, 0, NULL);
  dv_title_observe_l9(&s, true, 0, NULL);
  assert(s.l6_state == 1 && s.l9_state == 1);
  // A malformed intervening L6 is unknown coverage, never a reset of A.
  dv_title_observe_l6(&s, true, 0, 0);
  dv_title_observe_l6(&s, true, 20, 4000);
  assert(s.l6_state == 3 && s.l9_state == 1);
  printf("title mastering: %u gap cases plus consistent/invalid controls passed\n", cases);
  return 0;
}
