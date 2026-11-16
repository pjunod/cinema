// Control-only: the base reconstruction reproduces the composed master, with
// no black remap, for every stream family base_gpu_render serves: Profile 5,
// Profile 8.1, and Profile 7 rendered without its enhancement layer. For each,
// a synthetic 64x64 frame with a true-0 letterbox and dark chroma is rendered
// by the production base_gpu_render (master policy, RGB48 export) and compared
// with the composed master evaluated here in binary64 from the same public
// RPU fields.
//
// The frames carry what real streams carry: an RPU source range, a mastering
// display with P3 primaries and a 0.005-nit minimum, and MaxCLL. Those are
// what libplacebo maps into a frame's colour description; when that
// description reached the render it ran a tone map whose black-point
// compensation lifted the letterbox from code 0 to 988 (0.005 nits) and
// pulled every channel below a pixel's largest toward it.
//
// Needs a Vulkan device (a software one is enough); no decoder, no VA-API.
// build_target_controls.sh builds it; test_base_master_black.py runs it, and
// the image build runs it on the runtime's software device.
#include "../fel_renderer.h"
#include "../base_dv_renderer.h"
#include <libavutil/mastering_display_metadata.h>

enum { SIZE = 64, BAR = 16, PATCH = 16, PATCHES = 4 };
struct family {
  const char *name;
  int profile;
  bool full_range, residual;
  // 10-bit codes of the coded signal: true black, then blue, red and green
  // under 1 nit and a neutral below the 0.005 nits black was lifted to.
  int black[3], patches[PATCHES][3];
  // The RPU's signal-to-RGB matrix in 1/8192, its offsets in 1/2^28, and its
  // linear matrix in 1/16384.
  int ycc_to_rgb[9], ycc_offset[3], rgb_to_lms[9];
};
#define NCL_MATRICES \
  {9575, 0, 14742, 9575, -1645, -5713, 9575, 18818, 0}, \
  {16777216, 134217728, 134217728}, \
  {7222, 8771, 390, 2654, 12430, 1300, 0, 422, 15962}
#define NCL_PICTURE \
  {64, 512, 512}, {{120, 540, 500}, {120, 500, 545}, {130, 495, 495}, {70, 512, 512}}
static const struct family families[] = {
    // Profile 8.1: limited BT.2020 NCL Y'CbCr, identity reshaping.
    {"p8", 8, false, false, NCL_PICTURE, NCL_MATRICES},
    // Profile 7 without its enhancement layer (segment_decode_render's
    // base-only mode): the same signal, residuals declared and not applied.
    {"p7_base_only", 7, false, true, NCL_PICTURE, NCL_MATRICES},
    // Profile 5: full-range IPT-PQ-C2, with the crosstalk matrix.
    {"p5", 5, true, false,
     {0, 512, 512}, {{100, 540, 500}, {100, 490, 540}, {110, 500, 490}, {8, 512, 512}},
     {8192, 799, 1681, 8192, -933, 1091, 8192, 267, -5545},
     {0, 134217728, 134217728},
     {17081, -349, -349, -349, 17081, -349, -349, -349, 17081}},
};

static const int *coded(const struct family *f, int x, int y) {
  return y < BAR || y >= SIZE - BAR ? f->black : f->patches[x / PATCH];
}

static AVFrame *picture(const struct family *f) {
  AVFrame *frame = av_frame_alloc();
  require(frame != NULL, "control frame");
  frame->format = AV_PIX_FMT_YUV420P10LE;
  frame->width = frame->height = SIZE;
  frame->color_range = f->full_range ? AVCOL_RANGE_JPEG : AVCOL_RANGE_MPEG;
  frame->color_primaries = AVCOL_PRI_BT2020;
  frame->color_trc = AVCOL_TRC_SMPTE2084;
  frame->colorspace = f->profile == 5 ? AVCOL_SPC_UNSPECIFIED : AVCOL_SPC_BT2020_NCL;
  require(av_frame_get_buffer(frame, 0) == 0, "control frame planes");
  for (int p = 0; p < 3; p++) {
    int step = p ? 2 : 1;
    for (int y = 0; y < SIZE / step; y++) {
      uint16_t *row = (uint16_t *)(frame->data[p] + (size_t)y * frame->linesize[p]);
      for (int x = 0; x < SIZE / step; x++)
        row[x] = (uint16_t)coded(f, x * step, y * step)[p];
    }
  }
  size_t size;
  AVDOVIMetadata *data = av_dovi_metadata_alloc(&size);
  require(data != NULL, "control RPU metadata");
  AVDOVIRpuDataHeader *h = av_dovi_get_header(data);
  AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  AVDOVIColorMetadata *c = av_dovi_get_color(data);
  h->rpu_type = 2;
  h->rpu_format = 18;
  h->vdr_rpu_profile = f->profile == 5 ? 0 : 1;
  h->coef_log2_denom = 23;
  h->vdr_rpu_normalized_idc = 1;
  h->bl_video_full_range_flag = f->full_range;
  h->bl_bit_depth = 10;
  h->el_bit_depth = 10;
  h->vdr_bit_depth = 12;
  h->disable_residual_flag = !f->residual;
  h->el_spatial_resampling_filter_flag = f->residual;
  m->nlq_method_idc = f->residual ? AV_DOVI_NLQ_LINEAR_DZ : AV_DOVI_NLQ_NONE;
  m->num_x_partitions = m->num_y_partitions = 1;
  for (int comp = 0; comp < 3; comp++) {
    // Identity reshaping.
    m->curves[comp].num_pivots = 2;
    m->curves[comp].pivots[1] = 1023;
    m->curves[comp].mapping_idc[0] = AV_DOVI_MAPPING_POLYNOMIAL;
    m->curves[comp].poly_order[0] = 1;
    m->curves[comp].poly_coef[0][1] = INT64_C(1) << 23;
    c->ycc_to_rgb_offset[comp] = (AVRational){f->ycc_offset[comp], 1 << 28};
    if (f->residual) {
      // A residual the base-only render must leave out.
      m->nlq[comp].nlq_offset = 512;
      m->nlq[comp].vdr_in_max = INT64_C(1) << 20;
      m->nlq[comp].linear_deadzone_slope = INT64_C(1) << 23;
    }
  }
  for (int i = 0; i < 9; i++) {
    c->ycc_to_rgb_matrix[i] = (AVRational){f->ycc_to_rgb[i], 8192};
    c->rgb_to_lms_matrix[i] = (AVRational){f->rgb_to_lms[i], 16384};
  }
  c->signal_eotf = 65535;
  c->signal_bit_depth = 12;
  c->signal_color_space = f->profile == 5 ? 2 : 0;
  c->signal_full_range_flag = 1;
  c->source_min_pq = 62;    // 0.005 nits
  c->source_max_pq = 3079;  // 1000 nits
  AVFrameSideData *side = av_frame_new_side_data(frame, AV_FRAME_DATA_DOVI_METADATA, size);
  require(side != NULL, "control RPU side data");
  memcpy(side->data, data, size);
  av_free(data);
  AVMasteringDisplayMetadata *mastering = av_mastering_display_metadata_create_side_data(frame);
  require(mastering != NULL, "control mastering display");
  const int p3[4][2] = {{34000, 16000}, {13250, 34500}, {7500, 3000}, {15635, 16450}};
  for (int i = 0; i < 3; i++)
    for (int j = 0; j < 2; j++)
      mastering->display_primaries[i][j] = (AVRational){p3[i][j], 50000};
  mastering->white_point[0] = (AVRational){p3[3][0], 50000};
  mastering->white_point[1] = (AVRational){p3[3][1], 50000};
  mastering->has_primaries = 1;
  mastering->min_luminance = (AVRational){50, 10000};
  mastering->max_luminance = (AVRational){1000, 1};
  mastering->has_luminance = 1;
  AVContentLightMetadata *light = av_content_light_metadata_create_side_data(frame);
  require(light != NULL, "control content light");
  light->MaxCLL = 1000;
  light->MaxFALL = 400;
  return frame;
}

static double pq_eotf(double e) {
  const double m1 = 2610.0 / 16384, m2 = 2523.0 / 4096 * 128, c1 = 3424.0 / 4096,
               c2 = 2413.0 / 128, c3 = 2392.0 / 128;
  double p = pow(fmin(1, fmax(0, e)), 1 / m2);
  return pow(fmax(p - c1, 0) / (c2 - c3 * p), 1 / m1);
}

static double pq_oetf(double y) {
  const double m1 = 2610.0 / 16384, m2 = 2523.0 / 4096 * 128, c1 = 3424.0 / 4096,
               c2 = 2413.0 / 128, c3 = 2392.0 / 128;
  double p = pow(fmax(y, 0), m1);
  return pow((c1 + c2 * p) / (1 + c3 * p), m2);
}

// The composed master of one coded sample, as RGB48 codes: the RPU's
// nonlinear matrix on the composed signal less its offsets, PQ to linear
// LMS, the RPU's linear matrix and the BT.2020 LMS-to-RGB matrix libplacebo
// states for Dolby Vision, and PQ again.
static void composed(const struct family *f, const int code[3], double rgb48[3]) {
  static const double lms_to_rgb[3][3] = {{3.06441879, -2.16597676, 0.10155818},
                                          {-0.65612108, 1.78554118, -0.12943749},
                                          {0.01736321, -0.04725154, 1.03004253}};
  double signal[3], lms[3], mapped[3];
  for (int i = 0; i < 3; i++)
    signal[i] = fmin(1023 / 1024.0, code[i] / 1024.0) - f->ycc_offset[i] / 268435456.0;
  for (int i = 0; i < 3; i++) {
    double value = 0;
    for (int j = 0; j < 3; j++)
      value += f->ycc_to_rgb[3 * i + j] / 8192.0 * signal[j];
    lms[i] = pq_eotf(value);
  }
  for (int i = 0; i < 3; i++) {
    mapped[i] = 0;
    for (int j = 0; j < 3; j++)
      mapped[i] += f->rgb_to_lms[3 * i + j] / 16384.0 * lms[j];
  }
  for (int i = 0; i < 3; i++) {
    double value = 0;
    for (int j = 0; j < 3; j++)
      value += lms_to_rgb[i][j] * mapped[j];
    rgb48[i] = 65535 * pq_oetf(value);
  }
}

static uint8_t rendered[SIZE * SIZE * 6];
static void keep(const uint8_t *data, size_t size, void *unused) {
  (void)unused;
  require(size == sizeof(rendered), "control RGB48 master size");
  memcpy(rendered, data, size);
}

static bool reconstructs(const struct family *f, int index) {
  AVFrame *frame = picture(f);
  struct dv_decoded_mapping decoded;
  require(dv_map_decoded_metadata(frame, f->profile, false, &decoded), "control RPU admission");
  require(!decoded.dovi.nlq_active, "base-only mapping applies no residual");
  struct base_mapping mapping = {.dovi = decoded.dovi, .profile = f->profile,
      .polynomial_segments = decoded.polynomial_segments,
      .mmr_segments = decoded.mmr_segments};
  memset(rendered, 0xff, sizeof(rendered));
  base_gpu_render(frame, &mapping, index, "0/1000", "41/1000", keep, false);
  av_frame_free(&frame);
  // Every sample two or more pixels inside its flat region: bilinear chroma
  // taps there read one code, so the expectation needs no resampling model.
  double worst = 0;
  int letterbox = 0;
  for (int y = 0; y < SIZE; y++)
    for (int x = 0; x < SIZE; x++) {
      bool bar = y < BAR || y >= SIZE - BAR;
      int row = y < BAR ? y : y >= SIZE - BAR ? y - (SIZE - BAR) : y - BAR;
      int rows = bar ? BAR : SIZE - 2 * BAR;
      if (row < 2 || row >= rows - 2 || (!bar && (x % PATCH < 2 || x % PATCH >= PATCH - 2)))
        continue;
      double expected[3];
      composed(f, coded(f, x, y), expected);
      for (int ch = 0; ch < 3; ch++) {
        const uint8_t *at = rendered + ((size_t)y * SIZE + x) * 6 + 2 * ch;
        int actual = at[0] | at[1] << 8;
        if (bar) {
          // True black is code 0, exactly: no floor, no flare.
          if (actual != 0) {
            fprintf(stderr, "%s letterbox (%d,%d) channel %d is code %d, composed 0\n",
                    f->name, x, y, ch, actual);
            return false;
          }
          letterbox++;
          continue;
        }
        worst = fmax(worst, fabs(actual - expected[ch]));
        // Two RGB48 codes: what the Profile 7 reconstruction holds to its own
        // composed master on real frames (0.5 of it is the RGB48 rounding).
        if (fabs(actual - expected[ch]) > 2) {
          fprintf(stderr, "%s patch %d (%d,%d) channel %d is code %d, composed %.2f\n",
                  f->name, x / PATCH, x, y, ch, actual, expected[ch]);
          return false;
        }
      }
    }
  require(letterbox == 2 * (BAR - 4) * SIZE * 3, "control letterbox coverage");
  printf("test_base_master_black_%s_letterbox_is_code_zero: PASS\n", f->name);
  printf("test_base_master_black_%s_dark_chroma_is_the_composed_master: PASS (max %.2f RGB48 codes)\n",
         f->name, worst);
  return true;
}

int main(void) {
  base_gpu_init(SIZE, SIZE);
  bool passed = true;
  for (size_t i = 0; i < sizeof(families) / sizeof(*families); i++)
    passed = reconstructs(&families[i], (int)i) && passed;
  base_gpu_close();
  if (!passed)
    return 1;
  puts("test_base_master_black: PASS");
  return 0;
}
