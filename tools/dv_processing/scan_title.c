#define _POSIX_C_SOURCE 200809L
// Complete selected-video frame accounting. Software decode only: this helper
// does not link libplacebo, create a GPU context, or emit reconstructed pixels.
#include <errno.h>
#include <inttypes.h>
#include <libavcodec/avcodec.h>
#include <libavcodec/bsf.h>
#include <libavformat/avformat.h>
#include <libavutil/opt.h>
#include <libavutil/parseutils.h>
#include <libdovi/rpu_parser.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct title_stats {
  uint64_t frames, valid, missing, invalid;
  unsigned max_pq;
  // 0: unseen, 1: consistent, 2: unknown, 3: inconsistent.
  int l6_state, l9_state;
  unsigned min_luma, max_luma, primary_index;
  uint16_t xy[8];
};
static struct title_stats stats;
static int64_t first_pts = AV_NOPTS_VALUE, last_end = AV_NOPTS_VALUE;
static void need(bool ok, const char *why) {
  if (!ok) {
    fprintf(stderr, "DV title scan refused: %s\n", why);
    exit(1);
  }
}
static void unknown_constants(void) {
  if (stats.l6_state != 3) stats.l6_state = 2;
  if (stats.l9_state != 3) stats.l9_state = 2;
}
static void observe(AVFrame *frame) {
  need(!(frame->flags & AV_FRAME_FLAG_CORRUPT) && !frame->decode_error_flags,
       "corrupt decoded display frame");
  need(frame->pts >= 0 && frame->duration > 0 &&
       frame->pts < INT64_MAX - frame->duration,
       "observed complete display-frame timing");
  if (first_pts == AV_NOPTS_VALUE) first_pts = frame->pts;
  // At most one source tick of packet-clock rounding, never a missing frame.
  if (last_end != AV_NOPTS_VALUE)
    need(frame->pts >= last_end - 1 && frame->pts <= last_end + 1,
         "contiguous decoded title coverage");
  last_end = frame->pts + frame->duration;
  need(stats.frames < 100000000, "bounded title display-frame count");
  stats.frames++;
  AVFrameSideData *side = av_frame_get_side_data(frame, AV_FRAME_DATA_DOVI_RPU_BUFFER);
  if (!side) {
    stats.missing++;
    unknown_constants();
    return;
  }
  if (!side->size || side->size > 4096) {
    stats.invalid++;
    unknown_constants();
    return;
  }
  uint8_t nal[4098] = {0x7c, 0x01};
  memcpy(nal + 2, side->data, side->size);
  DoviRpuOpaque *rpu = dovi_parse_unspec62_nalu(nal, side->size + 2);
  const DoviVdrDmData *dm = NULL;
  const DoviRpuDataHeader *header = NULL;
  if (rpu && !dovi_rpu_get_error(rpu)) {
    header = dovi_rpu_get_header(rpu);
    dm = dovi_rpu_get_vdr_dm_data(rpu);
  }
  // The rendering contract admits fresh metadata only. A reused/cached block
  // must not be called fresh just because the decoder retained side data.
  if (!header || header->use_prev_vdr_rpu_flag || !dm) {
    stats.invalid++;
    unknown_constants();
    goto done;
  }
  if (!dm->dm_data.level1) {
    stats.missing++;
  } else if (dm->dm_data.level1->max_pq > 4095 ||
             dm->dm_data.level1->avg_pq > dm->dm_data.level1->max_pq ||
             dm->dm_data.level1->min_pq > dm->dm_data.level1->avg_pq) {
    stats.invalid++;
  } else {
    stats.valid++;
    if (dm->dm_data.level1->max_pq > stats.max_pq)
      stats.max_pq = dm->dm_data.level1->max_pq;
  }
  if (!dm->dm_data.level6 || !dm->dm_data.level6->max_display_mastering_luminance ||
      dm->dm_data.level6->max_display_mastering_luminance > 10000 ||
      dm->dm_data.level6->min_display_mastering_luminance > 10000) {
    if (stats.l6_state != 3) stats.l6_state = 2;
  } else {
    unsigned min = dm->dm_data.level6->min_display_mastering_luminance;
    unsigned max = dm->dm_data.level6->max_display_mastering_luminance;
    if (!stats.l6_state) {
      stats.min_luma = min;
      stats.max_luma = max;
      stats.l6_state = 1;
    } else if (stats.l6_state == 1 &&
               (stats.min_luma != min || stats.max_luma != max)) {
      stats.l6_state = 3;
    }
  }
  if (!dm->dm_data.level9) {
    if (stats.l9_state != 3) stats.l9_state = 2;
  } else {
    unsigned index = dm->dm_data.level9->source_primary_index;
    uint16_t xy[8] = {0};
    if (index == 255) {
      xy[0] = dm->dm_data.level9->source_primary_red_x;
      xy[1] = dm->dm_data.level9->source_primary_red_y;
      xy[2] = dm->dm_data.level9->source_primary_green_x;
      xy[3] = dm->dm_data.level9->source_primary_green_y;
      xy[4] = dm->dm_data.level9->source_primary_blue_x;
      xy[5] = dm->dm_data.level9->source_primary_blue_y;
      xy[6] = dm->dm_data.level9->source_primary_white_x;
      xy[7] = dm->dm_data.level9->source_primary_white_y;
    }
    if (!stats.l9_state) {
      stats.primary_index = index;
      memcpy(stats.xy, xy, sizeof(xy));
      stats.l9_state = 1;
    } else if (stats.l9_state == 1 &&
               (stats.primary_index != index || memcmp(stats.xy, xy, sizeof(xy)))) {
      stats.l9_state = 3;
    }
  }
done:
  if (dm) dovi_rpu_free_vdr_dm_data(dm);
  if (header) dovi_rpu_free_header(header);
  if (rpu) dovi_rpu_free(rpu);
}
static int receive(AVCodecContext *decoder, AVFrame *frame) {
  int ret;
  while ((ret = avcodec_receive_frame(decoder, frame)) >= 0) {
    observe(frame);
    av_frame_unref(frame);
  }
  need(ret == AVERROR(EAGAIN) || ret == AVERROR_EOF, "video decode");
  return ret;
}
static int split_receive(AVBSFContext *split, AVCodecContext *decoder,
                         AVPacket *packet, AVFrame *frame) {
  int ret;
  while ((ret = av_bsf_receive_packet(split, packet)) >= 0) {
    need(!(packet->flags & AV_PKT_FLAG_CORRUPT), "corrupt video packet");
    need(avcodec_send_packet(decoder, packet) >= 0, "decoder send");
    receive(decoder, frame);
    av_packet_unref(packet);
  }
  need(ret == AVERROR(EAGAIN) || ret == AVERROR_EOF, "base split");
  return ret;
}
int main(int argc, char **argv) {
  need(argc == 3, "usage: scan_title HELD_SOURCE VIDEO_INDEX");
  char *end;
  errno = 0;
  long index = strtol(argv[2], &end, 10);
  need(!errno && end != argv[2] && !*end && index >= 0 && index < 256,
       "selected video index");
  av_log_set_level(AV_LOG_ERROR);
  av_max_alloc(64ULL * 1024 * 1024);
  AVFormatContext *input = avformat_alloc_context();
  need(input != NULL, "demux allocation");
  input->max_streams = 256;
  input->probesize = 1024 * 1024;
  input->max_analyze_duration = AV_TIME_BASE;
  need(avformat_open_input(&input, argv[1], NULL, NULL) >= 0, "source open");
  // Supported catalogue containers expose codec parameters on opening. Avoid
  // a second analysis decoder and never seek past the start of the title.
  need((unsigned long)index < input->nb_streams, "selected stream exists");
  AVStream *stream = input->streams[index];
  need(stream->codecpar->codec_type == AVMEDIA_TYPE_VIDEO &&
       stream->codecpar->codec_id == AV_CODEC_ID_HEVC, "selected HEVC video");
  for (unsigned i = 0; i < input->nb_streams; i++)
    if (i != (unsigned long)index) input->streams[i]->discard = AVDISCARD_ALL;
  const AVBitStreamFilter *filter = av_bsf_get_by_name("dovi_split");
  AVBSFContext *split = NULL;
  need(filter && av_bsf_alloc(filter, &split) >= 0, "base splitter");
  need(avcodec_parameters_copy(split->par_in, stream->codecpar) >= 0,
       "split parameters");
  split->time_base_in = stream->time_base;
  need(av_opt_set(split, "mode", "bl_rpu", AV_OPT_SEARCH_CHILDREN) >= 0 &&
       av_bsf_init(split) >= 0, "base splitter open");
  const AVCodec *codec = avcodec_find_decoder(AV_CODEC_ID_HEVC);
  need(codec != NULL, "software HEVC decoder");
  AVCodecContext *decoder = avcodec_alloc_context3(codec);
  need(decoder && avcodec_parameters_to_context(decoder, split->par_out) >= 0,
       "decoder parameters");
  decoder->thread_count = 1;
  decoder->pkt_timebase = stream->time_base;
  decoder->max_pixels = 4096 * 2160;
  decoder->err_recognition = AV_EF_CAREFUL | AV_EF_EXPLODE;
  need(avcodec_open2(decoder, codec, NULL) >= 0, "software decoder open");
  AVPacket *packet = av_packet_alloc(), *out = av_packet_alloc();
  AVFrame *frame = av_frame_alloc();
  need(packet && out && frame, "bounded decode buffers");
  int ret;
  while ((ret = av_read_frame(input, packet)) >= 0) {
    if (packet->stream_index == index) {
      need(!(packet->flags & AV_PKT_FLAG_CORRUPT), "corrupt demux packet");
      need(av_bsf_send_packet(split, packet) >= 0, "split send");
      split_receive(split, decoder, out, frame);
    }
    av_packet_unref(packet);
  }
  need(ret == AVERROR_EOF && (!input->pb || !input->pb->error ||
       input->pb->error == AVERROR_EOF), "complete demux EOF");
  need(av_bsf_send_packet(split, NULL) >= 0 &&
       split_receive(split, decoder, out, frame) == AVERROR_EOF, "complete split drain");
  need(avcodec_send_packet(decoder, NULL) >= 0 &&
       receive(decoder, frame) == AVERROR_EOF, "complete decoder drain");
  need(stats.frames > 0 && (stream->nb_frames <= 0 ||
       (uint64_t)stream->nb_frames == stats.frames), "complete declared frame coverage");
  need(stream->start_time != AV_NOPTS_VALUE && first_pts == stream->start_time &&
       last_end < INT64_MAX, "observed declared video origin");
  // Demuxers can return EOF for a truncated container without pb->error.
  // Require independent selected-video count/duration, never container/audio
  // duration, before EOF can establish a complete-title aggregate.
  bool endpoint_known = stream->nb_frames > 0;
  if (stream->duration > 0 && stream->duration != AV_NOPTS_VALUE) {
    need(stream->start_time < INT64_MAX - stream->duration,
         "bounded declared video end");
    int64_t end = stream->start_time + stream->duration;
    need(last_end >= end - 1 && last_end <= end + 1, "complete selected video duration");
    endpoint_known = true;
  }
  const AVDictionaryEntry *duration = av_dict_get(stream->metadata, "DURATION", NULL, 0);
  if (duration) {
    int64_t duration_us;
    need(av_parse_time(&duration_us, duration->value, 1) >= 0 && duration_us > 0,
         "selected video duration tag");
    // Matroska's selected-track DURATION tag is a timestamp endpoint, unlike
    // the relative AVStream.duration above. Do not subtract a nonzero origin.
    need(av_compare_ts(last_end + 1, stream->time_base,
                      duration_us, AV_TIME_BASE_Q) >= 0 &&
         av_compare_ts(last_end - 1, stream->time_base,
                      duration_us, AV_TIME_BASE_Q) <= 0,
         "complete selected video duration tag");
    endpoint_known = true;
  }
  need(endpoint_known, "independent complete selected-video endpoint unavailable");
  printf("{\"schema\":1,\"complete\":true,\"decoded_frames\":%"PRIu64
         ",\"valid_l1_frames\":%"PRIu64",\"missing_l1_frames\":%"PRIu64
         ",\"invalid_l1_frames\":%"PRIu64",\"max_l1_pq\":",
         stats.frames, stats.valid, stats.missing, stats.invalid);
  if (stats.valid) printf("%u", stats.max_pq); else printf("null");
  printf(",\"mastering_luminance\":{");
  if (stats.l6_state == 1)
    printf("\"state\":\"consistent\",\"value\":{\"min_1e4_nits\":%u,\"max_nits\":%u}",
           stats.min_luma, stats.max_luma);
  else printf("\"state\":\"%s\"", stats.l6_state == 3 ? "inconsistent" : "unknown");
  printf("},\"mastering_primaries\":{");
  if (stats.l9_state == 1) {
    printf("\"state\":\"consistent\",\"value\":{\"index\":%u,\"custom_xy\":", stats.primary_index);
    if (stats.primary_index == 255) {
      printf("[");
      for (unsigned i = 0; i < 8; i++) printf("%s%u", i ? "," : "", stats.xy[i]);
      printf("]");
    } else printf("null");
    printf("}");
  } else printf("\"state\":\"%s\"", stats.l9_state == 3 ? "inconsistent" : "unknown");
  printf("}}\n");
  need(!ferror(stdout) && fflush(stdout) == 0, "aggregate output");
  av_frame_free(&frame);
  av_packet_free(&packet);
  av_packet_free(&out);
  avcodec_free_context(&decoder);
  av_bsf_free(&split);
  avformat_close_input(&input);
  return 0;
}
