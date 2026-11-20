// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_M7_TITLE_CENSUS_H
#define PLURX_M7_TITLE_CENSUS_H

#include "coded_au.h"
#include "m7_limits.h"

#define M7_TITLE_CENSUS_WINDOWS 16
#define M7_TITLE_CENSUS_FRAMES M7_MAX_WINDOW_FRAMES
#define M7_TITLE_MAX_PACKET_BYTES (64 * 1024 * 1024)

struct m7_title_coded_frame {
  struct picture_time time;
  char au[65], rpu[65], vcl[65];
};
struct m7_title_census_window {
  int64_t first_pts, end_pts;
  unsigned count;
  struct m7_title_coded_frame frames[M7_TITLE_CENSUS_FRAMES];
};
static struct m7_title_census_window m7_windows[M7_TITLE_CENSUS_WINDOWS];
static unsigned m7_window_count;
static uint64_t m7_video_packet_bytes;

static int64_t m7_census_ticks(const char *text) {
  char *end;
  errno = 0;
  long long value = strtoll(text, &end, 10);
  need(!errno && end != text && !*end && value >= 0 && value < INT64_MAX,
       "bounded M7 census presentation time");
  return value;
}

static void m7_title_select_windows(int argc, char **argv) {
  need(argc >= 2 && argc % 2 == 0 && argc / 2 <= M7_TITLE_CENSUS_WINDOWS,
       "bounded M7 census windows");
  m7_window_count = (unsigned)argc / 2;
  for (unsigned i = 0; i < m7_window_count; i++) {
    struct m7_title_census_window *window = &m7_windows[i];
    window->first_pts = m7_census_ticks(argv[2 * i]);
    window->end_pts = m7_census_ticks(argv[2 * i + 1]);
    need(window->first_pts < window->end_pts &&
             (!i || m7_windows[i - 1].end_pts <= window->first_pts),
         "ordered disjoint M7 census windows");
  }
}

static bool m7_title_observe_coded(const AVPacket *packet) {
  struct m7_title_census_window *match = NULL;
  for (unsigned i = 0; i < m7_window_count; i++) {
    struct m7_title_census_window *window = &m7_windows[i];
    if (packet->pts >= window->first_pts && packet->pts < window->end_pts) {
      match = window;
      break;
    }
  }
  struct AVSHA *vcl = NULL;
  if (match) {
    need(match->count < M7_TITLE_CENSUS_FRAMES, "bounded M7 census frame count");
    vcl = av_sha_alloc();
    need(vcl && av_sha_init(vcl, 256) == 0, "M7 source VCL SHA256");
  }
  const struct coded_picture picture = inspect_coded_picture(packet, vcl, false);
  need(picture.bl == 1 && picture.el == 1 && picture.rpus == 1 &&
           picture.rpu_size > 2 && picture.rpu_size <= MAX_RPU_BYTES + 1 &&
           picture.rpu[0] == 0x7c && picture.rpu[1] == 1 && rpu.held &&
           rpu.size == picture.rpu_size - 2 &&
           memcmp(rpu.bytes, picture.rpu + 2, rpu.size) == 0,
       "one fresh same-access-unit BL, EL and RPU in the M7 selected track");
  need((uint64_t)packet->size <= UINT64_MAX - m7_video_packet_bytes,
       "bounded M7 selected video packet extent");
  m7_video_packet_bytes += (uint64_t)packet->size;
  if (!match) return false;

  struct m7_title_coded_frame *frame = &match->frames[match->count++];
  frame->time = (struct picture_time){packet->pts, packet->duration};
  hash_bytes(packet->data, packet->size, frame->au);
  hash_bytes(picture.rpu, picture.rpu_size, frame->rpu);
  finish_coded_vcl(vcl, frame->vcl);
  return true;
}

static int m7_title_by_pts(const void *left, const void *right) {
  const struct m7_title_coded_frame *a = left, *b = right;
  return by_pts(&a->time, &b->time);
}

// Only after complete selected-video EOF has authenticated the sorted
// presentation order. Persist only the requested bounded intervals.
static void m7_title_write_windows(FILE *file, const AVStream *stream, long index,
                                   int64_t first_pts, int64_t end_pts) {
  need(fprintf(file,
        "{\"schema\":2,\"video_index\":%ld,\"time_base_num\":%d,\"time_base_den\":%d,"
        "\"width\":%d,\"height\":%d,\"title_frames\":%"PRIu64
        ",\"video_packet_bytes\":%"PRIu64",\"first_pts\":%"PRId64",\"end_pts\":%"PRId64
        ",\"windows\":[",
        index, stream->time_base.num, stream->time_base.den, stream->codecpar->width,
        stream->codecpar->height, stats.frames, m7_video_packet_bytes, first_pts, end_pts) > 0,
       "M7 census header");
  for (unsigned i = 0; i < m7_window_count; i++) {
    struct m7_title_census_window *window = &m7_windows[i];
    need(window->count > 0, "complete M7 census interval");
    qsort(window->frames, window->count, sizeof(window->frames[0]), m7_title_by_pts);
    const struct m7_title_coded_frame *last = &window->frames[window->count - 1];
    need(window->frames[0].time.pts == window->first_pts &&
             last->time.pts + last->time.duration == window->end_pts,
         "exact M7 census interval boundaries");
    need(fprintf(file, "%s{\"first_pts\":%"PRId64",\"end_pts\":%"PRId64",\"frames\":[",
                 i ? "," : "", window->first_pts, window->end_pts) > 0, "M7 census window header");
    uint64_t previous = 0;
    for (unsigned j = 0; j < window->count; j++) {
      const struct m7_title_coded_frame *frame = &window->frames[j];
      const struct picture_time *when = bsearch(&frame->time, times, stats.frames,
                                                sizeof(*times), by_pts);
      need(when && when->duration == frame->time.duration, "M7 coded picture matches sorted title clock");
      uint64_t ordinal = (uint64_t)(when - times);
      need(!j || ordinal == previous + 1, "complete M7 source presentation ordinal coverage");
      previous = ordinal;
      need(fprintf(file, "%s{\"ordinal\":%"PRIu64",\"pts\":%"PRId64",\"duration\":%"PRId64
                   ",\"au\":\"%s\",\"rpu\":\"%s\",\"vcl\":\"%s\"}",
                   j ? "," : "", ordinal, frame->time.pts, frame->time.duration,
                   frame->au, frame->rpu, frame->vcl) > 0, "M7 census frame write");
    }
    need(fputs("]}", file) >= 0, "M7 census window completion");
  }
  need(fputs("],\"complete\":true}\n", file) >= 0 && fclose(file) == 0,
       "complete M7 census file");
}

#endif
