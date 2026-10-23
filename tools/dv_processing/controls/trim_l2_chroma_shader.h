// Isolated second dispatch: post-SOP B.3 linear RGB L2 chroma/saturation.
#ifndef PLURX_TRIM_L2_CHROMA_SHADER_H
#define PLURX_TRIM_L2_CHROMA_SHADER_H
#include "trim_candidate_shader.h"
#include "trim_l2_chroma.h"
static inline pl_tex dv_trim_l2_chroma_render(struct dv_trim_gpu *state, pl_log log,
    pl_gpu gpu, pl_tex source, const struct dv_trim_chroma *chroma) {
  require(dv_trim_chroma_valid(chroma), "candidate L2 chroma parameters");
  // A neutral/unapplied pass returns the exact post-SOP texture. It does not
  // create a second PQ round trip or interpret unsupported L8 fields as L2.
  if (chroma->value[0] == 0 && chroma->value[1] == 0) return source;
  if (!state->dispatch) state->dispatch = pl_dispatch_create(log, gpu);
  if (!state->output) {
    pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
        PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_HOST_READABLE);
    require(fmt != NULL, "L2 chroma float format");
    state->output = pl_tex_create(gpu, pl_tex_params(.w = source->params.w,
        .h = source->params.h, .format = fmt, .renderable = true,
        .sampleable = true, .host_readable = true));
  }
  require(state->dispatch && state->output, "L2 chroma shader context");
  pl_shader sh = pl_dispatch_begin(state->dispatch);
  struct pl_shader_desc input = {
    .desc = {.name = "post_sop", .type = PL_DESC_SAMPLED_TEX},
    .binding = {.object = source, .address_mode = PL_TEX_ADDRESS_CLAMP,
                .sample_mode = PL_TEX_SAMPLE_NEAREST},
  };
  // Actual per-dispatch parameters and target-primary coefficients, also
  // serialized in the receipt identity. No external callback or stale LUT.
  struct pl_shader_var vars[] = {
    {.var = pl_var_vec2("cw_sg"), .data = chroma->value, .dynamic = true},
    {.var = pl_var_vec3("target_luma"), .data = dv_trim_bt2020_luma, .dynamic = true},
  };
  require(pl_shader_custom(sh, &(struct pl_custom_shader){
      .description = DV_TRIM_L2_CHROMA_METHOD, .header = dv_trim_transfer,
      .body = "vec3 f=decode_pq(texelFetch(post_sop,ivec2(gl_FragCoord.xy),0).rgb);\n"
              "float y=dot(target_luma,f);\n"
              "vec3 g=vec3(0.0);\n"
              "if(y>0.0) g=(1.0+cw_sg.x)*y*pow(f/y,vec3(1.0+cw_sg.y));\n"
              "color=vec4(encode_pq(g),1.0);",
      .input = PL_SHADER_SIG_NONE, .output = PL_SHADER_SIG_COLOR,
      .descriptors = &input, .num_descriptors = 1,
      .variables = vars, .num_variables = 2,
      .output_w = source->params.w, .output_h = source->params.h,
    }), "candidate L2 chroma shader assembly");
  require(pl_dispatch_finish(state->dispatch,
      pl_dispatch_params(.shader = &sh, .target = state->output)),
      "candidate L2 chroma shader dispatch");
  return state->output;
}
#endif
