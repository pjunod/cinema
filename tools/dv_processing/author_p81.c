// Post-reconstruction authoring. Input metadata must come from the same
// accepted renderer generation. This tool does not qualify source
// reconstruction itself.
#include <errno.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/dovi_meta.h>
#include <libavutil/intreadwrite.h>
#include <libavutil/pixfmt.h>
#include <libavutil/sha.h>
#include <libdovi/rpu_parser.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define MAX_FRAMES 64
#define MAX_NAL 65536
#define MAX_PACKET (32 * 1024 * 1024)
static void need(int ok, const char *why) {
  if (!ok) {
    fprintf(stderr, "P8.1 author refused: %s\n", why);
    exit(1);
  }
}
static int64_t field(char **s, int64_t min, int64_t max, char delimiter) {
  char *end;
  errno = 0;
  long long v = strtoll(*s, &end, 10);
  need(!errno && end != *s && *end == delimiter && v >= min && v <= max,
       "exact rational field");
  *s = end + 1;
  return v;
}
static void digest(const uint8_t *bytes, size_t size, char out[65]) {
  struct AVSHA *sha = av_sha_alloc();
  uint8_t hash[32];
  need(sha != NULL, "hash allocation");
  need(av_sha_init(sha, 256) == 0, "SHA256");
  av_sha_update(sha, bytes, size);
  av_sha_final(sha, hash);
  av_free(sha);
  for (int i = 0; i < 32; i++)
    snprintf(out + 2 * i, 3, "%02x", hash[i]);
}
struct row {
  int64_t pts, duration;
  int pd, dd, seen;
  const DoviData *nal;
  char source_hash[65], adapted_hash[65];
};
// The reconstructed master has not applied display trims or cropped pixels.
// Retain those source instructions exactly for the eventual DV display mapper.
static bool retained_metadata_equal(const DoviVdrDmData *a,
                                    const DoviVdrDmData *b) {
  if (!a || !b || a->dm_data.level2.len != b->dm_data.level2.len ||
      a->dm_data.level8.len != b->dm_data.level8.len ||
      !!a->dm_data.level3 != !!b->dm_data.level3 ||
      !!a->dm_data.level4 != !!b->dm_data.level4 ||
      !!a->dm_data.level5 != !!b->dm_data.level5)
    return false;
  for (size_t i = 0; i < a->dm_data.level2.len; i++) {
    const DoviExtMetadataBlockLevel2 *x = a->dm_data.level2.list[i];
    const DoviExtMetadataBlockLevel2 *y = b->dm_data.level2.list[i];
    if (!x || !y || x->target_max_pq != y->target_max_pq ||
        x->trim_slope != y->trim_slope || x->trim_offset != y->trim_offset ||
        x->trim_power != y->trim_power ||
        x->trim_chroma_weight != y->trim_chroma_weight ||
        x->trim_saturation_gain != y->trim_saturation_gain ||
        x->ms_weight != y->ms_weight)
      return false;
  }
  if (a->dm_data.level3 &&
      (a->dm_data.level3->min_pq_offset != b->dm_data.level3->min_pq_offset ||
       a->dm_data.level3->max_pq_offset != b->dm_data.level3->max_pq_offset ||
       a->dm_data.level3->avg_pq_offset != b->dm_data.level3->avg_pq_offset))
    return false;
  for (size_t i = 0; i < a->dm_data.level8.len; i++) {
    const DoviExtMetadataBlockLevel8 *x = a->dm_data.level8.list[i];
    const DoviExtMetadataBlockLevel8 *y = b->dm_data.level8.list[i];
    if (!x || !y || x->length != 10 || y->length != 10 ||
        x->target_display_index != y->target_display_index ||
        x->trim_slope != y->trim_slope || x->trim_offset != y->trim_offset ||
        x->trim_power != y->trim_power ||
        x->trim_chroma_weight != y->trim_chroma_weight ||
        x->trim_saturation_gain != y->trim_saturation_gain ||
        x->ms_weight != y->ms_weight)
      return false;
  }
  if (a->dm_data.level4 &&
      (a->dm_data.level4->anchor_pq != b->dm_data.level4->anchor_pq ||
       a->dm_data.level4->anchor_power != b->dm_data.level4->anchor_power))
    return false;
  if (a->dm_data.level5) {
    const DoviExtMetadataBlockLevel5 *x = a->dm_data.level5;
    const DoviExtMetadataBlockLevel5 *y = b->dm_data.level5;
    if (x->active_area_left_offset != y->active_area_left_offset ||
        x->active_area_right_offset != y->active_area_right_offset ||
        x->active_area_top_offset != y->active_area_top_offset ||
        x->active_area_bottom_offset != y->active_area_bottom_offset)
      return false;
  }
  return true;
}

static const DoviData *adapt(const char *dir, int index, char hash[65]) {
  char path[4096];
  int n = snprintf(path, sizeof(path), "%s/frame-%03d.nal", dir, index);
  need(n > 0 && n < (int)sizeof(path), "metadata path");
  FILE *f = fopen(path, "rb");
  need(f != NULL, "frame metadata exists");
  uint8_t bytes[MAX_NAL + 1];
  size_t size = fread(bytes, 1, sizeof(bytes), f);
  need(!ferror(f) && size > 2 && size <= MAX_NAL, "bounded metadata read");
  need(fclose(f) == 0 && bytes[0] == 0x7c && bytes[1] == 1, "UNSPEC62 header");
  digest(bytes, size, hash);
  DoviRpuOpaque *r = dovi_parse_unspec62_nalu(bytes, size);
  need(r && !dovi_rpu_get_error(r), "source metadata parse");
  const DoviRpuDataHeader *h = dovi_rpu_get_header(r);
  need(h && h->guessed_profile == 7 && !h->use_prev_vdr_rpu_flag &&
           h->vdr_dm_metadata_present_flag && !h->disable_residual_flag,
       "fresh FEL metadata");
  const DoviVdrDmData *dm = dovi_rpu_get_vdr_dm_data(r);
  need(dm && dm->dm_data.level2.len <= 16 && dm->dm_data.level8.len <= 16 &&
           dm->dm_data.level10.len == 0 && !dm->dm_data.level255,
       "unsupported creative or complex adaptation metadata");
  dovi_rpu_free_header(h);
  need(dovi_convert_rpu_with_mode(r, 2) == 0 && dovi_rpu_remove_mapping(r) == 0,
       "P8.1 adaptation and remove already-applied mapping");
  const DoviVdrDmData *adapted_dm = dovi_rpu_get_vdr_dm_data(r);
  need(retained_metadata_equal(dm, adapted_dm),
       "source display trims and active area preserved");
  dovi_rpu_free_vdr_dm_data(adapted_dm);
  dovi_rpu_free_vdr_dm_data(dm);
  h = dovi_rpu_get_header(r);
  need(h && h->guessed_profile == 8 && h->disable_residual_flag,
       "adapted P8 residual independence");
  dovi_rpu_free_header(h);
  const DoviData *nal = dovi_write_unspec62_nalu(r);
  need(nal && nal->len > 2 && nal->len <= MAX_NAL && nal->data[0] == 0x7c &&
           nal->data[1] == 1,
       "adapted metadata serialization");
  dovi_rpu_free(r);
  return nal;
}
static void validate_packet(const AVPacket *p) {
  int offset = 0, first_slices = 0;
  while (offset < p->size) {
    need(p->size - offset >= 4, "NAL prefix");
    uint32_t size = AV_RB32(p->data + offset);
    offset += 4;
    need(size >= 3 && size <= (uint32_t)(p->size - offset), "NAL bounds");
    const uint8_t *nal = p->data + offset;
    int type = (nal[0] >> 1) & 63;
    need(!(nal[0] & 0x80) && (nal[0] & 1) == 0 && (nal[1] >> 3) == 0 &&
             (nal[1] & 7) == 1,
         "single-layer temporal-id-zero HEVC");
    need(type != 62 && type != 63, "no existing RPU or enhancement dependency");
    if (type <= 31 && (nal[2] & 0x80))
      first_slices++;
    offset += (int)size;
  }
  need(first_slices == 1, "one complete picture per encoded packet");
}
// movenc requires every mapped decode interval to be strictly smaller than
// INT_MAX. Validate the whole endpoint in the actual post-header output clock.
static bool fmp4_grid_bounds(int64_t first, int64_t step, int count) {
  return first >= 0 && step > 0 && step < INT_MAX && count > 0 &&
         count <= MAX_FRAMES && first <= INT64_MAX - (int64_t)count * step;
}
int main(int argc, char **argv) {
  need(argc == 5 || (argc == 6 && !strcmp(argv[5], "fmp4")),
       "usage: author_p81 ENCODED TIMING.tsv RPU_DIR OUTPUT [fmp4]");
  bool fmp4 = argc == 6;
  FILE *timing = fopen(argv[2], "r");
  need(timing != NULL, "renderer timeline");
  struct row rows[MAX_FRAMES] = {0};
  int count = 0;
  char line[160];
  while (fgets(line, sizeof(line), timing)) {
    need(count < MAX_FRAMES, "bounded renderer generation");
    char *s = line;
    struct row *r = &rows[count];
    r->pts = field(&s, 0, INT64_MAX, '/');
    r->pd = (int)field(&s, 1, INT_MAX, '\t');
    r->duration = field(&s, 1, INT_MAX, '/');
    r->dd = (int)field(&s, 1, INT_MAX, '\n');
    need(!*s, "complete timeline row");
    if (count)
      need(av_compare_ts(rows[count - 1].pts,
                         (AVRational){1, rows[count - 1].pd}, r->pts,
                         (AVRational){1, r->pd}) < 0,
           "strict presentation order");
    r->nal = adapt(argv[3], count, r->source_hash);
    digest(r->nal->data, r->nal->len, r->adapted_hash);
    count++;
  }
  need(!ferror(timing) && fclose(timing) == 0 && count > 0,
       "complete nonempty timeline");
  AVFormatContext *in = NULL, *out = NULL;
  need(avformat_open_input(&in, argv[1], NULL, NULL) >= 0 &&
           avformat_find_stream_info(in, NULL) >= 0,
       "encoded container");
  need(in->nb_streams == 1, "video-only intermediate");
  AVStream *src = in->streams[0];
  AVCodecParameters *par = src->codecpar;
  need(par->codec_id == AV_CODEC_ID_HEVC &&
           par->profile == AV_PROFILE_HEVC_MAIN_10 &&
           par->format == AV_PIX_FMT_YUV420P10LE && par->width >= 64 &&
           par->width <= 3840 && par->height >= 64 && par->height <= 2160 &&
           par->color_primaries == AVCOL_PRI_BT2020 &&
           par->color_trc == AVCOL_TRC_SMPTE2084 &&
           par->color_space == AVCOL_SPC_BT2020_NCL &&
           par->color_range == AVCOL_RANGE_MPEG,
       "HDR10 base coding contract");
  need(par->extradata_size >= 23 && par->extradata[0] == 1 &&
           (par->extradata[21] & 3) == 3,
       "length-prefixed HEVC");
  // FFmpeg dovi_rpuenc.c levels 1..9; select against the shortest actual
  // interval rather than an average that hides a high-rate VFR burst.
  static const int pps[9] = {
      1280 * 720 * 24,  1280 * 720 * 30,  1920 * 1080 * 24,
      1920 * 1080 * 30, 1920 * 1080 * 60, 3840 * 2160 * 24,
      3840 * 2160 * 30, 3840 * 2160 * 48, 3840 * 2160 * 60};
  static const int widths[9] = {1280, 1280, 1920, 2560, 3840,
                                3840, 3840, 3840, 3840};
  static const int mbps[9] = {50, 50, 70, 70, 70, 130, 130, 130, 130};
  int level = 0;
  for (int candidate = 0; candidate < 9 && !level; candidate++) {
    int fits = par->width <= widths[candidate];
    for (int i = 0; i < count; i++)
      fits &= av_compare_ts(rows[i].duration, (AVRational){1, rows[i].dd},
                            (int64_t)par->width * par->height,
                            (AVRational){1, pps[candidate]}) >= 0;
    if (fits)
      level = candidate + 1;
  }
  need(level > 0, "bounded Dolby Vision coded picture rate");
  if (fmp4) {
    int64_t first_pts = av_rescale_q(rows[0].pts,
                                  (AVRational){1, rows[0].pd}, src->time_base);
    int64_t step = av_rescale_q(rows[0].duration,
                              (AVRational){1, rows[0].dd}, src->time_base);
    need(step > 0 && step <= INT64_MAX / count && first_pts >= 0 &&
             av_compare_ts(first_pts, src->time_base, rows[0].pts,
                           (AVRational){1, rows[0].pd}) == 0 &&
             av_compare_ts(step, src->time_base, rows[0].duration,
                           (AVRational){1, rows[0].dd}) == 0 &&
             first_pts <= INT64_MAX - (int64_t)count * step,
         "fMP4 exact bounded frame grid");
    for (int i = 0; i < count; i++)
      need(av_compare_ts(rows[i].duration, (AVRational){1, rows[i].dd},
                         step, src->time_base) == 0 &&
               av_compare_ts(rows[i].pts, (AVRational){1, rows[i].pd},
                             first_pts + i * step, src->time_base) == 0,
           "fMP4 uniform contiguous frame grid");
  }
  need(avformat_alloc_output_context2(&out, NULL,
                                      fmp4 ? "mp4" : "matroska", argv[4]) >= 0 &&
           out,
       "output muxer");
  AVStream *dst = avformat_new_stream(out, NULL);
  need(dst && avcodec_parameters_copy(dst->codecpar, par) >= 0,
       "copy HDR10 codec parameters");
  dst->avg_frame_rate = (AVRational){rows[0].dd, (int)rows[0].duration};
  dst->time_base = src->time_base;
  // hev1 permits the retained in-band parameter sets; never relabel them hvc1.
  dst->codecpar->codec_tag = fmp4 ? MKTAG('h', 'e', 'v', '1') : 0;
  AVDictionary *mux_options = NULL;
  if (fmp4) {
    // frag_discont makes FFmpeg keep the original nonzero decode origin in
    // tfdt. No automatic keyframe cuts: the complete window is one fragment.
    out->avoid_negative_ts = AVFMT_AVOID_NEG_TS_DISABLED;
    out->strict_std_compliance = FF_COMPLIANCE_UNOFFICIAL;
    need(av_dict_set(&mux_options, "movflags",
                     "empty_moov+frag_custom+default_base_moof+frag_discont",
                     0) >= 0 &&
             av_dict_set(&mux_options, "use_editlist", "0", 0) >= 0 &&
             av_dict_set_int(&mux_options, "video_track_timescale",
                             src->time_base.den, 0) >= 0,
         "fMP4 mux policy");
  }
  need(!av_packet_side_data_get(par->coded_side_data, par->nb_coded_side_data,
                                AV_PKT_DATA_DOVI_CONF),
       "no existing DV config");
  AVPacketSideData *side = av_packet_side_data_new(
      &dst->codecpar->coded_side_data, &dst->codecpar->nb_coded_side_data,
      AV_PKT_DATA_DOVI_CONF, sizeof(AVDOVIDecoderConfigurationRecord), 0);
  need(side != NULL, "DV config allocation");
  AVDOVIDecoderConfigurationRecord *cfg = (void *)side->data;
  memset(cfg, 0, sizeof(*cfg));
  cfg->dv_version_major = 1;
  cfg->dv_profile = 8;
  cfg->dv_level = (uint8_t)level;
  cfg->rpu_present_flag = cfg->bl_present_flag = 1;
  cfg->dv_bl_signal_compatibility_id = 1;
  need(avio_open(&out->pb, argv[4], AVIO_FLAG_WRITE) >= 0 &&
           avformat_write_header(out, &mux_options) >= 0,
       "output header");
  need(!mux_options, "all mux options consumed");
  av_dict_free(&mux_options);
  if (fmp4) {
    int64_t first = av_rescale_q(rows[0].pts,
                               (AVRational){1, rows[0].pd}, dst->time_base);
    int64_t step = av_rescale_q(rows[0].duration,
                              (AVRational){1, rows[0].dd}, dst->time_base);
    need(fmp4_grid_bounds(first, step, count),
         "fMP4 bounded output-clock frame grid");
    for (int i = 0; i < count; i++)
      need(av_compare_ts(first + (int64_t)i * step, dst->time_base,
                         rows[i].pts, (AVRational){1, rows[i].pd}) == 0 &&
               av_compare_ts(step, dst->time_base, rows[i].duration,
                             (AVRational){1, rows[i].dd}) == 0,
           "fMP4 exact output-clock frame grid");
  }
  AVPacket *p = av_packet_alloc();
  need(p != NULL, "packet allocation");
  int rc, packets = 0;
  while ((rc = av_read_frame(in, p)) >= 0) {
    need(p->stream_index == 0 && p->pts != AV_NOPTS_VALUE &&
             p->dts != AV_NOPTS_VALUE && p->size > 0 && p->size <= MAX_PACKET,
         "complete timestamped encoded packet");
    validate_packet(p);
    int match = -1;
    for (int i = 0; i < count; i++)
      if (av_compare_ts(p->pts, src->time_base, rows[i].pts,
                        (AVRational){1, rows[i].pd}) == 0)
        match = i;
    need(match >= 0 && !rows[match].seen,
         "unique exact presentation association");
    if (fmp4)
      need(p->pts == p->dts && match == packets &&
               (packets != 0 || (p->flags & AV_PKT_FLAG_KEY)),
           "fMP4 no-reorder keyframe-led encoded grid");
    struct row *r = &rows[match];
    r->seen = 1;
    // The encoder's MP4 stts describes decode intervals; its demuxer may
    // expose a nominal duration. Author the renderer's explicit presentation
    // duration in Matroska, independently of the retained encoder DTS.
    int64_t duration =
        av_rescale_q(r->duration, (AVRational){1, r->dd}, src->time_base);
    need(duration > 0 && av_compare_ts(duration, src->time_base, r->duration,
                                       (AVRational){1, r->dd}) == 0,
         "duration representable in encoder clock");
    p->duration = duration;
    need(av_compare_ts(r->duration, (AVRational){1, r->dd},
                       ((int64_t)p->size + r->nal->len + 4) * 8,
                       (AVRational){1, mbps[level - 1] * 1000000}) >= 0,
         "Dolby Vision peak access-unit bitrate bound");
    int original = p->size;
    need(av_grow_packet(p, (int)r->nal->len + 4) >= 0,
         "metadata packet allocation");
    AV_WB32(p->data + original, r->nal->len);
    memcpy(p->data + original + 4, r->nal->data, r->nal->len);
    printf("{\"encoded_packet\":%d,\"renderer_frame\":%d,\"pts\":%lld,\"dts\":%"
           "lld,\"duration\":%lld,\"time_base\":\"%d/"
           "%d\",\"source_rpu_sha256\":\"%s\",\"adapted_rpu_sha256\":\"%s\"}\n",
           packets++, match, (long long)p->pts, (long long)p->dts,
           (long long)p->duration, src->time_base.num, src->time_base.den,
           r->source_hash, r->adapted_hash);
    int64_t original_dts = p->dts;
    av_packet_rescale_ts(p, src->time_base, dst->time_base);
    p->pos = -1;
    need(p->dts != AV_NOPTS_VALUE &&
             av_compare_ts(p->dts, dst->time_base, original_dts,
                           src->time_base) == 0,
         "container clock exactly represents encoder DTS");
    need(av_compare_ts(p->pts, dst->time_base, r->pts,
                       (AVRational){1, r->pd}) == 0 &&
             av_compare_ts(p->duration, dst->time_base, r->duration,
                           (AVRational){1, r->dd}) == 0,
         "container clock exactly represents renderer presentation interval");
    need(av_interleaved_write_frame(out, p) >= 0, "write authored packet");
    av_packet_unref(p);
  }
  need(rc == AVERROR_EOF && packets == count, "complete encoded generation");
  for (int i = 0; i < count; i++) {
    need(rows[i].seen, "no lost renderer frame");
    dovi_data_free(rows[i].nal);
  }
  need(av_write_trailer(out) >= 0 && avio_closep(&out->pb) >= 0,
       "finalize authored generation");
  av_packet_free(&p);
  avformat_close_input(&in);
  avformat_free_context(out);
  need(fflush(stdout) == 0, "receipt stdout flush");
  return 0;
}
