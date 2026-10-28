// SPDX-License-Identifier: Apache-2.0
// CPU-only public mapping contracts, not a decoder/GPU/vendor comparison.
#include <assert.h>
#include <float.h>
#include <stdio.h>
#include <string.h>
#include <libavutil/mem.h>
#include "dv_metadata.h"
#include "controls/p7_mapping_fixture.h"

static void close_enough(double actual, double expected) {
  assert(isfinite(actual) && fabs(actual - expected) <= 4e-6 * fmax(1, fabs(expected)));
}

// Independent scalar evaluation of the public polynomial/MMR basis. Compare
// fixed-point source coefficients to the binary32 API projection over a grid,
// including every pivot edge, not just a duplicate mapper invocation.
static double evaluate(const AVDOVIReshapingCurve *curve,
                       const struct pl_dovi_metadata *mapped, int comp,
                       const double in[3], bool projected) {
  int piece = 0;
  double x = in[comp];
  while (piece < curve->num_pivots - 2 &&
         x >= curve->pivots[piece + 1] / 1023.0)
    piece++;
  const double denom = 8388608.0;
  double value;
  if (curve->mapping_idc[piece] == AV_DOVI_MAPPING_POLYNOMIAL) {
    value = 0;
    for (int i = curve->poly_order[piece]; i >= 0; i--)
      value = value * x + (projected ? mapped->comp[comp].poly_coeffs[piece][i] :
                                      curve->poly_coef[piece][i] / denom);
  } else {
    value = projected ? mapped->comp[comp].mmr_constant[piece] :
                        curve->mmr_constant[piece] / denom;
    for (int order = 0; order < curve->mmr_order[piece]; order++) {
      double a = pow(in[0], order + 1), b = pow(in[1], order + 1),
             c = pow(in[2], order + 1);
      const double basis[] = {a, b, c, a*b, a*c, b*c, a*b*c};
      for (int k = 0; k < 7; k++)
        value += basis[k] * (projected ? mapped->comp[comp].mmr_coeffs[piece][order][k] :
                                           curve->mmr_coef[piece][order][k] / denom);
    }
  }
  double low = curve->pivots[0] / 1023.0;
  double high = curve->pivots[curve->num_pivots - 1] / 1023.0;
  return fmin(high, fmax(low, value));
}

static void public_piecewise_fel_mapping(void) {
  size_t size;
  AVDOVIMetadata *data = av_dovi_metadata_alloc(&size);
  assert(data);
  p7_mapping_fixture(data);
  struct dv_decoded_mapping out;
  assert(dv_map_metadata(data, size, 7, true, &out));
  assert(out.source == data && out.polynomial_segments == 8 && out.mmr_segments == 2);
  assert(out.dovi.nlq_active && out.dovi.comp[0].num_pivots == 9);
  AVFrame *frame = av_frame_alloc();
  assert(frame);
  frame->pts = 125;
  AVFrameSideData *side = av_frame_new_side_data(frame, AV_FRAME_DATA_DOVI_METADATA, size);
  assert(side);
  memcpy(side->data, data, size);
  assert(dv_map_decoded_metadata(frame, 7, true, &out) && frame->pts == 125);
  assert(out.source == (const void *)side->data);
  av_frame_free(&frame);
  assert(dv_map_metadata(data, size, 7, true, &out));
  const AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  for (int comp = 0; comp < 3; comp++) {
    const AVDOVIReshapingCurve *curve = &m->curves[comp];
    assert(out.dovi.comp[comp].num_pivots == curve->num_pivots);
    for (int i = 0; i < curve->num_pivots; i++)
      close_enough(out.dovi.comp[comp].pivots[i], curve->pivots[i] / 1023.0);
    for (int i = 0; i < curve->num_pivots - 1; i++) {
      assert((int)out.dovi.comp[comp].method[i] == (int)curve->mapping_idc[i]);
      if (curve->mapping_idc[i] == AV_DOVI_MAPPING_POLYNOMIAL) {
        for (int k = 0; k < 3; k++)
          close_enough(out.dovi.comp[comp].poly_coeffs[i][k],
                       k <= curve->poly_order[i] ? curve->poly_coef[i][k] / 8388608.0 : 0);
      } else {
        assert(out.dovi.comp[comp].mmr_order[i] == curve->mmr_order[i]);
        close_enough(out.dovi.comp[comp].mmr_constant[i], curve->mmr_constant[i] / 8388608.0);
        for (int order = 0; order < curve->mmr_order[i]; order++)
          for (int k = 0; k < 7; k++)
            close_enough(out.dovi.comp[comp].mmr_coeffs[i][order][k],
                         curve->mmr_coef[i][order][k] / 8388608.0);
      }
    }
    close_enough(out.dovi.nlq[comp].offset, 512.0 / 1023);
    close_enough(out.dovi.nlq[comp].deadzone_slope, 1023.0 * 2048 / 8388608);
    close_enough(out.dovi.nlq[comp].deadzone_threshold, -1024.0 / 8388608);
    // Exact neutral code contributes zero; endpoints obey the original bound.
    for (int code = 0; code <= 1023; code++) {
      double delta = code - 512;
      double expected = delta == 0 ? 0 : copysign((fabs(delta) - .5) * 2048 / 8388608, delta);
      double normalized = code / 1023.0 - out.dovi.nlq[comp].offset;
      double actual = code == 512 ? 0 : copysign(fabs(normalized) *
          out.dovi.nlq[comp].deadzone_slope + out.dovi.nlq[comp].deadzone_threshold, delta);
      close_enough(actual, expected);
      assert(fabs(actual) < .125);
    }
  }
  unsigned samples = 0;
  for (int a = 0; a <= 16; a++)
    for (int b = 0; b <= 16; b++)
      for (int c = 0; c <= 16; c++) {
        const double in[] = {a/16.0, b/16.0, c/16.0};
        for (int comp = 0; comp < 3; comp++) {
          close_enough(evaluate(&m->curves[comp], &out.dovi, comp, in, false),
                       evaluate(&m->curves[comp], &out.dovi, comp, in, true));
          samples++;
        }
      }
  for (int edge = 0; edge < 9; edge++) {
    const double in[] = {m->curves[0].pivots[edge]/1023.0, .25, .75};
    close_enough(evaluate(&m->curves[0], &out.dovi, 0, in, false),
                 evaluate(&m->curves[0], &out.dovi, 0, in, true));
  }
  // The same shared mapper deliberately omits EL for the base-only lane.
  assert(dv_map_metadata(data, size, 7, false, &out) && !out.dovi.nlq_active);
  // Preserve narrowed signalled clamp endpoints rather than widening to 0/1.
  AVDOVIDataMapping *change = av_dovi_get_mapping(data);
  change->curves[1].pivots[0] = 16;change->curves[1].pivots[1] = 1000;
  assert(dv_map_metadata(data, size, 7, true, &out));
  close_enough(out.dovi.comp[1].pivots[0], 16.0/1023);
  close_enough(out.dovi.comp[1].pivots[1], 1000.0/1023);
  av_free(data);
  printf("public piecewise FEL mapping: %u scalar samples; no GPU qualification\n", samples);
}

static void malformed_metadata_refuses(void) {
  size_t size;
  AVDOVIMetadata *data = av_dovi_metadata_alloc(&size);
  assert(data);
  struct dv_decoded_mapping out;
#define REFUSE(change) do { p7_mapping_fixture(data); change; \
  assert(!dv_map_metadata(data, size, 7, true, &out)); } while (0)
  AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  AVDOVIRpuDataHeader *h = av_dovi_get_header(data);
  AVDOVIColorMetadata *c = av_dovi_get_color(data);
  REFUSE(m->curves[0].num_pivots = 10);
  REFUSE(m->curves[0].pivots[1] = 0);
  REFUSE(m->curves[0].pivots[8] = 1024);
  REFUSE(m->curves[0].poly_order[0] = 3);
  REFUSE(m->curves[1].mmr_order[0] = 4);
  REFUSE(m->curves[1].mapping_idc[0] = 2);
  REFUSE(m->curves[0].poly_coef[0][0] = INT64_MAX);
  REFUSE(m->curves[1].mmr_coef[0][0][0] = INT64_MIN);
  REFUSE(m->num_x_partitions = 2);
  REFUSE(m->nlq[0].vdr_in_max = 16384);
  REFUSE(m->nlq[0].linear_deadzone_slope = UINT64_MAX);
  REFUSE(m->nlq_method_idc = AV_DOVI_NLQ_NONE);
  REFUSE(h->disable_residual_flag = 1);
  REFUSE(h->el_bit_depth = 12);
  REFUSE(h->ext_mapping_idc_0_4 = 1);
  REFUSE(c->ycc_to_rgb_matrix[0].den = 0);
  REFUSE(c->rgb_to_lms_matrix[0].num = INT32_MAX);
#undef REFUSE
  p7_mapping_fixture(data);
  assert(!dv_map_metadata(data, sizeof(*data)-1, 7, true, &out));
  size_t offset = data->mapping_offset;
  data->mapping_offset = size;
  assert(!dv_map_metadata(data, size, 7, true, &out));
  data->mapping_offset = offset;
  offset = data->ext_block_offset;
  data->ext_block_offset = data->color_offset;
  assert(!dv_map_metadata(data, size, 7, true, &out));
  data->ext_block_offset = offset;
  data->num_ext_blocks = 33;
  assert(!dv_map_metadata(data, size, 7, true, &out));
  assert(!dv_map_decoded_metadata(NULL, 7, true, &out));
  av_free(data);
}
int main(void) {
  public_piecewise_fel_mapping();
  malformed_metadata_refuses();
  return 0;
}
