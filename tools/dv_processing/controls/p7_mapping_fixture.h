// SPDX-License-Identifier: Apache-2.0
// Deidentified numeric metadata only, observed through public ffprobe.
// Eight nonidentity luma pieces and two third-order chroma MMR pieces.
// No media, title, timeline, packet or device identity is included.
static void p7_mapping_fixture(AVDOVIMetadata *data) {
  AVDOVIRpuDataHeader *h = av_dovi_get_header(data);
  AVDOVIDataMapping *m = av_dovi_get_mapping(data);
  AVDOVIColorMetadata *c = av_dovi_get_color(data);
  memset(h, 0, sizeof(*h));
  memset(m, 0, sizeof(*m));
  memset(c, 0, sizeof(*c));
  h->rpu_type = 2;
  h->rpu_format = 18;
  h->vdr_rpu_profile = 1;
  h->vdr_rpu_level = 0;
  h->chroma_resampling_explicit_filter_flag = 0;
  h->coef_data_type = 0;
  h->coef_log2_denom = 23;
  h->vdr_rpu_normalized_idc = 1;
  h->bl_video_full_range_flag = 0;
  h->bl_bit_depth = 10;
  h->el_bit_depth = 10;
  h->vdr_bit_depth = 12;
  h->spatial_resampling_filter_flag = 0;
  h->el_spatial_resampling_filter_flag = 1;
  h->disable_residual_flag = 0;
  m->vdr_rpu_id = 0;
  m->mapping_color_space = 0;
  m->mapping_chroma_format_idc = 0;
  m->nlq_method_idc = 0;
  m->num_x_partitions = 1;
  m->num_y_partitions = 1;
  m->curves[0].num_pivots = 9;
  { const uint16_t v[] = {0, 128, 256, 384, 512, 640, 768, 896, 1023};
    memcpy(m->curves[0].pivots, v, sizeof(v)); }
  m->curves[0].mapping_idc[0] = 0;
  m->curves[0].poly_order[0] = 1;
  { const int64_t v[] = {3626, 8388608};
    memcpy(m->curves[0].poly_coef[0], v, sizeof(v)); }
  m->curves[0].mapping_idc[1] = 0;
  m->curves[0].poly_order[1] = 1;
  { const int64_t v[] = {3626, 8388607};
    memcpy(m->curves[0].poly_coef[1], v, sizeof(v)); }
  m->curves[0].mapping_idc[2] = 0;
  m->curves[0].poly_order[2] = 1;
  { const int64_t v[] = {26593, 8296738};
    memcpy(m->curves[0].poly_coef[2], v, sizeof(v)); }
  m->curves[0].mapping_idc[3] = 0;
  m->curves[0].poly_order[3] = 1;
  { const int64_t v[] = {34580, 8275440};
    memcpy(m->curves[0].poly_coef[3], v, sizeof(v)); }
  m->curves[0].mapping_idc[4] = 0;
  m->curves[0].poly_order[4] = 1;
  { const int64_t v[] = {-22003, 8388606};
    memcpy(m->curves[0].poly_coef[4], v, sizeof(v)); }
  m->curves[0].mapping_idc[5] = 0;
  m->curves[0].poly_order[5] = 1;
  { const int64_t v[] = {-22004, 8388608};
    memcpy(m->curves[0].poly_coef[5], v, sizeof(v)); }
  m->curves[0].mapping_idc[6] = 0;
  m->curves[0].poly_order[6] = 1;
  { const int64_t v[] = {-22004, 8388608};
    memcpy(m->curves[0].poly_coef[6], v, sizeof(v)); }
  m->curves[0].mapping_idc[7] = 0;
  m->curves[0].poly_order[7] = 1;
  { const int64_t v[] = {-22004, 8388608};
    memcpy(m->curves[0].poly_coef[7], v, sizeof(v)); }
  m->nlq[0].nlq_offset = 512;
  m->nlq[0].vdr_in_max = 1048576;
  m->nlq[0].linear_deadzone_slope = 2048;
  m->nlq[0].linear_deadzone_threshold = 0;
  m->curves[1].num_pivots = 2;
  { const uint16_t v[] = {0, 1023};
    memcpy(m->curves[1].pivots, v, sizeof(v)); }
  m->curves[1].mapping_idc[0] = 1;
  m->curves[1].mmr_order[0] = 3;
  m->curves[1].mmr_constant[0] = 26163;
  { const int64_t v[][7] = {
      {-230431, 11051188, 7235011, -5083283, 1418656, -9439323, 8448648},
      {2927811, -9479913, -11794478, -849379, -11216816, 14289784, 1876340},
      {862433, 8308635, 4327467, -587533, -6229824, 13894123, -2858171},
    };
    memcpy(m->curves[1].mmr_coef[0], v, sizeof(v)); }
  m->nlq[1].nlq_offset = 512;
  m->nlq[1].vdr_in_max = 1048576;
  m->nlq[1].linear_deadzone_slope = 2048;
  m->nlq[1].linear_deadzone_threshold = 0;
  m->curves[2].num_pivots = 2;
  { const uint16_t v[] = {0, 1023};
    memcpy(m->curves[2].pivots, v, sizeof(v)); }
  m->curves[2].mapping_idc[0] = 1;
  m->curves[2].mmr_order[0] = 3;
  m->curves[2].mmr_constant[0] = 566438;
  { const int64_t v[][7] = {
      {-2612310, -432811, 7716669, -5781593, 7848329, -3804433, 5194520},
      {4692332, 2594927, 2650192, 2542064, -18029098, -1373643, 2228015},
      {-564564, -205162, -2612863, 3987001, -7321630, 5786752, 802095},
    };
    memcpy(m->curves[2].mmr_coef[0], v, sizeof(v)); }
  m->nlq[2].nlq_offset = 512;
  m->nlq[2].vdr_in_max = 1048576;
  m->nlq[2].linear_deadzone_slope = 2048;
  m->nlq[2].linear_deadzone_threshold = 0;
  { const AVRational v[] = {{9574, 8192}, {0, 8192}, {13802, 8192}, {9574, 8192}, {-1540, 8192}, {-5348, 8192}, {9574, 8192}, {17610, 8192}, {0, 8192}};
    memcpy(c->ycc_to_rgb_matrix, v, sizeof(v)); }
  { const AVRational v[] = {{16777216, 268435456}, {134217728, 268435456}, {134217728, 268435456}};
    memcpy(c->ycc_to_rgb_offset, v, sizeof(v)); }
  { const AVRational v[] = {{7222, 16384}, {8771, 16384}, {390, 16384}, {2654, 16384}, {12430, 16384}, {1300, 16384}, {0, 16384}, {422, 16384}, {15962, 16384}};
    memcpy(c->rgb_to_lms_matrix, v, sizeof(v)); }
  c->signal_eotf = 65535;
  c->signal_eotf_param0 = 0;
  c->signal_eotf_param1 = 0;
  c->signal_eotf_param2 = 0;
  c->signal_bit_depth = 12;
  c->signal_color_space = 0;
  c->signal_chroma_format = 0;
  c->signal_full_range_flag = 1;
}
