// Isolated SOP-only candidate. This file is not part of production build.sh.
#ifndef PLURX_TRIM_CANDIDATE_SHADER_H
#define PLURX_TRIM_CANDIDATE_SHADER_H
#include "../trim_selection.h"
#include "../display_target.h"
static const char dv_trim_transfer[] =
  "vec3 decode_pq(vec3 x) {\n"
  " vec3 p=pow(clamp(x,0.0,1.0),vec3(32.0/2523.0));\n"
  " return 10000.0*pow(max(p-3424.0/4096.0,0.0)/(2413.0/128.0-2392.0/128.0*p),vec3(16384.0/2610.0));\n"
  "}\n"
  "vec3 encode_pq(vec3 x) {\n"
  " vec3 p=pow(clamp(x/10000.0,0.0,1.0),vec3(2610.0/16384.0));\n"
  " return pow((3424.0/4096.0+2413.0/128.0*p)/(1.0+2392.0/128.0*p),vec3(2523.0/32.0));\n"
  "}\n";
struct dv_trim_gpu { pl_dispatch dispatch; pl_tex output; };
static inline pl_tex dv_trim_render(struct dv_trim_gpu *state, pl_log log,
    pl_gpu gpu, pl_tex source, const float sop[3], float target) {
  if (!state->dispatch) state->dispatch = pl_dispatch_create(log, gpu);
  if (!state->output) {
    pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
        PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_HOST_READABLE);
    require(fmt != NULL, "candidate float format");
    state->output = pl_tex_create(gpu, pl_tex_params(.w = source->params.w,
        .h = source->params.h, .format = fmt, .renderable = true,
        .sampleable = true, .host_readable = true));
  }
  require(state->dispatch && state->output, "candidate shader context");
  float values[4] = {sop[0], sop[1], sop[2], target};
  require(isfinite(values[0]) && values[0] >= 0.5f && values[0] <= 1.5f &&
      isfinite(values[1]) && values[1] >= -0.5f && values[1] <= 0.5f &&
      isfinite(values[2]) && values[2] >= 0.5f && values[2] <= 1.5f &&
      isfinite(target) && target > 0 && target <= 10000, "candidate B.3 parameters");
  pl_shader sh = pl_dispatch_begin(state->dispatch);
  struct pl_shader_desc input = {
    .desc = {.name = "mapped", .type = PL_DESC_SAMPLED_TEX},
    .binding = {.object = source, .address_mode = PL_TEX_ADDRESS_CLAMP,
                .sample_mode = PL_TEX_SAMPLE_NEAREST},
  };
  // Each dispatch receives the actual parameters. There is no tone-map LUT or
  // callback reading mutable external state, so shader reuse cannot retain a
  // previous shot's curve. The receipt identity also includes these float bits.
  struct pl_shader_var var = {.var = pl_var_vec4("sop_target"), .data = values,
                              .dynamic = true};
  const char *body = sop[0] == 1 && sop[1] == 0 && sop[2] == 1 ?
      "color=texelFetch(mapped,ivec2(gl_FragCoord.xy),0);" :
      "vec3 pq=texelFetch(mapped,ivec2(gl_FragCoord.xy),0).rgb;\n"
      "vec3 e=decode_pq(pq)/sop_target.w;\n"
      "vec3 f=pow(clamp(e*sop_target.x+sop_target.y,0.0,1.0),vec3(sop_target.z));\n"
      "color=vec4(encode_pq(f*sop_target.w),1.0);";
  require(pl_shader_custom(sh, &(struct pl_custom_shader){
      .description = DV_TRIM_METHOD, .header = dv_trim_transfer, .body = body,
      .input = PL_SHADER_SIG_NONE, .output = PL_SHADER_SIG_COLOR,
      .descriptors = &input, .num_descriptors = 1,
      .variables = &var, .num_variables = 1,
      .output_w = source->params.w, .output_h = source->params.h,
    }), "candidate B.3 shader assembly");
  require(pl_dispatch_finish(state->dispatch,
      pl_dispatch_params(.shader = &sh, .target = state->output)),
      "candidate B.3 shader dispatch");
  return state->output;
}
static inline void dv_trim_gpu_destroy(struct dv_trim_gpu *state, pl_gpu gpu) {
  pl_tex_destroy(gpu, &state->output);
  pl_dispatch_destroy(&state->dispatch);
}
#endif
