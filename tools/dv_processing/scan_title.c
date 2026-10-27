#define _POSIX_C_SOURCE 200809L
// Complete selected-video Dolby Vision metadata accounting, read from the
// coded bitstream. Nothing is decoded: a whole-title software decode spent
// hours of one core per title to learn facts the access units already carry.
//
// Each access unit is judged by the rules the renderer's own pipeline applies
// (dovi_split bl_rpu, then the HEVC decoder), so the counts describe exactly
// the pictures the renderer will see and the RPU each one is handed:
//  - enhancement-layer NALs (UNSPEC63) are removed before anything else;
//  - an access unit without a base-layer VCL NAL yields no picture;
//  - RASL pictures that follow an IRAP with NoRaslOutputFlag are not output;
//  - a picture's RPU is the last UNSPEC62 NAL after the first NAL with
//    nuh_layer_id 0, temporal id 0 and more than its two header bytes.
// Display-order coverage is proven from the presentation timestamps, which are
// the timestamps the decoder gives its frames.
//
// `scan_title --ride-along HELD_PIPE` reads the same selected stream from a
// pipe instead: the content-analysis pass's ffmpeg copies it there, unfiltered,
// as Matroska, while it reads the file for its index. A remux re-times packets
// into milliseconds, so that mode proves nothing from timestamps; it reports
// how many packets it read, and the daemon accepts the aggregate only when that
// equals the samples of the complete index built from the same demux.
//
// One difference is accepted: the decoder drops an RPU its own parser rejects,
// and this scanner can only ask libdovi. Both parse the same syntax; the
// renderer still refuses any picture that arrives without a fresh RPU.
#include <errno.h>
#include <inttypes.h>
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/parseutils.h>
#include <libdovi/rpu_parser.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "title_constants.h"

#define MAX_TITLE_FRAMES 100000000ULL
#define MAX_UNIT_NALS 4096

enum {
  NAL_RASL_N = 8, NAL_RASL_R = 9, NAL_BLA_W_LP = 16, NAL_BLA_N_LP = 18,
  NAL_IDR_W_RADL = 19, NAL_IDR_N_LP = 20, NAL_CRA = 21, NAL_IRAP_END = 23,
  NAL_EOS = 36, NAL_EOB = 37, NAL_RPU = 62, NAL_EL = 63,
};

struct title_stats {
  uint64_t frames, valid, missing, invalid;
  unsigned max_pq;
  struct dv_title_constants mastering;
};
struct picture_time {
  int64_t pts, duration;
};
struct nal {
  const uint8_t *data;
  size_t size;
};

static struct title_stats stats;
static struct picture_time *times;
static size_t time_capacity;
static struct nal nals[MAX_UNIT_NALS];
static uint64_t packets;
static bool ride_along;

static void need(bool ok, const char *why) {
  if (!ok) {
    fprintf(stderr, "DV title scan refused: %s\n", why);
    exit(1);
  }
}
static void unknown_constants(void) {
  dv_title_constants_unknown(&stats.mastering);
}
static int nal_type(const struct nal *nal) { return (nal->data[0] >> 1) & 0x3f; }
static int nal_layer(const struct nal *nal) {
  return ((nal->data[0] & 1) << 5) | (nal->data[1] >> 3);
}
static int nal_temporal_plus1(const struct nal *nal) { return nal->data[1] & 7; }

// A NAL whose two-byte header cannot be read is skipped, as the decoder's
// packet splitter skips it. Trailing zero bytes are never part of a NAL.
static size_t add_nal(size_t count, const uint8_t *data, size_t size) {
  while (size && !data[size - 1]) size--;
  if (size < 2 || (data[0] & 0x80) || !(data[1] & 7)) return count;
  need(count < MAX_UNIT_NALS, "bounded access-unit NAL count");
  nals[count].data = data;
  nals[count].size = size;
  return count + 1;
}
static size_t split_length_prefixed(const uint8_t *data, size_t size, int length_size) {
  size_t count = 0, at = 0;
  while (at < size) {
    need(size - at >= (size_t)length_size, "complete NAL length");
    size_t length = 0;
    for (int i = 0; i < length_size; i++) length = (length << 8) | data[at++];
    need(length <= size - at, "NAL inside its access unit");
    count = add_nal(count, data + at, length);
    at += length;
  }
  return count;
}
static size_t next_start_code(const uint8_t *data, size_t size, size_t at) {
  for (; at + 3 <= size; at++)
    if (!data[at] && !data[at + 1] && data[at + 2] == 1) return at;
  return size;
}
static size_t split_annex_b(const uint8_t *data, size_t size) {
  size_t count = 0, at = next_start_code(data, size, 0);
  while (at < size) {
    size_t start = at + 3, end = next_start_code(data, size, start);
    count = add_nal(count, data + start, end - start);
    at = end;
  }
  return count;
}

// The RPU payload as the decoder attaches it: raw bytes after the two-byte
// NAL header, emulation prevention intact. NULL when the picture gets none.
static void observe_rpu(const struct nal *rpu) {
  if (!rpu) {
    stats.missing++;
    unknown_constants();
    return;
  }
  size_t size = rpu->size - 2;
  if (!size || size > 4096) {
    stats.invalid++;
    unknown_constants();
    return;
  }
  uint8_t nal[4098] = {0x7c, 0x01};
  memcpy(nal + 2, rpu->data + 2, size);
  DoviRpuOpaque *parsed = dovi_parse_unspec62_nalu(nal, size + 2);
  const DoviVdrDmData *dm = NULL;
  const DoviRpuDataHeader *header = NULL;
  if (parsed && !dovi_rpu_get_error(parsed)) {
    header = dovi_rpu_get_header(parsed);
    dm = dovi_rpu_get_vdr_dm_data(parsed);
  }
  // The rendering contract admits fresh metadata only.
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
  dv_title_observe_l6(&stats.mastering, dm->dm_data.level6 != NULL,
      dm->dm_data.level6 ? dm->dm_data.level6->min_display_mastering_luminance : 0,
      dm->dm_data.level6 ? dm->dm_data.level6->max_display_mastering_luminance : 0);
  if (!dm->dm_data.level9) {
    dv_title_observe_l9(&stats.mastering, false, 0, NULL);
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
    dv_title_observe_l9(&stats.mastering, true, index, xy);
  }
done:
  if (dm) dovi_rpu_free_vdr_dm_data(dm);
  if (header) dovi_rpu_free_header(header);
  if (parsed) dovi_rpu_free(parsed);
}

// The decoder starts as if after an end of sequence, and an EOS/EOB NAL
// reaches only the next packet it is handed (or its own, at the front).
static bool seen_irap, eos_pending = true, skip_rasl;

// One demuxed access unit of the selected stream.
static void observe_unit(const AVPacket *packet, int length_size) {
  size_t count = length_size ? split_length_prefixed(packet->data, packet->size, length_size)
                             : split_annex_b(packet->data, packet->size);
  // dovi_split bl_rpu: the enhancement layer never reaches the decoder, and
  // a packet left empty is never handed to it at all.
  size_t kept = 0;
  for (size_t i = 0; i < count; i++)
    if (nal_type(&nals[i]) != NAL_EL) nals[kept++] = nals[i];
  if (!kept) return;
  bool last_eos = eos_pending, at_start = true;
  eos_pending = false;
  int picture = -1;
  for (size_t i = 0; i < kept; i++) {
    int type = nal_type(&nals[i]);
    if (type == NAL_EOS || type == NAL_EOB) {
      if (at_start) last_eos = true;
      else eos_pending = true;
    } else {
      at_start = false;
    }
    if (picture < 0 && type < 32 && !nal_layer(&nals[i])) picture = type;
  }
  if (picture < 0) return;
  if (picture >= NAL_BLA_W_LP && picture <= NAL_IRAP_END) {
    // NoRaslOutputFlag: always for IDR and BLA, for a CRA after an EOS.
    skip_rasl = picture <= NAL_IDR_N_LP || (picture == NAL_CRA && last_eos);
    seen_irap = true;
  } else {
    need(seen_irap, "title begins at a random access point");
    if ((picture == NAL_RASL_N || picture == NAL_RASL_R) && skip_rasl) return;
  }
  need(stats.frames < MAX_TITLE_FRAMES, "bounded title display-frame count");
  if (ride_along) goto timed;
  need(packet->pts >= 0 && packet->duration > 0 &&
       packet->pts < INT64_MAX - packet->duration,
       "observed complete display-frame timing");
  if (stats.frames == time_capacity) {
    size_t grown = time_capacity ? time_capacity * 2 : 65536;
    struct picture_time *resized = realloc(times, grown * sizeof(*times));
    need(resized != NULL, "bounded title timing buffer");
    times = resized;
    time_capacity = grown;
  }
  times[stats.frames].pts = packet->pts;
  times[stats.frames].duration = packet->duration;
timed:
  stats.frames++;
  const struct nal *rpu = NULL;
  for (size_t i = kept; i-- > 1;) {
    if (nal_type(&nals[i]) == NAL_RPU && nals[i].size > 2 && !nal_layer(&nals[i]) &&
        nal_temporal_plus1(&nals[i]) == 1) {
      rpu = &nals[i];
      break;
    }
  }
  observe_rpu(rpu);
}

static int by_pts(const void *a, const void *b) {
  int64_t x = ((const struct picture_time *)a)->pts;
  int64_t y = ((const struct picture_time *)b)->pts;
  return (x > y) - (x < y);
}

// The NAL length size of a length-prefixed (hvcC) stream, or 0 for Annex B,
// decided the way the decoder decides it.
static int nal_length_size(const AVCodecParameters *par) {
  const uint8_t *extra = par->extradata;
  if (par->extradata_size <= 3 || (!extra[0] && !extra[1] && extra[2] <= 1)) return 0;
  need(par->extradata_size >= 23, "complete HEVC decoder configuration");
  return (extra[21] & 3) + 1;
}

int main(int argc, char **argv) {
  need(argc == 3, "usage: scan_title HELD_SOURCE VIDEO_INDEX | scan_title --ride-along HELD_PIPE");
  ride_along = !strcmp(argv[1], "--ride-along");
  long index = 0;
  if (!ride_along) {
    char *end;
    errno = 0;
    index = strtol(argv[2], &end, 10);
    need(!errno && end != argv[2] && !*end && index >= 0 && index < 256,
         "selected video index");
  }
  av_log_set_level(AV_LOG_ERROR);
  av_max_alloc(64ULL * 1024 * 1024);
  AVFormatContext *input = avformat_alloc_context();
  need(input != NULL, "demux allocation");
  input->max_streams = 256;
  input->probesize = 1024 * 1024;
  input->max_analyze_duration = AV_TIME_BASE;
  const AVInputFormat *carrier = ride_along ? av_find_input_format("matroska") : NULL;
  need(!ride_along || carrier, "ride-along demuxer");
  need(avformat_open_input(&input, argv[ride_along ? 2 : 1], carrier, NULL) >= 0,
       "source open");
  need(!ride_along || input->nb_streams == 1, "one ride-along video stream");
  // Supported catalogue containers expose codec parameters on opening. Avoid
  // a second analysis pass and never seek past the start of the title.
  need((unsigned long)index < input->nb_streams, "selected stream exists");
  AVStream *stream = input->streams[index];
  need(stream->codecpar->codec_type == AVMEDIA_TYPE_VIDEO &&
       stream->codecpar->codec_id == AV_CODEC_ID_HEVC, "selected HEVC video");
  for (unsigned i = 0; i < input->nb_streams; i++)
    if (i != (unsigned long)index) input->streams[i]->discard = AVDISCARD_ALL;
  int length_size = nal_length_size(stream->codecpar);
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "bounded demux buffer");
  int ret;
  while ((ret = av_read_frame(input, packet)) >= 0) {
    if (packet->stream_index == index) {
      need(!(packet->flags & AV_PKT_FLAG_CORRUPT), "corrupt demux packet");
      packets++;
      observe_unit(packet, length_size);
    }
    av_packet_unref(packet);
  }
  need(ret == AVERROR_EOF && (!input->pb || !input->pb->error ||
       input->pb->error == AVERROR_EOF), "complete demux EOF");
  if (ride_along) {
    need(stats.frames > 0, "complete declared frame coverage");
    goto report;
  }
  need(stats.frames > 0 && (stream->nb_frames <= 0 ||
       (uint64_t)stream->nb_frames == stats.frames), "complete declared frame coverage");
  // Pictures leave the decoder in presentation order; prove that order is
  // contiguous, allowing one source tick of packet-clock rounding per step.
  qsort(times, stats.frames, sizeof(*times), by_pts);
  int64_t first_pts = times[0].pts, last_end = times[0].pts;
  for (uint64_t i = 0; i < stats.frames; i++) {
    need(i == 0 || (times[i].pts >= last_end - 1 && times[i].pts <= last_end + 1),
         "contiguous decoded title coverage");
    last_end = times[i].pts + times[i].duration;
  }
  need(stream->start_time != AV_NOPTS_VALUE && first_pts == stream->start_time &&
       last_end < INT64_MAX, "observed declared video origin");
  // Demuxers can return EOF for a truncated container without pb->error.
  // Require independent selected-video count/duration, never container/audio
  // duration, before EOF can establish a complete-title aggregate.
  bool endpoint_known = stream->nb_frames > 0;
  if (stream->duration > 0 && stream->duration != AV_NOPTS_VALUE) {
    need(stream->start_time < INT64_MAX - stream->duration,
         "bounded declared video end");
    int64_t declared_end = stream->start_time + stream->duration;
    need(last_end >= declared_end - 1 && last_end <= declared_end + 1,
         "complete selected video duration");
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
report:
  // `decoded_frames` keeps its schema-1 name: it counts the pictures the
  // decoder would output, established here without decoding them.
  printf("{\"schema\":1,\"complete\":true,\"decoded_frames\":%"PRIu64
         ",\"valid_l1_frames\":%"PRIu64",\"missing_l1_frames\":%"PRIu64
         ",\"invalid_l1_frames\":%"PRIu64",\"max_l1_pq\":",
         stats.frames, stats.valid, stats.missing, stats.invalid);
  if (stats.valid) printf("%u", stats.max_pq); else printf("null");
  printf(",\"mastering_luminance\":{");
  if (stats.mastering.l6_state == 1)
    printf("\"state\":\"consistent\",\"value\":{\"min_1e4_nits\":%u,\"max_nits\":%u}",
           stats.mastering.min_luma, stats.mastering.max_luma);
  else printf("\"state\":\"%s\"", stats.mastering.l6_state == 3 ? "inconsistent" : "unknown");
  printf("},\"mastering_primaries\":{");
  if (stats.mastering.l9_state == 1) {
    printf("\"state\":\"consistent\",\"value\":{\"index\":%u,\"custom_xy\":", stats.mastering.primary_index);
    if (stats.mastering.primary_index == 255) {
      printf("[");
      for (unsigned i = 0; i < 8; i++) printf("%s%u", i ? "," : "", stats.mastering.xy[i]);
      printf("]");
    } else printf("null");
    printf("}");
  } else printf("\"state\":\"%s\"", stats.mastering.l9_state == 3 ? "inconsistent" : "unknown");
  printf("}");
  if (ride_along) printf(",\"ride_along_packets\":%"PRIu64, packets);
  printf("}\n");
  need(!ferror(stdout) && fflush(stdout) == 0, "aggregate output");
  free(times);
  av_packet_free(&packet);
  avformat_close_input(&input);
  return 0;
}
