#ifndef PLURX_DV_YUV420P10_H
#define PLURX_DV_YUV420P10_H
// GPU-side BT.2020 NCL limited-range PQ yuv420p10le export of an already
// reconstructed, master-clipped RGB/PQ texture at the export raster.
// Replaces the CPU RGB48 pack and the encoder's single-threaded zscale:
// a 4K frame leaves the GPU as 25 MB of 10-bit YCbCr instead of 133 MB of
// RGBA32F. Planar, because NUT has no P010 tag (ffmpeg reads one back as
// rgb555le); the encoder repacks planar to P010 for hardware upload.
//
// A fixed matrix on exact texel fetches: luma at full resolution, Cb and Cr
// at half resolution with left-sited chroma (the [1 2 1]/4 horizontal by
// [1 1]/2 vertical filter centred on even columns, edges clamped). One
// compute pass writes all three planes where R16 is storable, otherwise one
// fragment pass per plane. A general pl_render_image into a three-plane
// target cost more than the whole reconstruction at 4K.
//
// Where the device offers host-mapped buffers, the planes download into one
// of two such buffers without waiting: the writer thread waits for its
// buffer before sending, so the GPU finishes one frame while the next is
// decoded and submitted. Callers must keep that frame's decoded surfaces
// alive until it has been written.
#include <libplacebo/dispatch.h>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/custom.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

struct dv_yuv10 {
  pl_tex planes[3];
  pl_dispatch dispatch;
  // Two download buffers, alternated per frame: the writer may still be
  // sending the previous frame while this one downloads. `frame` is the one
  // just downloaded.
  uint8_t *buffers[2], *frame;
  // Host-mapped download targets, when the device offers them: the
  // download is queued and the writer thread waits for its buffer, so the
  // GPU finishes one frame while the next is decoded and submitted.
  pl_buf mapped[2], frame_buf;
  pl_gpu gpu;
  int next, width, height;
};
static struct dv_yuv10 dv_yuv10_context;
static bool dv_yuv10_fused;

static bool dv_yuv10_init(pl_log log, pl_gpu gpu, int width, int height) {
  if (width % 2 || height % 2)
    return false;
  pl_fmt r16 = pl_find_fmt(gpu, PL_FMT_UNORM, 1, 16, 16,
                           PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_HOST_READABLE);
  // The fused pass runs 16x16 invocations per group; Vulkan guarantees 128.
  dv_yuv10_fused = r16 && (r16->caps & PL_FMT_CAP_STORABLE) && r16->glsl_format &&
                   gpu->glsl.compute && gpu->glsl.max_group_threads >= 256;
  if (!r16 || r16->texel_size != 2)
    return false;
  dv_yuv10_context = (struct dv_yuv10){.width = width, .height = height, .gpu = gpu};
  dv_yuv10_context.dispatch = pl_dispatch_create(log, gpu);
  if (!dv_yuv10_context.dispatch)
    return false;
  for (int c = 0; c < 3; c++) {
    dv_yuv10_context.planes[c] = pl_tex_create(gpu, pl_tex_params(
        .w = c ? width / 2 : width, .h = c ? height / 2 : height,
        .format = r16, .renderable = true, .host_readable = true,
        .storable = dv_yuv10_fused));
    if (!dv_yuv10_context.planes[c])
      return false;
  }
  size_t bytes = (size_t)width * height * 3;
  // Each plane lands at its offset in one buffer. The Cr plane starts at
  // 5/2 W H bytes, which a transfer queue may refuse when (W/2)(H/2) is odd.
  size_t cr = (size_t)width * height * 5 / 2;
  size_t align = gpu->limits.align_tex_xfer_offset > 4 ? gpu->limits.align_tex_xfer_offset : 4;
  bool mapped = gpu->limits.thread_safe && gpu->limits.max_mapped_size >= bytes &&
                ((size_t)width * height * 2) % align == 0 && cr % align == 0;
  for (int b = 0; b < 2 && mapped; b++) {
    dv_yuv10_context.mapped[b] = pl_buf_create(gpu, pl_buf_params(
        .size = bytes, .host_mapped = true, .host_readable = true));
    mapped = dv_yuv10_context.mapped[b] && dv_yuv10_context.mapped[b]->data;
  }
  if (mapped) {
    dv_yuv10_context.frame = dv_yuv10_context.mapped[0]->data;
    return true;
  }
  for (int b = 0; b < 2; b++)
    pl_buf_destroy(gpu, &dv_yuv10_context.mapped[b]);
  for (int b = 0; b < 2; b++) {
    dv_yuv10_context.buffers[b] = malloc(bytes);
    if (!dv_yuv10_context.buffers[b])
      return false;
  }
  dv_yuv10_context.frame = dv_yuv10_context.buffers[0];
  return true;
}

// The texture the reconstruction renders into ahead of this export. 16-bit
// UNORM rather than 32-bit float halves the memory traffic of writing it and
// of the export reading it back, at 1/65535 steps against the export's
// 1/876 luma code. NULL when the device lacks the format; the caller then
// keeps its float texture.
static pl_tex dv_yuv10_source_texture(pl_gpu gpu, int width, int height) {
  pl_fmt u16 = pl_find_fmt(gpu, PL_FMT_UNORM, 4, 16, 16,
                           PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE);
  return u16 ? pl_tex_create(gpu, pl_tex_params(.w = width, .h = height, .format = u16,
                                                 .renderable = true, .sampleable = true))
             : NULL;
}

// One yuv420p10le frame: Y, then Cb, then Cr, 16-bit little-endian samples
// carrying ten significant low bits, no row padding.
static size_t dv_yuv10_frame_bytes(void) {
  return (size_t)dv_yuv10_context.width * dv_yuv10_context.height * 3;
}

// BT.2020 NCL: Kr 0.2627, Kb 0.0593. Limited range, 10-bit codes: luma
// 64 + 876 Y', chroma 512 + 896 C. Each code is written as code / 65535 to
// an R16 UNORM plane, so the downloaded 16-bit sample is the code itself.
static const char dv_yuv10_header[] =
    "const vec3 dv_kY = vec3(0.2627, 0.6780, 0.0593);\n"
    "vec3 dv_rgb_at(ivec2 p) {\n"
    "  ivec2 size = textureSize(dv_rgb, 0);\n"
    "  return clamp(texelFetch(dv_rgb, clamp(p, ivec2(0), size - 1), 0).rgb,\n"
    "               0.0, 1.0);\n"
    "}\n"
    "float dv_code(float code) { return floor(code + 0.5) / 65535.0; }\n";

static const char *const dv_yuv10_bodies[3] = {
    "vec3 rgb = dv_rgb_at(ivec2(gl_FragCoord.xy));\n"
    "color = vec4(dv_code(64.0 + 876.0 * dot(dv_kY, rgb)), 0.0, 0.0, 1.0);\n",
    // Cb: (B' - Y') / 1.8814
    "ivec2 p = ivec2(gl_FragCoord.xy) * 2;\n"
    "vec3 rgb = 0.125 * (dv_rgb_at(p + ivec2(-1, 0)) + dv_rgb_at(p + ivec2(-1, 1))\n"
    "                  + dv_rgb_at(p + ivec2( 1, 0)) + dv_rgb_at(p + ivec2( 1, 1)))\n"
    "         + 0.25  * (dv_rgb_at(p) + dv_rgb_at(p + ivec2(0, 1)));\n"
    "color = vec4(dv_code(512.0 + 896.0 * (rgb.b - dot(dv_kY, rgb)) / 1.8814),\n"
    "             0.0, 0.0, 1.0);\n",
    // Cr: (R' - Y') / 1.4746
    "ivec2 p = ivec2(gl_FragCoord.xy) * 2;\n"
    "vec3 rgb = 0.125 * (dv_rgb_at(p + ivec2(-1, 0)) + dv_rgb_at(p + ivec2(-1, 1))\n"
    "                  + dv_rgb_at(p + ivec2( 1, 0)) + dv_rgb_at(p + ivec2( 1, 1)))\n"
    "         + 0.25  * (dv_rgb_at(p) + dv_rgb_at(p + ivec2(0, 1)));\n"
    "color = vec4(dv_code(512.0 + 896.0 * (rgb.r - dot(dv_kY, rgb)) / 1.4746),\n"
    "             0.0, 0.0, 1.0);\n",
};

// One compute pass per frame: each invocation owns a 2x2 luma block and
// its co-sited chroma sample, so every source texel is fetched once
// (six fetches per block instead of sixteen across three passes).
static const char dv_yuv10_fused_body[] =
    "ivec2 b = ivec2(gl_GlobalInvocationID.xy);\n"
    "if (all(lessThan(b, imageSize(dv_cb)))) {\n"
    "ivec2 p = b * 2;\n"
    "vec3 l0 = dv_rgb_at(p + ivec2(-1, 0)), l1 = dv_rgb_at(p + ivec2(-1, 1));\n"
    "vec3 c00 = dv_rgb_at(p), c01 = dv_rgb_at(p + ivec2(0, 1));\n"
    "vec3 c10 = dv_rgb_at(p + ivec2(1, 0)), c11 = dv_rgb_at(p + ivec2(1, 1));\n"
    "imageStore(dv_y, p, vec4(dv_code(64.0 + 876.0 * dot(dv_kY, c00))));\n"
    "imageStore(dv_y, p + ivec2(0, 1), vec4(dv_code(64.0 + 876.0 * dot(dv_kY, c01))));\n"
    "imageStore(dv_y, p + ivec2(1, 0), vec4(dv_code(64.0 + 876.0 * dot(dv_kY, c10))));\n"
    "imageStore(dv_y, p + ivec2(1, 1), vec4(dv_code(64.0 + 876.0 * dot(dv_kY, c11))));\n"
    "vec3 rgb = 0.125 * (l0 + l1 + c10 + c11) + 0.25 * (c00 + c01);\n"
    "float y = dot(dv_kY, rgb);\n"
    "imageStore(dv_cb, b, vec4(dv_code(512.0 + 896.0 * (rgb.b - y) / 1.8814)));\n"
    "imageStore(dv_cr, b, vec4(dv_code(512.0 + 896.0 * (rgb.r - y) / 1.4746)));\n"
    "}\n";

static bool dv_yuv10_dispatch_fused(pl_tex rgb) {
  pl_shader sh = pl_dispatch_begin(dv_yuv10_context.dispatch);
  struct pl_shader_desc descriptors[4] = {
      {.desc = {.name = "dv_rgb", .type = PL_DESC_SAMPLED_TEX},
       .binding = {.object = rgb, .address_mode = PL_TEX_ADDRESS_CLAMP,
                   .sample_mode = PL_TEX_SAMPLE_NEAREST}},
      {.desc = {.name = "dv_y", .type = PL_DESC_STORAGE_IMG, .access = PL_DESC_ACCESS_WRITEONLY},
       .binding = {.object = dv_yuv10_context.planes[0]}},
      {.desc = {.name = "dv_cb", .type = PL_DESC_STORAGE_IMG, .access = PL_DESC_ACCESS_WRITEONLY},
       .binding = {.object = dv_yuv10_context.planes[1]}},
      {.desc = {.name = "dv_cr", .type = PL_DESC_STORAGE_IMG, .access = PL_DESC_ACCESS_WRITEONLY},
       .binding = {.object = dv_yuv10_context.planes[2]}},
  };
  int gx = 16, gy = 16;
  int bw = dv_yuv10_context.width / 2, bh = dv_yuv10_context.height / 2;
  if (!pl_shader_custom(sh, &(struct pl_custom_shader){
             .description = "plurx yuv420p10 fused export",
             .header = dv_yuv10_header,
             .body = dv_yuv10_fused_body,
             .input = PL_SHADER_SIG_NONE,
             .output = PL_SHADER_SIG_NONE,
             .descriptors = descriptors,
             .num_descriptors = 4,
             .compute = true,
             .compute_group_size = {gx, gy},
         })) {
    pl_dispatch_abort(dv_yuv10_context.dispatch, &sh);
    return false;
  }
  return pl_dispatch_compute(dv_yuv10_context.dispatch, pl_dispatch_compute_params(
      .shader = &sh,
      .dispatch_size = {(bw + gx - 1) / gx, (bh + gy - 1) / gy, 1}));
}

// Convert the already L1-mapped absolute-PQ native picture using the public
// library primaries/EOTF implementation. This is also used by the native SDR
// qualification adapter, so that adapter exercises the production conversion.
static inline bool dv_yuv10_convert_sdr_rgb(pl_renderer renderer, struct pl_frame native, pl_tex output) {
  native.color.hdr = (struct pl_hdr_metadata){.max_luma = 100, .min_luma = PL_COLOR_HDR_BLACK};
  struct pl_frame target = native;
  target.planes[0].texture = output;
  target.color = (struct pl_color_space){.primaries = PL_COLOR_PRIM_BT_709,
    .transfer = PL_COLOR_TRC_GAMMA24,
    .hdr = {.max_luma = 100, .min_luma = PL_COLOR_HDR_BLACK}};
  struct pl_color_map_params cmap = pl_color_map_default_params;
  cmap.tone_mapping_function = &pl_tone_map_clip;
  cmap.gamut_mapping = &pl_gamut_map_clip;
  cmap.metadata = PL_HDR_METADATA_NONE;
  struct pl_render_params params = pl_render_fast_params;
  params.upscaler = params.downscaler = &pl_filter_bilinear;
  params.skip_anti_aliasing = true; params.disable_linear_scaling = true;
  params.dither_params = NULL; params.peak_detect_params = NULL;
  params.color_adjustment = NULL; params.sigmoid_params = NULL;
  params.color_map_params = &cmap;
  return pl_render_image(renderer, &native, &target, &params);
}

static bool dv_yuv10_export_color(pl_renderer renderer, pl_gpu gpu, pl_tex rgb, bool sdr) {
  if (sdr) {
    // Let libplacebo encode the BT.709 matrix and limited 10-bit planes;
    // the optimized BT.2020 shader below remains byte-identical for HDR.
    struct pl_color_space color = {.primaries = PL_COLOR_PRIM_BT_709,
      .transfer = PL_COLOR_TRC_GAMMA24,
      .hdr = {.max_luma = 100, .min_luma = PL_COLOR_HDR_BLACK}};
    struct pl_frame source = {.num_planes = 1,
      .planes = {{.texture = rgb, .components = 4, .component_mapping = {0, 1, 2, -1}}},
      .repr = {.sys = PL_COLOR_SYSTEM_RGB, .levels = PL_COLOR_LEVELS_FULL}, .color = color};
    struct pl_frame target = {.num_planes = 3,
      .repr = {.sys = PL_COLOR_SYSTEM_BT_709, .levels = PL_COLOR_LEVELS_LIMITED,
               .bits = {.sample_depth = 16, .color_depth = 10}}, .color = color};
    for (int c = 0; c < 3; c++)
      target.planes[c] = (struct pl_plane){.texture = dv_yuv10_context.planes[c],
        .components = 1, .component_mapping = {c}};
    pl_frame_set_chroma_location(&target, PL_CHROMA_LEFT);
    struct pl_render_params params = pl_render_fast_params;
    params.upscaler = params.downscaler = &pl_filter_bilinear;
    params.skip_anti_aliasing = true;
    params.disable_linear_scaling = true;
    params.dither_params = NULL;
    params.peak_detect_params = NULL;
    params.color_adjustment = NULL;
    params.sigmoid_params = NULL;
    if (!pl_render_image(renderer, &source, &target, &params)) return false;
    struct pl_render_errors errors = pl_renderer_get_errors(renderer);
    if (errors.errors || errors.num_disabled_hooks) return false;
  } else if (dv_yuv10_fused) {
    if (!dv_yuv10_dispatch_fused(rgb))
      return false;
  } else
  for (int c = 0; c < 3; c++) {
    pl_shader sh = pl_dispatch_begin(dv_yuv10_context.dispatch);
    struct pl_shader_desc source = {
        .desc = {.name = "dv_rgb", .type = PL_DESC_SAMPLED_TEX},
        .binding = {.object = rgb,
                    .address_mode = PL_TEX_ADDRESS_CLAMP,
                    .sample_mode = PL_TEX_SAMPLE_NEAREST},
    };
    if (!pl_shader_custom(sh, &(struct pl_custom_shader){
            .description = "plurx yuv420p10 plane export",
            .header = dv_yuv10_header,
            .body = dv_yuv10_bodies[c],
            .input = PL_SHADER_SIG_NONE,
            .output = PL_SHADER_SIG_COLOR,
            .descriptors = &source,
            .num_descriptors = 1,
        })) {
      pl_dispatch_abort(dv_yuv10_context.dispatch, &sh);
      return false;
    }
    if (!pl_dispatch_finish(dv_yuv10_context.dispatch,
                            pl_dispatch_params(.shader = &sh,
                                               .target = dv_yuv10_context.planes[c])))
      return false;
  }
  int slot = dv_yuv10_context.next;
  dv_yuv10_context.next ^= 1;
  pl_buf buf = dv_yuv10_context.mapped[slot];
  uint8_t *frame = buf ? buf->data : dv_yuv10_context.buffers[slot];
  size_t at = 0;
  for (int c = 0; c < 3; c++) {
    int w = c ? dv_yuv10_context.width / 2 : dv_yuv10_context.width;
    int h = c ? dv_yuv10_context.height / 2 : dv_yuv10_context.height;
    if (!pl_tex_download(gpu, buf ? pl_tex_transfer_params(
                                        .tex = dv_yuv10_context.planes[c], .buf = buf,
                                        .buf_offset = at, .row_pitch = (size_t)w * 2)
                                  : pl_tex_transfer_params(
                                        .tex = dv_yuv10_context.planes[c], .ptr = frame + at,
                                        .row_pitch = (size_t)w * 2)))
      return false;
    at += (size_t)w * h * 2;
  }
  if (buf)
    pl_gpu_flush(gpu);
  dv_yuv10_context.frame = frame;
  dv_yuv10_context.frame_buf = buf;
  return true;
}

static inline bool dv_yuv10_export(pl_renderer renderer, pl_gpu gpu, pl_tex rgb) {
  return dv_yuv10_export_color(renderer, gpu, rgb, false);
}

// Blocks until a queued download has landed in its mapped buffer. Called by
// the writer thread before it sends a frame, and by the diagnostic hash.
static void dv_yuv10_wait_buffer(pl_buf buf) {
  while (buf && pl_buf_poll(dv_yuv10_context.gpu, buf, UINT64_MAX)) {
  }
}
static void dv_yuv10_wait(void) { dv_yuv10_wait_buffer(dv_yuv10_context.frame_buf); }

static void dv_yuv10_close(pl_gpu gpu) {
  for (int c = 0; c < 3; c++)
    pl_tex_destroy(gpu, &dv_yuv10_context.planes[c]);
  pl_dispatch_destroy(&dv_yuv10_context.dispatch);
  for (int b = 0; b < 2; b++) {
    dv_yuv10_wait_buffer(dv_yuv10_context.mapped[b]);
    pl_buf_destroy(gpu, &dv_yuv10_context.mapped[b]);
    free(dv_yuv10_context.buffers[b]);
  }
  dv_yuv10_context.frame = NULL;
  dv_yuv10_context.frame_buf = NULL;
}
#endif
