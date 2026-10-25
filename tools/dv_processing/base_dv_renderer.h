#ifndef PLURX_BASE_DV_RENDERER_H
#define PLURX_BASE_DV_RENDERER_H
// Public FFmpeg/libplacebo reconstruction; optional separate public-spec export.
#include <libplacebo/utils/libav.h>
#include "dv_trace.h"
#include "dv_metadata.h"
struct base_gpu {
  pl_log log;
  pl_vulkan vk;
  pl_renderer renderer;
  pl_tex textures[4], output_texture, native_texture;
  float *rgba;
  int width, height;
};
static struct base_gpu base_context;
struct base_mapping {
  struct pl_dovi_metadata dovi;
  int profile, polynomial_segments, mmr_segments;
};
static void base_metadata(const AVFrame *frame, const uint8_t *nal, size_t size,
                          int profile, struct base_mapping *out) {
  require(size > 2 && size <= 4098, "bounded base RPU");
  DoviRpuOpaque *raw = dovi_parse_unspec62_nalu(nal, size);
  require(raw && !dovi_rpu_get_error(raw), "fresh base RPU CRC/parse");
  const DoviRpuDataHeader *h = dovi_rpu_get_header(raw);
  const DoviRpuDataMapping *mapping = dovi_rpu_get_data_mapping(raw);
  const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(raw);
  require(h && dm && mapping && h->guessed_profile == profile &&
              (profile == 5 || profile == 7 || profile == 8),
          "decoded base RPU profile agrees with container");
  require(!h->use_prev_vdr_rpu_flag && h->vdr_dm_metadata_present_flag &&
              h->rpu_type == 2 && h->rpu_format == 18 &&
              h->vdr_rpu_profile == (profile == 5 ? 0 : 1) &&
              h->vdr_rpu_level == 0 && h->vdr_seq_info_present_flag &&
              h->coefficient_data_type == 0 &&
              h->coefficient_log2_denom == 23 &&
              h->bl_bit_depth_minus8 == 2 && h->vdr_bit_depth_minus8 == 4 &&
              h->vdr_rpu_normalized_idc == 1 &&
              !h->chroma_resampling_explicit_filter_flag &&
              !h->spatial_resampling_filter_flag &&
              h->bl_video_full_range_flag == (profile == 5) &&
              (profile == 7 || h->disable_residual_flag),
          "unsupported fresh base RPU normalization/header");
  require(mapping->num_x_partitions_minus1 == 0 &&
              mapping->num_y_partitions_minus1 == 0,
          "unsupported base mapping partitions");
  require(dm->signal_bit_depth == 12 &&
              dm->signal_color_space == (profile == 5 ? 2 : 0) &&
              dm->signal_chroma_format == 0 && dm->signal_eotf == 65535 &&
              dm->signal_eotf_param0 == 0 && dm->signal_eotf_param1 == 0 &&
              dm->signal_eotf_param2 == 0 && dm->signal_full_range_flag == 1,
          "unsupported base reconstructed signal");
  // Display metadata (L2/L8 trims, L9 primaries, L10, L11, L254, L255) is
  // carried unapplied by the master reconstruction: any version is accepted.
  if (dm->dm_data.level5)
    require((unsigned)dm->dm_data.level5->active_area_left_offset +
                    dm->dm_data.level5->active_area_right_offset <
                (unsigned)frame->width &&
                (unsigned)dm->dm_data.level5->active_area_top_offset +
                    dm->dm_data.level5->active_area_bottom_offset <
                (unsigned)frame->height,
            "invalid base active area");
  struct dv_decoded_mapping decoded;
  require(dv_map_decoded_metadata(frame, profile, false, &decoded),
          "bounded decoder-derived base metadata mapping");
  *out = (struct base_mapping){.dovi = decoded.dovi, .profile = profile,
      .polynomial_segments = decoded.polynomial_segments,
      .mmr_segments = decoded.mmr_segments};
  dv_target_metadata(dm);
  dovi_rpu_free_vdr_dm_data(dm);
  dovi_rpu_free_data_mapping(mapping);
  dovi_rpu_free_header(h);
  dovi_rpu_free(raw);
}
static void base_gpu_init(int width, int height) {
  base_context.width = width;
  base_context.height = height;
  if (!EXPORT_WIDTH && !EXPORT_HEIGHT) {
    EXPORT_WIDTH = width;
    EXPORT_HEIGHT = height;
  }
  require(EXPORT_WIDTH > 0 && EXPORT_HEIGHT > 0 &&
              EXPORT_WIDTH <= width && EXPORT_HEIGHT <= height,
          "bounded explicit base export raster");
  base_context.log = pl_log_create(PL_API_VER,
      pl_log_params(.log_cb = probe_log, .log_level = PL_LOG_WARN));
  require(base_context.log != NULL, "base GPU log");
  base_context.vk = pl_vulkan_create(base_context.log,
                                    pl_vulkan_params(.allow_software = true));
  require(base_context.vk != NULL, "base GPU device");
  require(dv_gpu_runtime(base_context.vk), "observed base GPU runtime");
  pl_gpu gpu = base_context.vk->gpu;
  base_context.renderer = pl_renderer_create(base_context.log, gpu);
  pl_fmt fmt = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
                           PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_HOST_READABLE |
                           ((dv_target.nits || EXPORT_WIDTH != width || EXPORT_HEIGHT != height) ?
                            PL_FMT_CAP_SAMPLEABLE | PL_FMT_CAP_LINEAR : 0) |
                           (EXPORT_YUV10 ? PL_FMT_CAP_SAMPLEABLE : 0));
  require(base_context.renderer && fmt && fmt->texel_size == 16,
          "base renderer/output format");
  base_context.output_texture = EXPORT_YUV10 ? dv_yuv10_source_texture(gpu, EXPORT_WIDTH, EXPORT_HEIGHT) : NULL;
  if (!base_context.output_texture)
    base_context.output_texture = pl_tex_create(gpu,
        pl_tex_params(.w = EXPORT_WIDTH, .h = EXPORT_HEIGHT, .format = fmt,
                      .renderable = true, .host_readable = true,
                      .sampleable = EXPORT_YUV10));
  require(!EXPORT_YUV10 || dv_yuv10_init(base_context.log, gpu, EXPORT_WIDTH, EXPORT_HEIGHT),
          "GPU yuv420p10 export planes");
  base_context.rgba = malloc(EXPORT_FLOAT_COUNT * sizeof(float));
  require(base_context.output_texture && base_context.rgba, "base output buffer");
  if (dv_target.nits || EXPORT_WIDTH != width || EXPORT_HEIGHT != height) {
    base_context.native_texture = pl_tex_create(gpu,
        pl_tex_params(.w = width, .h = height, .format = fmt,
                      .renderable = true, .sampleable = true));
    require(base_context.native_texture != NULL, "native base float texture");
  }
  printf("{\"kind\":\"gpu_context_created\",\"width\":%d,\"height\":%d,"
         "\"processing_mode\":\"base-rpu\"}\n", width, height);
}
static void base_gpu_render(const AVFrame *frame, const struct base_mapping *mapping,
                            int index, const char *pts, const char *duration,
                            void (*sink)(const uint8_t *, size_t, void *), bool diagnostic_hashes) {
  pl_gpu gpu = base_context.vk->gpu;
  struct pl_frame source = {0};
  // A GPU-decoded surface is mapped in place. If this Vulkan device cannot
  // import it, copy it to host memory once and keep doing so for the window.
  AVFrame *host = NULL;
  const AVFrame *input = frame;
  if (frame->format == AV_PIX_FMT_VAAPI && dv_hw_host_copy) {
    input = host = dv_host_frame(frame);
    require(host != NULL, "decoded surface host copy");
  }
  if (!pl_map_avframe_ex(gpu, &source,
          pl_avframe_params(.frame = input, .tex = base_context.textures,
                            .map_dovi = true))) {
    require(frame->format == AV_PIX_FMT_VAAPI && host == NULL,
            "public base AVFrame upload");
    dv_hw_host_copy = true;
    input = host = dv_host_frame(frame);
    require(host != NULL, "decoded surface host copy");
    require(pl_map_avframe_ex(gpu, &source,
                pl_avframe_params(.frame = input, .tex = base_context.textures,
                                  .map_dovi = true)),
            "public base AVFrame upload");
  }
  struct pl_dovi_metadata dovi = mapping->dovi;
  source.repr.dovi = &dovi;
  source.enhancement_layer = NULL;
  require(source.repr.sys == PL_COLOR_SYSTEM_DOLBYVISION,
          "actual Dolby Vision base representation");
  // The same explicit master policy as the FEL path, never a display trim pass.
  struct pl_frame destination = {.num_planes = 1,
      .planes = {{.texture = base_context.native_texture ? base_context.native_texture : base_context.output_texture, .components = 3,
                  .component_mapping = {0, 1, 2}}},
      .repr = {.sys = PL_COLOR_SYSTEM_RGB, .levels = PL_COLOR_LEVELS_FULL,
               .bits = {.color_depth = 32}}, .color = pl_color_space_hdr10};
  source.color.hdr.max_luma = destination.color.hdr.max_luma = 10000;
  source.color.hdr.min_luma = destination.color.hdr.min_luma = 0.005;
  struct pl_color_map_params cmap = pl_color_map_default_params;
  cmap.tone_mapping_function = &pl_tone_map_clip;
  cmap.gamut_mapping = &pl_gamut_map_clip;
  cmap.metadata = PL_HDR_METADATA_NONE;
  struct pl_render_params params = pl_render_fast_params;
  params.color_map_params = &cmap;
  params.plane_upscaler = &pl_filter_nearest;
  params.plane_downscaler = &pl_filter_nearest;
  params.dither_params = NULL;
  params.peak_detect_params = NULL;
  params.color_adjustment = NULL;
  params.sigmoid_params = NULL;
  pl_renderer_reset_errors(base_context.renderer, NULL);
  require(pl_render_image(base_context.renderer, &source, &destination, &params),
          "base DV render");
  if (dv_target.nits)
    destination.planes[0].texture = dv_target_render(base_context.log, gpu,
                                                    destination.planes[0].texture, index);
  if (base_context.native_texture)
    export_native_rgb(base_context.renderer, destination, base_context.output_texture);
  struct pl_render_errors errors = pl_renderer_get_errors(base_context.renderer);
  require(errors.errors == 0 && errors.num_disabled_hooks == 0,
          "base renderer errors");
  const uint8_t *exported = NULL;
  size_t exported_bytes = 0;
  uint8_t *packed = NULL;
  if (EXPORT_YUV10) {
    require(dv_yuv10_export_color(base_context.renderer, gpu, base_context.output_texture, dv_target.sdr),
            "base GPU yuv420p10 export");
    pl_unmap_avframe(gpu, &source);
    exported = dv_yuv10_context.frame;
    exported_bytes = dv_yuv10_frame_bytes();
  } else {
  require(pl_tex_download(gpu, pl_tex_transfer_params(
              .tex = base_context.output_texture, .ptr = base_context.rgba)),
          "base RGB download");
  pl_unmap_avframe(gpu, &source);
  size_t pixels = EXPORT_FLOAT_COUNT / 4;
  packed = malloc(pixels * 6);
  require(packed != NULL, "base packed RGB buffer");
  require(dv_pack_rgba_rgb48(base_context.rgba, pixels, packed, pixels * 6),
          "finite base RGBA");
  exported = packed;
  exported_bytes = pixels * 6;
  }
  av_frame_free(&host);
  sink(exported, exported_bytes, NULL);
  char rgb_hash[65];
  if (diagnostic_hashes) {
    if (EXPORT_YUV10)
      dv_yuv10_wait();
    struct AVSHA *sha = av_sha_alloc();
    uint8_t hash[32];
    require(sha && av_sha_init(sha, 256) == 0, "base RGB SHA256");
    av_sha_update(sha, exported, exported_bytes);
    av_sha_final(sha, hash);
    av_free(sha);
    for (int i = 0; i < 32; i++) snprintf(rgb_hash + 2*i, 3, "%02x", hash[i]);
  }
  free(packed);
  printf("{\"kind\":\"rendered_frame\",\"frame\":%d,\"width\":%d,\"height\":%d,"
         "\"pts\":\"%s\",\"duration\":\"%s\",\"el_bound\":false,\"nlq_active\":false,"
         "\"fel_contributed\":false,\"profile\":%d,\"polynomial_segments\":%d,"
         "\"mmr_segments\":%d",
         index, EXPORT_WIDTH, EXPORT_HEIGHT, pts, duration, mapping->profile,
         mapping->polynomial_segments, mapping->mmr_segments);
  printf(",\"applied_operations\":[\"RepresentationNormalization\"");
  if (mapping->polynomial_segments) printf(",\"PolynomialReshape\"");
  if (mapping->mmr_segments) printf(",\"MmrReshape\"");
  printf(",\"RpuColorConversion\",\"TargetMapping\"");
  if (dv_target.nits)
    printf(",{\"DisplayTargetMapping\":{\"nits\":%u}}", dv_target.nits);
  if (dv_target_trims_applied()) printf(",\"CreativeTrimApplication\"");
  printf("]");
  if (diagnostic_hashes)
    printf(EXPORT_YUV10 ? ",\"yuv10_sha256\":\"%s\"" : ",\"rgb_sha256\":\"%s\"", rgb_hash);
  printf(",\"creative_trims_applied\":%s,\"render_errors\":%u,"
         "\"production_qualified\":false}\n", dv_target_trims_applied() ? "true" : "false", errors.errors);
}
static void base_gpu_close(void) {
  pl_gpu gpu = base_context.vk->gpu;
  dv_target_destroy(gpu);
  for (int i = 0; i < 4; i++) pl_tex_destroy(gpu, &base_context.textures[i]);
  pl_tex_destroy(gpu, &base_context.output_texture);
  pl_tex_destroy(gpu, &base_context.native_texture);
  if (EXPORT_YUV10) dv_yuv10_close(gpu);
  pl_renderer_destroy(&base_context.renderer);
  pl_vulkan_destroy(&base_context.vk);
  pl_log_destroy(&base_context.log);
  free(base_context.rgba);
}
#endif
