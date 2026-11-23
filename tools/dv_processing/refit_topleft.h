// SPDX-License-Identifier: Apache-2.0
// Diagnostic CPU packing for an admitted composed FEL window. The pipeline
// is intentionally separate from the native libplacebo export.
#ifndef PLURX_REFIT_TOPLEFT_H
#define PLURX_REFIT_TOPLEFT_H

#define REFIT_TOPLEFT_FORMAT "plurx-composed-model-b-topleft-v1"
#define REFIT_TOPLEFT_FRAMES 48

static inline bool refit_topleft_color(const AVDOVIColorMetadata *color) {
  // This version is bound to the calibrated Michael source's PQ RGB
  // interpretation. An arbitrary Dolby matrix need not describe BT.2020 RGB.
  static const int matrix[9] = {9574, 0, 13802, 9574, -1540, -5348, 9574, 17610, 0};
  static const int offset[3] = {16777216, 134217728, 134217728};
  static const int lms[9] = {7222, 8771, 390, 2654, 12430, 1300, 0, 422, 15962};
  if (color->signal_eotf != UINT16_MAX || color->signal_eotf_param0 != 0 ||
      color->signal_eotf_param1 != 0 || color->signal_eotf_param2 != 0 ||
      color->signal_bit_depth != 12 || color->signal_color_space != 0 ||
      color->signal_chroma_format != 0 || color->signal_full_range_flag != 1)
    return false;
  for (int i = 0; i < 9; i++) {
    if (color->ycc_to_rgb_matrix[i].den != 8192 ||
        color->ycc_to_rgb_matrix[i].num != matrix[i] ||
        color->rgb_to_lms_matrix[i].den != 16384 ||
        color->rgb_to_lms_matrix[i].num != lms[i])
      return false;
  }
  for (int i = 0; i < 3; i++) {
    if (color->ycc_to_rgb_offset[i].den != (1 << 28) ||
        color->ycc_to_rgb_offset[i].num != offset[i])
      return false;
  }
  return true;
}

static inline bool refit_topleft_mapping(const AVDOVIMetadata *data) {
  const AVDOVIRpuDataHeader *header = av_dovi_get_header(data);
  const AVDOVIDataMapping *mapping = av_dovi_get_mapping(data);
  if (header->disable_residual_flag || mapping->mapping_color_space != 0 ||
      mapping->mapping_chroma_format_idc != 0 || mapping->num_x_partitions != 1 ||
      mapping->num_y_partitions != 1)
    return false;
  for (int i = 0; i < 3; i++) {
    const AVDOVIReshapingCurve *curve = &mapping->curves[i];
    const AVDOVINLQParams *nlq = &mapping->nlq[i];
    if (curve->num_pivots != 2 || curve->pivots[0] != 0 || curve->pivots[1] != 1023 ||
        curve->mapping_idc[0] != AV_DOVI_MAPPING_POLYNOMIAL ||
        curve->poly_order[0] != 1 || curve->poly_coef[0][0] != 0 ||
        curve->poly_coef[0][1] != 8388608 || curve->poly_coef[0][2] != 0 ||
        nlq->nlq_offset != 512 || nlq->vdr_in_max != 1048576 ||
        nlq->linear_deadzone_slope != 2048 || nlq->linear_deadzone_threshold != 0)
      return false;
  }
  return true;
}

// The frozen Model B arithmetic, including its representational truncation.
static inline void refit_topleft_rgb(const AVDOVIColorMetadata *color,
                                     const uint16_t composed[3], uint16_t rgb[3]) {
  int64_t signal[3];
  for (int i = 0; i < 3; i++)
    signal[i] = (int64_t)composed[i] * 16 - color->ycc_to_rgb_offset[i].num / 4096;
  for (int c = 0; c < 3; c++) {
    int64_t raw = 0;
    for (int i = 0; i < 3; i++)
      raw += signal[i] * color->ycc_to_rgb_matrix[c * 3 + i].num;
    // Arithmetic right shift (floor), without relying on implementation-defined signed shifts.
    int64_t code = raw / 8192 - (raw % 8192 < 0);
    rgb[c] = code < 0 ? 0 : code > UINT16_MAX ? UINT16_MAX : (uint16_t)code;
  }
}

// Limited-range BT.2020 NCL on PQ code values. With normalized R'G'B',
// Y' = (2627 R' + 6780 G' + 593 B') / 10000, and the chroma divisors
// are 1.8814 and 1.4746. Every division rounds to nearest, ties upward.
static inline uint16_t refit_topleft_round(int64_t num, int64_t den) {
  return (uint16_t)((num + den / 2) / den);
}

static inline uint16_t refit_topleft_luma(const uint16_t rgb[3], int64_t *y) {
  *y = 2627 * (int64_t)rgb[0] + 6780 * (int64_t)rgb[1] + 593 * (int64_t)rgb[2];
  return 64 + refit_topleft_round(876 * *y, INT64_C(10000) * UINT16_MAX);
}

static inline uint16_t refit_topleft_chroma(uint16_t channel, int64_t y, int divisor) {
  int64_t den = (int64_t)divisor * UINT16_MAX;
  return refit_topleft_round(512 * den + 896 * (10000 * (int64_t)channel - y), den);
}

static inline void refit_topleft_store(uint8_t *plane, size_t index, uint16_t value) {
  plane[2 * index] = (uint8_t)value;
  plane[2 * index + 1] = (uint8_t)(value >> 8);
}

#endif
