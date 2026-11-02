#define _POSIX_C_SOURCE 200809L
// Complete selected-video Dolby Vision metadata accounting, read from the
// coded bitstream. Nothing is decoded: a whole-title software decode spent
// hours of one core per title to learn facts the access units already carry.
//
// Each access unit is judged by the rules the renderer's own pipeline applies
// (dovi_split bl_rpu, then the HEVC decoder), so the counts describe exactly
// the pictures the renderer will see and the RPU each one is handed:
//  - NALs are split as ff_h2645_packet_split splits them (length-prefixed
//    input only; every catalogued Dolby Vision title is Matroska or MP4);
//  - enhancement-layer NALs (UNSPEC63) never reach the decoder;
//  - a packet with a base-layer VCL NAL of a decodable type is a picture;
//  - RASL pictures that follow an IRAP with NoRaslOutputFlag are not output;
//  - a picture's RPU is the last UNSPEC62 NAL after the first NAL with
//    nuh_layer_id 0, temporal id 0 and more than its two header bytes; an RPU
//    in a packet that produced no picture is carried to the next picture.
// Where the decoder would refuse a picture or withhold pictures from output,
// the scan refuses, as the decoding scanner did: a first slice missing or
// repeated, an unknown PPS, an oversize picture, pictures discarded at a new
// sequence (no_output_of_prior_pics_flag), or a PPS that lets slices hide
// their picture (output_flag_present_flag). Display-order coverage is proven
// from the presentation timestamps, which are the timestamps the decoder gives
// its frames.
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
#define MAX_RPU_BYTES 4096

enum {
  NAL_RASL_N = 8, NAL_RASL_R = 9, NAL_BLA_W_LP = 16, NAL_IDR_W_RADL = 19,
  NAL_IDR_N_LP = 20, NAL_CRA = 21, NAL_PPS = 34, NAL_EOS = 36, NAL_EOB = 37,
  NAL_RPU = 62, NAL_EL = 63,
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
static bool known_pps[64];

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
// Types the decoder decodes; reserved VCL types are skipped like any unknown NAL.
static bool picture_type(int type) { return type <= 9 || (type >= 16 && type <= 21); }

// RBSP bit reader over a NAL payload (after its header), skipping emulation
// prevention bytes. Reads past the end yield zeros and mark the reader short.
struct bits {
  const uint8_t *data;
  size_t size, at;
  int bit, zeros;
  bool short_read;
};
static unsigned bit1(struct bits *b) {
  if (!b->bit) {
    if (b->at < b->size && b->zeros >= 2 && b->data[b->at] == 3) {
      b->at++;
      b->zeros = 0;
    }
    if (b->at >= b->size) {
      b->short_read = true;
      return 0;
    }
    b->zeros = b->data[b->at] ? 0 : b->zeros + 1;
  }
  unsigned value = (b->data[b->at] >> (7 - b->bit)) & 1;
  if (++b->bit == 8) {
    b->bit = 0;
    b->at++;
  }
  return value;
}
static uint32_t golomb(struct bits *b) {
  int leading = 0;
  while (!bit1(b) && !b->short_read)
    if (++leading > 31) {
      b->short_read = true;
      return 0;
    }
  uint32_t value = 0;
  for (int i = 0; i < leading; i++) value = (value << 1) | bit1(b);
  return (UINT32_C(1) << leading) - 1 + value;
}
static struct bits payload_bits(const uint8_t *data, size_t size) {
  struct bits b = {data + 2, size - 2, 0, 0, 0, false};
  return b;
}

// A PPS the decoder could use: its id, and whether it lets slices withhold
// their picture from output.
static void observe_pps(const uint8_t *data, size_t size) {
  if (size < 3) return;
  struct bits b = payload_bits(data, size);
  uint32_t id = golomb(&b);
  (void)golomb(&b);
  (void)bit1(&b);
  unsigned output_flag_present = bit1(&b);
  if (b.short_read || id >= 64) return;
  need(!output_flag_present, "a PPS lets slices withhold pictures from output");
  known_pps[id] = true;
}

// A NAL the decoder's splitter keeps: a readable header, forbidden bit clear,
// temporal id present, nuh_layer_id below 63, and a payload. The enhancement
// layer is removed here, before anything is counted, as dovi_split removes it.
static size_t add_nal(size_t count, const uint8_t *data, size_t size) {
  size_t trimmed = size;
  while (trimmed > 2 && !data[trimmed - 1]) trimmed--;
  if (size < 2 || trimmed < 2 || (data[0] & 0x80) || !(data[1] & 7)) return count;
  struct nal nal = {data, size};
  if (nal_layer(&nal) == 63 || nal_type(&nal) == NAL_EL) return count;
  need(count < MAX_UNIT_NALS, "bounded access-unit NAL count");
  nals[count] = nal;
  return count + 1;
}
// ff_h2645_packet_split over length-prefixed input: a NAL needs at least four
// bytes left to start, and a zero or overrunning length refuses the packet.
static size_t split_length_prefixed(const uint8_t *data, size_t size, int length_size) {
  size_t count = 0, at = 0;
  while (size - at >= 4) {
    need(size - at > (size_t)length_size, "complete NAL length");
    size_t length = 0;
    for (int i = 0; i < length_size; i++) length = (length << 8) | data[at++];
    need(length > 0 && length <= size - at, "NAL inside its access unit");
    count = add_nal(count, data + at, length);
    at += length;
  }
  return count;
}

// The decoder's held RPU: raw bytes after the two-byte NAL header, emulation
// prevention intact, exactly what it attaches to the next picture it starts.
static struct {
  bool held;
  size_t size;
  uint8_t bytes[MAX_RPU_BYTES];
} rpu;

static void hold_rpu(const struct nal *nal) {
  rpu.held = true;
  rpu.size = nal->size - 2;
  if (rpu.size <= MAX_RPU_BYTES) memcpy(rpu.bytes, nal->data + 2, rpu.size);
}

// One picture's metadata: the RPU the decoder attaches to it, if any.
static void observe_rpu(void) {
  if (!rpu.held) {
    stats.missing++;
    unknown_constants();
    return;
  }
  rpu.held = false;
  size_t size = rpu.size;
  if (!size || size > MAX_RPU_BYTES) {
    stats.invalid++;
    unknown_constants();
    return;
  }
  uint8_t nal[MAX_RPU_BYTES + 2] = {0x7c, 0x01};
  memcpy(nal + 2, rpu.bytes, size);
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
static bool seen_picture, eos_pending = true, skip_rasl;

// One demuxed access unit of the selected stream.
static void observe_unit(const AVPacket *packet, int length_size) {
  size_t count = split_length_prefixed(packet->data, packet->size, length_size);
  // dovi_split hands the decoder nothing for a packet it emptied.
  if (!count) return;
  bool last_eos = eos_pending, at_start = true;
  eos_pending = false;
  // The RPU: the last qualifying UNSPEC62 after the first NAL. It replaces
  // one still held from a packet that produced no picture.
  for (size_t i = count; i-- > 1;) {
    if (nal_type(&nals[i]) == NAL_RPU && nals[i].size > 2 && !nal_layer(&nals[i]) &&
        nal_temporal_plus1(&nals[i]) == 1) {
      hold_rpu(&nals[i]);
      break;
    }
  }
  int picture = -1;
  bool no_output_of_prior_pics = false;
  for (size_t i = 0; i < count; i++) {
    int type = nal_type(&nals[i]);
    if (type == NAL_EOS || type == NAL_EOB) {
      if (at_start) last_eos = true;
      else eos_pending = true;
    } else {
      at_start = false;
    }
    if (nal_layer(&nals[i])) continue;
    if (type == NAL_PPS) {
      observe_pps(nals[i].data, nals[i].size);
      continue;
    }
    if (!picture_type(type)) continue;
    struct bits b = payload_bits(nals[i].data, nals[i].size);
    bool first_slice = bit1(&b);
    bool irap = type >= NAL_BLA_W_LP;
    if (picture < 0) {
      need(first_slice, "first slice in a picture missing");
      picture = type;
      if (irap) no_output_of_prior_pics = bit1(&b);
      uint32_t pps = golomb(&b);
      need(!b.short_read && pps < 64 && known_pps[pps], "slice names an unknown PPS");
    } else {
      need(!first_slice, "two first slices in one picture");
    }
  }
  if (picture < 0) return;
  bool irap = picture >= NAL_BLA_W_LP;
  // A new sequence outputs or discards the pictures still waiting: discarding
  // them leaves a hole in the title the decoding scanner refused as one.
  bool idr_or_bla = picture >= NAL_BLA_W_LP && picture <= NAL_IDR_N_LP;
  bool new_sequence = idr_or_bla || last_eos;
  if (seen_picture && new_sequence)
    need(!(no_output_of_prior_pics || (picture == NAL_CRA && last_eos)),
         "pictures discarded at a new sequence");
  if (irap) {
    // NoRaslOutputFlag: always for IDR and BLA, for a CRA after an EOS.
    skip_rasl = picture <= NAL_IDR_N_LP || (picture == NAL_CRA && last_eos);
  } else {
    need(seen_picture, "title begins at a random access point");
    // The RPU stays held: the decoder never starts this picture.
    if ((picture == NAL_RASL_N || picture == NAL_RASL_R) && skip_rasl) return;
  }
  seen_picture = true;
  need(packet->pts >= 0 && packet->duration > 0 &&
       packet->pts < INT64_MAX - packet->duration,
       "observed complete display-frame timing");
  need(stats.frames < MAX_TITLE_FRAMES, "bounded title display-frame count");
  if (stats.frames == time_capacity) {
    size_t grown = time_capacity ? time_capacity * 2 : 65536;
    struct picture_time *resized = realloc(times, grown * sizeof(*times));
    need(resized != NULL, "bounded title timing buffer");
    times = resized;
    time_capacity = grown;
  }
  times[stats.frames].pts = packet->pts;
  times[stats.frames].duration = packet->duration;
  stats.frames++;
  observe_rpu();
}

static int by_pts(const void *a, const void *b) {
  int64_t x = ((const struct picture_time *)a)->pts;
  int64_t y = ((const struct picture_time *)b)->pts;
  return (x > y) - (x < y);
}

// The NAL length size of a length-prefixed stream, from a version-1 HEVC
// decoder configuration record; its parameter sets seed the known PPSs.
static int configuration(const AVCodecParameters *par) {
  const uint8_t *extra = par->extradata;
  size_t size = (size_t)par->extradata_size;
  need(size >= 23 && extra[0] == 1, "length-prefixed HEVC with a version-1 configuration");
  size_t at = 23;
  for (unsigned arrays = extra[22]; arrays; arrays--) {
    need(size - at >= 3, "complete configuration array");
    unsigned type = extra[at] & 0x3f, count = ((unsigned)extra[at + 1] << 8) | extra[at + 2];
    at += 3;
    for (; count; count--) {
      need(size - at >= 2, "complete configuration NAL length");
      size_t length = ((size_t)extra[at] << 8) | extra[at + 1];
      at += 2;
      need(length <= size - at, "configuration NAL inside its record");
      if (type == NAL_PPS && length > 2) observe_pps(extra + at, length);
      at += length;
    }
  }
  return (extra[21] & 3) + 1;
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
  // a second analysis pass and never seek past the start of the title.
  need((unsigned long)index < input->nb_streams, "selected stream exists");
  AVStream *stream = input->streams[index];
  need(stream->codecpar->codec_type == AVMEDIA_TYPE_VIDEO &&
       stream->codecpar->codec_id == AV_CODEC_ID_HEVC, "selected HEVC video");
  // The decoding scanner's max_pixels.
  need(stream->codecpar->width > 0 && stream->codecpar->height > 0 &&
       (int64_t)stream->codecpar->width * stream->codecpar->height <= 4096 * 2160,
       "bounded picture size");
  for (unsigned i = 0; i < input->nb_streams; i++)
    if (i != (unsigned long)index) input->streams[i]->discard = AVDISCARD_ALL;
  int length_size = configuration(stream->codecpar);
  AVPacket *packet = av_packet_alloc();
  need(packet != NULL, "bounded demux buffer");
  int ret;
  while ((ret = av_read_frame(input, packet)) >= 0) {
    if (packet->stream_index == index) {
      need(!(packet->flags & AV_PKT_FLAG_CORRUPT), "corrupt demux packet");
      observe_unit(packet, length_size);
    }
    av_packet_unref(packet);
  }
  need(ret == AVERROR_EOF && (!input->pb || !input->pb->error ||
       input->pb->error == AVERROR_EOF), "complete demux EOF");
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
  printf("}}\n");
  need(!ferror(stdout) && fflush(stdout) == 0, "aggregate output");
  free(times);
  av_packet_free(&packet);
  avformat_close_input(&input);
  return 0;
}
