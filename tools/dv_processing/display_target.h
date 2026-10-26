// Stateless, post-reconstruction display mapping for display-targeted exports.
// Closed-form Dolby Vision display management (dv_dm.h): CM v4 for shots carrying CM v4.0
// metadata (L254/L8, with L8 trims interpolated to the target), CM v2.9 otherwise (L2 trims).
// It replaces the earlier ST 2094-10 Annex B.2 per-channel curve.
#ifndef PLURX_DISPLAY_TARGET_H
#define PLURX_DISPLAY_TARGET_H
#include <errno.h>
#include <limits.h>
#include <libplacebo/shaders/custom.h>
#include "dv_dm.h"

#define DV_TARGET_METHOD "dolby-closed-form-cm4-cm29-v5"
// Creative trims applied to served output, per family. Both CM v4 (L8) and CM v2.9 (L2) trims are
// accepted by pixel comparison against the reference renderer on real titles (BUILD.md). Rust
// mirrors these as DISPLAY_TARGET_CREATIVE_TRIMS_CM4 / _CM29; changing either needs a new method
// identity. A frame that used trims reports CreativeTrimApplication.
#define DV_TARGET_TRIMS_CM4 true
#define DV_TARGET_TRIMS_CM29 true
static struct {
  unsigned nits, min_1e4, title_peak;
  unsigned max_pq, avg_pq;
  bool frame_valid, sdr;
  dv_dm_frame dm;
} dv_target;

static inline bool dv_target_uint(const char *text, unsigned *value) {
  if (!text || !*text) return false;
  for (const char *p = text; *p; p++)
    if (*p < '0' || *p > '9') return false;
  errno = 0;
  char *end;
  unsigned long result = strtoul(text, &end, 10);
  if (errno || *end || result > UINT_MAX) return false;
  *value = (unsigned)result;
  return true;
}
static inline bool dv_target_policy(const char *policy, const char *floor,
                                    const char *peak) {
  static const unsigned buckets[] = {100, 400, 600, 1000, 1600, 2000, 4000};
  for (unsigned i = 0; i < sizeof(buckets) / sizeof(*buckets); i++) {
    char expected[48];
    if (buckets[i] == 100) snprintf(expected, sizeof(expected), "bt709-gamma24-target-100");
    else snprintf(expected, sizeof(expected), "bt2020-pq-target-%u", buckets[i]);
    if (strcmp(policy, expected)) continue;
    unsigned min_value, peak_value;
    if (!dv_target_uint(floor, &min_value) || min_value > 10000 ||
        !dv_target_uint(peak, &peak_value) || !peak_value || peak_value > 10000 ||
        min_value >= peak_value * 10000) return false;
    dv_target.nits = buckets[i];
    dv_target.sdr = buckets[i] == 100;
    dv_target.min_1e4 = min_value;
    dv_target.title_peak = peak_value;
    return true;
  }
  return false;
}
static inline const char *dv_target_policy_name(void) {
  static char name[48];
  if (!dv_target.nits) return "bt2020-pq-master-clip";
  if (dv_target.sdr) return "bt709-gamma24-target-100";
  snprintf(name, sizeof(name), "bt2020-pq-target-%u", dv_target.nits);
  return name;
}
// Per frame: validate L1 and prepare the shot's display-management state from the whole RPU.
// HDR buckets map to a P3-D65 display inside the BT.2020/PQ container; the SDR bucket to BT.709.
// Target black is the fixed title minimum; the mastering display is the L9 gamut at the fixed title peak.
static inline void dv_target_metadata(const DoviVdrDmData *dm) {
  if (!dv_target.nits) return;
  dv_target.frame_valid = false;
  const DoviExtMetadataBlockLevel1 *l1 = dm ? dm->dm_data.level1 : NULL;
  require(l1 && l1->min_pq <= l1->avg_pq && l1->avg_pq <= l1->max_pq &&
              l1->max_pq <= 4095,
          "display-target-l1-invalid: whole-session master fallback required");
  dv_target.max_pq = l1->max_pq;
  dv_target.avg_pq = l1->avg_pq;
  const char *why = dv_dm_prepare(dm, dv_target.nits, dv_target.min_1e4 / 10000.0,
                                  dv_target.sdr ? DM_BT709 : DM_P3D65, dv_target.title_peak,
                                  DV_TARGET_TRIMS_CM4, DV_TARGET_TRIMS_CM29, &dv_target.dm);
  require(why == NULL, "display-target-dm-invalid: whole-session master fallback required");
  dv_target.frame_valid = true;
}

// Whether this frame's mapping applied creative trims (an L8 or L2 trim was selected).
static inline bool dv_target_trims_applied(void) {
  return dv_target.nits && dv_target.frame_valid && strcmp(dv_target.dm.trim, "l1-only") != 0;
}
static inline pl_tex dv_target_render(pl_log log, pl_gpu gpu, pl_tex source,
                                      int frame) {
  if (!dv_target.nits) return source;
  require(dv_target.frame_valid, "fresh RPU display metadata required for targeted export");
  pl_tex out = dv_dm_render(log, gpu, source, &dv_target.dm);
  require(out != NULL, "targeted display-management shader");
  printf("{\"kind\":\"display_target_frame\",\"frame\":%d,\"nits\":%u,"
         "\"method\":\"%s\",\"curve\":\"%s-%s\",\"max_pq\":%u,\"avg_pq\":%u,"
         "\"max0_fallback\":%s,\"title_min_1e4\":%u,\"title_peak\":%u,"
         "\"trim_targets\":%d,\"detail_weight\":%.4f}\n",
         frame, dv_target.nits, DV_TARGET_METHOD, dv_target.dm.algo == 4 ? "cm4" : "cm29",
         dv_target.dm.trim, dv_target.max_pq, dv_target.avg_pq,
         dv_target.max_pq ? "false" : "true", dv_target.min_1e4, dv_target.title_peak,
         dv_target.dm.trim_targets, dv_target.dm.weight);
  return out;
}
static inline void dv_target_destroy(pl_gpu gpu) {
  dv_dm_destroy(gpu);
}
#endif
