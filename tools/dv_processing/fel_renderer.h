// Actual libdovi parser with synthetic textures: not an encoded HEVC
// association proof.
#include <float.h>
#include <libavutil/mem.h>
#include <libavutil/sha.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "nlq_clipping.h"

#include <libdovi/rpu_parser.h>
#include <libplacebo/filters.h>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/colorspace.h>
#include <libplacebo/shaders/sampling.h>
#include <libplacebo/utils/upload.h>
#include <libplacebo/vulkan.h>

static int WIDTH, HEIGHT, ELWIDTH, ELHEIGHT;
#define FLOAT_COUNT ((size_t)WIDTH * (size_t)HEIGHT * 4)

static void require(bool condition, const char *message) {
  if (!condition) {
    fprintf(stderr, "Probe failed: %s\n", message);
    exit(EXIT_FAILURE);
  }
}

static void save(const char *name, const void *data, size_t length) {
  FILE *file = fopen(name, "wb");
  require(file != NULL, "opening output artifact");
  bool written = fwrite(data, 1, length, file) == length;
  int closed = fclose(file);
  require(written && closed == 0, "writing and closing output artifact");
}

static void probe_log(void *priv, enum pl_log_level level,
                      const char *message) {
  (void)priv;
  fprintf(stderr, "level%d: %s\n", level, message);
}

// Bounded parser-to-shader projection. Only the generated identity P7 controls
// are admitted here; these guards are not a production profile capability.
static void map_parsed_rpu(const uint8_t *bytes, size_t size,
                           struct pl_dovi_metadata *out, bool observations,
                           int picture_width, int picture_height) {
  require(bytes && size > 0 && size <= 65536,
          "bounded fresh decoder-attached RPU bytes");
  DoviRpuOpaque *rpu = dovi_parse_unspec62_nalu(bytes, size);
  require(rpu != NULL, "RPU parser allocation");
  const char *error = dovi_rpu_get_error(rpu);
  if (error) {
    fprintf(stderr, "RPU parser rejected input: %s\n", error);
    dovi_rpu_free(rpu);
    exit(EXIT_FAILURE);
  }
  const DoviRpuDataHeader *header = dovi_rpu_get_header(rpu);
  require(header != NULL, "parsed RPU header");
  require(header->guessed_profile == 7 && header->el_type &&
              strcmp(header->el_type, "FEL") == 0,
          "synthetic P7 FEL profile");
  require(!header->disable_residual_flag && !header->use_prev_vdr_rpu_flag &&
              header->vdr_dm_metadata_present_flag &&
              header->coefficient_data_type == 0 &&
              header->coefficient_log2_denom == 23 &&
              header->bl_bit_depth_minus8 == 2 &&
              header->el_bit_depth_minus8 == 2 &&
              header->vdr_bit_depth_minus8 == 4,
          "bounded fresh P7 control header subset");
  require(header->rpu_type == 2 && header->rpu_format == 18 &&
              header->vdr_rpu_profile == 1 && header->vdr_rpu_level == 0 &&
              header->vdr_seq_info_present_flag &&
              !header->chroma_resampling_explicit_filter_flag &&
              header->vdr_rpu_normalized_idc == 1 &&
              header->reserved_zero_3bits == 0,
          "unsupported normalization/explicit filter header");
  const DoviRpuDataMapping *mapping = dovi_rpu_get_data_mapping(rpu);
  const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(rpu);
  require(!header->bl_video_full_range_flag,
          "unsupported full-range base metadata");
  require(!header->spatial_resampling_filter_flag &&
              header->el_spatial_resampling_filter_flag,
          "unsupported spatial resampling flags");
  require(mapping && mapping->num_x_partitions_minus1 == 0 &&
              mapping->num_y_partitions_minus1 == 0,
          "unsupported mapping partitions");
  require(dm && dm->signal_bit_depth == 12 && dm->signal_color_space == 0 &&
              dm->signal_chroma_format == 0,
          "unsupported reconstructed signal representation");
  require(mapping && dm && mapping->nlq && mapping->nlq_method_idc == 0 &&
              mapping->nlq_num_pivots_minus2 == 0,
          "LINEAR_DZ mapping and DM metadata");
  require(dm->signal_eotf == 65535 && dm->signal_full_range_flag == 1,
          "PQ full-range reconstructed control representation");
  // This master-domain reconstruction does not target a display. L2/L8 display
  // trims remain unapplied and are exported in the original RPU for a later
  // P8.1 display mapper. HDR10 output explicitly reports that limitation.
  require(dm->dm_data.level2.len <= 16 && dm->dm_data.level8.len <= 16,
          "unsupported creative trims");
  // L4 is retained as opaque per-frame display metadata. Its temporal
  // display-mapping semantics are not implemented by this master renderer.
  // L3 offsets describe display-mapping statistics, not master reconstruction.
  // Their original values also travel in the exported, unapplied metadata.
  for (size_t i = 0; i < dm->dm_data.level8.len; i++)
    require(dm->dm_data.level8.list && dm->dm_data.level8.list[i] &&
                dm->dm_data.level8.list[i]->length == 10,
            "unsupported extended L8 trim format");
  require(dm->dm_data.level10.len == 0 && !dm->dm_data.level255,
          "unsupported metadata level");
  if (dm->dm_data.level5)
    require((unsigned)dm->dm_data.level5->active_area_left_offset +
                    dm->dm_data.level5->active_area_right_offset <
                (unsigned)picture_width &&
                (unsigned)dm->dm_data.level5->active_area_top_offset +
                    dm->dm_data.level5->active_area_bottom_offset <
                (unsigned)picture_height,
            "invalid active area metadata");
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
  require(dm->signal_eotf_param0 == 0 && dm->signal_eotf_param1 == 0 &&
              dm->signal_eotf_param2 == 0,
          "unsupported EOTF parameters");
  memset(out, 0, sizeof(*out));
  double denominator = (double)(1ULL << header->coefficient_log2_denom);
  for (int component = 0; component < 3; component++) {
    const DoviReshapingCurve *curve = &mapping->curves[component];
    const DoviPolynomialCurve *poly = curve->polynomial;
    require(curve->mapping_idc == 0 && curve->pivots.len == 2 &&
                curve->pivots.data && curve->pivots.data[0] == 0 &&
                curve->pivots.data[1] == 1023 && poly &&
                poly->poly_order_minus1.len == 1 &&
                poly->poly_order_minus1.data &&
                poly->poly_order_minus1.data[0] == 0 &&
                poly->poly_coef_int.len == 1 && poly->poly_coef.len == 1 &&
                poly->poly_coef_int.list && poly->poly_coef.list &&
                poly->poly_coef_int.list[0] && poly->poly_coef.list[0] &&
                poly->poly_coef_int.list[0]->len == 2 &&
                poly->poly_coef.list[0]->len == 2,
            "single identity polynomial segment");
    const int64_t *integer = poly->poly_coef_int.list[0]->data;
    const uint64_t *fraction = poly->poly_coef.list[0]->data;
    require(integer && fraction && integer[0] == 0 && integer[1] == 1 &&
                fraction[0] == 0 && fraction[1] == 0,
            "identity polynomial coefficient control");
    out->comp[component].num_pivots = 2;
    out->comp[component].pivots[1] = curve->pivots.data[1] / 1023.0f;
    out->comp[component].poly_coeffs[0][1] =
        integer[1] + fraction[1] / denominator;
    const DoviRpuDataNlq *nlq = mapping->nlq;
    require(nlq->nlq_offset[component] <= 1023 &&
                nlq->linear_deadzone_slope_int[component] == 0 &&
                nlq->linear_deadzone_threshold_int[component] == 0 &&
                nlq->linear_deadzone_slope[component] < (1ULL << 23) &&
                nlq->linear_deadzone_threshold[component] < (1ULL << 23),
            "bounded fractional NLQ parameters");
    require(nlq_clip_is_nonbinding(nlq->nlq_offset[component],
                                   nlq->linear_deadzone_slope[component],
                                   nlq->linear_deadzone_threshold[component],
                                   nlq->vdr_in_max_int[component],
                                   nlq->vdr_in_max[component]),
            "unsupported bounded vdr_in_max residual clipping");
    double slope = nlq->linear_deadzone_slope[component];
    double threshold = nlq->linear_deadzone_threshold[component];
    out->nlq[component].offset = nlq->nlq_offset[component] / 1023.0f;
    out->nlq[component].deadzone_slope = 1023.0 * slope / denominator;
    out->nlq[component].deadzone_threshold =
        (threshold - slope / 2) / denominator;
  }
  const int16_t nonlinear[] = {
      dm->ycc_to_rgb_coef0, dm->ycc_to_rgb_coef1, dm->ycc_to_rgb_coef2,
      dm->ycc_to_rgb_coef3, dm->ycc_to_rgb_coef4, dm->ycc_to_rgb_coef5,
      dm->ycc_to_rgb_coef6, dm->ycc_to_rgb_coef7, dm->ycc_to_rgb_coef8,
  };
  const int16_t linear[] = {
      dm->rgb_to_lms_coef0, dm->rgb_to_lms_coef1, dm->rgb_to_lms_coef2,
      dm->rgb_to_lms_coef3, dm->rgb_to_lms_coef4, dm->rgb_to_lms_coef5,
      dm->rgb_to_lms_coef6, dm->rgb_to_lms_coef7, dm->rgb_to_lms_coef8,
  };
  const uint32_t offsets[] = {
      dm->ycc_to_rgb_offset0,
      dm->ycc_to_rgb_offset1,
      dm->ycc_to_rgb_offset2,
  };
  const int16_t expected_nonlinear[] = {9574,  0,    13802, 9574, -1540,
                                        -5348, 9574, 17610, 0};
  const int16_t expected_linear[] = {7222, 8771, 390, 2654, 12430,
                                     1300, 0,    422, 15962};
  const uint32_t expected_offsets[] = {16777216, 134217728, 134217728};
  require(memcmp(nonlinear, expected_nonlinear, sizeof(nonlinear)) == 0 &&
              memcmp(linear, expected_linear, sizeof(linear)) == 0 &&
              memcmp(offsets, expected_offsets, sizeof(offsets)) == 0,
          "unsupported source color matrix/offset subset");
  for (int component = 0; component < 3; component++) {
    out->nonlinear_offset[component] =
        offsets[component] / (double)(1ULL << 28);
    for (int column = 0; column < 3; column++) {
      out->nonlinear.m[component][column] =
          nonlinear[3 * component + column] / 8192.0f;
      out->linear.m[component][column] =
          linear[3 * component + column] / 16384.0f;
    }
  }
  out->nlq_active = true;
  if (observations)
    printf("{\"kind\":\"parsed_rpu\",\"bytes\":%zu,\"profile\":7,\"el_type\":"
           "\"FEL\","
           "\"parser_error\":false,\"bl_depth\":10,\"el_depth\":10,\"vdr_"
           "depth\":12,"
           "\"mapping_segments\":1,\"nlq_method\":\"LINEAR_DZ\",\"creative_l2_"
           "count\":%zu,"
           "\"creative_l8_count\":%zu,\"creative_trims_applied\":false}\n",
           size, dm->dm_data.level2.len, dm->dm_data.level8.len);
  dovi_rpu_free_vdr_dm_data(dm);
  dovi_rpu_free_data_mapping(mapping);
  dovi_rpu_free_header(header);
  dovi_rpu_free(rpu);
}

static void save_rgb48le(const char *name, const float *rgba) {
  uint8_t *output = malloc((size_t)WIDTH * HEIGHT * 6);
  require(output != NULL, "packed RGB allocation");
  for (int pixel = 0; pixel < WIDTH * HEIGHT; pixel++) {
    for (int component = 0; component < 3; component++) {
      float value = fminf(1, fmaxf(0, rgba[4 * pixel + component]));
      uint16_t code = (uint16_t)lrintf(value * 65535);
      size_t offset = (size_t)(3 * pixel + component) * 2;
      output[offset] = (uint8_t)code;
      output[offset + 1] = (uint8_t)(code >> 8);
    }
  }
  save(name, output, (size_t)WIDTH * HEIGHT * 6);
  free(output);
}

static void save_frame(const char *test, const char *stage, const float *rgba) {
  for (size_t index = 0; index < FLOAT_COUNT; index++) {
    require(isfinite(rgba[index]), "finite input/output frame components");
  }
  char path[160];
  int length =
      snprintf(path, sizeof(path), "outputs/%s-%s.rgba32f", test, stage);
  require(length > 0 && (size_t)length < sizeof(path), "float output path");
  save(path, rgba, FLOAT_COUNT * sizeof(float));
  length = snprintf(path, sizeof(path), "outputs/%s-%s.rgb48le", test, stage);
  require(length > 0 && (size_t)length < sizeof(path), "integer output path");
  save_rgb48le(path, rgba);
}

struct segment_gpu {
  pl_log log;
  pl_vulkan vk;
  pl_gpu gpu;
  pl_fmt format, plane_format;
  pl_renderer renderer;
  float *output;
};
static struct segment_gpu context;
static void gpu_init(int width, int height, int el_width, int el_height) {
  require(width >= 64 && height >= 64 && width <= 3840 && height <= 2160 &&
              width % 2 == 0 && height % 2 == 0,
          "supported even raster bound");
  require(el_width >= 32 && el_height >= 32 && el_width % 2 == 0 &&
              el_height % 2 == 0 &&
              ((el_width == width && el_height == height) ||
               (2 * el_width == width && 2 * el_height == height)),
          "unsupported declared enhancement layer ratio");
  WIDTH = width;
  HEIGHT = height;
  ELWIDTH = el_width;
  ELHEIGHT = el_height;
  pl_log log =
      pl_log_create(PL_API_VER, pl_log_params(.log_cb = probe_log,
                                              .log_level = PL_LOG_WARN, ));
  require(log != NULL, "logging context");
  pl_vulkan vk =
      pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
  require(vk != NULL, "software Vulkan device");
  pl_gpu gpu = vk->gpu;
  pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
                              PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE |
                                  PL_FMT_CAP_HOST_READABLE);
  require(format != NULL, "RGBA32F format");
  require(format->type == PL_FMT_FLOAT && !format->opaque &&
              format->num_components == 4 && format->texel_size == 16,
          "host-compatible packed float texture");
  for (int component = 0; component < 4; component++) {
    require(format->host_bits[component] == 32 &&
                format->sample_order[component] == component,
            "RGBA32F component order and precision");
  }
  printf("{\"kind\":\"environment\",\"api\":%d,\"output_format\":\"%s\","
         "\"format_pixel_size\":%zu,\"input_domain\":\"actual paired decoded "
         "native Main10 components\","
         "\"rpu_input\":\"libdovi parsed UNSPEC62 NAL; guarded synthetic "
         "subset\"}\n",
         PL_API_VER, format->name, format->texel_size);

  pl_renderer renderer = pl_renderer_create(log, gpu);
  require(renderer != NULL, "dispatch and renderer");

  context = (struct segment_gpu){
      .log = log, .vk = vk, .gpu = gpu, .format = format, .renderer = renderer};
  context.plane_format =
      pl_find_fmt(gpu, PL_FMT_FLOAT, 1, 32, 32, PL_FMT_CAP_SAMPLEABLE);
  require(context.plane_format && context.plane_format->texel_size == 4,
          "native float plane format");
  context.output = malloc((size_t)FLOAT_COUNT * sizeof(float));
  require(context.output, "bounded raster output buffer");
  printf("{\"kind\":\"gpu_context_created\",\"width\":%d,\"height\":%d}\n",
         WIDTH, HEIGHT);
}
static void gpu_render(uint16_t *decoded[2], const uint8_t *rpu,
                       size_t rpu_size, int frame_index, const char *pts,
                       const char *duration, enum pl_chroma_location bl_chroma,
                       enum pl_chroma_location el_chroma,
                       void (*write_rgb)(const uint8_t *, size_t, void *),
                       void *rgb_output, bool debug) {
  struct pl_dovi_metadata parsed;
  map_parsed_rpu(rpu, rpu_size, &parsed, true, WIDTH, HEIGHT);
  pl_gpu gpu = context.gpu;
  pl_fmt format = context.format;
  pl_renderer renderer = context.renderer;
  float *output = context.output;
  char frame_name[32];
  snprintf(frame_name, sizeof(frame_name), "frame-%03d", frame_index);
  struct pl_dovi_metadata metadata = parsed;
  metadata.nlq_active = true;
  struct pl_frame enhancement_frame = {
      .num_planes = 3,
      .repr = {.sys = PL_COLOR_SYSTEM_RGB, .levels = PL_COLOR_LEVELS_FULL},
      .color = pl_color_space_hdr10};
  struct pl_frame source = {
      .num_planes = 3,
      .repr = {.sys = PL_COLOR_SYSTEM_DOLBYVISION,
               .levels = PL_COLOR_LEVELS_FULL,
               .dovi = &metadata,
               .bits = {.sample_depth = 10, .color_depth = 10}},
      .color = pl_color_space_hdr10,
      .enhancement_layer = &enhancement_frame};
  pl_tex textures[2][3] = {{0}};
  for (int layer = 0; layer < 2; layer++) {
    int width = layer ? ELWIDTH : WIDTH, height = layer ? ELHEIGHT : HEIGHT;
    size_t offset = 0;
    struct pl_frame *frame = layer ? &enhancement_frame : &source;
    for (int c = 0; c < 3; c++) {
      int w = c ? width / 2 : width, h = c ? height / 2 : height;
      size_t count = (size_t)w * h;
      float *plane = malloc(count * sizeof(float));
      require(plane != NULL, "native plane staging buffer");
      for (size_t i = 0; i < count; i++) {
        require(decoded[layer][offset + i] <= 1023,
                "native10-bit sample range");
        plane[i] = decoded[layer][offset + i] / 1023.0f;
      }
      textures[layer][c] = pl_tex_create(
          gpu, pl_tex_params(.w = w, .h = h, .format = context.plane_format,
                             .sampleable = true, .initial_data = plane));
      free(plane);
      require(textures[layer][c] != NULL, "native plane texture");
      frame->planes[c] = (struct pl_plane){
          .texture = textures[layer][c],
          .components = 1,
          .component_mapping = {c, PL_CHANNEL_NONE, PL_CHANNEL_NONE,
                                PL_CHANNEL_NONE}};
      offset += count;
    }
  }
  pl_frame_set_chroma_location(&source, bl_chroma);
  pl_frame_set_chroma_location(&enhancement_frame, el_chroma);
  // Native point resampling has exact texel-edge ties (e.g. odd BL x on
  // half-sized left-sited EL). At non-power-of-two rasters, interpolation of
  // normalized float coordinates can approach that edge from either side.
  // Resolve ties toward the positive texel with a fixed 1/64 reference-pixel
  // bias. Our admitted ratios are 1, 1/2 and 1/4; every non-tie sample is at
  // least 1/8 texel from an edge, so this does not change its nearest texel.
  // This explicit point policy is backend mechanics, not a Dolby filter claim.
  for (int c = 0; c < 3; c++) {
    source.planes[c].shift_x -= 1.0f / 64;
    source.planes[c].shift_y -= 1.0f / 64;
    enhancement_frame.planes[c].shift_x -= 1.0f / 64;
    enhancement_frame.planes[c].shift_y -= 1.0f / 64;
  }

  pl_tex output_texture = pl_tex_create(
      gpu, pl_tex_params(.w = WIDTH, .h = HEIGHT, .format = format,
                         .renderable = true, .host_readable = true));
  require(output_texture != NULL, "output float texture");
  struct pl_frame destination = {.num_planes = 1,
                                 .planes = {{.texture = output_texture,
                                             .components = 3,
                                             .component_mapping = {0, 1, 2}}},
                                 .repr = {.sys = PL_COLOR_SYSTEM_RGB,
                                          .levels = PL_COLOR_LEVELS_FULL,
                                          .bits = {.color_depth = 32}},
                                 .color = pl_color_space_hdr10};
  source.color.hdr.max_luma = destination.color.hdr.max_luma = 10000;
  source.color.hdr.min_luma = destination.color.hdr.min_luma = 0.005;
  struct pl_color_map_params color_map = pl_color_map_default_params;
  color_map.tone_mapping_function = &pl_tone_map_clip;
  color_map.gamut_mapping = &pl_gamut_map_clip;
  color_map.metadata = PL_HDR_METADATA_NONE;
  struct pl_render_params parameters = pl_render_fast_params;
  parameters.color_map_params = &color_map;
  parameters.plane_upscaler = &pl_filter_nearest;
  parameters.plane_downscaler = &pl_filter_nearest;
  parameters.dither_params = NULL;
  parameters.peak_detect_params = NULL;
  parameters.color_adjustment = NULL;
  parameters.sigmoid_params = NULL;
  pl_renderer_reset_errors(renderer, NULL);
  require(pl_render_image(renderer, &source, &destination, &parameters),
          "full renderer export");
  struct pl_render_errors errors = pl_renderer_get_errors(renderer);
  require(errors.errors == 0 && errors.num_disabled_hooks == 0,
          "renderer error-free export");
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output_texture,
                                                      .ptr = output, )),
          "full renderer frame download");
  if (debug) {
    require(WIDTH == 64 && HEIGHT == 64, "debug evidence restricted to64x64");
    save_frame(frame_name, "rendered", output);
  }
  uint8_t *packed = malloc((size_t)WIDTH * HEIGHT * 6);
  require(packed != NULL, "RGB output allocation");
  for (size_t pixel = 0; pixel < (size_t)WIDTH * HEIGHT; pixel++) {
    for (int c = 0; c < 4; c++)
      require(isfinite(output[4 * pixel + c]), "finite rendered RGBA");
    for (int c = 0; c < 3; c++) {
      uint16_t code =
          (uint16_t)lrintf(fminf(1, fmaxf(0, output[4 * pixel + c])) * 65535);
      packed[6 * pixel + 2 * c] = (uint8_t)code;
      packed[6 * pixel + 2 * c + 1] = (uint8_t)(code >> 8);
    }
  }
  write_rgb(packed, (size_t)WIDTH * HEIGHT * 6, rgb_output);
  struct AVSHA *sha = av_sha_alloc();
  uint8_t digest[32];
  char rgb_hash[65];
  require(sha && av_sha_init(sha, 256) == 0, "rendered RGB SHA256");
  av_sha_update(sha, packed, (size_t)WIDTH * HEIGHT * 6);
  av_sha_final(sha, digest);
  av_free(sha);
  for (int i = 0; i < 32; i++)
    snprintf(rgb_hash + 2 * i, 3, "%02x", digest[i]);

  free(packed);
  printf("{\"kind\":\"rendered_frame\",\"frame\":%d,\"width\":%d,\"height\":%d,"
         "\"pts\":\"%s\",\"duration\":\"%s\",\"el_bound\":true,\"nlq_active\":"
         "true,\"rgb_sha256\":\"%s\",\"render_errors\":%u,\"production_"
         "qualified\":false}\n",
         frame_index, WIDTH, HEIGHT, pts, duration, rgb_hash, errors.errors);
  for (int layer = 0; layer < 2; layer++)
    for (int c = 0; c < 3; c++)
      pl_tex_destroy(gpu, &textures[layer][c]);
  pl_tex_destroy(gpu, &output_texture);
}
static void gpu_close(void) {
  pl_renderer_destroy(&context.renderer);
  pl_vulkan_destroy(&context.vk);
  pl_log_destroy(&context.log);
  free(context.output);
}
