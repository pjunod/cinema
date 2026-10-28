// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_DV_METADATA_H
#define PLURX_DV_METADATA_H

// One public decoder-metadata admission path for BL-only and paired FEL.
// Callers independently bind the same AVFrame's fresh raw RPU to its coded AU.
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <libavutil/frame.h>
#include <libavutil/dovi_meta.h>
#include <libplacebo/utils/libav.h>
#include "nlq_clipping.h"

struct dv_decoded_mapping {
  struct pl_dovi_metadata dovi;
  const AVDOVIMetadata *source;
  int polynomial_segments, mmr_segments;
};

static inline bool dv_metadata_rational(AVRational q, double limit) {
  return q.den > 0 && isfinite(av_q2d(q)) && fabs(av_q2d(q)) <= limit;
}

static inline bool dv_metadata_coefficient(int64_t value) {
  // FFmpeg's fixed coefficients have a signed 32-bit Golomb integer and
  // 23 fractional bits. libplacebo intentionally projects them to binary32;
  // do not invent exact-LSB preservation or replace them with an identity.
  return value >= -(INT64_C(1) << 54) && value < (INT64_C(1) << 54);
}

static inline bool dv_map_metadata(const AVDOVIMetadata *data, size_t size,
                                   int profile, bool fel,
                                   struct dv_decoded_mapping *out) {
  if (!data || (uintptr_t)data % _Alignof(AVDOVIMetadata) || !out ||
      (profile != 5 && profile != 7 && profile != 8) ||
      (fel && profile != 7) || size < sizeof(*data) || size > 65536 ||
      size < sizeof(AVDOVIRpuDataHeader) || size < sizeof(AVDOVIDataMapping) ||
      size < sizeof(AVDOVIColorMetadata))
    return false;
  if (data->header_offset < sizeof(*data) ||
      data->header_offset % _Alignof(AVDOVIRpuDataHeader) ||
      data->mapping_offset % _Alignof(AVDOVIDataMapping) ||
      data->color_offset % _Alignof(AVDOVIColorMetadata) ||
      data->header_offset > size - sizeof(AVDOVIRpuDataHeader) ||
      data->mapping_offset > size - sizeof(AVDOVIDataMapping) ||
      data->color_offset > size - sizeof(AVDOVIColorMetadata) ||
      data->header_offset + sizeof(AVDOVIRpuDataHeader) > data->mapping_offset ||
      data->mapping_offset + sizeof(AVDOVIDataMapping) > data->color_offset ||
      data->color_offset + sizeof(AVDOVIColorMetadata) > data->ext_block_offset ||
      data->num_ext_blocks < 0 || data->num_ext_blocks > 32 ||
      data->ext_block_offset > size ||
      data->ext_block_size < sizeof(AVDOVIDmData) ||
      (size_t)data->num_ext_blocks >
          (size - data->ext_block_offset) / data->ext_block_size)
    return false;
  const AVDOVIRpuDataHeader *h = av_dovi_get_header(data);
  const AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  const AVDOVIColorMetadata *c = av_dovi_get_color(data);
  if (h->rpu_type != 2 || h->rpu_format != 18 ||
      h->vdr_rpu_profile != (profile == 5 ? 0 : 1) || h->vdr_rpu_level != 0 ||
      h->coef_data_type != 0 || h->coef_log2_denom != 23 ||
      h->bl_bit_depth != 10 || h->vdr_bit_depth != 12 ||
      h->vdr_rpu_normalized_idc != 1 ||
      h->bl_video_full_range_flag != (profile == 5) ||
      h->chroma_resampling_explicit_filter_flag || h->spatial_resampling_filter_flag ||
      h->ext_mapping_idc_0_4 || h->ext_mapping_idc_5_7 ||
      (profile != 7 && !h->disable_residual_flag) ||
      m->mapping_color_space != 0 || m->mapping_chroma_format_idc != 0 ||
      m->num_x_partitions != 1 || m->num_y_partitions != 1 ||
      c->signal_bit_depth != 12 || c->signal_color_space != (profile == 5 ? 2 : 0) ||
      c->signal_chroma_format != 0 || c->signal_full_range_flag != 1 ||
      c->signal_eotf != 65535 || c->signal_eotf_param0 ||
      c->signal_eotf_param1 || c->signal_eotf_param2)
    return false;
  if (fel && (h->el_bit_depth != 10 || h->disable_residual_flag ||
              !h->el_spatial_resampling_filter_flag ||
              m->nlq_method_idc != AV_DOVI_NLQ_LINEAR_DZ))
    return false;
  struct dv_decoded_mapping result = {.source = data};
  for (int comp = 0; comp < 3; comp++) {
    const AVDOVIReshapingCurve *curve = &m->curves[comp];
    if (curve->num_pivots < 2 || curve->num_pivots > AV_DOVI_MAX_PIECES + 1 ||
        curve->pivots[curve->num_pivots - 1] > 1023)
      return false;
    for (int i = 0; i < curve->num_pivots - 1; i++) {
      if (curve->pivots[i] >= curve->pivots[i + 1])
        return false;
      if (curve->mapping_idc[i] == AV_DOVI_MAPPING_POLYNOMIAL) {
        if (curve->poly_order[i] < 1 || curve->poly_order[i] > 2)
          return false;
        for (int j = 0; j <= curve->poly_order[i]; j++)
          if (!dv_metadata_coefficient(curve->poly_coef[i][j]))
            return false;
        result.polynomial_segments++;
      } else if (curve->mapping_idc[i] == AV_DOVI_MAPPING_MMR) {
        if (curve->mmr_order[i] < 1 || curve->mmr_order[i] > 3 ||
            !dv_metadata_coefficient(curve->mmr_constant[i]))
          return false;
        for (int j = 0; j < curve->mmr_order[i]; j++)
          for (int k = 0; k < 7; k++)
            if (!dv_metadata_coefficient(curve->mmr_coef[i][j][k]))
              return false;
        result.mmr_segments++;
      } else {
        return false;
      }
    }
    // Limits follow the public source fields: signed16 / 2^13 or 2^14
    // matrices and unsigned32 / 2^28 offsets. No fixed identity/matrix subset.
    if (!dv_metadata_rational(c->ycc_to_rgb_offset[comp], 16) ||
        c->ycc_to_rgb_offset[comp].num < 0)
      return false;
    if (fel) {
      const AVDOVINLQParams *n = &m->nlq[comp];
      const uint64_t mask = (UINT64_C(1) << 23) - 1;
      if (!nlq_clip_is_nonbinding(n->nlq_offset, n->linear_deadzone_slope,
                                  n->linear_deadzone_threshold,
                                  n->vdr_in_max >> 23, n->vdr_in_max & mask))
        return false;
    }
  }
  for (int i = 0; i < 9; i++)
    if (!dv_metadata_rational(c->ycc_to_rgb_matrix[i], 4) ||
        !dv_metadata_rational(c->rgb_to_lms_matrix[i], 2))
      return false;
  // Public mapper preserves every active polynomial/MMR piece, signalled
  // pivot clamp and matrix. The public shader reshapes BL before adding NLQ
  // residuals, then performs the nonlinear/linear color transforms.
  pl_map_dovi_metadata(&result.dovi, data);
  result.dovi.nlq_active = fel;
  for (int comp = 0; comp < 3; comp++) {
    if (!isfinite(result.dovi.nonlinear_offset[comp]) ||
        (fel && (!isfinite(result.dovi.nlq[comp].offset) ||
                 !isfinite(result.dovi.nlq[comp].deadzone_slope) ||
                 !isfinite(result.dovi.nlq[comp].deadzone_threshold))))
      return false;
    const AVDOVIReshapingCurve *curve = &m->curves[comp];
    for (int i = 0; i < curve->num_pivots; i++)
      if (!isfinite(result.dovi.comp[comp].pivots[i]))
        return false;
    for (int i = 0; i < curve->num_pivots - 1; i++) {
      if (curve->mapping_idc[i] == AV_DOVI_MAPPING_POLYNOMIAL) {
        for (int j = 0; j < 3; j++)
          if (!isfinite(result.dovi.comp[comp].poly_coeffs[i][j]))
            return false;
      } else {
        if (!isfinite(result.dovi.comp[comp].mmr_constant[i]))
          return false;
        for (int j = 0; j < curve->mmr_order[i]; j++)
          for (int k = 0; k < 7; k++)
            if (!isfinite(result.dovi.comp[comp].mmr_coeffs[i][j][k]))
              return false;
      }
    }
    for (int i = 0; i < 3; i++)
      if (!isfinite(result.dovi.nonlinear.m[comp][i]) ||
          !isfinite(result.dovi.linear.m[comp][i]))
        return false;
  }
  *out = result;
  return true;
}

static inline bool dv_map_decoded_metadata(const AVFrame *frame, int profile,
                                           bool fel,
                                           struct dv_decoded_mapping *out) {
  if (!frame)
    return false;
  const AVFrameSideData *side =
      av_frame_get_side_data(frame, AV_FRAME_DATA_DOVI_METADATA);
  return side && dv_map_metadata((const void *)side->data, side->size, profile, fel, out);
}
#endif
