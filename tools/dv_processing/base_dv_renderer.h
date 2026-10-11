#ifndef PLURX_BASE_DV_RENDERER_H
#define PLURX_BASE_DV_RENDERER_H
// Decode and GPU mapping are public FFmpeg/libplacebo APIs. No color-math fork.
#include <libplacebo/utils/libav.h>
#include "dv_trace.h"
struct base_gpu {
  pl_log log;
  pl_vulkan vk;
  pl_renderer renderer;
  pl_tex textures[4], output_texture;
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
  require(dm->dm_data.level2.len <= 16 && dm->dm_data.level8.len <= 16 &&
              !dm->dm_data.level10.len && !dm->dm_data.level255,
          "unsupported base display metadata subset");
  for (size_t i = 0; i < dm->dm_data.level8.len; i++)
    require(dm->dm_data.level8.list[i] && dm->dm_data.level8.list[i]->length == 10,
            "unsupported base extended trim format");
  if (dm->dm_data.level5)
    require((unsigned)dm->dm_data.level5->active_area_left_offset +
                    dm->dm_data.level5->active_area_right_offset <
                (unsigned)frame->width &&
                (unsigned)dm->dm_data.level5->active_area_top_offset +
                    dm->dm_data.level5->active_area_bottom_offset <
                (unsigned)frame->height,
            "invalid base active area");
  if (dm->dm_data.level9)
    require(dm->dm_data.level9->length == 1 &&
                dm->dm_data.level9->source_primary_index == 0,
            "unsupported explicit source primaries");
  if (dm->dm_data.level11)
    require(dm->dm_data.level11->content_type == 1 &&
                dm->dm_data.level11->whitepoint == 0 &&
                dm->dm_data.level11->reference_mode_flag &&
                dm->dm_data.level11->reserved_byte2 == 0 &&
                dm->dm_data.level11->reserved_byte3 == 0,
            "unsupported content type/whitepoint policy");
  if (dm->dm_data.level254)
    require(dm->dm_data.level254->dm_mode == 0 &&
                dm->dm_data.level254->dm_version_index == 2,
            "unsupported CM mode/version");
  AVFrameSideData *side = av_frame_get_side_data(frame, AV_FRAME_DATA_DOVI_METADATA);
  require(side && side->size >= sizeof(AVDOVIMetadata) + sizeof(AVDOVIDataMapping) && side->size <= 65536,
          "actual decoder-derived base metadata required");
  const AVDOVIMetadata *data = (const void *)side->data;
  require(data->header_offset >= sizeof(*data) &&
              data->header_offset % _Alignof(AVDOVIRpuDataHeader) == 0 &&
              data->mapping_offset % _Alignof(AVDOVIDataMapping) == 0 &&
              data->color_offset % _Alignof(AVDOVIColorMetadata) == 0 &&
              data->header_offset <= side->size - sizeof(AVDOVIRpuDataHeader) &&
              data->mapping_offset <= side->size - sizeof(AVDOVIDataMapping) &&
              data->color_offset <= side->size - sizeof(AVDOVIColorMetadata),
          "bounded decoded metadata offsets");
  require(data->header_offset + sizeof(AVDOVIRpuDataHeader) <= data->mapping_offset &&
              data->mapping_offset + sizeof(AVDOVIDataMapping) <= data->color_offset &&
              data->color_offset + sizeof(AVDOVIColorMetadata) <= data->ext_block_offset,
          "nonoverlapping decoded metadata sections");
  require(data->num_ext_blocks >= 0 && data->num_ext_blocks <= 32 &&
              data->ext_block_offset <= side->size &&
              data->ext_block_size >= sizeof(AVDOVIDmData) &&
              (size_t)data->num_ext_blocks <=
                  (side->size - data->ext_block_offset) / data->ext_block_size,
          "bounded decoded extension metadata");
  const AVDOVIRpuDataHeader *dh = av_dovi_get_header(data);
  require(dh->ext_mapping_idc_0_4 == 0 && dh->ext_mapping_idc_5_7 == 0,
          "unsupported extended base inverse mapping");
  const AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  const AVDOVIColorMetadata *color = av_dovi_get_color(data);
  require(dh->bl_bit_depth == 10 && dh->vdr_bit_depth == 12 &&
              dh->coef_log2_denom == 23 && dh->vdr_rpu_normalized_idc == 1 &&
              dh->bl_video_full_range_flag == (profile == 5) &&
              m->mapping_color_space == 0 && m->mapping_chroma_format_idc == 0 &&
              m->num_x_partitions == 1 && m->num_y_partitions == 1 &&
              color->signal_eotf == 65535 && color->signal_eotf_param0 == 0 &&
              color->signal_eotf_param1 == 0 && color->signal_eotf_param2 == 0,
          "decoded base metadata representation");
  *out = (struct base_mapping){.profile = profile};
  for (int c = 0; c < 3; c++) {
    const AVDOVIReshapingCurve *curve = &m->curves[c];
    require(curve->num_pivots >= 2 && curve->num_pivots <= 9 &&
                curve->pivots[0] == 0 &&
                curve->pivots[curve->num_pivots - 1] == 1023,
            "bounded complete base curve");
    for (int i = 0; i < curve->num_pivots - 1; i++) {
      require(curve->pivots[i] < curve->pivots[i + 1],
              "strict base curve pivots");
      if (curve->mapping_idc[i] == AV_DOVI_MAPPING_POLYNOMIAL) {
        require(curve->poly_order[i] >= 1 && curve->poly_order[i] <= 2,
                "supported base polynomial order");
        out->polynomial_segments++;
      } else {
        require(curve->mapping_idc[i] == AV_DOVI_MAPPING_MMR &&
                    curve->mmr_order[i] >= 1 && curve->mmr_order[i] <= 3,
                "supported base MMR order");
        out->mmr_segments++;
      }
    }
  }
  // This public mapper carries arbitrary signalled polynomial/MMR/color
  // coefficients. No identity-curve or fixed-matrix substitution occurs.
  pl_map_dovi_metadata(&out->dovi, data);
  out->dovi.nlq_active = false; // Explicitly omitted EL: never compose residuals.
  dovi_rpu_free_vdr_dm_data(dm);
  dovi_rpu_free_data_mapping(mapping);
  dovi_rpu_free_header(h);
  dovi_rpu_free(raw);
}
static void base_gpu_init(int width, int height) {
  base_context.width = width;
  base_context.height = height;
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
                           PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_HOST_READABLE);
  require(base_context.renderer && fmt && fmt->texel_size == 16,
          "base renderer/output format");
  base_context.output_texture = pl_tex_create(gpu,
      pl_tex_params(.w = width, .h = height, .format = fmt,
                    .renderable = true, .host_readable = true));
  base_context.rgba = malloc((size_t)width * height * 16);
  require(base_context.output_texture && base_context.rgba, "base output buffer");
  printf("{\"kind\":\"gpu_context_created\",\"width\":%d,\"height\":%d,"
         "\"processing_mode\":\"base-rpu\"}\n", width, height);
}
static void base_gpu_render(const AVFrame *frame, const struct base_mapping *mapping,
                            int index, const char *pts, const char *duration,
                            void (*sink)(const uint8_t *, size_t, void *), bool diagnostic_hashes) {
  pl_gpu gpu = base_context.vk->gpu;
  struct pl_frame source = {0};
  require(pl_map_avframe_ex(gpu, &source,
              pl_avframe_params(.frame = frame, .tex = base_context.textures,
                                .map_dovi = true)), "public base AVFrame upload");
  struct pl_dovi_metadata dovi = mapping->dovi;
  source.repr.dovi = &dovi;
  source.enhancement_layer = NULL;
  require(source.repr.sys == PL_COLOR_SYSTEM_DOLBYVISION,
          "actual Dolby Vision base representation");
  // The same explicit master policy as the FEL path, never a display trim pass.
  struct pl_frame destination = {.num_planes = 1,
      .planes = {{.texture = base_context.output_texture, .components = 3,
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
  struct pl_render_errors errors = pl_renderer_get_errors(base_context.renderer);
  require(errors.errors == 0 && errors.num_disabled_hooks == 0,
          "base renderer errors");
  require(pl_tex_download(gpu, pl_tex_transfer_params(
              .tex = base_context.output_texture, .ptr = base_context.rgba)),
          "base RGB download");
  pl_unmap_avframe(gpu, &source);
  size_t pixels = (size_t)frame->width * frame->height;
  uint8_t *packed = malloc(pixels * 6);
  require(packed != NULL, "base packed RGB buffer");
  for (size_t i = 0; i < pixels; i++) {
    for (int c = 0; c < 4; c++)
      require(isfinite(base_context.rgba[4 * i + c]), "finite base RGBA");
    for (int c = 0; c < 3; c++) {
      uint16_t code = dv_pack_finite_rgb48(base_context.rgba[4 * i + c]);
      packed[6 * i + 2 * c] = (uint8_t)code;
      packed[6 * i + 2 * c + 1] = (uint8_t)(code >> 8);
    }
  }
  sink(packed, pixels * 6, NULL);
  char rgb_hash[65];
  if (diagnostic_hashes) {
    struct AVSHA *sha = av_sha_alloc();
    uint8_t hash[32];
    require(sha && av_sha_init(sha, 256) == 0, "base RGB SHA256");
    av_sha_update(sha, packed, pixels * 6);
    av_sha_final(sha, hash);
    av_free(sha);
    for (int i = 0; i < 32; i++) snprintf(rgb_hash + 2*i, 3, "%02x", hash[i]);
  }
  free(packed);
  printf("{\"kind\":\"rendered_frame\",\"frame\":%d,\"width\":%d,\"height\":%d,"
         "\"pts\":\"%s\",\"duration\":\"%s\",\"el_bound\":false,\"nlq_active\":false,"
         "\"fel_contributed\":false,\"profile\":%d,\"polynomial_segments\":%d,"
         "\"mmr_segments\":%d",
         index, frame->width, frame->height, pts, duration, mapping->profile,
         mapping->polynomial_segments, mapping->mmr_segments);
  printf(",\"applied_operations\":[\"RepresentationNormalization\"");
  if (mapping->polynomial_segments) printf(",\"PolynomialReshape\"");
  if (mapping->mmr_segments) printf(",\"MmrReshape\"");
  printf(",\"RpuColorConversion\",\"TargetMapping\"]");
  if (diagnostic_hashes) printf(",\"rgb_sha256\":\"%s\"", rgb_hash);
  printf(",\"creative_trims_applied\":false,\"render_errors\":%u,"
         "\"production_qualified\":false}\n", errors.errors);
}
static void base_gpu_close(void) {
  pl_gpu gpu = base_context.vk->gpu;
  for (int i = 0; i < 4; i++) pl_tex_destroy(gpu, &base_context.textures[i]);
  pl_tex_destroy(gpu, &base_context.output_texture);
  pl_renderer_destroy(&base_context.renderer);
  pl_vulkan_destroy(&base_context.vk);
  pl_log_destroy(&base_context.log);
  free(base_context.rgba);
}
#endif
