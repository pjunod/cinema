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
#include <unistd.h>

#include "nlq_clipping.h"
#include "dv_metadata.h"
#include "dv_trace.h"
#include "rgb48.h"
#include "yuv420p10.h"

#include <libdovi/rpu_parser.h>
#include <libplacebo/filters.h>
#include <libplacebo/renderer.h>
#include <libplacebo/shaders/colorspace.h>
#include <libplacebo/shaders/sampling.h>
#include <libplacebo/utils/upload.h>
#include <libplacebo/utils/libav.h>
#include <libplacebo/vulkan.h>

static int WIDTH, HEIGHT, ELWIDTH, ELHEIGHT;
static int EXPORT_WIDTH, EXPORT_HEIGHT;
// pq-bilinear-yuv420p10-v1: the export leaves the GPU as BT.2020 NCL limited 4:2:0 10-bit.
static bool EXPORT_YUV10;
#define FLOAT_COUNT ((size_t)WIDTH * (size_t)HEIGHT * 4)
#define EXPORT_FLOAT_COUNT ((size_t)EXPORT_WIDTH * (size_t)EXPORT_HEIGHT * 4)

// Once the export writer thread runs, a refusal on either thread leaves
// without exit-time teardown: the other thread may be inside Vulkan or
// mid-write, and loader and driver destructors racing it can hang or crash
// instead of reporting the refusal.
static bool dv_threads_live;
static void dv_exit(int code) {
  if (!dv_threads_live)
    exit(code);
  fflush(stdout);
  fflush(stderr);
  _exit(code);
}
static void require(bool condition, const char *message) {
  if (!condition) {
    fprintf(stderr, "Probe failed: %s\n", message);
    dv_exit(EXIT_FAILURE);
  }
}

#include "display_target.h"

// Resample only the already reconstructed and master-clipped native RGB/PQ
// texture. This separate pass cannot resize BL/EL before their DV operations.
static void export_native_rgb(pl_renderer renderer, struct pl_frame native,
                              pl_tex output) {
  if (dv_target.sdr) {
    require(dv_yuv10_convert_sdr_rgb(renderer, native, output), "BT.709 gamma2.4 export");
    return;
  }
  struct pl_frame destination = native;
  destination.planes[0].texture = output;
  struct pl_render_params params = pl_render_fast_params;
  params.upscaler = params.downscaler = &pl_filter_bilinear;
  params.skip_anti_aliasing = true; // four-tap bilinear on native RGB, not DV
  params.disable_linear_scaling = true;
  params.dither_params = NULL;
  params.peak_detect_params = NULL;
  params.color_adjustment = NULL;
  params.sigmoid_params = NULL;
  require(pl_render_image(renderer, &native, &destination, &params),
          "native RGB/PQ bilinear export");
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

// Actual frames use the shared public decoder metadata mapper. The no-frame
// branch retains the original identity-only synthetic control projection.
static void map_fel_rpu(const AVFrame *frame, const uint8_t *bytes, size_t size,
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
    dv_exit(EXIT_FAILURE);
  }
  const DoviRpuDataHeader *header = dovi_rpu_get_header(rpu);
  require(header != NULL, "parsed RPU header");
  require(header->guessed_profile == 7 && header->el_type &&
              strcmp(header->el_type, "FEL") == 0,
          frame ? "fresh P7 FEL profile" : "synthetic P7 FEL profile");
  require(!header->disable_residual_flag && !header->use_prev_vdr_rpu_flag &&
              header->vdr_dm_metadata_present_flag &&
              header->coefficient_data_type == 0 &&
              header->coefficient_log2_denom == 23 &&
              header->bl_bit_depth_minus8 == 2 &&
              header->el_bit_depth_minus8 == 2 &&
              header->vdr_bit_depth_minus8 == 4,
          frame ? "bounded fresh P7 header subset" : "bounded fresh P7 control header subset");
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
  dv_target_metadata(dm);
  // Without a display target this master-domain reconstruction maps no display:
  // L2/L8 trims stay unapplied and are exported in the original RPU for a later
  // P8.1 display mapper. With a target, display_target.h applies them.
  // Display metadata (L2/L8 trims, L3/L4, L9 primaries, L10, L11, L254,
  // L255) is carried unapplied by this master reconstruction and exported in
  // the original RPU: any version is accepted.
  if (dm->dm_data.level5)
    require((unsigned)dm->dm_data.level5->active_area_left_offset +
                    dm->dm_data.level5->active_area_right_offset <
                (unsigned)picture_width &&
                (unsigned)dm->dm_data.level5->active_area_top_offset +
                    dm->dm_data.level5->active_area_bottom_offset <
                (unsigned)picture_height,
            "invalid active area metadata");
  require(dm->signal_eotf_param0 == 0 && dm->signal_eotf_param1 == 0 &&
              dm->signal_eotf_param2 == 0,
          "unsupported EOTF parameters");
  struct dv_decoded_mapping decoded = {0};
  memset(out, 0, sizeof(*out));
  double denominator = (double)(1ULL << header->coefficient_log2_denom);
  for (int component = 0; component < 3; component++) {
    if (!frame) {
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
    }
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
    if (!frame) {
      double slope = nlq->linear_deadzone_slope[component];
      double threshold = nlq->linear_deadzone_threshold[component];
      out->nlq[component].offset = nlq->nlq_offset[component] / 1023.0f;
      out->nlq[component].deadzone_slope = 1023.0 * slope / denominator;
      out->nlq[component].deadzone_threshold =
          (threshold - slope / 2) / denominator;
    }
  }
  if (frame) {
    require(dv_map_decoded_metadata(frame, 7, true, &decoded),
            "bounded decoder-derived FEL metadata mapping");
    *out = decoded.dovi;
    for (int component = 0; component < 3; component++) {
      const DoviRpuDataNlq *nlq = mapping->nlq;
      const AVDOVINLQParams *actual =
          &av_dovi_get_mapping(decoded.source)->nlq[component];
      require(actual->nlq_offset == nlq->nlq_offset[component] &&
                  actual->linear_deadzone_slope == nlq->linear_deadzone_slope[component] &&
                  actual->linear_deadzone_threshold == nlq->linear_deadzone_threshold[component] &&
                  actual->vdr_in_max == (nlq->vdr_in_max_int[component] << 23) +
                                            nlq->vdr_in_max[component],
              "raw and decoded FEL NLQ metadata agree");
    }
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
  if (frame) {
    const AVDOVIColorMetadata *color = av_dovi_get_color(decoded.source);
    for (int i = 0; i < 3; i++) {
      AVRational offset = color->ycc_to_rgb_offset[i];
      require((int64_t)offset.num * (INT64_C(1) << 28) ==
                  (int64_t)offsets[i] * offset.den,
              "raw and decoded FEL color offsets agree");
      for (int j = 0; j < 3; j++) {
        AVRational n = color->ycc_to_rgb_matrix[3 * i + j];
        AVRational l = color->rgb_to_lms_matrix[3 * i + j];
        require((int64_t)n.num * 8192 == (int64_t)nonlinear[3 * i + j] * n.den &&
                    (int64_t)l.num * 16384 == (int64_t)linear[3 * i + j] * l.den,
                "raw and decoded FEL color matrices agree");
      }
    }
  } else {
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
  }
  out->nlq_active = true;
  if (observations && frame)
    printf("{\"kind\":\"parsed_rpu\",\"bytes\":%zu,\"profile\":7,\"el_type\":\"FEL\","
           "\"parser_error\":false,\"bl_depth\":10,\"el_depth\":10,\"vdr_depth\":12,"
           "\"mapping_source\":\"validated_decoder_metadata\",\"mapping_segments\":%d,"
           "\"polynomial_segments\":%d,\"mmr_segments\":%d,\"nlq_method\":\"LINEAR_DZ\","
           "\"creative_l2_count\":%zu,\"creative_l8_count\":%zu,\"creative_trims_applied\":%s}\n",
           size, decoded.polynomial_segments + decoded.mmr_segments,
           decoded.polynomial_segments, decoded.mmr_segments,
           dm->dm_data.level2.len, dm->dm_data.level8.len, dv_target_trims_applied() ? "true" : "false");
  if (observations && !frame)
    printf("{\"kind\":\"parsed_rpu\",\"bytes\":%zu,\"profile\":7,\"el_type\":"
           "\"FEL\","
           "\"parser_error\":false,\"bl_depth\":10,\"el_depth\":10,\"vdr_"
           "depth\":12,"
           "\"mapping_segments\":1,\"nlq_method\":\"LINEAR_DZ\",\"creative_l2_"
           "count\":%zu,"
           "\"creative_l8_count\":%zu,\"creative_trims_applied\":%s}\n",
           size, dm->dm_data.level2.len, dm->dm_data.level8.len, dv_target_trims_applied() ? "true" : "false");
  dovi_rpu_free_vdr_dm_data(dm);
  dovi_rpu_free_data_mapping(mapping);
  dovi_rpu_free_header(header);
  dovi_rpu_free(rpu);
}
static inline void map_parsed_rpu(const uint8_t *bytes, size_t size,
                                   struct pl_dovi_metadata *out, bool observations,
                                   int width, int height) {
  map_fel_rpu(NULL, bytes, size, out, observations, width, height);
}

static void save_rgb48le(const char *name, const float *rgba) {
  uint8_t *output = malloc((size_t)WIDTH * HEIGHT * 6);
  require(output != NULL, "packed RGB allocation");
  for (int pixel = 0; pixel < WIDTH * HEIGHT; pixel++) {
    for (int component = 0; component < 3; component++) {
      uint16_t code = dv_pack_finite_rgb48(rgba[4 * pixel + component]);
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
  pl_tex textures[2][3], output_texture, native_texture;
  pl_tex mapped[2][4];
  float *output, *staging;
};
static struct segment_gpu context;
static void gpu_init(int width, int height, int el_width, int el_height) {
  require(width >= 64 && height >= 64 && width <= 4096 && height <= 2160 &&
              width % 2 == 0 && height % 2 == 0,
          "supported even raster bound");
  require(el_width >= 32 && el_height >= 32 && el_width % 2 == 0 &&
              el_height % 2 == 0 &&
              ((el_width == width && el_height == height) ||
               (2 * el_width == width && 2 * el_height == height)),
          "unsupported declared enhancement layer ratio");
  WIDTH = width;
  HEIGHT = height;
  if (!EXPORT_WIDTH && !EXPORT_HEIGHT) {
    EXPORT_WIDTH = width;
    EXPORT_HEIGHT = height;
  }
  require(EXPORT_WIDTH > 0 && EXPORT_HEIGHT > 0 &&
              EXPORT_WIDTH <= width && EXPORT_HEIGHT <= height,
          "bounded explicit export raster");
  ELWIDTH = el_width;
  ELHEIGHT = el_height;
  pl_log log =
      pl_log_create(PL_API_VER, pl_log_params(.log_cb = probe_log,
                                              .log_level = PL_LOG_WARN, ));
  require(log != NULL, "logging context");
  pl_vulkan vk =
      pl_vulkan_create(log, pl_vulkan_params(.allow_software = true));
  require(vk != NULL, "Vulkan device");
  require(dv_gpu_runtime(vk), "actual Vulkan runtime identity");
  pl_gpu gpu = vk->gpu;
  pl_fmt format = pl_find_fmt(gpu, PL_FMT_FLOAT, 4, 32, 32,
                              PL_FMT_CAP_RENDERABLE | PL_FMT_CAP_SAMPLEABLE |
                                  PL_FMT_CAP_HOST_READABLE |
                                  ((EXPORT_WIDTH != width || EXPORT_HEIGHT != height) ?
                                   PL_FMT_CAP_LINEAR : 0));
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
  context.output = malloc(EXPORT_FLOAT_COUNT * sizeof(float));
  require(context.output, "bounded raster output buffer");
  context.staging = malloc((size_t)WIDTH * HEIGHT * sizeof(float));
  require(context.staging != NULL, "bounded reusable native plane staging");
  for (int layer = 0; layer < 2; layer++) {
    for (int c = 0; c < 3; c++) {
      int w = layer ? ELWIDTH : WIDTH, h = layer ? ELHEIGHT : HEIGHT;
      if (c) { w /= 2; h /= 2; }
      context.textures[layer][c] = pl_tex_create(gpu,
          pl_tex_params(.w = w, .h = h, .format = context.plane_format,
                        .sampleable = true, .host_writable = true));
      require(context.textures[layer][c] != NULL, "persistent native plane texture");
    }
  }
  context.output_texture = EXPORT_YUV10 ? dv_yuv10_source_texture(gpu, EXPORT_WIDTH, EXPORT_HEIGHT) : NULL;
  if (!context.output_texture)
    context.output_texture = pl_tex_create(gpu,
        pl_tex_params(.w = EXPORT_WIDTH, .h = EXPORT_HEIGHT, .format = format,
                      .renderable = true, .host_readable = true,
                      .sampleable = EXPORT_YUV10));
  require(context.output_texture != NULL, "persistent export float texture");
  require(!EXPORT_YUV10 || dv_yuv10_init(log, gpu, EXPORT_WIDTH, EXPORT_HEIGHT),
          "GPU yuv420p10 export planes");
  if (dv_target.nits || EXPORT_WIDTH != WIDTH || EXPORT_HEIGHT != HEIGHT) {
    context.native_texture = pl_tex_create(gpu,
        pl_tex_params(.w = WIDTH, .h = HEIGHT, .format = format,
                      .renderable = true, .sampleable = true));
    require(context.native_texture != NULL, "persistent native reconstructed float texture");
  }
  printf("{\"kind\":\"gpu_context_created\",\"width\":%d,\"height\":%d}\n",
         WIDTH, HEIGHT);
}
static struct pl_frame fel_mapped[2];
static int fel_mapped_live;
// Release mapped decoder surfaces once the frame's GPU work is complete.
static void fel_unmap(pl_gpu gpu) {
  for (int layer = 0; layer < fel_mapped_live; layer++)
    pl_unmap_avframe(gpu, &fel_mapped[layer]);
  fel_mapped_live = 0;
}
// `hardware` holds the two decoder surfaces when the layers were decoded on
// the GPU; they are mapped in place (no host round trip) instead of uploaded.
static void gpu_render_mapped(uint16_t *decoded[2], const AVFrame *hardware[2],
                       const uint8_t *rpu,
                       size_t rpu_size, int frame_index, const char *pts,
                       const char *duration, enum pl_chroma_location bl_chroma,
                       enum pl_chroma_location el_chroma,
                       void (*write_rgb)(const uint8_t *, size_t, void *),
                       void *rgb_output, bool debug,
                       const struct pl_dovi_metadata *validated) {
  struct pl_dovi_metadata parsed;
  if (validated)
    parsed = *validated;
  else
    map_parsed_rpu(rpu, rpu_size, &parsed, true, WIDTH, HEIGHT);
  pl_gpu gpu = context.gpu;
  pl_renderer renderer = context.renderer;
  float *output = context.output;
  char frame_name[32];
  snprintf(frame_name, sizeof(frame_name), "frame-%03d", frame_index);
  struct pl_dovi_metadata metadata = parsed;
  metadata.nlq_active = true;
  int polynomial_segments = 0, mmr_segments = 0;
  for (int comp = 0; comp < 3; comp++)
    for (int piece = 0; piece < metadata.comp[comp].num_pivots - 1; piece++) {
      if (metadata.comp[comp].method[piece] == AV_DOVI_MAPPING_POLYNOMIAL)
        polynomial_segments++;
      else
        mmr_segments++;
    }
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
  if (hardware) {
    // Map each decoded surface, then restate the representation the native
    // path uses: DV-coded full-range BL with the parsed metadata, and raw EL
    // residual components. The mapped sample/colour depth (10 in 16) keeps
    // the same code/1023 normalization as the host upload.
    bool mapped = true;
    for (int layer = 0; layer < 2 && mapped; layer++) {
      struct pl_frame *live = &fel_mapped[layer];
      if (!pl_map_avframe_ex(gpu, live, pl_avframe_params(
              .frame = hardware[layer], .tex = context.mapped[layer]))) {
        mapped = false;
        break;
      }
      fel_mapped_live = layer + 1;
      struct pl_frame *frame = layer ? &enhancement_frame : &source;
      frame->num_planes = live->num_planes;
      for (int p = 0; p < live->num_planes; p++)
        frame->planes[p] = live->planes[p];
      frame->repr.bits = live->repr.bits;
    }
    if (!mapped) {
      // This Vulkan device cannot import the decoder's surfaces: copy them
      // to host memory and take the ordinary upload path from now on.
      fel_unmap(gpu);
      dv_hw_host_copy = true;
      source.num_planes = enhancement_frame.num_planes = 3;
      source.repr.bits = (struct pl_bit_encoding){.sample_depth = 10, .color_depth = 10};
      enhancement_frame.repr.bits = (struct pl_bit_encoding){0};
      require(dv_host_pack != NULL, "decoded surface host copy");
      // The caller owns and frees decoded[].
      for (int layer = 0; layer < 2; layer++)
        if (!decoded[layer])
          decoded[layer] = dv_host_pack(hardware[layer]);
      hardware = NULL;
    }
  }
  if (!hardware)
  for (int layer = 0; layer < 2; layer++) {
    int width = layer ? ELWIDTH : WIDTH, height = layer ? ELHEIGHT : HEIGHT;
    size_t offset = 0;
    struct pl_frame *frame = layer ? &enhancement_frame : &source;
    for (int c = 0; c < 3; c++) {
      int w = c ? width / 2 : width, h = c ? height / 2 : height;
      size_t count = (size_t)w * h;
      float *plane = context.staging;
      require(dv_unpack_10bit_plane(decoded[layer] + offset, count, plane),
              "native10-bit sample range");
      require(pl_tex_upload(gpu, pl_tex_transfer_params(
          .tex = context.textures[layer][c], .ptr = plane)), "native plane upload");
      frame->planes[c] = (struct pl_plane){
          .texture = context.textures[layer][c],
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
  for (int c = 0; c < source.num_planes; c++) {
    source.planes[c].shift_x -= 1.0f / 64;
    source.planes[c].shift_y -= 1.0f / 64;
  }
  for (int c = 0; c < enhancement_frame.num_planes; c++) {
    enhancement_frame.planes[c].shift_x -= 1.0f / 64;
    enhancement_frame.planes[c].shift_y -= 1.0f / 64;
  }

  pl_tex output_texture = context.output_texture;
  bool resized = EXPORT_WIDTH != WIDTH || EXPORT_HEIGHT != HEIGHT;
  pl_tex native_texture = (resized || dv_target.nits) ? context.native_texture : output_texture;
  struct pl_frame destination = {.num_planes = 1,
                                 .planes = {{.texture = native_texture,
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
  if (dv_target.nits)
    destination.planes[0].texture = dv_target_render(context.log, gpu, native_texture, frame_index);
  if (resized || dv_target.nits) export_native_rgb(renderer, destination, output_texture);
  struct pl_render_errors errors = pl_renderer_get_errors(renderer);
  require(errors.errors == 0 && errors.num_disabled_hooks == 0,
          "renderer error-free export");
  if (EXPORT_YUV10) {
    require(!debug, "debug evidence is RGB48 only");
    require(dv_yuv10_export_color(renderer, gpu, output_texture, dv_target.sdr), "GPU yuv420p10 export");
    fel_unmap(gpu);
    write_rgb(dv_yuv10_context.frame, dv_yuv10_frame_bytes(), rgb_output);
    char yuv10_field[86] = "";
    if (dv_frame_hashes()) {
      dv_yuv10_wait();
      struct AVSHA *sha = av_sha_alloc();
      uint8_t digest[32];
      char hash[65];
      require(sha && av_sha_init(sha, 256) == 0, "exported yuv420p10 SHA256");
      av_sha_update(sha, dv_yuv10_context.frame, dv_yuv10_frame_bytes());
      av_sha_final(sha, digest);
      av_free(sha);
      for (int i = 0; i < 32; i++)
        snprintf(hash + 2 * i, 3, "%02x", digest[i]);
      snprintf(yuv10_field, sizeof(yuv10_field), ",\"yuv10_sha256\":\"%s\"", hash);
    }
    printf("{\"kind\":\"rendered_frame\",\"frame\":%d,\"width\":%d,\"height\":%d,"
           "\"pts\":\"%s\",\"duration\":\"%s\",\"el_bound\":true,\"nlq_active\":"
           "true%s,\"render_errors\":%u,\"polynomial_segments\":%d,\"mmr_segments\":%d,\"production_qualified\":false}\n",
           frame_index, EXPORT_WIDTH, EXPORT_HEIGHT, pts, duration, yuv10_field,
           errors.errors, polynomial_segments, mmr_segments);
    return;
  }
  require(pl_tex_download(gpu, pl_tex_transfer_params(.tex = output_texture,
                                                      .ptr = output, )),
          "full renderer frame download");
  fel_unmap(gpu);
  if (debug) {
    require(WIDTH == 64 && HEIGHT == 64 && EXPORT_WIDTH == WIDTH &&
                EXPORT_HEIGHT == HEIGHT, "debug evidence restricted to64x64");
    save_frame(frame_name, "rendered", output);
  }
  uint8_t *packed = malloc(EXPORT_FLOAT_COUNT / 4 * 6);
  require(packed != NULL, "RGB output allocation");
  require(dv_pack_rgba_rgb48(output, EXPORT_FLOAT_COUNT / 4, packed,
                            EXPORT_FLOAT_COUNT / 4 * 6),
          "finite rendered RGBA");
  write_rgb(packed, EXPORT_FLOAT_COUNT / 4 * 6, rgb_output);
  char rgb_field[84] = "";
  if (debug || dv_frame_hashes()) {
    struct AVSHA *sha = av_sha_alloc();
    uint8_t digest[32];
    char rgb_hash[65];
    require(sha && av_sha_init(sha, 256) == 0, "rendered RGB SHA256");
    av_sha_update(sha, packed, EXPORT_FLOAT_COUNT / 4 * 6);
    av_sha_final(sha, digest);
    av_free(sha);
    for (int i = 0; i < 32; i++)
      snprintf(rgb_hash + 2 * i, 3, "%02x", digest[i]);
    snprintf(rgb_field, sizeof(rgb_field), ",\"rgb_sha256\":\"%s\"", rgb_hash);
  }
  free(packed);
  printf("{\"kind\":\"rendered_frame\",\"frame\":%d,\"width\":%d,\"height\":%d,"
         "\"pts\":\"%s\",\"duration\":\"%s\",\"el_bound\":true,\"nlq_active\":"
         "true%s,\"render_errors\":%u,\"polynomial_segments\":%d,\"mmr_segments\":%d,\"production_qualified\":false}\n",
         frame_index, EXPORT_WIDTH, EXPORT_HEIGHT, pts, duration, rgb_field, errors.errors, polynomial_segments, mmr_segments);
}
// Compatibility entry point for direct synthetic textures without AVFrames.
static inline void gpu_render(uint16_t *decoded[2], const AVFrame *hardware[2],
                       const uint8_t *rpu, size_t rpu_size, int frame_index,
                       const char *pts, const char *duration,
                       enum pl_chroma_location bl_chroma,
                       enum pl_chroma_location el_chroma,
                       void (*write_rgb)(const uint8_t *, size_t, void *),
                       void *rgb_output, bool debug) {
  gpu_render_mapped(decoded, hardware, rpu, rpu_size, frame_index, pts, duration,
                    bl_chroma, el_chroma, write_rgb, rgb_output, debug, NULL);
}
static void gpu_close(void) {
  dv_target_destroy(context.gpu);
  for (int layer = 0; layer < 2; layer++) {
    for (int c = 0; c < 3; c++)
      pl_tex_destroy(context.gpu, &context.textures[layer][c]);
    for (int c = 0; c < 4; c++)
      pl_tex_destroy(context.gpu, &context.mapped[layer][c]);
  }
  pl_tex_destroy(context.gpu, &context.output_texture);
  pl_tex_destroy(context.gpu, &context.native_texture);
  if (EXPORT_YUV10) dv_yuv10_close(context.gpu);
  pl_renderer_destroy(&context.renderer);
  pl_vulkan_destroy(&context.vk);
  pl_log_destroy(&context.log);
  free(context.output);
  free(context.staging);
}
