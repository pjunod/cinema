#ifndef PLURX_DV_TITLE_CONSTANTS_H
#define PLURX_DV_TITLE_CONSTANTS_H

#include <stdbool.h>
#include <stdint.h>
#include <string.h>

// Missing coverage and the first observed identity are independent facts.
// A later contradiction outranks unknown coverage, including after a gap.
struct dv_title_constants {
  // 0: unseen, 1: consistent, 2: unknown, 3: inconsistent.
  int l6_state, l9_state;
  bool l6_seen, l9_seen;
  unsigned min_luma, max_luma, primary_index;
  uint16_t xy[8];
};

static inline void dv_title_observe_l6(struct dv_title_constants *s, bool present,
                                     unsigned min_luma, unsigned max_luma) {
  if (!present || !max_luma || max_luma > 10000 || min_luma > 10000) {
    if (s->l6_state != 3) s->l6_state = 2;
  } else if (!s->l6_seen) {
    s->l6_seen = true;
    s->min_luma = min_luma;
    s->max_luma = max_luma;
    if (!s->l6_state) s->l6_state = 1;
  } else if (s->min_luma != min_luma || s->max_luma != max_luma) {
    s->l6_state = 3;
  }
}

static inline void dv_title_observe_l9(struct dv_title_constants *s, bool present,
                                     unsigned index, const uint16_t xy[8]) {
  if (!present) {
    if (s->l9_state != 3) s->l9_state = 2;
    return;
  }
  const uint16_t zero[8] = {0};
  const uint16_t *coordinates = index == 255 ? xy : zero;
  if (!s->l9_seen) {
    s->l9_seen = true;
    s->primary_index = index;
    memcpy(s->xy, coordinates, sizeof(s->xy));
    if (!s->l9_state) s->l9_state = 1;
  } else if (s->primary_index != index ||
             memcmp(s->xy, coordinates, sizeof(s->xy))) {
    s->l9_state = 3;
  }
}

static inline void dv_title_constants_unknown(struct dv_title_constants *s) {
  dv_title_observe_l6(s, false, 0, 0);
  dv_title_observe_l9(s, false, 0, NULL);
}

#endif
