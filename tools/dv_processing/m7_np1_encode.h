// SPDX-License-Identifier: Apache-2.0
#ifndef PLURX_M7_NP1_ENCODE_H
#define PLURX_M7_NP1_ENCODE_H

#include <fcntl.h>
#include <libavutil/intreadwrite.h>
#include <libavutil/mem.h>
#include <libavutil/sha.h>
#include <stdbool.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "m7_limits.h"

/*
 * mux_rgb --m7-encode-np1-v1
 * Inherited fd 4 is the owned output directory. Fd 6 contains packed
 * composed yuv420p10le frames (Y, Cb, Cr). Fd 7 contains the PLXM7R01 plan.
 * Outputs within fd 4 are encoded.mp4 and encode.receipt, both exclusive.
 *
 * Plan scalars and ROI records are little-endian; digests are raw SHA-256.
 * Header (212 bytes): 8-byte magic, twelve u32s (mode 0=no ROI/1=zero/2=NP1,
 * width, height, frames, timescale, frame_ticks, ABR kb/s, initial ABR kb/s,
 * authored rate low/high kb/s, light-present, reserved=0), eight u16 ST2086
 * xy coordinates in RGBW order, two u32 mastering luminances max/min in
 * 1/10000 nits, two u16 MaxCLL/MaxFALL, four digests (frozen recipe, frozen
 * protocol, composed package, independently verified equivalence gates).
 * Gate digest is zero for controls and nonzero for NP1.
 * Each frame has 204 bytes: u64 source ordinal, i64 source PTS/duration,
 * u32 source time-base numerator/denominator, i64 encoder PTS, u32 ROI
 * count, five digests (original AU, RPU, VCL, raw BL, composed planes), then
 * count 16-byte ROIs (u16 left/top/right/bottom, i32 qoffset numerator,
 * u32 denominator), in the priority order emitted by the Rust map.
 * Trailer (24 bytes): END1, u32 body length 16, u64 frame and ROI totals.
 *
 * Receipt: same header with PLXM7E01 magic. CFG1 has a 256-byte body:
 * u32 avcodec/avformat/avutil versions, unused-option mask (bit 0 is
 * sc_threshold=0), thread count, GOP, max B frames, buffer bits, then i64
 * effective context target/peak bps, u64 output extent and actual mux packet
 * payload bytes, then six digests (plan, requested avcodec dictionary,
 * codec-open extradata, mux readback extradata, MP4, avcodec configuration).
 * Dictionary hash covers each key and value with a NUL delimiter, in the
 * order they were inserted. Configuration hash excludes its final NUL.
 * AU01 (364-byte body) per frame: original 204-byte plan row, SHA-256 of
 * ordered ROI bytes, raw encoder packet, muxed packet, and muxed length-
 * prefixed VCL NALs, i64 muxed PTS/DTS/duration, u32 packet bytes and flags
 * (bit 0 is key). END1 uses the same counts as the plan, followed by EOF.
 * The daemon owns launch/reap, generation checks, independent validation
 * of coded controls, and the authored-rate and publication decisions.
 */

enum {
  M7_NP1_DIRECTORY_FD = 4,
  M7_NP1_PLANES_FD = 6,
  M7_NP1_PLAN_FD = 7,
  M7_NP1_FRAMES = M7_MAX_WINDOW_FRAMES,
  M7_NP1_GRID = 16,
  M7_NP1_MAX_ROIS = (M7_MAX_RASTER_WIDTH / M7_NP1_GRID) *
                   (M7_MAX_RASTER_HEIGHT / M7_NP1_GRID) + 1,
  M7_NP1_RECIPE_HEADER = 212,
  M7_NP1_FRAME_ROW = 204,
  M7_NP1_ROI_BYTES = 16,
  M7_NP1_TRAILER = 24,
  M7_NP1_CFG_BYTES = 256,
  M7_NP1_AU_BYTES = 364,
  M7_NP1_HASH_BYTES = 32,
  M7_NP1_MAX_OUTPUT = 64 * 1024 * 1024,
  M7_NP1_AVIO_BUFFER = 65536,
  M7_NP1_MIN_ABR = 83334,
  M7_NP1_PEAK_ABR = 125000,
  M7_NP1_BUF_KBIT = 136000,
};

struct m7_np1_recipe {
  uint8_t wire[M7_NP1_RECIPE_HEADER];
  uint32_t width, height, timescale, frame_ticks, target;
  uint32_t mode, light_present, max_luminance, min_luminance;
  uint16_t primaries[8], cll, fall;
};

struct m7_np1_row {
  uint8_t wire[M7_NP1_FRAME_ROW];
  off_t roi_offset;
  uint32_t roi_count;
  uint8_t roi_sha[M7_NP1_HASH_BYTES], raw_au_sha[M7_NP1_HASH_BYTES];
  uint8_t mux_au_sha[M7_NP1_HASH_BYTES], mux_vcl_sha[M7_NP1_HASH_BYTES];
  uint32_t packet_bytes, packet_flags;
};

struct m7_np1_recipe_read {
  struct m7_np1_recipe recipe;
  struct m7_np1_row rows[M7_NP1_FRAMES];
  uint64_t roi_total;
  off_t plan_extent;
  uint8_t plan_sha[M7_NP1_HASH_BYTES];
};

struct m7_np1_output {
  int fd;
  int64_t position, extent, read_bytes;
};

static void m7_np1_need(bool ok, const char *what) {
  if (!ok) {
    fprintf(stderr, "M7 NP1 refused: %s\n", what);
    exit(1);
  }
}

static void m7_np1_av(int status, const char *what) {
  if (status < 0) {
    char detail[AV_ERROR_MAX_STRING_SIZE] = {0};
    av_strerror(status, detail, sizeof(detail));
    fprintf(stderr, "M7 NP1 refused: %s: %s\n", what, detail);
    exit(1);
  }
}

static struct AVSHA *m7_np1_sha_new(void) {
  struct AVSHA *sha = av_sha_alloc();
  m7_np1_need(sha != NULL && av_sha_init(sha, 256) == 0, "SHA-256 allocation");
  return sha;
}

static void m7_np1_hash(const void *bytes, size_t size, uint8_t digest[M7_NP1_HASH_BYTES]) {
  struct AVSHA *sha = m7_np1_sha_new();
  av_sha_update(sha, bytes, size);
  av_sha_final(sha, digest);
  av_free(sha);
}

static bool m7_np1_zero_hash(const uint8_t *bytes) {
  static const uint8_t zero[M7_NP1_HASH_BYTES] = {0};
  return memcmp(bytes, zero, sizeof(zero)) == 0;
}

static off_t m7_np1_input_size(int fd) {
  struct stat facts;
  int flags = fcntl(fd, F_GETFL);
  m7_np1_need(flags >= 0 && (flags & O_ACCMODE) == O_RDONLY &&
                  fstat(fd, &facts) == 0 && S_ISREG(facts.st_mode) && facts.st_size > 0,
              "owned read-only regular input descriptor");
  return facts.st_size;
}

static void m7_np1_pread(int fd, void *bytes, size_t size, off_t offset) {
  uint8_t *out = bytes;
  size_t read = 0;
  while (read < size) {
    ssize_t count = pread(fd, out + read, size - read, offset + (off_t)read);
    if (count < 0 && errno == EINTR) continue;
    m7_np1_need(count > 0, "complete held input bytes");
    read += (size_t)count;
  }
}

static void m7_np1_plan_bytes(void *data, size_t size, off_t *offset, off_t extent,
                              struct AVSHA *sha) {
  m7_np1_need(*offset <= extent && size <= (uint64_t)(extent - *offset),
              "bounded recipe extent");
  m7_np1_pread(M7_NP1_PLAN_FD, data, size, *offset);
  *offset += (off_t)size;
  av_sha_update(sha, data, size);
}

static void m7_np1_validate_recipe(struct m7_np1_recipe *recipe) {
  const uint8_t *h = recipe->wire;
  m7_np1_need(!memcmp(h, "PLXM7R01", 8), "recipe version");
  recipe->mode = AV_RL32(h + 8);
  recipe->width = AV_RL32(h + 12);
  recipe->height = AV_RL32(h + 16);
  recipe->timescale = AV_RL32(h + 24);
  recipe->frame_ticks = AV_RL32(h + 28);
  recipe->target = AV_RL32(h + 32);
  uint32_t initial = AV_RL32(h + 36);
  uint32_t low = AV_RL32(h + 40), high = AV_RL32(h + 44);
  recipe->light_present = AV_RL32(h + 48);
  for (unsigned i = 0; i < 8; i++)
    recipe->primaries[i] = AV_RL16(h + 56 + 2 * i);
  recipe->max_luminance = AV_RL32(h + 72);
  recipe->min_luminance = AV_RL32(h + 76);
  recipe->cll = AV_RL16(h + 80);
  recipe->fall = AV_RL16(h + 82);
  m7_np1_need(recipe->mode <= 2 &&
                  m7_supported_raster(recipe->width, recipe->height) &&
                  AV_RL32(h + 20) == M7_NP1_FRAMES &&
                  ((recipe->timescale == 24 && recipe->frame_ticks == 1) ||
                   (recipe->timescale == 24000 && recipe->frame_ticks == 1001)) &&
                  initial >= M7_NP1_MIN_ABR && initial <= M7_NP1_PEAK_ABR &&
                  recipe->target >= M7_NP1_MIN_ABR && recipe->target <= M7_NP1_PEAK_ABR &&
                  (recipe->mode == 2 || recipe->target == initial) &&
                  low > 0 && high >= low && AV_RL32(h + 52) == 0,
              "frozen NP1 raster, cadence and rate controls");
  for (unsigned i = 0; i < 8; i += 2) {
    unsigned x = recipe->primaries[i], y = recipe->primaries[i+1];
    m7_np1_need(y > 0 && x + y <= 50000, "valid source ST 2086 coordinates");
  }
  m7_np1_need(recipe->min_luminance < recipe->max_luminance &&
                  recipe->max_luminance <= 100000000 &&
                  ((recipe->light_present == 0 && !recipe->cll && !recipe->fall) ||
                   (recipe->light_present == 1 && recipe->cll > 0 && recipe->cll <= 10000 &&
                    recipe->fall <= recipe->cll)),
              "valid source HDR metadata");
  m7_np1_need(!m7_np1_zero_hash(h + 84) && !m7_np1_zero_hash(h + 116) &&
                  !m7_np1_zero_hash(h + 148) &&
                  (m7_np1_zero_hash(h + 180) == (recipe->mode != 2)),
              "recipe, protocol, input and control gate bindings");
}

static AVRegionOfInterest m7_np1_decode_roi(const uint8_t *r) {
  return (AVRegionOfInterest){
      .self_size = sizeof(AVRegionOfInterest),
      .left = AV_RL16(r), .top = AV_RL16(r + 2),
      .right = AV_RL16(r + 4), .bottom = AV_RL16(r + 6),
      .qoffset = {(int32_t)AV_RL32(r + 8), (int)AV_RL32(r + 12)},
  };
}

static unsigned m7_np1_validate_rois(const struct m7_np1_recipe *recipe, const uint8_t *rois,
                                      uint32_t count) {
  if (recipe->mode == 0) {
    m7_np1_need(count == 0, "none gate carries no ROI");
    return 0;
  }
  int width = (int)recipe->width, height = (int)recipe->height;
  int aligned_width = (width + M7_NP1_GRID - 1) / M7_NP1_GRID * M7_NP1_GRID;
  AVRegionOfInterest previous = {0};
  unsigned negative = 0;
  for (uint32_t i = 0; i < count; i++) {
    AVRegionOfInterest roi = m7_np1_decode_roi(rois + i * M7_NP1_ROI_BYTES);
    const bool full = roi.left == 0 && roi.top == 0 && roi.right == width && roi.bottom == height;
    const int q = roi.qoffset.num, den = roi.qoffset.den;
    if (recipe->mode == 1) {
      m7_np1_need(count == 1 && full && q == 0 && den == 1, "exact zero gate ROI");
    } else if (q >= 0) {
      m7_np1_need(i + 1 == count && full && q <= 6000000 && den == 63000000,
                  "last full-frame balancing region");
    } else {
      const bool canonical_offset = (q == -6 && den == 63) ||
                                    (q > -6000000 && den == 63000000);
      m7_np1_need(canonical_offset && roi.top < height &&
                      roi.bottom == roi.top + M7_NP1_GRID && !(roi.top % M7_NP1_GRID) &&
                      roi.left < width && roi.left < roi.right && roi.right <= aligned_width &&
                      !(roi.left % M7_NP1_GRID) && !(roi.right % M7_NP1_GRID) &&
                      (!i || roi.top > previous.top ||
                       (roi.top == previous.top && roi.left >= previous.right &&
                        (roi.left != previous.right || q != previous.qoffset.num ||
                         den != previous.qoffset.den))),
                  "ordered, merged, bounded negative ROI");
      negative++;
    }
    previous = roi;
  }
  m7_np1_need(recipe->mode != 1 || count == 1, "zero gate frame ROI coverage");
  return negative;
}

static void m7_np1_parse_plan(struct m7_np1_recipe_read *plan) {
  off_t extent = m7_np1_input_size(M7_NP1_PLAN_FD);
  plan->plan_extent = extent;
  const size_t roi_capacity = M7_NP1_MAX_ROIS * M7_NP1_ROI_BYTES;
  m7_np1_need((uint64_t)extent <= M7_NP1_RECIPE_HEADER +
                  M7_NP1_FRAMES * (M7_NP1_FRAME_ROW + roi_capacity) + M7_NP1_TRAILER,
              "bounded plan file");
  struct AVSHA *sha = m7_np1_sha_new();
  off_t at = 0;
  m7_np1_plan_bytes(plan->recipe.wire, sizeof(plan->recipe.wire), &at, extent, sha);
  m7_np1_validate_recipe(&plan->recipe);
  uint64_t first_ordinal = 0;
  int64_t previous_pts = INT64_MIN;
  uint32_t time_num = 0, time_den = 0;
  uint64_t negative = 0;
  uint8_t *rois = av_malloc(roi_capacity);
  m7_np1_need(rois != NULL, "bounded ROI workspace");
  for (unsigned i = 0; i < M7_NP1_FRAMES; i++) {
    struct m7_np1_row *row = &plan->rows[i];
    uint8_t *w = row->wire;
    m7_np1_plan_bytes(w, sizeof(row->wire), &at, extent, sha);
    uint64_t ordinal = AV_RL64(w);
    int64_t pts = (int64_t)AV_RL64(w + 8), duration = (int64_t)AV_RL64(w + 16);
    if (!i) {
      first_ordinal = ordinal;
      time_num = AV_RL32(w + 24);
      time_den = AV_RL32(w + 28);
    }
    m7_np1_need(ordinal <= 999999 && ordinal == first_ordinal + i &&
                    pts > previous_pts && duration > 0 && pts <= INT64_MAX - duration &&
                    time_num > 0 && time_num <= INT_MAX && time_den > 0 && time_den <= INT_MAX &&
                    AV_RL32(w + 24) == time_num && AV_RL32(w + 28) == time_den &&
                    AV_RL64(w + 32) == (uint64_t)i * plan->recipe.frame_ticks,
                "exact source ordinal and two clock domains");
    previous_pts = pts;
    row->roi_count = AV_RL32(w + 40);
    uint32_t blocks = (plan->recipe.width + M7_NP1_GRID - 1) / M7_NP1_GRID *
                      ((plan->recipe.height + M7_NP1_GRID - 1) / M7_NP1_GRID);
    m7_np1_need(row->roi_count <= blocks + 1, "bounded ROI count");
    for (unsigned j = 0; j < 5; j++)
      m7_np1_need(!m7_np1_zero_hash(w + 44 + j * M7_NP1_HASH_BYTES), "exact frame provenance");
    row->roi_offset = at;
    size_t bytes = (size_t)row->roi_count * M7_NP1_ROI_BYTES;
    m7_np1_plan_bytes(rois, bytes, &at, extent, sha);
    negative += m7_np1_validate_rois(&plan->recipe, rois, row->roi_count);
    m7_np1_hash(rois, bytes, row->roi_sha);
    plan->roi_total += row->roi_count;
  }
  av_free(rois);
  uint8_t trailer[M7_NP1_TRAILER];
  m7_np1_plan_bytes(trailer, sizeof(trailer), &at, extent, sha);
  m7_np1_need(!memcmp(trailer, "END1", 4) && AV_RL32(trailer + 4) == 16 &&
                  AV_RL64(trailer + 8) == M7_NP1_FRAMES &&
                  AV_RL64(trailer + 16) == plan->roi_total && at == extent &&
                  (plan->recipe.mode != 2 || negative > 0),
              "exact completed plan and nonempty candidate ROI");
  av_sha_final(sha, plan->plan_sha);
  av_free(sha);
}

static void m7_np1_dict(AVDictionary **options, const char *name, const char *value) {
  m7_np1_av(av_dict_set(options, name, value, 0), "fixed avcodec option");
}

static AVDictionary *m7_np1_options(const struct m7_np1_recipe *recipe,
                                   uint8_t digest[M7_NP1_HASH_BYTES]) {
  AVDictionary *options = NULL;
  char x265[1024], light[40] = "", bitrate[20];
  if (recipe->light_present) {
    int n = snprintf(light, sizeof(light), ":max-cll=%u,%u",
                     (unsigned)recipe->cll, (unsigned)recipe->fall);
    m7_np1_need(n > 0 && (size_t)n < sizeof(light), "content light string");
  }
  const uint16_t *p = recipe->primaries;
  int n = snprintf(
      x265, sizeof(x265),
      "hdr10=1:hdr10-opt=0:aq-mode=2:repeat-headers=1:open-gop=0:bframes=0:"
      "level-idc=5.1:high-tier=1:hrd=1:chromaloc=2:scenecut=0:pools=none:"
      "frame-threads=1:wpp=0:master-display=G(%u,%u)B(%u,%u)R(%u,%u)WP(%u,%u)L(%u,%u)"
      "%s:cbqpoffs=-9:crqpoffs=-9:qg-size=32%s",
      (unsigned)p[2], (unsigned)p[3], (unsigned)p[4], (unsigned)p[5],
      (unsigned)p[0], (unsigned)p[1], (unsigned)p[6], (unsigned)p[7],
      recipe->max_luminance, recipe->min_luminance, light,
      recipe->mode == 2 ? ":cbqpoffs=-9:crqpoffs=-12:aq-strength=0" : "");
  m7_np1_need(n > 0 && (size_t)n < sizeof(x265), "fixed x265 dictionary");
  n = snprintf(bitrate, sizeof(bitrate), "%uk", recipe->target);
  m7_np1_need(n > 0 && (size_t)n < sizeof(bitrate), "ABR string");
  const struct { const char *name, *value; } entries[] = {
      {"preset", "slow"}, {"profile", "main10"}, {"x265-params", x265}, {"b", bitrate},
      {"maxrate", "125000k"}, {"bufsize", "136000k"}, {"bf", "0"}, {"flags", "+cgop"},
      {"g", "48"}, {"keyint_min", "48"}, {"sc_threshold", "0"},
      {"color_primaries", "bt2020"}, {"color_trc", "smpte2084"}, {"colorspace", "bt2020nc"},
      {"color_range", "tv"}, {"threads", "1"},
  };
  struct AVSHA *sha = m7_np1_sha_new();
  for (unsigned i = 0; i < sizeof(entries) / sizeof(entries[0]); i++) {
    m7_np1_dict(&options, entries[i].name, entries[i].value);
    av_sha_update(sha, (const uint8_t *)entries[i].name, strlen(entries[i].name) + 1);
    av_sha_update(sha, (const uint8_t *)entries[i].value, strlen(entries[i].value) + 1);
  }
  av_sha_final(sha, digest);
  av_free(sha);
  return options;
}

static AVCodecContext *m7_np1_open_codec(const struct m7_np1_recipe *recipe,
                                        uint8_t opts_sha[M7_NP1_HASH_BYTES],
                                        uint32_t *unused) {
  const AVCodec *codec = avcodec_find_encoder_by_name("libx265");
  m7_np1_need(codec && codec->id == AV_CODEC_ID_HEVC, "pinned libx265 encoder is unavailable");
  AVCodecContext *enc = avcodec_alloc_context3(codec);
  m7_np1_need(enc != NULL, "encoder allocation");
  enc->width = (int)recipe->width;
  enc->height = (int)recipe->height;
  enc->pix_fmt = AV_PIX_FMT_YUV420P10LE;
  enc->time_base = (AVRational){1, (int)recipe->timescale};
  enc->framerate = (AVRational){(int)recipe->timescale, (int)recipe->frame_ticks};
  enc->sample_aspect_ratio = (AVRational){0, 1};
  enc->color_range = AVCOL_RANGE_MPEG;
  enc->color_primaries = AVCOL_PRI_BT2020;
  enc->color_trc = AVCOL_TRC_SMPTE2084;
  enc->colorspace = AVCOL_SPC_BT2020_NCL;
  enc->field_order = AV_FIELD_PROGRESSIVE;
  enc->bits_per_raw_sample = 10;
  // The supplied durations are in enc->time_base; libavcodec otherwise clears them.
  enc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER | AV_CODEC_FLAG_FRAME_DURATION;
  AVDictionary *options = m7_np1_options(recipe, opts_sha);
  m7_np1_av(avcodec_open2(enc, codec, &options), "open pinned libx265 encoder");
  const AVDictionaryEntry *entry = NULL;
  while ((entry = av_dict_iterate(options, entry))) {
    m7_np1_need(!strcmp(entry->key, "sc_threshold") && !strcmp(entry->value, "0"),
                "unrecognized avcodec option");
    *unused |= 1;
  }
  av_dict_free(&options);
  m7_np1_need(enc->thread_count == 1 && enc->gop_size == M7_NP1_FRAMES &&
                  !enc->max_b_frames && enc->bit_rate == (int64_t)recipe->target * 1000 &&
                  enc->rc_max_rate == (int64_t)M7_NP1_PEAK_ABR * 1000 &&
                  enc->rc_buffer_size == M7_NP1_BUF_KBIT * 1000 &&
                  ((unsigned)enc->flags & AV_CODEC_FLAG_CLOSED_GOP) &&
                  enc->extradata && enc->extradata_size > 0 && enc->extradata_size <= 65536,
              "effective bounded encoder context");
  return enc;
}

static int m7_np1_mp4_write(void *opaque, const uint8_t *bytes, int size) {
  struct m7_np1_output *stream = opaque;
  if (size < 0 || size > M7_NP1_MAX_OUTPUT - stream->position)
    return AVERROR(ENOSPC);
  int written = 0;
  while (written < size) {
    ssize_t count = pwrite(stream->fd, bytes + written, (size_t)(size - written),
                           (off_t)(stream->position + written));
    if (count < 0 && errno == EINTR) continue;
    if (count <= 0) return AVERROR(count < 0 ? errno : EIO);
    written += (int)count;
  }
  stream->position += size;
  if (stream->extent < stream->position)
    stream->extent = stream->position;
  return size;
}

static int m7_np1_mp4_read(void *opaque, uint8_t *bytes, int size) {
  struct m7_np1_output *stream = opaque;
  if (size <= 0) return AVERROR(EINVAL);
  int64_t remaining = stream->extent - stream->position;
  if (remaining <= 0) return AVERROR_EOF;
  if (size > remaining) size = (int)remaining;
  if (size > 2LL * M7_NP1_MAX_OUTPUT - stream->read_bytes) return AVERROR(EFBIG);
  ssize_t count;
  do {
    count = pread(stream->fd, bytes, (size_t)size, (off_t)stream->position);
  } while (count < 0 && errno == EINTR);
  if (count <= 0) return AVERROR(count < 0 ? errno : EIO);
  stream->position += count;
  stream->read_bytes += count;
  return (int)count;
}

static int64_t m7_np1_mp4_seek(void *opaque, int64_t offset, int whence) {
  struct m7_np1_output *stream = opaque;
  whence &= ~AVSEEK_FORCE;
  if (whence == AVSEEK_SIZE) return stream->extent;
  int64_t origin;
  switch (whence) {
    case SEEK_SET: origin = 0; break;
    case SEEK_CUR: origin = stream->position; break;
    case SEEK_END: origin = stream->extent; break;
    default: return AVERROR(EINVAL);
  }
  if (offset < -origin || offset > M7_NP1_MAX_OUTPUT - origin)
    return AVERROR(EINVAL);
  stream->position = origin + offset;
  return stream->position;
}

static AVIOContext *m7_np1_avio(struct m7_np1_output *out, bool writing) {
  uint8_t *buffer = av_malloc(M7_NP1_AVIO_BUFFER);
  m7_np1_need(buffer != NULL, "focused AVIO buffer");
  AVIOContext *io = avio_alloc_context(buffer, M7_NP1_AVIO_BUFFER, writing, out,
                                     writing ? NULL : m7_np1_mp4_read,
                                     writing ? m7_np1_mp4_write : NULL, m7_np1_mp4_seek);
  m7_np1_need(io != NULL, "held MP4 AVIO");
  return io;
}

static void m7_np1_free_avio(AVIOContext **io) {
  av_freep(&(*io)->buffer);
  avio_context_free(io);
}

static AVFormatContext *m7_np1_open_mux(const AVCodecContext *enc, AVIOContext *io) {
  AVFormatContext *muxer = NULL;
  m7_np1_av(avformat_alloc_output_context2(&muxer, NULL, "mp4", NULL), "MP4 muxer");
  m7_np1_need(muxer != NULL, "MP4 output context");
  muxer->pb = io;
  muxer->flags |= AVFMT_FLAG_CUSTOM_IO;
  AVStream *video = avformat_new_stream(muxer, NULL);
  m7_np1_need(video != NULL, "one video track");
  m7_np1_av(avcodec_parameters_from_context(video->codecpar, enc), "encoded sample entry");
  video->codecpar->codec_tag = MKTAG('h', 'v', 'c', '1');
  video->time_base = enc->time_base;
  char timescale[16];
  int n = snprintf(timescale, sizeof(timescale), "%d", enc->time_base.den);
  m7_np1_need(n > 0 && (size_t)n < sizeof(timescale), "exact MP4 video timescale");
  AVDictionary *options = NULL;
  m7_np1_dict(&options, "movflags", "+empty_moov+default_base_moof+frag_keyframe+frag_discont");
  m7_np1_dict(&options, "video_track_timescale", timescale);
  m7_np1_av(avformat_write_header(muxer, &options), "fragmented MP4 init");
  m7_np1_need(!av_dict_count(options) && !av_cmp_q(video->time_base, enc->time_base),
              "effective mux clock and options");
  av_dict_free(&options);
  return muxer;
}

static void m7_np1_fill_frame(AVFrame *frame, const struct m7_np1_recipe *recipe,
                              const struct m7_np1_row *row, unsigned index,
                              uint8_t *roi_bytes) {
  m7_np1_av(av_frame_make_writable(frame), "one writable input frame");
  struct AVSHA *sha = m7_np1_sha_new();
  off_t offset = (off_t)index * recipe->width * recipe->height * 3;
  for (unsigned plane = 0; plane < 3; plane++) {
    unsigned width = recipe->width, height = recipe->height;
    if (plane) {
      width /= 2;
      height /= 2;
    }
    for (unsigned y = 0; y < height; y++) {
      uint8_t *bytes = frame->data[plane] + (size_t)y * (size_t)frame->linesize[plane];
      m7_np1_pread(M7_NP1_PLANES_FD, bytes, (size_t)width * 2, offset);
      offset += (off_t)width * 2;
      av_sha_update(sha, bytes, (size_t)width * 2);
      for (unsigned x = 0; x < width; x++)
        m7_np1_need(AV_RL16(bytes + 2 * x) <= 1023, "ten-bit encoder input code");
    }
  }
  uint8_t digest[M7_NP1_HASH_BYTES];
  av_sha_final(sha, digest);
  av_free(sha);
  m7_np1_need(!memcmp(digest, row->wire + 172, sizeof(digest)),
              "held composed frame matches its exact input SHA-256");
  frame->pts = (int64_t)index * recipe->frame_ticks;
  frame->duration = recipe->frame_ticks;
  frame->pict_type = index ? AV_PICTURE_TYPE_NONE : AV_PICTURE_TYPE_I;
  frame->color_range = AVCOL_RANGE_MPEG;
  frame->color_primaries = AVCOL_PRI_BT2020;
  frame->color_trc = AVCOL_TRC_SMPTE2084;
  frame->colorspace = AVCOL_SPC_BT2020_NCL;
  av_frame_remove_side_data(frame, AV_FRAME_DATA_REGIONS_OF_INTEREST);
  if (row->roi_count) {
    size_t extent = (size_t)row->roi_count * M7_NP1_ROI_BYTES;
    m7_np1_pread(M7_NP1_PLAN_FD, roi_bytes, extent, row->roi_offset);
    m7_np1_hash(roi_bytes, extent, digest);
    m7_np1_need(!memcmp(digest, row->roi_sha, sizeof(digest)), "held ordered ROI bytes");
    AVFrameSideData *side = av_frame_new_side_data(
        frame, AV_FRAME_DATA_REGIONS_OF_INTEREST, (size_t)row->roi_count * sizeof(AVRegionOfInterest));
    m7_np1_need(side != NULL, "per-frame ordered ROI side data");
    AVRegionOfInterest *regions = (AVRegionOfInterest *)side->data;
    for (unsigned i = 0; i < row->roi_count; i++)
      regions[i] = m7_np1_decode_roi(roi_bytes + i * M7_NP1_ROI_BYTES);
  }
}

static void m7_np1_drain(AVCodecContext *enc, AVFormatContext *mux, AVPacket *packet,
                         struct m7_np1_recipe_read *plan, unsigned *count, bool flushing) {
  int status;
  const struct m7_np1_recipe *recipe = &plan->recipe;
  while ((status = avcodec_receive_packet(enc, packet)) >= 0) {
    int64_t pts = (int64_t)*count * recipe->frame_ticks;
    m7_np1_need(*count < M7_NP1_FRAMES && packet->size > 0 &&
                    packet->size <= M7_NP1_MAX_OUTPUT && packet->pts == pts && packet->dts == pts &&
                    packet->duration == recipe->frame_ticks &&
                    (!!(packet->flags & AV_PKT_FLAG_KEY) == (*count == 0)),
                "one closed-GOP encoder packet per exact frame");
    m7_np1_hash(packet->data, (size_t)packet->size, plan->rows[*count].raw_au_sha);
    packet->stream_index = 0;
    m7_np1_av(av_interleaved_write_frame(mux, packet), "write encoded AU");
    (*count)++;
  }
  m7_np1_need(status == (flushing ? AVERROR_EOF : AVERROR(EAGAIN)), "complete encoder drain");
}

static void m7_np1_encode(struct m7_np1_recipe_read *plan, AVCodecContext *enc,
                          struct m7_np1_output *out) {
  AVIOContext *io = m7_np1_avio(out, true);
  AVFormatContext *mux = m7_np1_open_mux(enc, io);
  AVFrame *frame = av_frame_alloc();
  AVPacket *packet = av_packet_alloc();
  uint8_t *roi_bytes = av_malloc(M7_NP1_MAX_ROIS * M7_NP1_ROI_BYTES);
  m7_np1_need(frame && packet && roi_bytes, "bounded encoder working set");
  frame->format = enc->pix_fmt;
  frame->width = enc->width;
  frame->height = enc->height;
  frame->sample_aspect_ratio = enc->sample_aspect_ratio;
  m7_np1_av(av_frame_get_buffer(frame, 32), "one packed input frame");
  unsigned count = 0;
  for (unsigned i = 0; i < M7_NP1_FRAMES; i++) {
    m7_np1_fill_frame(frame, &plan->recipe, &plan->rows[i], i, roi_bytes);
    m7_np1_av(avcodec_send_frame(enc, frame), "submit composed pixels with ROI");
    m7_np1_drain(enc, mux, packet, plan, &count, false);
  }
  m7_np1_av(avcodec_send_frame(enc, NULL), "flush encoder");
  m7_np1_drain(enc, mux, packet, plan, &count, true);
  m7_np1_need(count == M7_NP1_FRAMES, "exact encoded frame total");
  m7_np1_av(av_write_trailer(mux), "complete MP4 trailer");
  avio_flush(io);
  m7_np1_av(io->error, "flush bounded MP4 output");
  m7_np1_need(out->extent > 0, "nonempty completed MP4");
  av_free(roi_bytes);
  av_packet_free(&packet);
  av_frame_free(&frame);
  avformat_free_context(mux);
  m7_np1_free_avio(&io);
}

static void m7_np1_vcl_sha(const AVPacket *packet, uint8_t digest[M7_NP1_HASH_BYTES]) {
  struct AVSHA *sha = m7_np1_sha_new();
  unsigned first_slices = 0;
  size_t at = 0, size = (size_t)packet->size;
  while (at < size) {
    m7_np1_need(size - at >= 4, "complete muxed NAL length");
    size_t length = AV_RB32(packet->data + at);
    m7_np1_need(length >= 2 && length <= size - at - 4, "bounded muxed NAL");
    const uint8_t *nal = packet->data + at + 4;
    unsigned type = (nal[0] >> 1) & 63;
    m7_np1_need(!(nal[0] & 0x81) && !(nal[1] >> 3) && (nal[1] & 7) &&
                    type != 62 && type != 63,
                "single-layer coded HDR10 NAL");
    if (type <= 31) {
      m7_np1_need(length >= 3, "complete coded VCL header");
      first_slices += !!(nal[2] & 0x80);
      av_sha_update(sha, packet->data + at, 4 + length);
    }
    at += 4 + length;
  }
  m7_np1_need(first_slices == 1, "one independent coded picture per packet");
  av_sha_final(sha, digest);
  av_free(sha);
}

static uint64_t m7_np1_readback(struct m7_np1_recipe_read *plan, struct m7_np1_output *out,
                                 uint8_t extra_sha[M7_NP1_HASH_BYTES]) {
  out->position = 0;
  AVIOContext *io = m7_np1_avio(out, false);
  const AVInputFormat *mov = av_find_input_format("mov");
  AVFormatContext *input = avformat_alloc_context();
  m7_np1_need(mov && input, "held MP4 readback");
  input->pb = io;
  input->flags |= AVFMT_FLAG_CUSTOM_IO;
  m7_np1_av(avformat_open_input(&input, NULL, mov, NULL), "read complete encoded MP4");
  m7_np1_need(input->nb_streams == 1, "MP4 has one video track");
  const AVStream *stream = input->streams[0];
  const AVCodecParameters *codec = stream->codecpar;
  const struct m7_np1_recipe *recipe = &plan->recipe;
  m7_np1_need(codec->codec_type == AVMEDIA_TYPE_VIDEO && codec->codec_id == AV_CODEC_ID_HEVC &&
                  codec->codec_tag == MKTAG('h', 'v', 'c', '1') &&
                  codec->width == (int)recipe->width && codec->height == (int)recipe->height &&
                  stream->time_base.num == 1 && stream->time_base.den == (int)recipe->timescale &&
                  codec->extradata_size >= 23 && codec->extradata_size <= 65536 &&
                  codec->extradata && codec->extradata[0] == 1 &&
                  (codec->extradata[21] & 3) == 3,
              "muxed HEVC sample entry, raster, length size and clock");
  m7_np1_hash(codec->extradata, (size_t)codec->extradata_size, extra_sha);
  AVPacket *packet = av_packet_alloc();
  m7_np1_need(packet != NULL, "readback packet");
  unsigned count = 0;
  uint64_t total = 0;
  int status;
  while ((status = av_read_frame(input, packet)) >= 0) {
    int64_t pts = (int64_t)count * recipe->frame_ticks;
    m7_np1_need(count < M7_NP1_FRAMES && packet->stream_index == 0 && packet->size > 0 &&
                    (uint64_t)packet->size <= (uint64_t)out->extent - total &&
                    packet->pts == pts && packet->dts == pts && packet->duration == recipe->frame_ticks &&
                    (!!(packet->flags & AV_PKT_FLAG_KEY) == (count == 0)),
                "one held muxed packet per exact encoder frame");
    struct m7_np1_row *row = &plan->rows[count++];
    row->packet_bytes = (uint32_t)packet->size;
    row->packet_flags = !!(packet->flags & AV_PKT_FLAG_KEY);
    total += row->packet_bytes;
    m7_np1_hash(packet->data, (size_t)packet->size, row->mux_au_sha);
    m7_np1_vcl_sha(packet, row->mux_vcl_sha);
    av_packet_unref(packet);
  }
  m7_np1_need(status == AVERROR_EOF && count == M7_NP1_FRAMES && !io->error,
              "complete held MP4 readback");
  av_packet_free(&packet);
  avformat_close_input(&input);
  m7_np1_free_avio(&io);
  return total;
}

static void m7_np1_output_sha(const struct m7_np1_output *out,
                                uint8_t digest[M7_NP1_HASH_BYTES]) {
  struct stat facts;
  m7_np1_need(fstat(out->fd, &facts) == 0 && S_ISREG(facts.st_mode) &&
                  facts.st_size == out->extent && out->extent <= M7_NP1_MAX_OUTPUT,
              "exact encoded file extent");
  struct AVSHA *sha = m7_np1_sha_new();
  uint8_t buffer[M7_NP1_AVIO_BUFFER];
  for (int64_t offset = 0; offset < out->extent;) {
    int64_t remaining = out->extent - offset;
    size_t bytes = remaining < M7_NP1_AVIO_BUFFER ? (size_t)remaining : sizeof(buffer);
    m7_np1_pread(out->fd, buffer, bytes, (off_t)offset);
    av_sha_update(sha, buffer, bytes);
    offset += (int64_t)bytes;
  }
  av_sha_final(sha, digest);
  av_free(sha);
}

static void m7_np1_write(int fd, const void *bytes, size_t size) {
  const uint8_t *data = bytes;
  size_t written = 0;
  while (written < size) {
    ssize_t count = write(fd, data + written, size - written);
    if (count < 0 && errno == EINTR) continue;
    m7_np1_need(count > 0, "complete receipt write");
    written += (size_t)count;
  }
}

static void m7_np1_chunk(int fd, const char tag[4], const uint8_t *bytes, uint32_t size) {
  uint8_t header[8];
  memcpy(header, tag, 4);
  AV_WL32(header + 4, size);
  m7_np1_write(fd, header, sizeof(header));
  m7_np1_write(fd, bytes, size);
}

static void m7_np1_receipt(int fd, const struct m7_np1_recipe_read *plan,
                          const uint8_t config[M7_NP1_CFG_BYTES]) {
  m7_np1_write(fd, "PLXM7E01", 8);
  m7_np1_write(fd, plan->recipe.wire + 8, M7_NP1_RECIPE_HEADER - 8);
  m7_np1_chunk(fd, "CFG1", config, M7_NP1_CFG_BYTES);
  for (unsigned i = 0; i < M7_NP1_FRAMES; i++) {
    const struct m7_np1_row *row = &plan->rows[i];
    uint8_t body[M7_NP1_AU_BYTES];
    memcpy(body, row->wire, M7_NP1_FRAME_ROW);
    memcpy(body + 204, row->roi_sha, M7_NP1_HASH_BYTES);
    memcpy(body + 236, row->raw_au_sha, M7_NP1_HASH_BYTES);
    memcpy(body + 268, row->mux_au_sha, M7_NP1_HASH_BYTES);
    memcpy(body + 300, row->mux_vcl_sha, M7_NP1_HASH_BYTES);
    int64_t pts = (int64_t)i * plan->recipe.frame_ticks;
    AV_WL64(body + 332, (uint64_t)pts);
    AV_WL64(body + 340, (uint64_t)pts);
    AV_WL64(body + 348, plan->recipe.frame_ticks);
    AV_WL32(body + 356, row->packet_bytes);
    AV_WL32(body + 360, row->packet_flags);
    m7_np1_chunk(fd, "AU01", body, sizeof(body));
  }
  uint8_t trailer[16];
  AV_WL64(trailer, M7_NP1_FRAMES);
  AV_WL64(trailer + 8, plan->roi_total);
  m7_np1_chunk(fd, "END1", trailer, sizeof(trailer));
}

static int m7_np1_main(void) {
  av_log_set_level(AV_LOG_VERBOSE);
  av_max_alloc(M7_NP1_MAX_OUTPUT);
  struct stat dir;
  m7_np1_need(fstat(M7_NP1_DIRECTORY_FD, &dir) == 0 && S_ISDIR(dir.st_mode),
              "held job-local output directory");
  struct m7_np1_recipe_read plan = {0};
  m7_np1_parse_plan(&plan);
  const uint64_t plane_bytes = (uint64_t)plan.recipe.width * plan.recipe.height * 3 * M7_NP1_FRAMES;
  m7_np1_need((uint64_t)m7_np1_input_size(M7_NP1_PLANES_FD) == plane_bytes,
              "exact complete composed file size");
  uint8_t cfg[M7_NP1_CFG_BYTES];
  uint32_t unused = 0;
  memcpy(cfg + 64, plan.plan_sha, M7_NP1_HASH_BYTES);
  AVCodecContext *enc = m7_np1_open_codec(&plan.recipe, cfg + 96, &unused);
  m7_np1_hash(enc->extradata, (size_t)enc->extradata_size, cfg + 128);
  AV_WL32(cfg, avcodec_version());
  AV_WL32(cfg + 4, avformat_version());
  AV_WL32(cfg + 8, avutil_version());
  AV_WL32(cfg + 12, unused);
  AV_WL32(cfg + 16, enc->thread_count);
  AV_WL32(cfg + 20, enc->gop_size);
  AV_WL32(cfg + 24, enc->max_b_frames);
  AV_WL32(cfg + 28, enc->rc_buffer_size);
  AV_WL64(cfg + 32, (uint64_t)enc->bit_rate);
  AV_WL64(cfg + 40, (uint64_t)enc->rc_max_rate);
  const int exclusive = O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC;
  struct m7_np1_output out = {
      .fd = openat(M7_NP1_DIRECTORY_FD, "encoded.mp4", O_RDWR | exclusive, 0600),
  };
  m7_np1_need(out.fd >= 0, "exclusive encoded MP4 output");
  int receipt = openat(M7_NP1_DIRECTORY_FD, "encode.receipt", O_WRONLY | exclusive, 0600);
  m7_np1_need(receipt >= 0, "exclusive encoder receipt output");
  m7_np1_encode(&plan, enc, &out);
  avcodec_free_context(&enc);
  uint64_t payload = m7_np1_readback(&plan, &out, cfg + 160);
  AV_WL64(cfg + 48, (uint64_t)out.extent);
  AV_WL64(cfg + 56, payload);
  m7_np1_output_sha(&out, cfg + 192);
  const char *configuration = avcodec_configuration();
  m7_np1_hash(configuration, strlen(configuration), cfg + 224);
  m7_np1_need((uint64_t)m7_np1_input_size(M7_NP1_PLANES_FD) == plane_bytes &&
                  m7_np1_input_size(M7_NP1_PLAN_FD) == plan.plan_extent && close(out.fd) == 0,
              "complete unchanged input extent and closed MP4");
  m7_np1_receipt(receipt, &plan, cfg);
  m7_np1_need(close(receipt) == 0, "complete closed encoder receipt");
  return 0;
}

#endif
