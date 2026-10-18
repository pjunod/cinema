#ifndef PLURX_DV_YUV420P10_H
#define PLURX_DV_YUV420P10_H
// GPU-side BT.2020 NCL limited-range PQ yuv420p10le export of an already
// reconstructed, master-clipped RGB/PQ texture at the export raster.
// Replaces the CPU RGB48 pack and the encoder's single-threaded zscale:
// a 4K frame leaves the GPU as 25 MB of 10-bit YCbCr instead of 133 MB of
// RGBA32F. Planar, because NUT has no P010 tag (ffmpeg reads one back as
// rgb555le); the encoder repacks planar to P010 for hardware upload.
#include <libplacebo/renderer.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

struct dv_yuv10 {
  pl_tex planes[3];
  uint8_t *frame;
  int width, height;
};
static struct dv_yuv10 dv_yuv10_context;

static bool dv_yuv10_init(pl_gpu gpu, int width, int height) {
  if (width % 2 || height % 2)
    return false;
  pl_fmt r16 = pl_find_fmt(gpu, PL_FMT_UNORM, 1, 16, 16,
                           PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_HOST_READABLE);
  if (!r16 || r16->texel_size != 2)
    return false;
  dv_yuv10_context = (struct dv_yuv10){.width = width, .height = height};
  for (int c = 0; c < 3; c++) {
    dv_yuv10_context.planes[c] = pl_tex_create(gpu, pl_tex_params(
        .w = c ? width / 2 : width, .h = c ? height / 2 : height,
        .format = r16, .renderable = true, .host_readable = true));
    if (!dv_yuv10_context.planes[c])
      return false;
  }
  dv_yuv10_context.frame = malloc((size_t)width * height * 3);
  return dv_yuv10_context.frame != NULL;
}

// One yuv420p10le frame: Y, then Cb, then Cr, 16-bit little-endian samples
// carrying ten significant low bits, no row padding.
static size_t dv_yuv10_frame_bytes(void) {
  return (size_t)dv_yuv10_context.width * dv_yuv10_context.height * 3;
}

static bool dv_yuv10_export(pl_renderer renderer, pl_gpu gpu, pl_tex rgb) {
  struct pl_frame source = {
      .num_planes = 1,
      .planes = {{.texture = rgb, .components = 3, .component_mapping = {0, 1, 2}}},
      .repr = {.sys = PL_COLOR_SYSTEM_RGB, .levels = PL_COLOR_LEVELS_FULL,
               .bits = {.color_depth = 32}},
      .color = pl_color_space_hdr10};
  struct pl_frame target = {
      .num_planes = 3,
      .repr = {.sys = PL_COLOR_SYSTEM_BT_2020_NC,
               .levels = PL_COLOR_LEVELS_LIMITED,
               .bits = {.sample_depth = 16, .color_depth = 10}},
      .color = pl_color_space_hdr10};
  for (int c = 0; c < 3; c++)
    target.planes[c] = (struct pl_plane){
        .texture = dv_yuv10_context.planes[c], .components = 1,
        .component_mapping = {c == 0 ? PL_CHANNEL_Y : c == 1 ? PL_CHANNEL_CB : PL_CHANNEL_CR}};
  // Same mastering envelope on both sides: a pure matrix/range/subsample
  // conversion, never a tone or gamut map.
  source.color.hdr.max_luma = target.color.hdr.max_luma = 10000;
  source.color.hdr.min_luma = target.color.hdr.min_luma = 0.005;
  pl_frame_set_chroma_location(&target, PL_CHROMA_LEFT);
  struct pl_color_map_params cmap = pl_color_map_default_params;
  cmap.tone_mapping_function = &pl_tone_map_clip;
  cmap.gamut_mapping = &pl_gamut_map_clip;
  cmap.metadata = PL_HDR_METADATA_NONE;
  struct pl_render_params params = pl_render_fast_params;
  params.color_map_params = &cmap;
  params.upscaler = params.downscaler = &pl_filter_nearest;
  params.plane_upscaler = params.plane_downscaler = &pl_filter_bilinear;
  params.skip_anti_aliasing = true;
  params.disable_linear_scaling = true;
  params.dither_params = NULL;
  params.peak_detect_params = NULL;
  params.color_adjustment = NULL;
  params.sigmoid_params = NULL;
  pl_renderer_reset_errors(renderer, NULL);
  if (!pl_render_image(renderer, &source, &target, &params))
    return false;
  struct pl_render_errors errors = pl_renderer_get_errors(renderer);
  if (errors.errors || errors.num_disabled_hooks)
    return false;
  size_t at = 0;
  for (int c = 0; c < 3; c++) {
    int w = c ? dv_yuv10_context.width / 2 : dv_yuv10_context.width;
    int h = c ? dv_yuv10_context.height / 2 : dv_yuv10_context.height;
    if (!pl_tex_download(gpu, pl_tex_transfer_params(
            .tex = dv_yuv10_context.planes[c], .ptr = dv_yuv10_context.frame + at,
            .row_pitch = (size_t)w * 2)))
      return false;
    at += (size_t)w * h * 2;
  }
  return true;
}

static void dv_yuv10_close(pl_gpu gpu) {
  for (int c = 0; c < 3; c++)
    pl_tex_destroy(gpu, &dv_yuv10_context.planes[c]);
  free(dv_yuv10_context.frame);
  dv_yuv10_context.frame = NULL;
}
#endif
