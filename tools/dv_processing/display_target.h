// Stateless, post-reconstruction RGB display mapping. Public reference:
// SMPTE ST 2094-10:2021 Annex B.2 equations (2)-(4), B.3 equation (7).
// This is NOT libplacebo's modified ST2094-10/LMS/adaptive-exposure path.
#ifndef PLURX_DISPLAY_TARGET_H
#define PLURX_DISPLAY_TARGET_H
#include <errno.h>
#include <limits.h>
#include <libplacebo/shaders/custom.h>

#define DV_TARGET_METHOD "annex-b2-rgb-fixed-floor-no-expansion-linear-edge-v1"
static struct {
  unsigned nits, min_1e4, title_peak;
  unsigned max_pq, avg_pq;
  bool frame_valid;
} dv_target;
static struct {
  pl_dispatch dispatch;
  pl_tex texture;
} dv_target_gpu;

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
  static const unsigned buckets[] = {400, 600, 1000, 1600, 2000, 4000};
  for (unsigned i = 0; i < sizeof(buckets) / sizeof(*buckets); i++) {
    char expected[48];
    snprintf(expected, sizeof(expected), "bt2020-pq-target-%u", buckets[i]);
    if (strcmp(policy, expected)) continue;
    unsigned min_value, peak_value;
    if (!dv_target_uint(floor, &min_value) || min_value > 10000 ||
        !dv_target_uint(peak, &peak_value) || !peak_value || peak_value > 10000 ||
        min_value >= peak_value * 10000) return false;
    dv_target.nits = buckets[i];
    dv_target.min_1e4 = min_value;
    dv_target.title_peak = peak_value;
    return true;
  }
  return false;
}
static inline const char *dv_target_policy_name(void) {
  static char name[48];
  if (!dv_target.nits) return "bt2020-pq-master-clip";
  snprintf(name, sizeof(name), "bt2020-pq-target-%u", dv_target.nits);
  return name;
}
static inline void dv_target_metadata(const DoviVdrDmData *dm) {
  if (!dv_target.nits) return;
  dv_target.frame_valid = false;
  const DoviExtMetadataBlockLevel1 *l1 = dm ? dm->dm_data.level1 : NULL;
  require(l1 && l1->min_pq <= l1->avg_pq && l1->avg_pq <= l1->max_pq &&
              l1->max_pq <= 4095,
          "display-target-l1-invalid: whole-session master fallback required");
  dv_target.max_pq = l1->max_pq;
  dv_target.avg_pq = l1->avg_pq;
  dv_target.frame_valid = true;
}
static inline double dv_target_eotf(double pq) {
  double p = pow(pq, 32.0 / 2523.0);
  return 10000.0 * pow(fmax(p - 3424.0 / 4096.0, 0.0) /
                       (2413.0 / 128.0 - 2392.0 / 128.0 * p), 16384.0 / 2610.0);
}

// Affinely normalized form of the published three-anchor rational curve:
// t=(x-x1)/(x3-x1), u=(y-y1)/(y3-y1), u=t/(k*(1-t)+t).
// The middle anchor gives k=t2*(1-u2)/(u2*(1-t2)). Positive finite k proves
// nonnegative derivative and no pole throughout the ACTUAL interval [0,1].
// The GLSL expression avoids cancellation at t=1 for a very large k.
static inline const char *dv_target_curve(float limits[4], float *k) {
  require(dv_target.frame_valid, "fresh L1 required for targeted export");
  double floor = dv_target.min_1e4 / 10000.0;
  double peak = dv_target.max_pq ? dv_target_eotf(dv_target.max_pq / 4095.0)
                                   : dv_target.title_peak;
  double target = dv_target.nits;
  limits[0] = floor; limits[1] = peak; limits[2] = target; limits[3] = 0;
  *k = 1;
  if (peak <= target) {
    limits[3] = 1;
    return "no-expansion";
  }
  double avg = dv_target_eotf(dv_target.avg_pq / 4095.0);
  if (!(floor < avg && avg < peak)) return "degenerate-average-linear";
  double adapted = fmin(0.8 * target, sqrt(avg * sqrt(floor * target)));
  if (!(floor < adapted && adapted < target)) return "degenerate-adaptation-linear";
  double t = (avg - floor) / (peak - floor);
  double u = (adapted - floor) / (target - floor);
  double fit = t * (1 - u) / (u * (1 - t));
  float uniform = (float)fit;
  if (!isfinite(fit) || fit <= 0 || !isfinite(uniform) || uniform <= 0)
    return "nonfinite-or-nonmonotone-fit-linear";
  *k = uniform;
  return "annex-b2-rgb";
}
static const char dv_target_shader[] =
  "vec3 decode_pq(vec3 x) {\n"
  " vec3 p=pow(clamp(x,0.0,1.0),vec3(32.0/2523.0));\n"
  " return 10000.0*pow(max(p-3424.0/4096.0,0.0)/(2413.0/128.0-2392.0/128.0*p),vec3(16384.0/2610.0));\n"
  "}\n"
  "vec3 encode_pq(vec3 x) {\n"
  " vec3 p=pow(clamp(x/10000.0,0.0,1.0),vec3(2610.0/16384.0));\n"
  " return pow((3424.0/4096.0+2413.0/128.0*p)/(1.0+2392.0/128.0*p),vec3(2523.0/32.0));\n"
  "}\n"
  "vec3 target_rgb(vec3 pq) {\n"
  " pq=clamp(pq,0.0,1.0);\n"
  " vec3 x=decode_pq(pq);\n"
  " if (limits.w>0.5) return min(pq,encode_pq(vec3(limits.z)));\n"
  " vec3 t=clamp((x-limits.x)/(limits.y-limits.x),0.0,1.0);\n"
  " vec3 y=limits.x+(limits.z-limits.x)*t/(curve_k*(1.0-t)+t);\n"
  " vec3 mapped=encode_pq(clamp(y,0.0,limits.z));\n"
  " return mix(mapped,pq,lessThanEqual(x,vec3(limits.x)));\n"
  "}\n";

static inline pl_tex dv_target_render(pl_log log, pl_gpu gpu, pl_tex source,
                                      int frame) {
  if (!dv_target.nits) return source;
  float limits[4], k;
  const char *reason = dv_target_curve(limits, &k);
  if (!dv_target_gpu.dispatch) dv_target_gpu.dispatch = pl_dispatch_create(log, gpu);
  if (!dv_target_gpu.texture) {
    pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
        PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR);
    require(format != NULL, "targeted native float format");
    dv_target_gpu.texture = pl_tex_create(gpu, pl_tex_params(
        .w = source->params.w, .h = source->params.h, .format = format,
        .sampleable = true, .renderable = true));
  }
  require(dv_target_gpu.dispatch && dv_target_gpu.texture, "targeted native shader context");
  pl_shader shader = pl_dispatch_begin(dv_target_gpu.dispatch);
  struct pl_shader_desc input = {
    .desc = {.name = "master", .type = PL_DESC_SAMPLED_TEX},
    .binding = {.object = source, .address_mode = PL_TEX_ADDRESS_CLAMP,
                .sample_mode = PL_TEX_SAMPLE_NEAREST},
  };
  struct pl_shader_var variables[] = {
    {.var = pl_var_vec4("limits"), .data = limits, .dynamic = true},
    {.var = pl_var_float("curve_k"), .data = &k, .dynamic = true},
  };
  require(pl_shader_custom(shader, &(struct pl_custom_shader){
      .description = DV_TARGET_METHOD, .header = dv_target_shader,
      .body = "color=vec4(target_rgb(texelFetch(master,ivec2(gl_FragCoord.xy),0).rgb),1.0);",
      .input = PL_SHADER_SIG_NONE, .output = PL_SHADER_SIG_COLOR,
      .descriptors = &input, .num_descriptors = 1,
      .variables = variables, .num_variables = 2,
      .output_w = source->params.w, .output_h = source->params.h,
    }), "targeted native shader assembly");
  require(pl_dispatch_finish(dv_target_gpu.dispatch,
      pl_dispatch_params(.shader = &shader, .target = dv_target_gpu.texture)),
      "targeted native shader dispatch");
  printf("{\"kind\":\"display_target_frame\",\"frame\":%d,\"nits\":%u,"
         "\"method\":\"%s\",\"curve\":\"%s\",\"max_pq\":%u,\"avg_pq\":%u,"
         "\"max0_fallback\":%s,\"title_min_1e4\":%u,\"title_peak\":%u}\n",
         frame, dv_target.nits, DV_TARGET_METHOD, reason, dv_target.max_pq,
         dv_target.avg_pq, dv_target.max_pq ? "false" : "true", dv_target.min_1e4,
         dv_target.title_peak);
  return dv_target_gpu.texture;
}
static inline void dv_target_destroy(pl_gpu gpu) {
  pl_tex_destroy(gpu, &dv_target_gpu.texture);
  pl_dispatch_destroy(&dv_target_gpu.dispatch);
}
#endif
