// Control-only, unaccepted display-trim infrastructure. Never included by the
// production helper. Public source boundaries are recorded in the E contract.
#ifndef PLURX_TRIM_SELECTION_H
#define PLURX_TRIM_SELECTION_H
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <math.h>
#include <libdovi/rpu_parser.h>

#define DV_TRIM_METHOD "candidate-b3-linear-rgb-sop-l2-chroma-v4"
#define DV_TRIM_BRIDGE_METHOD "public-rescaling-l8-raw4096-assumed-v2"
#define DV_TRIM_L2_RAW_METHOD "etsi-ts103572-1.3.1-l2-sop-raw-v1"
#define DV_TRIM_L8_RAW_METHOD "l8-centered-raw4096-engineering-assumption-v1"
#define DV_TRIM_DIRECT_METHOD "direct-b3-parameters-v1"
#define DV_TRIM_B3_METHOD "st2094-10-2021-b3-eq8-linear-rgb-v1"
#define DV_TRIM_BOUNDARY_METHOD "b3-unit-clamp-zero-power-one-v1"
#define DV_TRIM_ID_WORDS 36
struct dv_trim_selection {
  // Fixed scalar layout: no pointers, padding, parser ownership or hidden state
  // is hashed. Even currently unapplied fields have an identity word.
  uint32_t word[DV_TRIM_ID_WORDS];
};
enum {
  DT_VERSION, DT_LEVEL, DT_TARGET_PQ, DT_INDEX, DT_LENGTH,
  DT_SLOPE, DT_OFFSET, DT_POWER, DT_CHROMA, DT_SATURATION, DT_MS,
  DT_MID, DT_CLIP, DT_SAT0, DT_HUE0 = DT_SAT0 + 6,
  DT_TARGET_MIN = DT_HUE0 + 6, DT_PRIMARY, DT_PRIMARY0,
  DT_TARGET_LENGTH = DT_PRIMARY0 + 8,
};
static inline void dv_trim_neutral(struct dv_trim_selection *out) {
  memset(out, 0, sizeof(*out));
  out->word[DT_VERSION] = 2;
  for (int i = DT_SLOPE; i <= DT_CLIP; i++) out->word[i] = 2048;
  for (int i = DT_SAT0; i < DT_HUE0 + 6; i++) out->word[i] = 128;
}
static inline unsigned dv_trim_bucket_pq(unsigned nits) {
  double p = pow(nits / 10000.0, 2610.0 / 16384.0);
  double pq = pow((3424.0/4096.0 + 2413.0/128.0*p) /
                  (1.0 + 2392.0/128.0*p), 2523.0/32.0);
  return (unsigned)floor(pq * 4095.0 + 0.5);
}
// The public libdovi parser's length-10 L8 contains the common SOP/chroma fields;
// longer forms carry additional controls. Missing fields use public defaults.
static inline bool dv_trim_l8(const DoviExtMetadataBlockLevel8 *x,
                               const DoviExtMetadataBlockLevel10 *target,
                               struct dv_trim_selection *out) {
  if (!x || !target || (x->length != 10 && x->length != 12 &&
      x->length != 13 && x->length != 19 && x->length != 25) ||
      (target->length != 5 && target->length != 21) ||
      target->target_max_pq > 4095 || target->target_min_pq > target->target_max_pq)
    return false;
  dv_trim_neutral(out);
  uint32_t *w = out->word;
  w[DT_LEVEL] = 8; w[DT_TARGET_PQ] = target->target_max_pq;
  w[DT_INDEX] = x->target_display_index; w[DT_LENGTH] = (uint32_t)x->length;
  w[DT_SLOPE] = x->trim_slope; w[DT_OFFSET] = x->trim_offset;
  w[DT_POWER] = x->trim_power; w[DT_CHROMA] = x->trim_chroma_weight;
  w[DT_SATURATION] = x->trim_saturation_gain; w[DT_MS] = x->ms_weight;
  if (x->length > 10) w[DT_MID] = x->target_mid_contrast;
  if (x->length > 12) w[DT_CLIP] = x->clip_trim;
  if (x->length > 13) {
    const uint32_t sat[] = {x->saturation_vector_field0, x->saturation_vector_field1,
      x->saturation_vector_field2, x->saturation_vector_field3,
      x->saturation_vector_field4, x->saturation_vector_field5};
    memcpy(w + DT_SAT0, sat, sizeof(sat));
  }
  if (x->length > 19) {
    const uint32_t hue[] = {x->hue_vector_field0, x->hue_vector_field1,
      x->hue_vector_field2, x->hue_vector_field3,
      x->hue_vector_field4, x->hue_vector_field5};
    memcpy(w + DT_HUE0, hue, sizeof(hue));
  }
  w[DT_TARGET_LENGTH] = (uint32_t)target->length;
  w[DT_TARGET_MIN] = target->target_min_pq;
  w[DT_PRIMARY] = target->target_primary_index;
  if (target->length == 21) {
    const uint32_t primaries[] = {target->target_primary_red_x, target->target_primary_red_y,
      target->target_primary_green_x, target->target_primary_green_y,
      target->target_primary_blue_x, target->target_primary_blue_y,
      target->target_primary_white_x, target->target_primary_white_y};
    memcpy(w + DT_PRIMARY0, primaries, sizeof(primaries));
  }
  return true;
}
static inline bool dv_trim_common_valid(const struct dv_trim_selection *x) {
  for (int i = DT_SLOPE; i <= DT_SATURATION; i++)
    if (x->word[i] > 4095) return false;
  return true;
}
// Returns a borrowed-free copy for EACH frame/shot. Selection compares the
// signalled 12-bit target against the bucket quantized by the same PQ rule:
// an authored nominal 1000-nit target is not lost to PQ rounding above 1000.
// An unresolved L8 index refuses; no preset target or stale L10 is invented.
static inline const char *dv_trim_select(const DoviVdrDmData *dm, unsigned nits,
                                         struct dv_trim_selection *out) {
  dv_trim_neutral(out);
  if (!dm || !nits || nits > 10000) return "missing metadata or invalid target";
  const DoviDmData *d = &dm->dm_data;
  if (d->level2.len > 32 || d->level8.len > 32 || d->level10.len > 32 ||
      (d->level2.len && !d->level2.list) ||
      (d->level8.len && !d->level8.list) ||
      (d->level10.len && !d->level10.list)) return "unbounded trim metadata";
  const unsigned ceiling = dv_trim_bucket_pq(nits);
  for (size_t i = 0; i < d->level2.len; i++) {
    const DoviExtMetadataBlockLevel2 *x = d->level2.list[i];
    if (!x || !x->target_max_pq || x->target_max_pq > 4095 ||
        x->ms_weight < -1 || x->ms_weight > 4095) return "invalid L2";
    for (size_t j = 0; j < i; j++)
      if (d->level2.list[j]->target_max_pq == x->target_max_pq) return "duplicate L2 target";
    struct dv_trim_selection candidate;
    dv_trim_neutral(&candidate);
    uint32_t *w = candidate.word;
    w[DT_LEVEL] = 2; w[DT_TARGET_PQ] = x->target_max_pq; w[DT_LENGTH] = 11;
    w[DT_SLOPE] = x->trim_slope; w[DT_OFFSET] = x->trim_offset;
    w[DT_POWER] = x->trim_power; w[DT_CHROMA] = x->trim_chroma_weight;
    w[DT_SATURATION] = x->trim_saturation_gain;
    w[DT_MS] = (uint32_t)(int32_t)x->ms_weight;
    if (!dv_trim_common_valid(&candidate)) return "invalid L2 trim range";
    if (w[DT_TARGET_PQ] <= ceiling && w[DT_TARGET_PQ] > out->word[DT_TARGET_PQ])
      *out = candidate;
  }
  for (size_t i = 0; i < d->level10.len; i++) {
    const DoviExtMetadataBlockLevel10 *x = d->level10.list[i];
    if (!x) return "invalid L10";
    for (size_t j = 0; j < i; j++)
      if (d->level10.list[j]->target_display_index == x->target_display_index)
        return "duplicate L10 index";
  }
  for (size_t i = 0; i < d->level8.len; i++) {
    const DoviExtMetadataBlockLevel8 *x = d->level8.list[i];
    if (!x) return "invalid L8";
    for (size_t j = 0; j < i; j++)
      if (d->level8.list[j]->target_display_index == x->target_display_index)
        return "duplicate L8 index";
    const DoviExtMetadataBlockLevel10 *target = NULL;
    for (size_t j = 0; j < d->level10.len; j++)
      if (d->level10.list[j]->target_display_index == x->target_display_index)
        target = d->level10.list[j];
    if (!target) return "unresolved L8 target (preset table not implemented)";
    struct dv_trim_selection candidate;
    if (!dv_trim_l8(x, target, &candidate) || !target->target_max_pq ||
        !dv_trim_common_valid(&candidate)) return "invalid L8/L10";
    for (size_t j = 0; j < i; j++) {
      const DoviExtMetadataBlockLevel8 *previous = d->level8.list[j];
      for (size_t k = 0; k < d->level10.len; k++)
        if (d->level10.list[k]->target_display_index == previous->target_display_index &&
            d->level10.list[k]->target_max_pq == target->target_max_pq)
          return "ambiguous equal-peak L8 targets";
    }
    // CM4 L8 is used to derive L2 in the public parser documentation. Prefer
    // its original controls on an exact cross-version tie, independent of order.
    if (target->target_max_pq <= ceiling &&
        target->target_max_pq >= out->word[DT_TARGET_PQ]) *out = candidate;
  }
  return NULL;
}
static inline bool dv_trim_sop_neutral(const struct dv_trim_selection *x) {
  return x->word[DT_SLOPE] == 2048 && x->word[DT_OFFSET] == 2048 &&
         x->word[DT_POWER] == 2048;
}
// US20210076042A1 explicitly rescales SOP after eq(4): L2 (1+s,o,1+p),
// L8 (1+2s,2o,1+2p). ETSI TS103572 v1.3.1 eq(4)-(6) defines the L2 raw
// normalization. Applying the same centered /4096 raw units to L8 is a FIXED
// ENGINEERING ASSUMPTION, not defined by that L2 standard or proved by libdovi's
// XML importer. Neither this interpretation nor B.3 RGB establishes Dolby DM.
static inline bool dv_trim_public_sop(const struct dv_trim_selection *x,
                                      float sop[3]) {
  unsigned level = x->word[DT_LEVEL];
  if (x->word[DT_VERSION] != 2 || !dv_trim_common_valid(x) ||
      (level != 2 && level != 8 && (level != 0 || !dv_trim_sop_neutral(x))))
    return false;
  float scale = level == 8 ? 2.0f : 1.0f;
  sop[0] = 1.0f + scale * ((int)x->word[DT_SLOPE] - 2048) / 4096.0f;
  sop[1] = scale * ((int)x->word[DT_OFFSET] - 2048) / 4096.0f;
  sop[2] = 1.0f + scale * ((int)x->word[DT_POWER] - 2048) / 4096.0f;
  return true;
}
// Envelope under the fixed L8 normalization assumption; direct controls use values
// within it. Parameters are rejected, never silently narrowed or clamped.
static inline bool dv_trim_b3_parameters_valid(const float sop[3], float target) {
  return isfinite(sop[0]) && sop[0] >= 0 && sop[0] < 2 &&
      isfinite(sop[1]) && sop[1] >= -1 && sop[1] < 1 &&
      isfinite(sop[2]) && sop[2] >= 0 && sop[2] < 2 &&
      isfinite(target) && target > 0 && target <= 10000;
}
// Directly parameterized ST2094-10 B.3 eq(8), target-normalized linear cd/m².
// Local boundary policy: exponent zero produces one even at clipped base zero.
// Both CPU and shader branch explicitly, avoiding undefined GLSL pow(0,0).
static inline double dv_trim_b3_component(double mapped, double target,
                                           double gain, double offset, double gamma) {
  if (gamma == 0) return target;
  return target * pow(fmin(1.0, fmax(0.0, mapped / target * gain + offset)), gamma);
}
#endif
